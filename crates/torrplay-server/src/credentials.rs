// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! The password, access tokens, and playback tokens. They live in the store
//! and need no engine.
//!
//! Password checks and credential changes take the credential lane, so one
//! password is hashed at a time and a check cannot race a change; hashing
//! runs on a blocking thread.

use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use axum::Extension;
use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use serde_json::{Map, Value};
use torrplay_api as api;
use torrplay_core::credentials::{
    ACCESS_TOKEN_LIFETIME, ACCESS_TOKEN_LIMIT, AccessToken, Grant, PLAYBACK_TOKEN_DEFAULT_LIFETIME,
    PLAYBACK_TOKEN_LIFETIME, PasswordHash, PlaybackGrant, Scope, SigningKey, new_secret,
    normalize_password, normalize_token_name,
};
use torrplay_core::engine::Engine;
use torrplay_core::id::{AccessTokenId, random_torrent_id_bytes};
use torrplay_core::pagination::{
    AccessTokenSortKey, CURSOR_PARAMETER, FilterFingerprint, PAGE_SIZE_PARAMETER, PageRequest,
    paginate,
};

use crate::app::{App, store_failed};
use crate::authentication::Caller;
use crate::body::{JSON_LIMIT_BYTES, read_limited};
use crate::headers::{MediaType, negotiate};
use crate::problem::{InvalidValue, Problem, ProblemType};
use crate::query::{Paging, Query, page_error, reject};
use crate::representation::{BASE, JSON, NO_CACHE, NO_STORE, json};
use crate::torrents::{object, parse_json, pointer, unknown_member};

pub(crate) fn password_path() -> String {
    format!("{BASE}/password")
}

pub(crate) fn access_tokens_path() -> String {
    format!("{BASE}/access-tokens")
}

pub(crate) fn playback_tokens_path() -> String {
    format!("{BASE}/playback-tokens")
}

fn access_token_path(id: &AccessTokenId) -> String {
    format!("{BASE}/access-tokens/{id}")
}

fn no_password() -> Problem {
    Problem::not_found("No password is set, so authentication is off.")
}

fn unknown_token() -> Problem {
    Problem::not_found("No access token has this ID.")
}

fn password(updated_at: SystemTime) -> api::Password {
    api::Password {
        updated_at: updated_at.into(),
        links: api::PasswordLinks {
            self_field: password_path(),
        },
    }
}

fn scope(scope: Scope) -> api::AccessTokenScope {
    match scope {
        Scope::Api => api::AccessTokenScope::Api,
        Scope::Playback => api::AccessTokenScope::Playback,
        Scope::Metrics => api::AccessTokenScope::Metrics,
    }
}

fn access_token(token: &AccessToken) -> api::AccessToken {
    api::AccessToken {
        id: token.id.to_string(),
        name: token.name.clone(),
        scope: scope(token.scope),
        created_at: token.created_at.into(),
        expires_at: token.expires_at.map(Into::into),
        last_used_at: token.last_used_at.map(Into::into),
        links: api::AccessTokenLinks {
            self_field: access_token_path(&token.id),
        },
    }
}

/// Reads a JSON body of at most the JSON limit as an object.
async fn json_body<E: Engine>(
    app: &App<E>,
    headers: &HeaderMap,
    body: Body,
) -> Result<Map<String, Value>, Problem> {
    if MediaType::of_request(headers).is_none_or(|media| media.essence != JSON) {
        return Err(Problem::new(
            ProblemType::UnsupportedMediaType,
            "The operation accepts application/json.",
        )
        .with_header(header::ACCEPT, HeaderValue::from_static(JSON)));
    }
    let bytes = read_limited(
        headers,
        body,
        JSON_LIMIT_BYTES,
        JSON_LIMIT_BYTES as u64,
        app.body_timeout(),
    )
    .await?;
    object(parse_json(&bytes)?)
}

/// Reads a string member, adding an error if it is not one.
fn string<'a>(member: &str, value: &'a Value, errors: &mut Vec<InvalidValue>) -> Option<&'a str> {
    let text = value.as_str();
    if text.is_none() {
        errors.push(InvalidValue::pointer(pointer(member), "Must be a string."));
    }
    text
}

fn required(member: &str, found: bool, errors: &mut Vec<InvalidValue>) {
    if !found {
        errors.push(InvalidValue::pointer(pointer(member), "Is required."));
    }
}

/// Whole seconds from a member, within `range`, adding an error otherwise.
fn lifetime(
    member: &str,
    value: &Value,
    range: &std::ops::RangeInclusive<Duration>,
    errors: &mut Vec<InvalidValue>,
) -> Option<Duration> {
    let lifetime = value
        .as_u64()
        .map(Duration::from_secs)
        .filter(|lifetime| range.contains(lifetime));
    if lifetime.is_none() {
        errors.push(InvalidValue::pointer(
            pointer(member),
            format!(
                "Must be an integer from {} to {}.",
                range.start().as_secs(),
                range.end().as_secs()
            ),
        ));
    }
    lifetime
}

fn checked(errors: Vec<InvalidValue>) -> Result<(), Problem> {
    if errors.is_empty() {
        Ok(())
    } else {
        Err(Problem::invalid_body(errors))
    }
}

/// Runs blocking password work off the async threads.
async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> T + Send + 'static,
) -> Result<T, Problem> {
    tokio::task::spawn_blocking(work).await.map_err(|error| {
        tracing::error!(%error, "password hashing panicked");
        Problem::internal()
    })
}

/// Checks `given` against the password under the attempt limit. The caller
/// holds the credential lane.
async fn check_password<E: Engine>(app: &App<E>, given: String) -> Result<(), Problem> {
    if let Err(wait) = app.limiter().check(Instant::now()) {
        // Whole seconds, rounded up so a retry is never early.
        let seconds = wait.as_secs() + u64::from(wait.subsec_nanos() > 0);
        return Err(Problem::new(
            ProblemType::TooManyAttempts,
            "Too many wrong passwords; try again later.",
        )
        .with_header(header::RETRY_AFTER, HeaderValue::from(seconds.max(1))));
    }
    let (hash, _) = app.credentials().password().ok_or_else(no_password)?;
    if blocking(move || hash.verify(&given)).await? {
        app.limiter().succeeded();
        Ok(())
    } else {
        app.limiter().failed(Instant::now());
        Err(Problem::new(
            ProblemType::InvalidCredentials,
            "The password does not match.",
        ))
    }
}

pub(crate) async fn get_password<E: Engine>(
    State(app): State<Arc<App<E>>>,
    headers: HeaderMap,
) -> Result<Response, Problem> {
    negotiate(&headers, JSON)?;
    let (_, updated_at) = app.credentials().password().ok_or_else(no_password)?;
    let mut response = json(StatusCode::OK, &password(updated_at))?;
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, NO_CACHE);
    Ok(response)
}

pub(crate) async fn set_password<E: Engine>(
    State(app): State<Arc<App<E>>>,
    headers: HeaderMap,
    body: Body,
) -> Result<Response, Problem> {
    negotiate(&headers, JSON)?;
    let members = json_body(&app, &headers, body).await?;
    let mut errors = Vec::new();
    let (mut new, mut current, mut revoke) = (None, None, false);
    for (member, value) in &members {
        match member.as_str() {
            "password" => {
                new = string(member, value, &mut errors).and_then(|text| {
                    normalize_password(text)
                        .map_err(|error| {
                            errors.push(InvalidValue::pointer("/password", error.to_string()));
                        })
                        .ok()
                });
            }
            "current_password" => {
                current = string(member, value, &mut errors).map(str::to_owned);
            }
            "revoke_access_tokens" => match value.as_bool() {
                Some(value) => revoke = value,
                None => errors.push(InvalidValue::pointer(
                    "/revoke_access_tokens",
                    "Must be a boolean.",
                )),
            },
            other => errors.push(unknown_member(other)),
        }
    }
    required("password", members.contains_key("password"), &mut errors);
    checked(errors)?;
    let new = new.ok_or_else(Problem::internal)?;
    app.require_running()?;

    let _lane = app.credential_lane().await;
    let enabled = app.credentials().is_enabled();
    match (enabled, current) {
        (true, None) => {
            return Err(Problem::invalid_body(vec![InvalidValue::pointer(
                "/current_password",
                "Is required while a password is set.",
            )]));
        }
        (false, Some(_)) => {
            return Err(Problem::invalid_body(vec![InvalidValue::pointer(
                "/current_password",
                "Must be omitted while no password is set.",
            )]));
        }
        (true, Some(current)) => check_password(&app, current).await?,
        (false, None) => {}
    }
    let hash = blocking(move || PasswordHash::new(&new))
        .await?
        .map_err(|error| {
            tracing::error!(%error, "a password could not be hashed");
            Problem::internal()
        })?;
    let now = app.now();
    app.store()
        .set_password(&hash, now, revoke)
        .await
        .map_err(store_failed)?;
    app.credentials().set_password(hash, now);
    if revoke {
        app.credentials().remove_tokens();
    }
    if enabled {
        tracing::info!(revoked = revoke, "the password was changed");
        json(StatusCode::OK, &password(now))
    } else {
        tracing::info!("a password was set; authentication is on");
        let mut response = json(StatusCode::CREATED, &password(now))?;
        response.headers_mut().insert(
            header::LOCATION,
            HeaderValue::try_from(password_path()).map_err(|_| Problem::internal())?,
        );
        Ok(response)
    }
}

pub(crate) async fn delete_password<E: Engine>(
    State(app): State<Arc<App<E>>>,
) -> Result<Response, Problem> {
    app.require_running()?;
    let _lane = app.credential_lane().await;
    if !app.credentials().is_enabled() {
        return Err(no_password());
    }
    let key = SigningKey::new().map_err(|error| {
        tracing::error!(%error, "no random bytes for a signing key");
        Problem::internal()
    })?;
    app.store()
        .remove_password(&key)
        .await
        .map_err(store_failed)?;
    app.credentials().remove_password(key);
    tracing::info!("the password was removed; authentication is off");
    Ok(StatusCode::NO_CONTENT.into_response())
}

pub(crate) async fn list_access_tokens<E: Engine>(
    State(app): State<Arc<App<E>>>,
    uri: Uri,
    headers: HeaderMap,
) -> Result<Response, Problem> {
    negotiate(&headers, JSON)?;
    let query = Query::parse(uri.query());
    let parameters = [PAGE_SIZE_PARAMETER, CURSOR_PARAMETER];
    let mut errors = query.unexpected(&parameters, &parameters);
    let paging = Paging::parse(&query, &mut errors);
    reject(errors)?;
    let request = PageRequest::<AccessTokenSortKey>::new(
        paging.page_size,
        paging.cursor,
        FilterFingerprint::none(),
    )
    .map_err(|error| Problem::malformed(vec![page_error(error)]))?;
    let tokens = app.credentials().tokens(app.now());
    let page = paginate(
        tokens.iter(),
        |token| AccessTokenSortKey::of(&token.id, token.created_at),
        &request,
    );
    let path = access_tokens_path();
    let body = api::AccessTokenPage {
        items: page.items.iter().map(|token| access_token(token)).collect(),
        links: api::AccessTokenPageLinks {
            next: page
                .next_cursor
                .as_deref()
                .map(|cursor| paging.link(&path, &[], Some(cursor))),
            self_field: paging.link(&path, &[], paging.cursor),
        },
    };
    let mut response = json(StatusCode::OK, &body)?;
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, NO_CACHE);
    Ok(response)
}

pub(crate) async fn create_access_token<E: Engine>(
    State(app): State<Arc<App<E>>>,
    headers: HeaderMap,
    body: Body,
) -> Result<Response, Problem> {
    negotiate(&headers, JSON)?;
    let members = json_body(&app, &headers, body).await?;
    let mut errors = Vec::new();
    let (mut given, mut name, mut token_scope, mut lifetime_given) = (None, None, Scope::Api, None);
    for (member, value) in &members {
        match member.as_str() {
            "password" => given = string(member, value, &mut errors).map(str::to_owned),
            "name" => {
                name = string(member, value, &mut errors).and_then(|text| {
                    normalize_token_name(text)
                        .map_err(|error| {
                            errors.push(InvalidValue::pointer("/name", error.to_string()));
                        })
                        .ok()
                });
            }
            "scope" => match value.as_str().and_then(Scope::parse) {
                Some(value) => token_scope = value,
                None => errors.push(InvalidValue::pointer("/scope", "Must be api or playback.")),
            },
            "expires_in_seconds" if value.is_null() => {}
            "expires_in_seconds" => {
                lifetime_given = lifetime(member, value, &ACCESS_TOKEN_LIFETIME, &mut errors);
            }
            other => errors.push(unknown_member(other)),
        }
    }
    required("password", members.contains_key("password"), &mut errors);
    required("name", members.contains_key("name"), &mut errors);
    checked(errors)?;
    let (given, name) = given.zip(name).ok_or_else(Problem::internal)?;
    if !app.credentials().is_enabled() {
        return Err(Problem::new(
            ProblemType::LifecycleConflict,
            "No password is set, so requests need no token.",
        ));
    }
    app.require_running()?;

    let _lane = app.credential_lane().await;
    check_password(&app, given).await?;
    let now = app.now();
    if app.credentials().tokens(now).len() >= ACCESS_TOKEN_LIMIT {
        return Err(Problem::new(
            ProblemType::TooManyAccessTokens,
            format!(
                "The application keeps at most {ACCESS_TOKEN_LIMIT} access tokens; revoke one first."
            ),
        ));
    }
    let random = |error| {
        tracing::error!(%error, "no random bytes for an access token");
        Problem::internal()
    };
    let (secret, digest) = new_secret().map_err(random)?;
    let token = AccessToken {
        id: AccessTokenId::from_random_bytes(random_torrent_id_bytes().map_err(random)?),
        name,
        scope: token_scope,
        created_at: now,
        expires_at: lifetime_given.and_then(|lifetime| now.checked_add(lifetime)),
        last_used_at: None,
        digest,
    };
    let expired = app.credentials().expired(now);
    app.store()
        .insert_access_token(&token, &expired)
        .await
        .map_err(store_failed)?;
    app.credentials().insert_token(token.clone(), &expired);
    tracing::info!(id = %token.id, scope = token.scope.as_str(), "an access token was created");

    let representation = access_token(&token);
    let body = api::CreatedAccessToken {
        id: representation.id,
        name: representation.name,
        scope: representation.scope,
        created_at: representation.created_at,
        expires_at: representation.expires_at,
        last_used_at: representation.last_used_at,
        links: representation.links,
        secret,
    };
    let mut response = json(StatusCode::CREATED, &body)?;
    let values = response.headers_mut();
    values.insert(
        header::LOCATION,
        HeaderValue::try_from(access_token_path(&token.id)).map_err(|_| Problem::internal())?,
    );
    values.insert(header::CACHE_CONTROL, NO_STORE);
    Ok(response)
}

pub(crate) async fn get_access_token<E: Engine>(
    State(app): State<Arc<App<E>>>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Response, Problem> {
    negotiate(&headers, JSON)?;
    let token = app
        .credentials()
        .token(&id, app.now())
        .ok_or_else(unknown_token)?;
    let mut response = json(StatusCode::OK, &access_token(&token))?;
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, NO_CACHE);
    Ok(response)
}

pub(crate) async fn delete_access_token<E: Engine>(
    State(app): State<Arc<App<E>>>,
    Path(id): Path<String>,
) -> Result<Response, Problem> {
    app.require_running()?;
    let _lane = app.credential_lane().await;
    let token = app
        .credentials()
        .token(&id, app.now())
        .ok_or_else(unknown_token)?;
    app.store()
        .delete_access_token(&token.id)
        .await
        .map_err(store_failed)?;
    app.credentials().remove_token(token.id.as_str());
    tracing::info!(id = %token.id, "an access token was revoked");
    Ok(StatusCode::NO_CONTENT.into_response())
}

pub(crate) async fn create_playback_token<E: Engine>(
    State(app): State<Arc<App<E>>>,
    caller: Option<Extension<Caller>>,
    headers: HeaderMap,
    body: Body,
) -> Result<Response, Problem> {
    negotiate(&headers, JSON)?;
    let members = json_body(&app, &headers, body).await?;
    let mut errors = Vec::new();
    let (mut torrent, mut lifetime_given) = (None, PLAYBACK_TOKEN_DEFAULT_LIFETIME);
    for (member, value) in &members {
        match member.as_str() {
            "torrent_id" if value.is_null() => {}
            "torrent_id" => torrent = string(member, value, &mut errors).map(str::to_owned),
            "expires_in_seconds" => {
                if let Some(value) = lifetime(member, value, &PLAYBACK_TOKEN_LIFETIME, &mut errors)
                {
                    lifetime_given = value;
                }
            }
            other => errors.push(unknown_member(other)),
        }
    }
    checked(errors)?;
    if !app.credentials().is_enabled() {
        return Err(Problem::new(
            ProblemType::LifecycleConflict,
            "No password is set, so content URLs need no token.",
        ));
    }
    // The guard admits only api access tokens here.
    let Some(Extension(Caller(Grant::Token { id: issuer, .. }))) = caller else {
        return Err(Problem::internal());
    };
    let torrent = match torrent {
        Some(id) => Some(
            app.registry()?
                .get(&id)
                .ok_or_else(|| {
                    Problem::invalid_body(vec![InvalidValue::pointer(
                        "/torrent_id",
                        "No torrent has this ID.",
                    )])
                })?
                .id,
        ),
        None => None,
    };
    let seconds = app
        .now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_err(|_| Problem::internal())?
        .as_secs();
    let expires_at = SystemTime::UNIX_EPOCH + Duration::from_secs(seconds) + lifetime_given;
    let grant = PlaybackGrant {
        issuer,
        expires_at,
        torrent,
    };
    let token = app
        .credentials()
        .key()
        .and_then(|key| grant.sign(&key))
        .ok_or_else(Problem::internal)?;
    let body = api::PlaybackToken {
        token,
        torrent_id: grant.torrent.as_ref().map(ToString::to_string),
        expires_at: expires_at.into(),
    };
    let mut response = json(StatusCode::OK, &body)?;
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, NO_STORE);
    Ok(response)
}
