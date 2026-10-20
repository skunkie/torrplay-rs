// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! The torrents collection, torrents, their configuration, and their
//! statistics.

use std::sync::Arc;
use std::time::SystemTime;

use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use serde_json::{Map, Value};
use torrplay_api as api;
use torrplay_core::engine::{Engine, EngineError, Source, SourceRejection};
use torrplay_core::lifecycle::{DesiredState, EntityTag, ObservedState};
use torrplay_core::metadata::Metadata;
use torrplay_core::pagination::{
    CURSOR_PARAMETER, PAGE_SIZE_PARAMETER, PageRequest, TorrentFilter, TorrentSortKey, paginate,
};
use torrplay_core::registry::{ConfigurationError, NewTorrent, Registration};

use crate::app::{App, TorrentView};
use crate::body::{
    FORM_LIMIT_BYTES, JSON_LIMIT_BYTES, TORRENT_FILE_LIMIT_BYTES, parse_form, read_limited,
};
use crate::headers::{EntityTags, MediaType, negotiate};
use crate::problem::{InvalidValue, Problem, ProblemType};
use crate::query::{Paging, Query, page_error, reject};
use crate::representation::{
    self, JSON, NO_CACHE, configuration_etag, json, torrent_etag, torrent_path, torrents_path,
};

/// The query parameter that filters torrents by observed state.
const OBSERVED_STATE_PARAMETER: &str = "observed_state";

/// The longest magnet URI accepted, in characters.
const MAGNET_URI_MAX_CHARS: usize = 8192;

/// The media type of merge patches (RFC 7396).
const MERGE_PATCH: &str = "application/merge-patch+json";

/// The media type of torrent file uploads.
const FORM_DATA: &str = "multipart/form-data";

pub(crate) fn not_found() -> Problem {
    Problem::not_found("No torrent has this ID.")
}

pub(crate) async fn list<E: Engine>(
    State(app): State<Arc<App<E>>>,
    uri: Uri,
    headers: HeaderMap,
) -> Result<Response, Problem> {
    negotiate(&headers, JSON)?;
    let query = Query::parse(uri.query());
    let mut errors = query.unexpected(
        &[
            PAGE_SIZE_PARAMETER,
            CURSOR_PARAMETER,
            OBSERVED_STATE_PARAMETER,
        ],
        &[PAGE_SIZE_PARAMETER, CURSOR_PARAMETER],
    );
    let paging = Paging::parse(&query, &mut errors);
    let mut states = Vec::new();
    for value in query.all(OBSERVED_STATE_PARAMETER) {
        match ObservedState::parse(value) {
            Some(state) => states.push(state),
            None => errors.push(InvalidValue::parameter(
                OBSERVED_STATE_PARAMETER,
                "Must be retrieving_metadata, downloading, paused, or failed.",
            )),
        }
    }
    reject(errors)?;
    let filter = TorrentFilter::new(states);
    let request =
        PageRequest::<TorrentSortKey>::new(paging.page_size, paging.cursor, filter.fingerprint())
            .map_err(|error| Problem::malformed(vec![page_error(error)]))?;

    let mut views = Vec::new();
    for record in app.registry().list() {
        let view = app.observe(record).await?;
        if filter.matches(view.observed_state()) {
            views.push(view);
        }
    }
    let page = paginate(views, |view| TorrentSortKey::of(&view.record), &request);
    let filters: Vec<(&str, &str)> = query
        .all(OBSERVED_STATE_PARAMETER)
        .map(|value| (OBSERVED_STATE_PARAMETER, value))
        .collect();
    let path = torrents_path();
    let body = api::TorrentPage {
        items: page.items.iter().map(representation::torrent).collect(),
        links: api::TorrentPageLinks {
            next: page
                .next_cursor
                .as_deref()
                .map(|cursor| paging.link(&path, &filters, Some(cursor))),
            self_field: paging.link(&path, &filters, paging.cursor),
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
    let media_type = MediaType::of_request(&headers);
    let (source, desired_state) = match media_type.as_ref().map(|media| media.essence.as_str()) {
        Some(JSON) => {
            let bytes = read_limited(
                &headers,
                body,
                JSON_LIMIT_BYTES,
                JSON_LIMIT_BYTES as u64,
                app.body_timeout(),
            )
            .await?;
            creation_from_json(&bytes)?
        }
        Some(FORM_DATA) => {
            let boundary = media_type
                .as_ref()
                .and_then(|media| media.parameter("boundary"))
                .ok_or_else(|| {
                    Problem::new(ProblemType::MalformedRequest, "The form has no boundary.")
                })?
                .to_owned();
            let bytes = read_limited(
                &headers,
                body,
                FORM_LIMIT_BYTES,
                TORRENT_FILE_LIMIT_BYTES as u64,
                app.body_timeout(),
            )
            .await?;
            creation_from_form(&bytes, &boundary)?
        }
        _ => {
            return Err(Problem::new(
                ProblemType::UnsupportedMediaType,
                "The operation accepts application/json and multipart/form-data.",
            )
            .with_header(
                header::ACCEPT,
                HeaderValue::from_static("application/json, multipart/form-data"),
            ));
        }
    };

    app.require_engine()?;
    let inspection = app
        .engine()
        .inspect(&source)
        .await
        .map_err(|error| source_problem(error, &source))?;
    let metadata = inspection
        .metadata
        .map(|reported| {
            Metadata::identify(&reported)
                .map(Arc::new)
                .map_err(|error| {
                    Problem::invalid_body(vec![InvalidValue::pointer(
                        source.pointer(),
                        error.to_string(),
                    )])
                })
        })
        .transpose()?;
    let registration = app
        .registry()
        .register(NewTorrent {
            info_hash: inspection.info_hash,
            metadata,
            desired_state,
            created_at: SystemTime::now(),
        })
        .map_err(|_| Problem::internal())?;
    let record = match registration {
        Registration::Duplicate {
            existing,
            metadata_supplied,
        } => {
            if metadata_supplied && let Source::TorrentFile(bytes) = &source {
                app.engine()
                    .supply_metadata(&existing, bytes)
                    .await
                    .map_err(|error| source_problem(error, &source))?;
            }
            return Err(Problem::duplicate(torrent_path(&existing)));
        }
        Registration::Created(record) => record,
    };
    if let Err(error) = app.engine().add(&record.id, &source, desired_state).await {
        app.registry().remove(record.id.as_str());
        return Err(source_problem(error, &source));
    }
    let view = TorrentView {
        record,
        failure: None,
    };
    let location = torrent_path(&view.record.id);
    let mut response = json(StatusCode::CREATED, &representation::torrent(&view))?;
    let location = HeaderValue::try_from(location).map_err(|_| Problem::internal())?;
    response.headers_mut().insert(header::LOCATION, location);
    Ok(response)
}

/// Maps an engine failure about a source, naming the request member that
/// carried an invalid one.
fn source_problem(error: EngineError, source: &Source) -> Problem {
    match error {
        EngineError::Rejected(SourceRejection::Invalid { detail }) => {
            Problem::invalid_body(vec![InvalidValue::pointer(source.pointer(), detail)])
        }
        error => error.into(),
    }
}

/// Escapes a member name as a JSON Pointer reference token (RFC 6901).
fn pointer(member: &str) -> String {
    format!("/{}", member.replace('~', "~0").replace('/', "~1"))
}

fn parse_json(bytes: &Bytes) -> Result<Value, Problem> {
    serde_json::from_slice(bytes)
        .map_err(|_| Problem::new(ProblemType::MalformedRequest, "The body is not valid JSON."))
}

fn object(value: Value) -> Result<Map<String, Value>, Problem> {
    match value {
        Value::Object(members) => Ok(members),
        _ => Err(Problem::invalid_body(vec![InvalidValue::pointer(
            "",
            "Must be an object.",
        )])),
    }
}

fn unknown_member(member: &str) -> InvalidValue {
    InvalidValue::pointer(pointer(member), "Not a writable member of the request.")
}

fn parse_desired_state(value: &Value, errors: &mut Vec<InvalidValue>) -> Option<DesiredState> {
    let state = value.as_str().and_then(DesiredState::parse);
    if state.is_none() {
        errors.push(InvalidValue::pointer(
            "/desired_state",
            "Must be started or paused.",
        ));
    }
    state
}

fn creation_from_json(bytes: &Bytes) -> Result<(Source, DesiredState), Problem> {
    let members = object(parse_json(bytes)?)?;
    let mut errors = Vec::new();
    let mut magnet_uri = None;
    let mut desired_state = DesiredState::Started;
    for (member, value) in &members {
        match member.as_str() {
            "magnet_uri" => match value {
                Value::String(uri) if uri.chars().count() <= MAGNET_URI_MAX_CHARS => {
                    magnet_uri = Some(uri.clone());
                }
                Value::String(_) => errors.push(InvalidValue::pointer(
                    "/magnet_uri",
                    "Must be at most 8192 characters.",
                )),
                _ => errors.push(InvalidValue::pointer("/magnet_uri", "Must be a string.")),
            },
            "desired_state" => {
                if let Some(state) = parse_desired_state(value, &mut errors) {
                    desired_state = state;
                }
            }
            other => errors.push(unknown_member(other)),
        }
    }
    if !members.contains_key("magnet_uri") {
        errors.push(InvalidValue::pointer("/magnet_uri", "Is required."));
    }
    match magnet_uri {
        Some(uri) if errors.is_empty() => Ok((Source::MagnetUri(uri), desired_state)),
        _ => Err(Problem::invalid_body(errors)),
    }
}

fn creation_from_form(bytes: &Bytes, boundary: &str) -> Result<(Source, DesiredState), Problem> {
    let parts = parse_form(bytes, boundary).map_err(|_| {
        Problem::new(
            ProblemType::MalformedRequest,
            "The body is not a valid multipart form.",
        )
    })?;
    let mut errors = Vec::new();
    let mut torrent_file = None;
    let mut desired_state = None;
    let mut seen = Vec::new();
    for part in parts {
        if seen.contains(&part.name) {
            errors.push(InvalidValue::pointer(
                pointer(&part.name),
                "Must appear once.",
            ));
            continue;
        }
        seen.push(part.name.clone());
        match part.name.as_str() {
            "torrent_file" => {
                if part.data.len() > TORRENT_FILE_LIMIT_BYTES {
                    return Err(Problem::new(
                        ProblemType::ContentTooLarge,
                        format!("The torrent file exceeds {TORRENT_FILE_LIMIT_BYTES} bytes."),
                    ));
                }
                torrent_file = Some(part.data);
            }
            "desired_state" => {
                let value = Value::String(String::from_utf8_lossy(&part.data).into_owned());
                desired_state = parse_desired_state(&value, &mut errors);
            }
            other => errors.push(unknown_member(other)),
        }
    }
    if !seen.iter().any(|name| name == "torrent_file") {
        errors.push(InvalidValue::pointer("/torrent_file", "Is required."));
    }
    match torrent_file {
        Some(file) if errors.is_empty() => Ok((
            Source::TorrentFile(file.to_vec()),
            desired_state.unwrap_or(DesiredState::Started),
        )),
        _ => Err(Problem::invalid_body(errors)),
    }
}

/// Answers `304` with `etag` if `If-None-Match` matches it.
pub(crate) fn not_modified(headers: &HeaderMap, etag: &EntityTag) -> Option<Response> {
    let tags = EntityTags::parse(headers, &header::IF_NONE_MATCH)?.ok()?;
    tags.matches_weakly(etag.opaque()).then(|| {
        let mut response = StatusCode::NOT_MODIFIED.into_response();
        if let Ok(value) = HeaderValue::try_from(etag.to_string()) {
            response.headers_mut().insert(header::ETAG, value);
        }
        response
    })
}

fn with_etag(mut response: Response, etag: &EntityTag) -> Result<Response, Problem> {
    let value = HeaderValue::try_from(etag.to_string()).map_err(|_| Problem::internal())?;
    response.headers_mut().insert(header::ETAG, value);
    Ok(response)
}

pub(crate) async fn get<E: Engine>(
    State(app): State<Arc<App<E>>>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Response, Problem> {
    negotiate(&headers, JSON)?;
    let record = app.registry().get(&id).ok_or_else(not_found)?;
    let view = app.observe(record).await?;
    let torrent = representation::torrent(&view);
    let etag = torrent_etag(&torrent)?;
    if let Some(response) = not_modified(&headers, &etag) {
        return Ok(response);
    }
    let mut response = with_etag(json(StatusCode::OK, &torrent)?, &etag)?;
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, NO_CACHE);
    Ok(response)
}

pub(crate) async fn delete<E: Engine>(
    State(app): State<Arc<App<E>>>,
    Path(id): Path<String>,
) -> Result<Response, Problem> {
    let record = app.registry().get(&id).ok_or_else(not_found)?;
    app.require_engine()?;
    match app.engine().remove(&record.id).await {
        // The engine may not hold a torrent whose creation is still adding
        // it; unregistering makes that creation's torrent unreachable.
        Ok(()) | Err(EngineError::UnknownTorrent) => {}
        Err(error) => return Err(error.into()),
    }
    app.registry().remove(&id).ok_or_else(not_found)?;
    app.forget(&record.id);
    Ok(StatusCode::NO_CONTENT.into_response())
}

pub(crate) async fn get_configuration<E: Engine>(
    State(app): State<Arc<App<E>>>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Response, Problem> {
    negotiate(&headers, JSON)?;
    let record = app.registry().get(&id).ok_or_else(not_found)?;
    let etag = configuration_etag(&record.configuration);
    if let Some(response) = not_modified(&headers, &etag) {
        return Ok(response);
    }
    let body = representation::configuration(&record.id, &record.configuration);
    let mut response = with_etag(json(StatusCode::OK, &body)?, &etag)?;
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, NO_CACHE);
    Ok(response)
}

pub(crate) async fn update_configuration<E: Engine>(
    State(app): State<Arc<App<E>>>,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Body,
) -> Result<Response, Problem> {
    negotiate(&headers, JSON)?;
    let record = app.registry().get(&id).ok_or_else(not_found)?;
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
            "Send If-Match with the configuration's current ETag.",
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
    let desired_state = desired_state_from_patch(parse_json(&bytes)?)?;
    let stale = || {
        Problem::new(
            ProblemType::PreconditionFailed,
            "The configuration changed since the ETag was issued.",
        )
    };
    let current = configuration_etag(&record.configuration);
    if !if_match.is_ok_and(|tags| tags.matches_strongly(current.opaque())) {
        return Err(stale());
    }
    app.require_engine()?;

    // The engine takes the change before it is stored, so a change the
    // engine refuses leaves the configuration and its ETag untouched.
    let previous = record.configuration.desired_state();
    if desired_state != previous {
        app.engine()
            .set_desired_state(&record.id, desired_state)
            .await?;
    }
    let update =
        match app
            .registry()
            .update_desired_state(&id, desired_state, &record.configuration.etag())
        {
            Ok(update) => update,
            Err(error) => {
                // A concurrent change won; the engine follows the stored value.
                if let Some(current) = app.registry().get(&id) {
                    let _ = app
                        .engine()
                        .set_desired_state(&record.id, current.configuration.desired_state())
                        .await;
                }
                return Err(match error {
                    ConfigurationError::NotFound => not_found(),
                    ConfigurationError::PreconditionFailed { .. } => stale(),
                });
            }
        };
    if update.changed && previous == DesiredState::Paused && desired_state == DesiredState::Started
    {
        app.clear_metadata_failure(&record.id);
    }
    let body = representation::configuration(&record.id, &update.configuration);
    with_etag(
        json(StatusCode::OK, &body)?,
        &configuration_etag(&update.configuration),
    )
}

/// Reads the desired state a merge patch sets. The patch must change at
/// least one member, and `desired_state` cannot be removed with `null`.
fn desired_state_from_patch(value: Value) -> Result<DesiredState, Problem> {
    let members = object(value)?;
    let mut errors = Vec::new();
    let mut desired_state = None;
    for (member, value) in &members {
        match member.as_str() {
            "desired_state" if value.is_null() => errors.push(InvalidValue::pointer(
                "/desired_state",
                "Cannot be removed; must be started or paused.",
            )),
            "desired_state" => desired_state = parse_desired_state(value, &mut errors),
            other => errors.push(unknown_member(other)),
        }
    }
    if members.is_empty() {
        errors.push(InvalidValue::pointer("", "Must set at least one member."));
    }
    match desired_state {
        Some(state) if errors.is_empty() => Ok(state),
        _ => Err(Problem::invalid_body(errors)),
    }
}

pub(crate) async fn statistics<E: Engine>(
    State(app): State<Arc<App<E>>>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Response, Problem> {
    negotiate(&headers, JSON)?;
    let record = app.registry().get(&id).ok_or_else(not_found)?;
    app.require_engine()?;
    let statistics = app.engine().torrent_statistics(&record.id).await?;
    let body = representation::torrent_statistics(&record.id, &statistics);
    let mut response = json(StatusCode::OK, &body)?;
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, NO_CACHE);
    Ok(response)
}

pub(crate) async fn cache_statistics<E: Engine>(
    State(app): State<Arc<App<E>>>,
    headers: HeaderMap,
) -> Result<Response, Problem> {
    negotiate(&headers, JSON)?;
    let statistics = app.cache_statistics().await?;
    let mut response = json(
        StatusCode::OK,
        &representation::cache_statistics(&statistics),
    )?;
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, NO_CACHE);
    Ok(response)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn pointers(problem: Problem) -> Vec<String> {
        problem.pointers().into_iter().map(str::to_owned).collect()
    }

    fn bytes(value: Value) -> Bytes {
        Bytes::from(serde_json::to_vec(&value).unwrap())
    }

    #[test]
    fn creation_bodies_name_each_invalid_member() {
        let problem = creation_from_json(&bytes(json!({
            "id": "trr_x",
            "desired_state": "stopped",
            "a/b": 1,
        })))
        .unwrap_err();
        assert_eq!(
            pointers(problem),
            ["/a~1b", "/desired_state", "/id", "/magnet_uri"]
        );
        let (source, state) = creation_from_json(&bytes(json!({
            "magnet_uri": "magnet:?xt=urn:btih:00",
            "desired_state": "paused",
        })))
        .unwrap();
        assert_eq!(source, Source::MagnetUri("magnet:?xt=urn:btih:00".into()));
        assert_eq!(state, DesiredState::Paused);
    }

    #[test]
    fn long_magnet_uris_are_invalid() {
        let uri = format!("magnet:?{}", "a".repeat(MAGNET_URI_MAX_CHARS));
        let problem = creation_from_json(&bytes(json!({ "magnet_uri": uri }))).unwrap_err();
        assert_eq!(pointers(problem), ["/magnet_uri"]);
    }

    #[test]
    fn patches_must_set_a_writable_member() {
        let cases = [
            (json!(null), vec![""]),
            (json!([]), vec![""]),
            (json!({}), vec![""]),
            (json!({ "desired_state": null }), vec!["/desired_state"]),
            (
                json!({ "desired_state": "stopped" }),
                vec!["/desired_state"],
            ),
            (
                json!({ "desired_state": "paused", "observed_state": "paused" }),
                vec!["/observed_state"],
            ),
        ];
        for (patch, expected) in cases {
            let problem = desired_state_from_patch(patch.clone()).unwrap_err();
            assert_eq!(pointers(problem), expected, "{patch}");
        }
        assert_eq!(
            desired_state_from_patch(json!({ "desired_state": "paused" })).unwrap(),
            DesiredState::Paused
        );
    }

    #[test]
    fn forms_require_one_torrent_file() {
        let form = |text: &'static str| Bytes::from_static(text.as_bytes());
        let problem = creation_from_form(
            &form("--b\r\nContent-Disposition: form-data; name=\"x\"\r\n\r\n1\r\n--b--"),
            "b",
        )
        .unwrap_err();
        assert_eq!(pointers(problem), ["/x", "/torrent_file"]);
        let problem = creation_from_form(
            &form(
                "--b\r\nContent-Disposition: form-data; name=\"torrent_file\"\r\n\r\n1\r\n\
--b\r\nContent-Disposition: form-data; name=\"torrent_file\"\r\n\r\n2\r\n--b--",
            ),
            "b",
        )
        .unwrap_err();
        assert_eq!(pointers(problem), ["/torrent_file"]);
    }
}
