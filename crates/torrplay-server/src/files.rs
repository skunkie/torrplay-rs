// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! A torrent's files and their content.

use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use futures_util::TryStreamExt;
use tokio_util::io::ReaderStream;
use torrplay_api as api;
use torrplay_core::engine::{ByteRange, Engine};
use torrplay_core::lifecycle::{EntityTag, ObservedState};
use torrplay_core::metadata::Metadata;
use torrplay_core::pagination::{
    CURSOR_PARAMETER, FileSortKey, FilterFingerprint, PAGE_SIZE_PARAMETER, PageRequest, paginate,
};

use crate::app::{App, TorrentView};
use crate::content::{RangeRequest, disposition, media_type, range_request};
use crate::headers::{EntityTags, negotiate};
use crate::problem::{Problem, ProblemType};
use crate::query::{Paging, Query, page_error, reject};
use crate::representation::{self, JSON, torrent_path};
use crate::torrents::{self, not_modified};

/// The chunk size of content streams.
const STREAM_CHUNK_BYTES: usize = 64 * 1024;

fn metadata_unavailable() -> Problem {
    Problem::new(
        ProblemType::LifecycleConflict,
        "The torrent's metadata is still being retrieved.",
    )
}

fn unknown_file() -> Problem {
    Problem::not_found("The torrent has no file with this ID.")
}

/// Returns the torrent `id` and its metadata, which must be available.
pub(crate) async fn with_metadata<E: Engine>(
    app: &App<E>,
    id: &str,
) -> Result<(TorrentView, Arc<Metadata>), Problem> {
    let record = app.registry()?.get(id).ok_or_else(torrents::not_found)?;
    let view = app.observe(record).await?;
    let metadata = view
        .record
        .metadata
        .clone()
        .ok_or_else(metadata_unavailable)?;
    Ok((view, metadata))
}

pub(crate) async fn list<E: Engine>(
    State(app): State<Arc<App<E>>>,
    Path(id): Path<String>,
    uri: Uri,
    headers: HeaderMap,
) -> Result<Response, Problem> {
    negotiate(&headers, JSON)?;
    let query = Query::parse(uri.query());
    let mut errors = query.unexpected(
        &[PAGE_SIZE_PARAMETER, CURSOR_PARAMETER],
        &[PAGE_SIZE_PARAMETER, CURSOR_PARAMETER],
    );
    let paging = Paging::parse(&query, &mut errors);
    reject(errors)?;
    let request =
        PageRequest::<FileSortKey>::new(paging.page_size, paging.cursor, FilterFingerprint::none())
            .map_err(|error| Problem::malformed(vec![page_error(error)]))?;
    let (view, metadata) = with_metadata(&app, &id).await?;
    let page = paginate(
        metadata.files().iter().enumerate(),
        |(position, _)| FileSortKey(*position as u64),
        &request,
    );
    let path = format!("{}/files", torrent_path(&view.record.id));
    let body = api::TorrentFilePage {
        items: page
            .items
            .iter()
            .map(|(_, file)| representation::file(&view.record.id, file))
            .collect(),
        links: api::TorrentFilePageLinks {
            next: page
                .next_cursor
                .as_deref()
                .map(|cursor| paging.link(&path, &[], Some(cursor))),
            self_field: paging.link(&path, &[], paging.cursor),
        },
    };
    representation::json(StatusCode::OK, &body)
}

pub(crate) async fn get<E: Engine>(
    State(app): State<Arc<App<E>>>,
    Path((id, file_id)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<Response, Problem> {
    negotiate(&headers, JSON)?;
    let (view, metadata) = with_metadata(&app, &id).await?;
    let (_, file) = metadata.file(&file_id).ok_or_else(unknown_file)?;
    representation::json(StatusCode::OK, &representation::file(&view.record.id, file))
}

pub(crate) async fn get_content<E: Engine>(
    State(app): State<Arc<App<E>>>,
    Path((id, file_id)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<Response, Problem> {
    content(&app, &id, &file_id, &headers, true).await
}

/// HEAD answers as GET would, from the application's records, without
/// opening a reader.
pub(crate) async fn head_content<E: Engine>(
    State(app): State<Arc<App<E>>>,
    Path((id, file_id)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<Response, Problem> {
    content(&app, &id, &file_id, &headers, false).await
}

async fn content<E: Engine>(
    app: &App<E>,
    id: &str,
    file_id: &str,
    headers: &HeaderMap,
    stream: bool,
) -> Result<Response, Problem> {
    let (view, metadata) = with_metadata(app, id).await?;
    let (position, file) = metadata.file(file_id).ok_or_else(unknown_file)?;
    if matches!(
        view.observed_state(),
        ObservedState::Paused | ObservedState::Failed
    ) {
        return Err(Problem::new(
            ProblemType::LifecycleConflict,
            "The torrent is not started; start it to stream its files.",
        ));
    }
    let name = representation::file_name(file);
    let (media_type, inline) = media_type(&name);
    negotiate(headers, media_type)?;
    if stream {
        app.require_engine()?;
    }
    // Info hash and position identify the file's bytes, which never change.
    let etag = EntityTag::new(format!("sha1-{}-{position}", view.record.info_hash));
    if let Some(response) = not_modified(headers, &etag) {
        return Ok(response);
    }
    let length = file.size_bytes();
    let mut request = headers
        .get(header::RANGE)
        .and_then(|value| value.to_str().ok())
        .map_or(RangeRequest::Full, |value| range_request(value, length));
    if request != RangeRequest::Full
        && let Some(if_range) = EntityTags::parse(headers, &header::IF_RANGE)
    {
        // A date, `*`, or another tag does not identify these bytes, so the
        // whole file is sent instead.
        let current = matches!(&if_range, Ok(EntityTags::List(_)))
            && if_range.is_ok_and(|tags| tags.matches_strongly(etag.opaque()));
        if !current {
            request = RangeRequest::Full;
        }
    }
    let (status, range) = match request {
        RangeRequest::Full => (StatusCode::OK, ByteRange::new(0, length)),
        RangeRequest::Partial(range) => (StatusCode::PARTIAL_CONTENT, Some(range)),
        RangeRequest::Unsatisfiable => {
            let content_range = HeaderValue::try_from(format!("bytes */{length}"))
                .map_err(|_| Problem::internal())?;
            return Err(Problem::new(
                ProblemType::RangeNotSatisfiable,
                "The range starts after the end of the file.",
            )
            .with_header(header::CONTENT_RANGE, content_range));
        }
    };
    let range = range.ok_or_else(Problem::internal)?;

    let body = if stream {
        let reader = app
            .engine()
            .open_reader(&view.record.id, file.index(), range)
            .await?;
        // A read failure after the headers ends the stream; it never
        // becomes a second response.
        Body::from_stream(
            ReaderStream::with_capacity(reader, STREAM_CHUNK_BYTES).map_err(StreamEnded::from),
        )
    } else {
        Body::empty()
    };
    let mut response = (status, body).into_response();
    let values = response.headers_mut();
    let header_value = |text: String| HeaderValue::try_from(text).map_err(|_| Problem::internal());
    values.insert(header::CONTENT_TYPE, HeaderValue::from_static(media_type));
    values.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    values.insert(header::CONTENT_LENGTH, HeaderValue::from(range.len_bytes()));
    if status == StatusCode::PARTIAL_CONTENT {
        values.insert(
            header::CONTENT_RANGE,
            header_value(format!(
                "bytes {}-{}/{length}",
                range.start(),
                range.end() - 1
            ))?,
        );
    }
    values.insert(
        header::CONTENT_DISPOSITION,
        header_value(disposition(&name, inline))?,
    );
    values.insert(header::ETAG, header_value(etag.to_string())?);
    values.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    values.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("sandbox"),
    );
    Ok(response)
}

/// Why a content stream ended early. Over HTTP/2, hyper resets the stream
/// with the `h2` reason it finds among an error's sources, so a stream the
/// engine canceled or whose piece did not arrive is reset with `CANCEL`;
/// other failures keep hyper's `INTERNAL_ERROR`. Over HTTP/1.1 the
/// connection closes either way.
#[derive(Debug)]
struct StreamEnded {
    error: std::io::Error,
    reason: Option<h2::Error>,
}

impl From<std::io::Error> for StreamEnded {
    fn from(error: std::io::Error) -> Self {
        let canceled = matches!(
            error.kind(),
            std::io::ErrorKind::ConnectionAborted | std::io::ErrorKind::TimedOut
        );
        Self {
            reason: canceled.then(|| h2::Reason::CANCEL.into()),
            error,
        }
    }
}

impl std::fmt::Display for StreamEnded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "the content stream ended early: {}", self.error)
    }
}

impl std::error::Error for StreamEnded {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match &self.reason {
            Some(reason) => Some(reason),
            None => Some(&self.error),
        }
    }
}
