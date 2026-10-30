// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! A torrent's torrent file, which `createTorrent` accepts as an upload, so a
//! client can re-create a deleted torrent with the same info hash and file
//! IDs. A persisted torrent's file lives in the store, and a temporary
//! torrent's in memory.

use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use torrplay_core::engine::{Engine, Source};
use torrplay_core::id::InfoHashV1;
use torrplay_core::lifecycle::EntityTag;
use torrplay_core::metadata::Metadata;
use torrplay_store::StoreError;

use crate::app::{App, store_failed};
use crate::content::disposition;
use crate::files::with_metadata;
use crate::headers::negotiate;
use crate::problem::Problem;
use crate::representation::NO_STORE;
use crate::torrents::{self, not_modified, with_etag};

const BITTORRENT: &str = "application/x-bittorrent";

/// The torrent file's validator. Once metadata is known, a torrent's file
/// never changes, so the info hash identifies its bytes.
fn etag(info_hash: &InfoHashV1) -> EntityTag {
    let hex = info_hash.to_string();
    EntityTag::new(format!("mti-{}", &hex[..24]))
}

/// The headers the torrent file is served with, before its body.
fn headers(
    info_hash: &InfoHashV1,
    metadata: &Metadata,
    size_bytes: usize,
) -> Result<Response, Problem> {
    let mut response = StatusCode::OK.into_response();
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(BITTORRENT));
    headers.insert(header::CONTENT_LENGTH, HeaderValue::from(size_bytes));
    // Path separators would name a directory the client does not mean.
    let name = metadata.display_name().replace(['/', '\\'], "_");
    let disposition = disposition(&format!("{name}.torrent"), false);
    headers.insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::try_from(disposition).map_err(|_| Problem::internal())?,
    );
    // Tracker URLs may carry a private tracker's passkey.
    headers.insert(header::CACHE_CONTROL, NO_STORE);
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    with_etag(response, &etag(info_hash))
}

/// The torrent file's bytes: a temporary torrent's from memory, a persisted
/// one's from the store. Metadata is recorded only once the file is kept,
/// so a torrent with metadata has one.
async fn metainfo<E: Engine>(app: &App<E>, id: &str) -> Result<Vec<u8>, Problem> {
    let record = app.registry()?.get(id).ok_or_else(torrents::not_found)?;
    match app.temporaries().source(&record.id) {
        Some(Source::Metainfo(bytes)) => Ok(bytes),
        Some(Source::MagnetUri(_)) => Err(Problem::internal()),
        None => match app.store().metainfo(&record.id).await {
            Ok(Some(bytes)) => Ok(bytes),
            Ok(None) => Err(Problem::internal()),
            Err(StoreError::UnknownTorrent) => Err(torrents::not_found()),
            Err(error) => Err(store_failed(error)),
        },
    }
}

pub(crate) async fn get<E: Engine>(
    State(app): State<Arc<App<E>>>,
    Path(id): Path<String>,
    request: HeaderMap,
) -> Result<Response, Problem> {
    negotiate(&request, BITTORRENT)?;
    let (view, metadata) = with_metadata(&app, &id).await?;
    let info_hash = view.record.info_hash;
    if let Some(response) = not_modified(&request, &etag(&info_hash)) {
        return Ok(response);
    }
    let bytes = metainfo(&app, &id).await?;
    let (parts, _) = headers(&info_hash, &metadata, bytes.len())?.into_parts();
    Ok(Response::from_parts(parts, Body::from(bytes)))
}

pub(crate) async fn head<E: Engine>(
    State(app): State<Arc<App<E>>>,
    Path(id): Path<String>,
    request: HeaderMap,
) -> Result<Response, Problem> {
    negotiate(&request, BITTORRENT)?;
    let (view, metadata) = with_metadata(&app, &id).await?;
    let info_hash = view.record.info_hash;
    if let Some(response) = not_modified(&request, &etag(&info_hash)) {
        return Ok(response);
    }
    // Content-Length needs the file's size, which only its bytes give.
    let bytes = metainfo(&app, &id).await?;
    headers(&info_hash, &metadata, bytes.len())
}
