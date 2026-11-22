// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! A torrent's poster: uploading, serving, and deleting its image. Posters
//! live with their torrent and need no engine.

use std::sync::Arc;
use std::time::SystemTime;

use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use torrplay_core::engine::Engine;
use torrplay_core::image::ImageInfo;
use torrplay_core::registry::TorrentRecord;
use torrplay_store::StoreError;

use crate::app::{App, store_failed};
use crate::headers::EntityTags;
use crate::images;
use crate::problem::{InvalidValue, Problem, ProblemType};
use crate::representation::{image_etag, torrent_path};
use crate::torrents;

fn no_poster() -> Problem {
    Problem::not_found("The torrent has no poster.")
}

fn record<E: Engine>(app: &App<E>, id: &str) -> Result<TorrentRecord, Problem> {
    app.registry()?.get(id).ok_or_else(torrents::not_found)
}

/// Maps a store failure, where an unknown torrent was deleted meanwhile.
fn stored(error: StoreError) -> Problem {
    match error {
        StoreError::UnknownTorrent => torrents::not_found(),
        error => store_failed(error),
    }
}

pub(crate) async fn get<E: Engine>(
    State(app): State<Arc<App<E>>>,
    Path(id): Path<String>,
    request: HeaderMap,
) -> Result<Response, Problem> {
    let record = record(&app, &id)?;
    let poster = record.poster.clone().ok_or_else(no_poster)?;
    if let Some(response) = images::not_modified_or_acceptable(&request, &poster)? {
        return Ok(response);
    }
    // The headers follow the poster as read, which a replacement stored
    // since the record was read may have changed.
    if let Some((poster, bytes)) = app.temporaries().poster(&record.id) {
        return images::serve(&poster, bytes);
    }
    let (poster, bytes) = app
        .store()
        .poster(&record.id)
        .await
        .map_err(stored)?
        .ok_or_else(no_poster)?;
    images::serve(&poster, bytes)
}

pub(crate) async fn head<E: Engine>(
    State(app): State<Arc<App<E>>>,
    Path(id): Path<String>,
    request: HeaderMap,
) -> Result<Response, Problem> {
    let poster = record(&app, &id)?.poster.ok_or_else(no_poster)?;
    match images::not_modified_or_acceptable(&request, &poster)? {
        Some(response) => Ok(response),
        None => images::headers(&poster),
    }
}

pub(crate) async fn set<E: Engine>(
    State(app): State<Arc<App<E>>>,
    Path(id): Path<String>,
    request: HeaderMap,
    body: Body,
) -> Result<Response, Problem> {
    record(&app, &id)?;
    app.require_running()?;
    let if_none_match = EntityTags::parse(&request, &header::IF_NONE_MATCH)
        .transpose()
        .map_err(|()| {
            Problem::malformed(vec![InvalidValue::parameter(
                "If-None-Match",
                "Must be * or a list of entity tags.",
            )])
        })?;
    let (image_type, bytes) = images::read_upload(&app, &request, body).await?;
    let poster = ImageInfo::of(image_type, &bytes, SystemTime::now());
    let (torrent_id, replaced) = {
        let _lane = app.write_lane().await;
        let current = app.registry()?.get(&id).ok_or_else(torrents::not_found)?;
        if let (Some(condition), Some(existing)) = (&if_none_match, &current.poster)
            && condition.matches_weakly(image_etag(existing).opaque())
        {
            return Err(Problem::new(
                ProblemType::PreconditionFailed,
                "If-None-Match matches the current poster.",
            ));
        }
        if !app
            .temporaries()
            .set_poster(&current.id, Some((poster.clone(), bytes.clone())))
        {
            app.store()
                .set_poster(&current.id, &poster, bytes.to_vec())
                .await
                .map_err(stored)?;
        }
        app.registry()?
            .set_poster(&id, Some(poster.clone()))
            .map_err(|_| torrents::not_found())?;
        (current.id, current.poster.is_some())
    };
    images::uploaded(
        &poster,
        replaced,
        format!("{}/poster", torrent_path(&torrent_id)),
    )
}

pub(crate) async fn delete<E: Engine>(
    State(app): State<Arc<App<E>>>,
    Path(id): Path<String>,
) -> Result<Response, Problem> {
    record(&app, &id)?;
    app.require_running()?;
    let _lane = app.write_lane().await;
    let current = record(&app, &id)?;
    if current.poster.is_none() {
        return Err(no_poster());
    }
    if !app.temporaries().set_poster(&current.id, None) {
        app.store()
            .delete_poster(&current.id)
            .await
            .map_err(stored)?;
    }
    app.registry()?
        .set_poster(&id, None)
        .map_err(|_| torrents::not_found())?;
    Ok(StatusCode::NO_CONTENT.into_response())
}
