// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! A profile's progress of a torrent's files. Progress lives in the store
//! and needs no engine, only the torrent's metadata for its file IDs.

use std::sync::Arc;
use std::time::SystemTime;

use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use serde_json::Value;
use torrplay_api as api;
use torrplay_core::engine::Engine;
use torrplay_core::id::{FileId, TorrentId};
use torrplay_core::metadata::Metadata;
use torrplay_core::pagination::{
    CURSOR_PARAMETER, FileSortKey, FilterFingerprint, PAGE_SIZE_PARAMETER, PageRequest, paginate,
};
use torrplay_core::profiles::{self, DURATION_MAX_MS, Profile, ProfileKind, ProgressUpdate};
use torrplay_store::{StoreError, StoredProgress};

use crate::app::{App, store_failed};
use crate::body::{JSON_LIMIT_BYTES, read_limited};
use crate::files::with_metadata;
use crate::headers::{MediaType, negotiate};
use crate::problem::{InvalidValue, Problem, ProblemType};
use crate::profiles as profile_routes;
use crate::query::{Paging, Query, page_error, reject};
use crate::representation::{self, JSON, NO_CACHE, json, progress_path};
use crate::torrents::{self, object, parse_json, unknown_member};

/// Maps a store failure, where an unknown profile or torrent was deleted
/// meanwhile.
fn stored(error: StoreError) -> Problem {
    match error {
        StoreError::UnknownProfile => profile_routes::not_found(),
        StoreError::UnknownTorrent => torrents::not_found(),
        error => store_failed(error),
    }
}

fn no_progress() -> Problem {
    Problem::not_found("The profile has no progress of this file.")
}

/// The profile, the torrent, and its metadata, which must be available.
async fn resolve<E: Engine>(
    app: &App<E>,
    profile: &str,
    torrent: &str,
) -> Result<(Profile, TorrentId, Arc<Metadata>), Problem> {
    let profile = profile_routes::find(app, profile)?;
    let (view, metadata) = with_metadata(app, torrent).await?;
    Ok((profile, view.record.id, metadata))
}

/// The file with the ID, which must be in the metadata.
fn file(metadata: &Metadata, id: &str) -> Result<FileId, Problem> {
    metadata
        .file(id)
        .map(|(_, file)| file.id().clone())
        .ok_or_else(|| Problem::not_found("The torrent has no file with this ID."))
}

pub(crate) async fn list<E: Engine>(
    State(app): State<Arc<App<E>>>,
    Path((profile, torrent)): Path<(String, String)>,
    uri: Uri,
    headers: HeaderMap,
) -> Result<Response, Problem> {
    negotiate(&headers, JSON)?;
    let query = Query::parse(uri.query());
    let parameters = [PAGE_SIZE_PARAMETER, CURSOR_PARAMETER];
    let mut errors = query.unexpected(&parameters, &parameters);
    let paging = Paging::parse(&query, &mut errors);
    reject(errors)?;
    let request =
        PageRequest::<FileSortKey>::new(paging.page_size, paging.cursor, FilterFingerprint::none())
            .map_err(|error| Problem::malformed(vec![page_error(error)]))?;
    let (profile, torrent, metadata) = resolve(&app, &profile, &torrent).await?;
    let saved = app.viewing()?.torrent_progress(&profile.id, &torrent);
    let page = paginate(
        metadata
            .files()
            .iter()
            .enumerate()
            .filter_map(|(position, file)| {
                saved
                    .get(file.id())
                    .map(|progress| (position, file.id(), *progress))
            }),
        |(position, _, _)| FileSortKey(*position as u64),
        &request,
    );
    let path = progress_path(&profile.id, &torrent);
    let body = api::ProgressPage {
        items: page
            .items
            .iter()
            .map(|(_, file, progress)| {
                representation::progress(&profile.id, &torrent, file, progress)
            })
            .collect(),
        links: api::ProgressPageLinks {
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

pub(crate) async fn get<E: Engine>(
    State(app): State<Arc<App<E>>>,
    Path((profile, torrent, file_id)): Path<(String, String, String)>,
    headers: HeaderMap,
) -> Result<Response, Problem> {
    negotiate(&headers, JSON)?;
    let (profile, torrent, metadata) = resolve(&app, &profile, &torrent).await?;
    let file = file(&metadata, &file_id)?;
    let progress = app
        .viewing()?
        .torrent_progress(&profile.id, &torrent)
        .get(&file)
        .copied()
        .ok_or_else(no_progress)?;
    let mut response = json(
        StatusCode::OK,
        &representation::progress(&profile.id, &torrent, &file, &progress),
    )?;
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, NO_CACHE);
    Ok(response)
}

/// Converts seconds to whole milliseconds, if they are a number within
/// bounds.
fn milliseconds(value: &Value) -> Option<u64> {
    let seconds = value
        .as_f64()
        .filter(|seconds| seconds.is_finite() && *seconds >= 0.0)?;
    let milliseconds = (seconds * 1000.0).round();
    (milliseconds <= DURATION_MAX_MS as f64).then_some(milliseconds as u64)
}

/// Reads a progress update: a position, a duration, and a viewed mark.
fn update(value: Value) -> Result<ProgressUpdate, Problem> {
    let members = object(value)?;
    let mut errors = Vec::new();
    let (mut position, mut duration, mut viewed) = (None, None, None);
    for (member, value) in &members {
        match member.as_str() {
            "position_seconds" => match milliseconds(value) {
                Some(found) => position = Some(found),
                None => errors.push(InvalidValue::pointer(
                    "/position_seconds",
                    "Must be a number from 0 to 1000000.",
                )),
            },
            "duration_seconds" => match milliseconds(value).filter(|&found| found > 0) {
                Some(found) => duration = Some(found),
                None => errors.push(InvalidValue::pointer(
                    "/duration_seconds",
                    "Must be a number above 0, to the millisecond, and at most 1000000.",
                )),
            },
            "viewed" => match value.as_bool() {
                Some(found) => viewed = Some(found),
                None => errors.push(InvalidValue::pointer("/viewed", "Must be true or false.")),
            },
            other => errors.push(unknown_member(other)),
        }
    }
    for (member, found) in [
        ("position_seconds", position.is_some()),
        ("duration_seconds", duration.is_some()),
        ("viewed", viewed.is_some()),
    ] {
        if !found && !members.contains_key(member) {
            errors.push(InvalidValue::pointer(format!("/{member}"), "Is required."));
        }
    }
    if let (Some(position), Some(duration)) = (position, duration)
        && position > duration
    {
        errors.push(InvalidValue::pointer(
            "/position_seconds",
            "Must not exceed the duration.",
        ));
    }
    match (position, duration, viewed) {
        (Some(position_ms), Some(duration_ms), Some(viewed)) if errors.is_empty() => {
            Ok(ProgressUpdate {
                position_ms,
                duration_ms,
                viewed,
            })
        }
        _ => Err(Problem::invalid_body(errors)),
    }
}

pub(crate) async fn put<E: Engine>(
    State(app): State<Arc<App<E>>>,
    Path((profile, torrent, file_id)): Path<(String, String, String)>,
    headers: HeaderMap,
    body: Body,
) -> Result<Response, Problem> {
    negotiate(&headers, JSON)?;
    let (profile, torrent, metadata) = resolve(&app, &profile, &torrent).await?;
    let file = file(&metadata, &file_id)?;
    if app.temporaries().contains(&torrent) {
        return Err(crate::torrents::temporary_conflict("record its progress"));
    }
    if profile.kind == ProfileKind::Device {
        return Err(profile_routes::device_conflict());
    }
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
    let update = update(parse_json(&bytes)?)?;
    app.require_running()?;
    let (progress, created) = {
        let _lane = app.write_lane().await;
        let viewing = app.viewing()?;
        let previous = viewing
            .torrent_progress(&profile.id, &torrent)
            .get(&file)
            .copied();
        let progress = profiles::apply_update(previous.as_ref(), update, SystemTime::now());
        app.store()
            .set_progress(&StoredProgress {
                profile: profile.id.clone(),
                torrent: torrent.clone(),
                file: file.clone(),
                progress,
            })
            .await
            .map_err(stored)?;
        viewing.set_progress(&profile.id, &torrent, &file, progress);
        (progress, previous.is_none())
    };
    let body = representation::progress(&profile.id, &torrent, &file, &progress);
    if !created {
        return json(StatusCode::OK, &body);
    }
    let location =
        HeaderValue::try_from(body.links.self_field.clone()).map_err(|_| Problem::internal())?;
    let mut response = json(StatusCode::CREATED, &body)?;
    response.headers_mut().insert(header::LOCATION, location);
    Ok(response)
}

pub(crate) async fn delete<E: Engine>(
    State(app): State<Arc<App<E>>>,
    Path((profile, torrent, file_id)): Path<(String, String, String)>,
) -> Result<Response, Problem> {
    let (profile, torrent, metadata) = resolve(&app, &profile, &torrent).await?;
    let file = file(&metadata, &file_id)?;
    app.require_running()?;
    let _lane = app.write_lane().await;
    let viewing = app.viewing()?;
    if !viewing
        .torrent_progress(&profile.id, &torrent)
        .contains_key(&file)
    {
        return Err(no_progress());
    }
    app.store()
        .delete_progress(&profile.id, &torrent, &file)
        .await
        .map_err(stored)?;
    viewing.remove_progress(&profile.id, &torrent, &file);
    // A viewing of the file stops inferring until the renderer plays it
    // again.
    if profile.kind == ProfileKind::Device {
        app.inference().forget(&(torrent, file));
    }
    Ok(StatusCode::NO_CONTENT.into_response())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn updates_round_to_milliseconds_and_name_each_invalid_member() {
        assert_eq!(
            update(json!({ "position_seconds": 1.2345, "duration_seconds": 10, "viewed": false }))
                .unwrap(),
            ProgressUpdate {
                position_ms: 1235,
                duration_ms: 10_000,
                viewed: false
            }
        );
        let pointers = |value| {
            update(value)
                .unwrap_err()
                .pointers()
                .into_iter()
                .map(str::to_owned)
                .collect::<Vec<_>>()
        };
        assert_eq!(
            pointers(json!({ "position_seconds": 11, "duration_seconds": 10, "viewed": false })),
            ["/position_seconds"]
        );
        assert_eq!(
            pointers(
                json!({ "position_seconds": -1, "duration_seconds": 0, "viewed": "no", "x": 1 })
            ),
            ["/duration_seconds", "/position_seconds", "/viewed", "/x"]
        );
        assert_eq!(
            pointers(json!({ "duration_seconds": 0.0001 })),
            ["/duration_seconds", "/position_seconds", "/viewed"]
        );
    }
}
