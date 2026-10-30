// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! A file's viewings: creating, reading, and removing the application
//! viewings that estimate how far a player without progress reports
//! watched.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use torrplay_api as api;
use torrplay_core::engine::Engine;
use torrplay_core::id::{TorrentId, ViewingId, random_torrent_id_bytes};

use crate::app::App;
use crate::files::{unknown_viewing, with_metadata};
use crate::headers::negotiate;
use crate::inference::{AppStatus, FileKey};
use crate::problem::Problem;
use crate::representation::{self, JSON, NO_CACHE, json};

/// The torrent `id`'s file `file_id`, whose metadata must be available.
/// Viewings are the application's own records, so only creating one, for
/// streaming, needs the engine; reading one releases a temporary torrent,
/// so reads and removals are changes that shutdown refuses.
async fn resolve<E: Engine>(app: &App<E>, id: &str, file_id: &str) -> Result<FileKey, Problem> {
    app.require_running()?;
    let (view, metadata) = with_metadata(app, id).await?;
    let (_, file) = metadata
        .file(file_id)
        .ok_or_else(|| Problem::not_found("The torrent has no file with this ID."))?;
    Ok((view.record.id.clone(), file.id().clone()))
}

fn representation(
    torrent: &TorrentId,
    file_id: &str,
    id: &str,
    status: &AppStatus,
) -> api::Viewing {
    let file = format!("{}/files/{file_id}", representation::torrent_path(torrent));
    api::Viewing {
        id: id.to_owned(),
        position_seconds: status.position.map(|(position, _)| position.as_secs_f64()),
        duration_seconds: status.position.map(|(_, duration)| duration.as_secs_f64()),
        streaming: status.streaming,
        created_at: status.created_at.into(),
        last_active_at: status.last_active_at.map(Into::into),
        expires_at: status.expires_at.into(),
        links: api::ViewingLinks {
            self_field: format!("{file}/viewings/{id}"),
            content: format!("{file}/content?viewing_id={id}"),
            torrent: representation::torrent_path(torrent),
            file,
        },
    }
}

pub(crate) async fn create<E: Engine>(
    State(app): State<Arc<App<E>>>,
    Path((id, file_id)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<Response, Problem> {
    negotiate(&headers, JSON)?;
    app.require_engine()?;
    let key = resolve(&app, &id, &file_id).await?;
    let viewing = ViewingId::from_random_bytes(random_torrent_id_bytes().map_err(|error| {
        tracing::error!(%error, "no random bytes for a viewing ID");
        Problem::internal()
    })?);
    let torrent = key.0.clone();
    let status = app.inference().create(viewing.clone(), key, app.now());
    let body = representation(&torrent, &file_id, viewing.as_str(), &status);
    let location =
        HeaderValue::try_from(body.links.self_field.clone()).map_err(|_| Problem::internal())?;
    let mut response = json(StatusCode::CREATED, &body)?;
    response.headers_mut().insert(header::LOCATION, location);
    Ok(response)
}

pub(crate) async fn get<E: Engine>(
    State(app): State<Arc<App<E>>>,
    Path((id, file_id, viewing_id)): Path<(String, String, String)>,
    headers: HeaderMap,
) -> Result<Response, Problem> {
    negotiate(&headers, JSON)?;
    let key = resolve(&app, &id, &file_id).await?;
    let status = app
        .inference()
        .read(&viewing_id, &key, app.now())
        .ok_or_else(unknown_viewing)?;
    let body = representation(&key.0, &file_id, &viewing_id, &status);
    let mut response = json(StatusCode::OK, &body)?;
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, NO_CACHE);
    Ok(response)
}

pub(crate) async fn delete<E: Engine>(
    State(app): State<Arc<App<E>>>,
    Path((id, file_id, viewing_id)): Path<(String, String, String)>,
) -> Result<Response, Problem> {
    let key = resolve(&app, &id, &file_id).await?;
    if app.inference().remove(&viewing_id, &key) {
        Ok(StatusCode::NO_CONTENT.into_response())
    } else {
        Err(unknown_viewing())
    }
}
