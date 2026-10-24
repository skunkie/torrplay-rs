// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! A torrent's poster: uploading, serving, and deleting its image. Posters
//! live in the store and need no engine.

use std::sync::Arc;
use std::time::SystemTime;

use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use torrplay_core::engine::Engine;
use torrplay_core::library::{POSTER_MAX_BYTES, PosterInfo, PosterType};
use torrplay_core::registry::TorrentRecord;
use torrplay_store::StoreError;

use crate::app::{App, store_failed};
use crate::body::read_limited;
use crate::headers::{MediaType, negotiate};
use crate::problem::{InvalidValue, Problem, ProblemType};
use crate::representation::{NO_CACHE, poster_etag, torrent_path};
use crate::torrents::{self, not_modified, with_etag};

/// The image types an upload may have, as `Accept` lists them.
const ACCEPTED: &str = "image/avif, image/gif, image/jpeg, image/png, image/webp";

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

/// The headers a poster is served with, before its body.
fn headers(poster: &PosterInfo) -> Result<Response, Problem> {
    let mut response = StatusCode::OK.into_response();
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(poster.poster_type.media_type()),
    );
    headers.insert(header::CONTENT_LENGTH, HeaderValue::from(poster.size_bytes));
    headers.insert(header::CACHE_CONTROL, NO_CACHE);
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("sandbox"),
    );
    with_etag(response, &poster_etag(poster))
}

/// Checks a read of the poster: the torrent and its poster exist, the
/// client accepts its type, and its `If-None-Match` does not match.
fn check_read<E: Engine>(
    app: &App<E>,
    id: &str,
    request: &HeaderMap,
) -> Result<Result<(TorrentRecord, PosterInfo), Response>, Problem> {
    let record = record(app, id)?;
    let poster = record.poster.clone().ok_or_else(no_poster)?;
    negotiate(request, poster.poster_type.media_type())?;
    if let Some(response) = not_modified(request, &poster_etag(&poster)) {
        return Ok(Err(response));
    }
    Ok(Ok((record, poster)))
}

pub(crate) async fn get<E: Engine>(
    State(app): State<Arc<App<E>>>,
    Path(id): Path<String>,
    request: HeaderMap,
) -> Result<Response, Problem> {
    let (record, _) = match check_read(&app, &id, &request)? {
        Ok(found) => found,
        Err(not_modified) => return Ok(not_modified),
    };
    // The headers follow the poster as read, which a replacement stored
    // since the record was read may have changed.
    let (poster, bytes) = app
        .store()
        .poster(&record.id)
        .await
        .map_err(stored)?
        .ok_or_else(no_poster)?;
    let (parts, _) = headers(&poster)?.into_parts();
    Ok(Response::from_parts(parts, Body::from(bytes)))
}

pub(crate) async fn head<E: Engine>(
    State(app): State<Arc<App<E>>>,
    Path(id): Path<String>,
    request: HeaderMap,
) -> Result<Response, Problem> {
    match check_read(&app, &id, &request)? {
        Ok((_, poster)) => headers(&poster),
        Err(not_modified) => Ok(not_modified),
    }
}

pub(crate) async fn put<E: Engine>(
    State(app): State<Arc<App<E>>>,
    Path(id): Path<String>,
    request: HeaderMap,
    body: Body,
) -> Result<Response, Problem> {
    let record = record(&app, &id)?;
    app.require_running()?;
    let Some(poster_type) =
        MediaType::of_request(&request).and_then(|media| PosterType::parse(&media.essence))
    else {
        return Err(Problem::new(
            ProblemType::UnsupportedMediaType,
            format!("The operation accepts {ACCEPTED}."),
        )
        .with_header(header::ACCEPT, HeaderValue::from_static(ACCEPTED)));
    };
    let bytes = read_limited(
        &request,
        body,
        POSTER_MAX_BYTES,
        POSTER_MAX_BYTES as u64,
        app.body_timeout(),
    )
    .await?;
    if !poster_type.matches(&bytes) {
        return Err(Problem::invalid_body(vec![InvalidValue::pointer(
            "",
            format!("Is not an {} image.", poster_type.media_type()),
        )]));
    }
    let poster = PosterInfo::of(poster_type, &bytes, SystemTime::now());
    let replaced = {
        let _lane = app.write_lane().await;
        let current = app.registry()?.get(&id).ok_or_else(torrents::not_found)?;
        app.store()
            .set_poster(&record.id, &poster, bytes.to_vec())
            .await
            .map_err(stored)?;
        app.registry()?
            .set_poster(&id, Some(poster.clone()))
            .map_err(|_| torrents::not_found())?;
        current.poster.is_some()
    };
    let etag = poster_etag(&poster);
    if replaced {
        return with_etag(StatusCode::NO_CONTENT.into_response(), &etag);
    }
    let mut response = with_etag(StatusCode::CREATED.into_response(), &etag)?;
    let location = HeaderValue::try_from(format!("{}/poster", torrent_path(&record.id)))
        .map_err(|_| Problem::internal())?;
    response.headers_mut().insert(header::LOCATION, location);
    Ok(response)
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
    app.store()
        .delete_poster(&current.id)
        .await
        .map_err(stored)?;
    app.registry()?
        .set_poster(&id, None)
        .map_err(|_| torrents::not_found())?;
    Ok(StatusCode::NO_CONTENT.into_response())
}
