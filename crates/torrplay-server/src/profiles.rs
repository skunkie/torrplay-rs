// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! The profiles collection, profiles, and their avatars. Profiles live in
//! the store and need no engine.

use std::sync::Arc;
use std::time::SystemTime;

use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use serde_json::Value;
use torrplay_api as api;
use torrplay_core::engine::Engine;
use torrplay_core::id::{ProfileId, random_torrent_id_bytes};
use torrplay_core::image::ImageInfo;
use torrplay_core::pagination::{FilterFingerprint, PageRequest, ProfileSortKey, paginate};
use torrplay_core::profiles::{self, PROFILES_MAX, Profile};
use torrplay_store::StoreError;

use crate::app::{App, store_failed};
use crate::body::{JSON_LIMIT_BYTES, read_limited};
use crate::headers::{EntityTags, MediaType, negotiate};
use crate::images;
use crate::problem::{InvalidValue, Problem, ProblemType};
use crate::query::{Paging, Query, page_error, reject};
use crate::representation::{
    self, JSON, NO_CACHE, json, profile_etag, profile_path, profiles_path,
};
use crate::torrents::{
    MERGE_PATCH, not_modified, object, parse_json, pointer, unknown_member, with_etag,
};

pub(crate) fn not_found() -> Problem {
    Problem::not_found("No profile has this ID.")
}

/// Returns the profile with the ID.
pub(crate) fn find<E: Engine>(app: &App<E>, id: &str) -> Result<Profile, Problem> {
    app.viewing()?.profile(id).ok_or_else(not_found)
}

/// Maps a store failure, where an unknown profile was deleted meanwhile.
fn stored(error: StoreError) -> Problem {
    match error {
        StoreError::UnknownProfile => not_found(),
        error => store_failed(error),
    }
}

/// Reads a profile's name from a body member, adding an error if it is not
/// one.
fn name(value: &Value, errors: &mut Vec<InvalidValue>) -> Option<String> {
    let normalized = match value {
        Value::String(text) => {
            profiles::normalize_profile_name(text).map_err(|error| error.to_string())
        }
        _ => Err("Must be a string.".to_owned()),
    };
    normalized
        .map_err(|detail| errors.push(InvalidValue::pointer("/name", detail)))
        .ok()
}

/// Reads a body of only a `name`, which `required` makes mandatory.
fn name_body(value: Value, required: bool) -> Result<Option<String>, Problem> {
    let members = object(value)?;
    let mut errors = Vec::new();
    let mut found = None;
    for (member, value) in &members {
        match member.as_str() {
            "name" if value.is_null() => errors.push(InvalidValue::pointer(
                "/name",
                "Cannot be removed; must be a name.",
            )),
            "name" => found = name(value, &mut errors),
            other => errors.push(unknown_member(other)),
        }
    }
    if members.is_empty() && !required {
        errors.push(InvalidValue::pointer("", "Must set at least one member."));
    }
    if required && !members.contains_key("name") {
        errors.push(InvalidValue::pointer(pointer("name"), "Is required."));
    }
    if errors.is_empty() {
        Ok(found)
    } else {
        Err(Problem::invalid_body(errors))
    }
}

/// Refuses a name another profile than `except` has.
fn require_free<E: Engine>(
    app: &App<E>,
    name: &str,
    except: Option<&ProfileId>,
) -> Result<(), Problem> {
    match app.viewing()?.named(name, except) {
        Some(other) => Err(Problem::duplicate_profile(profile_path(&other))),
        None => Ok(()),
    }
}

/// A profile's representation with its ETag and `status`.
fn respond(profile: &Profile, status: StatusCode) -> Result<Response, Problem> {
    let body = representation::profile(profile);
    let etag = profile_etag(&body)?;
    with_etag(json(status, &body)?, &etag)
}

pub(crate) async fn list<E: Engine>(
    State(app): State<Arc<App<E>>>,
    uri: Uri,
    headers: HeaderMap,
) -> Result<Response, Problem> {
    negotiate(&headers, JSON)?;
    let query = Query::parse(uri.query());
    let parameters = [
        torrplay_core::pagination::PAGE_SIZE_PARAMETER,
        torrplay_core::pagination::CURSOR_PARAMETER,
    ];
    let mut errors = query.unexpected(&parameters, &parameters);
    let paging = Paging::parse(&query, &mut errors);
    reject(errors)?;
    let request = PageRequest::<ProfileSortKey>::new(
        paging.page_size,
        paging.cursor,
        FilterFingerprint::none(),
    )
    .map_err(|error| Problem::malformed(vec![page_error(error)]))?;
    let page = paginate(
        app.viewing()?.profiles(),
        |profile| ProfileSortKey::of(&profile.id, profile.created_at),
        &request,
    );
    let path = profiles_path();
    let body = api::ProfilePage {
        items: page.items.iter().map(representation::profile).collect(),
        links: api::ProfilePageLinks {
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

pub(crate) async fn create<E: Engine>(
    State(app): State<Arc<App<E>>>,
    headers: HeaderMap,
    body: Body,
) -> Result<Response, Problem> {
    negotiate(&headers, JSON)?;
    app.viewing()?;
    if MediaType::of_request(&headers).is_none_or(|media| media.essence != JSON) {
        return Err(Problem::new(
            ProblemType::UnsupportedMediaType,
            "The operation accepts application/json.",
        )
        .with_header(header::ACCEPT, HeaderValue::from_static(JSON)));
    }
    let bytes = read_limited(
        &headers,
        body,
        JSON_LIMIT_BYTES,
        JSON_LIMIT_BYTES as u64,
        app.body_timeout(),
    )
    .await?;
    let name = name_body(parse_json(&bytes)?, true)?.ok_or_else(Problem::internal)?;
    app.require_running()?;
    let id = ProfileId::from_random_bytes(random_torrent_id_bytes().map_err(|error| {
        tracing::error!(%error, "no random bytes for a profile ID");
        Problem::internal()
    })?);
    let profile = Profile {
        id,
        name,
        created_at: SystemTime::now(),
        avatar: None,
    };
    {
        let _lane = app.write_lane().await;
        let viewing = app.viewing()?;
        if viewing.len() >= PROFILES_MAX {
            return Err(Problem::new(
                ProblemType::TooManyProfiles,
                format!("The application keeps at most {PROFILES_MAX} profiles; delete one first."),
            ));
        }
        require_free(&app, &profile.name, None)?;
        app.store()
            .insert_profile(&profile)
            .await
            .map_err(store_failed)?;
        viewing.set_profile(profile.clone());
    }
    let mut response = respond(&profile, StatusCode::CREATED)?;
    let location =
        HeaderValue::try_from(profile_path(&profile.id)).map_err(|_| Problem::internal())?;
    response.headers_mut().insert(header::LOCATION, location);
    Ok(response)
}

pub(crate) async fn get<E: Engine>(
    State(app): State<Arc<App<E>>>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Response, Problem> {
    negotiate(&headers, JSON)?;
    let profile = find(&app, &id)?;
    let body = representation::profile(&profile);
    let etag = profile_etag(&body)?;
    if let Some(response) = not_modified(&headers, &etag) {
        return Ok(response);
    }
    let mut response = with_etag(json(StatusCode::OK, &body)?, &etag)?;
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, NO_CACHE);
    Ok(response)
}

pub(crate) async fn update<E: Engine>(
    State(app): State<Arc<App<E>>>,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Body,
) -> Result<Response, Problem> {
    negotiate(&headers, JSON)?;
    find(&app, &id)?;
    if MediaType::of_request(&headers).is_none_or(|media| media.essence != MERGE_PATCH) {
        return Err(Problem::new(
            ProblemType::UnsupportedMediaType,
            "The operation accepts application/merge-patch+json.",
        )
        .with_header(
            HeaderName::from_static("accept-patch"),
            HeaderValue::from_static(MERGE_PATCH),
        ));
    }
    let Some(if_match) = EntityTags::parse(&headers, &header::IF_MATCH) else {
        return Err(Problem::new(
            ProblemType::PreconditionRequired,
            "Send If-Match with the profile's current ETag.",
        ));
    };
    let bytes = read_limited(
        &headers,
        body,
        JSON_LIMIT_BYTES,
        JSON_LIMIT_BYTES as u64,
        app.body_timeout(),
    )
    .await?;
    let name = name_body(parse_json(&bytes)?, false)?;
    app.require_running()?;
    let _lane = app.write_lane().await;
    let mut profile = find(&app, &id)?;
    let current = profile_etag(&representation::profile(&profile))?;
    if !if_match.is_ok_and(|tags| tags.matches_strongly(current.opaque())) {
        return Err(Problem::new(
            ProblemType::PreconditionFailed,
            "The profile changed since the ETag was issued.",
        ));
    }
    if let Some(name) = name.filter(|name| *name != profile.name) {
        require_free(&app, &name, Some(&profile.id))?;
        app.store()
            .rename_profile(&profile.id, &name)
            .await
            .map_err(stored)?;
        profile.name = name;
        app.viewing()?.set_profile(profile.clone());
    }
    respond(&profile, StatusCode::OK)
}

pub(crate) async fn delete<E: Engine>(
    State(app): State<Arc<App<E>>>,
    Path(id): Path<String>,
) -> Result<Response, Problem> {
    find(&app, &id)?;
    app.require_running()?;
    let _lane = app.write_lane().await;
    let profile = find(&app, &id)?;
    let viewing = app.viewing()?;
    if viewing.len() <= 1 {
        return Err(Problem::new(
            ProblemType::LifecycleConflict,
            "The application keeps at least one profile; create another first.",
        ));
    }
    app.store()
        .delete_profile(&profile.id)
        .await
        .map_err(stored)?;
    viewing.remove_profile(&profile.id);
    Ok(StatusCode::NO_CONTENT.into_response())
}

fn no_avatar() -> Problem {
    Problem::not_found("The profile has no avatar.")
}

pub(crate) async fn get_avatar<E: Engine>(
    State(app): State<Arc<App<E>>>,
    Path(id): Path<String>,
    request: HeaderMap,
) -> Result<Response, Problem> {
    let profile = find(&app, &id)?;
    let avatar = profile.avatar.ok_or_else(no_avatar)?;
    if let Some(response) = images::not_modified_or_acceptable(&request, &avatar)? {
        return Ok(response);
    }
    // The headers follow the avatar as read, which a replacement stored
    // since the profile was read may have changed.
    let (avatar, bytes) = app
        .store()
        .avatar(&profile.id)
        .await
        .map_err(stored)?
        .ok_or_else(no_avatar)?;
    images::serve(&avatar, bytes)
}

pub(crate) async fn head_avatar<E: Engine>(
    State(app): State<Arc<App<E>>>,
    Path(id): Path<String>,
    request: HeaderMap,
) -> Result<Response, Problem> {
    let avatar = find(&app, &id)?.avatar.ok_or_else(no_avatar)?;
    match images::not_modified_or_acceptable(&request, &avatar)? {
        Some(response) => Ok(response),
        None => images::headers(&avatar),
    }
}

pub(crate) async fn put_avatar<E: Engine>(
    State(app): State<Arc<App<E>>>,
    Path(id): Path<String>,
    request: HeaderMap,
    body: Body,
) -> Result<Response, Problem> {
    find(&app, &id)?;
    app.require_running()?;
    let (image_type, bytes) = images::read_upload(&app, &request, body).await?;
    let avatar = ImageInfo::of(image_type, &bytes, SystemTime::now());
    let (profile, replaced) = {
        let _lane = app.write_lane().await;
        let mut profile = find(&app, &id)?;
        app.store()
            .set_avatar(&profile.id, &avatar, bytes.to_vec())
            .await
            .map_err(stored)?;
        let replaced = profile.avatar.replace(avatar.clone()).is_some();
        app.viewing()?.set_profile(profile.clone());
        (profile, replaced)
    };
    images::uploaded(
        &avatar,
        replaced,
        format!("{}/avatar", profile_path(&profile.id)),
    )
}

pub(crate) async fn delete_avatar<E: Engine>(
    State(app): State<Arc<App<E>>>,
    Path(id): Path<String>,
) -> Result<Response, Problem> {
    find(&app, &id)?;
    app.require_running()?;
    let _lane = app.write_lane().await;
    let mut profile = find(&app, &id)?;
    if profile.avatar.is_none() {
        return Err(no_avatar());
    }
    app.store()
        .delete_avatar(&profile.id)
        .await
        .map_err(stored)?;
    profile.avatar = None;
    app.viewing()?.set_profile(profile);
    Ok(StatusCode::NO_CONTENT.into_response())
}
