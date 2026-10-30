// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! A file's preloads: creating, listing, reading, and canceling them.

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde_json::Value;
use torrplay_api as api;
use torrplay_core::engine::{Engine, EngineError};
use torrplay_core::id::{PreloadId, TorrentId, random_torrent_id_bytes};
use torrplay_core::metadata::{FileIndex, Metadata};
use torrplay_core::preload::{self, PreloadRequest, PreloadStatus};

use crate::app::App;
use crate::body::{JSON_LIMIT_BYTES, read_limited};
use crate::headers::{MediaType, negotiate};
use crate::problem::{InvalidValue, Problem, ProblemType};
use crate::representation::{self, JSON, NO_CACHE, json};
use crate::torrents::{self, object, parse_json, pointer, unknown_member};

/// The path of a file's preloads.
fn preloads_path(torrent: &TorrentId, file_id: &str) -> String {
    format!(
        "{}/files/{file_id}/preloads",
        representation::torrent_path(torrent)
    )
}

/// The torrent `id`'s file `file_id`: its engine index, from metadata that
/// must be available.
async fn resolve<E: Engine>(
    app: &App<E>,
    id: &str,
    file_id: &str,
) -> Result<(TorrentId, FileIndex), Problem> {
    let record = app.registry()?.get(id).ok_or_else(torrents::not_found)?;
    let view = app.observe(record).await?;
    let metadata: Arc<Metadata> = view.record.metadata.clone().ok_or_else(|| {
        Problem::new(
            ProblemType::LifecycleConflict,
            "The torrent's metadata is still being retrieved.",
        )
    })?;
    let (_, file) = metadata
        .file(file_id)
        .ok_or_else(|| Problem::not_found("The torrent has no file with this ID."))?;
    Ok((view.record.id.clone(), file.index()))
}

fn representation(torrent: &TorrentId, file_id: &str, status: &PreloadStatus) -> api::Preload {
    let file = format!("{}/files/{file_id}", representation::torrent_path(torrent));
    let count = |value: u64| i64::try_from(value).unwrap_or(i64::MAX);
    api::Preload {
        id: status.id.to_string(),
        state: match status.state {
            preload::PreloadState::Queued => api::PreloadState::Queued,
            preload::PreloadState::Downloading => api::PreloadState::Downloading,
            preload::PreloadState::Ready => api::PreloadState::Ready,
            preload::PreloadState::Expired => api::PreloadState::Expired,
            preload::PreloadState::Failed => api::PreloadState::Failed,
        },
        offset_bytes: status.offset_bytes.map(count),
        ranges: status
            .ranges
            .iter()
            .map(|range| api::PreloadRange {
                start_bytes: count(range.start()),
                length_bytes: count(range.len_bytes()),
            })
            .collect(),
        target_bytes: count(status.target_bytes),
        completed_bytes: count(status.completed_bytes),
        created_at: status.created_at.into(),
        expires_at: status.expires_at.into(),
        ended_at: status.ended_at.map(Into::into),
        failure: status.failure.as_ref().map(|failure| api::PreloadFailure {
            detail: failure.detail.clone(),
        }),
        links: api::PreloadLinks {
            self_field: format!("{file}/preloads/{}", status.id),
            content: format!("{file}/content"),
            file,
            torrent: representation::torrent_path(torrent),
        },
    }
}

/// Maps a preload operation's failure; a request's own offset past the
/// file's end is the request's fault.
fn problem(error: EngineError) -> Problem {
    match error {
        EngineError::RangeOutOfBounds => Problem::invalid_body(vec![InvalidValue::pointer(
            "/offset_bytes",
            "Must be below the file's size.",
        )]),
        error => error.into(),
    }
}

pub(crate) async fn list<E: Engine>(
    State(app): State<Arc<App<E>>>,
    Path((id, file_id)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<Response, Problem> {
    negotiate(&headers, JSON)?;
    let (torrent, file) = resolve(&app, &id, &file_id).await?;
    let preloads = app
        .engine()
        .preloads(&torrent, file)
        .await
        .map_err(problem)?;
    let body = api::PreloadPage {
        items: preloads
            .iter()
            .map(|status| representation(&torrent, &file_id, status))
            .collect(),
        links: api::PreloadPageLinks {
            self_field: preloads_path(&torrent, &file_id),
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
    Path((id, file_id)): Path<(String, String)>,
    headers: HeaderMap,
    body: Body,
) -> Result<Response, Problem> {
    negotiate(&headers, JSON)?;
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
    let request = parse_request(parse_json(&bytes)?)?;
    app.require_engine()?;
    let (torrent, file) = resolve(&app, &id, &file_id).await?;
    let preload = PreloadId::from_random_bytes(random_torrent_id_bytes().map_err(|error| {
        tracing::error!(%error, "no random bytes for a preload ID");
        Problem::internal()
    })?);
    let status = app
        .engine()
        .create_preload(&torrent, file, preload, request)
        .await
        .map_err(problem)?;
    // A preload restarts a temporary torrent's idle time.
    app.temporaries().touch(&torrent, app.now());
    let body = representation(&torrent, &file_id, &status);
    let location =
        HeaderValue::try_from(body.links.self_field.clone()).map_err(|_| Problem::internal())?;
    let mut response = json(StatusCode::CREATED, &body)?;
    response.headers_mut().insert(header::LOCATION, location);
    Ok(response)
}

/// Reads a preload's creation request: an object with an optional
/// `offset_bytes` and `expires_in_seconds`.
fn parse_request(value: Value) -> Result<PreloadRequest, Problem> {
    let members = object(value)?;
    let mut errors = Vec::new();
    let mut request = PreloadRequest {
        offset_bytes: None,
        expires_in: preload::EXPIRES_IN_DEFAULT,
    };
    for (member, value) in &members {
        match member.as_str() {
            "offset_bytes" => match value.as_u64() {
                Some(offset) => request.offset_bytes = Some(offset),
                None => errors.push(InvalidValue::pointer(
                    pointer(member),
                    "Must be a non-negative integer below the file's size.",
                )),
            },
            "expires_in_seconds" => {
                let expires_in = value
                    .as_u64()
                    .map(Duration::from_secs)
                    .filter(|expires_in| {
                        (preload::EXPIRES_IN_MIN..=preload::EXPIRES_IN_MAX).contains(expires_in)
                    });
                match expires_in {
                    Some(expires_in) => request.expires_in = expires_in,
                    None => errors.push(InvalidValue::pointer(
                        pointer(member),
                        format!(
                            "Must be an integer from {} to {}.",
                            preload::EXPIRES_IN_MIN.as_secs(),
                            preload::EXPIRES_IN_MAX.as_secs()
                        ),
                    )),
                }
            }
            member => errors.push(unknown_member(member)),
        }
    }
    if errors.is_empty() {
        Ok(request)
    } else {
        Err(Problem::invalid_body(errors))
    }
}

pub(crate) async fn get<E: Engine>(
    State(app): State<Arc<App<E>>>,
    Path((id, file_id, preload_id)): Path<(String, String, String)>,
    headers: HeaderMap,
) -> Result<Response, Problem> {
    negotiate(&headers, JSON)?;
    let (torrent, file) = resolve(&app, &id, &file_id).await?;
    let status = app
        .engine()
        .preloads(&torrent, file)
        .await
        .map_err(problem)?
        .into_iter()
        .find(|status| status.id.as_str() == preload_id)
        .ok_or_else(|| problem(EngineError::UnknownPreload))?;
    let mut response = json(StatusCode::OK, &representation(&torrent, &file_id, &status))?;
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, NO_CACHE);
    Ok(response)
}

pub(crate) async fn delete<E: Engine>(
    State(app): State<Arc<App<E>>>,
    Path((id, file_id, preload_id)): Path<(String, String, String)>,
) -> Result<Response, Problem> {
    app.require_engine()?;
    let (torrent, file) = resolve(&app, &id, &file_id).await?;
    // IDs are opaque: one of another shape names no preload.
    let preload = app
        .engine()
        .preloads(&torrent, file)
        .await
        .map_err(problem)?
        .into_iter()
        .find(|status| status.id.as_str() == preload_id)
        .ok_or_else(|| problem(EngineError::UnknownPreload))?;
    app.engine()
        .cancel_preload(&torrent, file, &preload.id)
        .await
        .map_err(problem)?;
    Ok(StatusCode::NO_CONTENT.into_response())
}
