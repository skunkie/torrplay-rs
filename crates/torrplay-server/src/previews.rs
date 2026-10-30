// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! Torrent previews: what adding an uploaded torrent file would create,
//! computed without adding it. Nothing is stored and no peer is contacted;
//! the engine only parses the file.

use std::sync::Arc;

use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::Response;
use bytes::Bytes;
use torrplay_api::types as api;
use torrplay_core::engine::{Engine, Source};
use url::Url;

use crate::app::App;
use crate::body::parse_form;
use crate::headers::{MediaType, negotiate};
use crate::problem::{InvalidValue, Problem, ProblemType};
use crate::representation::{self, JSON, NO_STORE, count, info_hashes, preview_file, torrent_path};
use crate::torrents::{
    form_boundary, identify, metainfo_part, pointer, read_form, source_problem, unknown_member,
};

const FORM_DATA: &str = "multipart/form-data";

/// How many files a preview lists.
const FILE_LIMIT: usize = 1000;

pub(crate) async fn create<E: Engine>(
    State(app): State<Arc<App<E>>>,
    headers: HeaderMap,
    body: Body,
) -> Result<Response, Problem> {
    negotiate(&headers, JSON)?;
    let media_type = MediaType::of_request(&headers);
    if media_type.as_ref().map(|media| media.essence.as_str()) != Some(FORM_DATA) {
        return Err(Problem::new(
            ProblemType::UnsupportedMediaType,
            "The operation accepts multipart/form-data.",
        )
        .with_header(header::ACCEPT, HeaderValue::from_static(FORM_DATA)));
    }
    let boundary = form_boundary(media_type.as_ref())?;
    let bytes = read_form(&app, &headers, body).await?;
    let source = Source::Metainfo(form_metainfo(&bytes, &boundary)?.to_vec());

    app.require_engine()?;
    let inspection = app
        .engine()
        .inspect(&source)
        .await
        .map_err(|error| source_problem(error, &source))?;
    // A torrent file always carries its metadata.
    let reported = inspection.metadata.ok_or_else(Problem::internal)?;
    let metadata = identify(&reported, &source)?;
    let existing = app
        .registry()?
        .list()
        .into_iter()
        .find(|record| record.info_hash == inspection.info_hash)
        .map(|record| torrent_path(&record.id));
    let files = metadata.files();
    let mut response = representation::json(
        StatusCode::OK,
        &api::TorrentPreview {
            info_hashes: info_hashes(&inspection.info_hash),
            name: metadata.display_name(),
            size_bytes: count(metadata.size_bytes()),
            file_count: count(files.len() as u64),
            files: files.iter().take(FILE_LIMIT).map(preview_file).collect(),
            files_truncated: files.len() > FILE_LIMIT,
            trackers: tracker_origins(&inspection.trackers),
            links: api::TorrentPreviewLinks { torrent: existing },
        },
    )?;
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, NO_STORE);
    Ok(response)
}

/// The form's one part, `metainfo`.
fn form_metainfo(bytes: &Bytes, boundary: &str) -> Result<Bytes, Problem> {
    let parts = parse_form(bytes, boundary).map_err(|_| {
        Problem::new(
            ProblemType::MalformedRequest,
            "The body is not a valid multipart form.",
        )
    })?;
    let mut errors = Vec::new();
    let mut metainfo = None;
    for part in parts {
        match part.name.as_str() {
            "metainfo" if metainfo.is_some() => {
                errors.push(InvalidValue::pointer(
                    pointer(&part.name),
                    "Must appear once.",
                ));
            }
            "metainfo" => metainfo = Some(metainfo_part(part.data)?),
            other => errors.push(unknown_member(other)),
        }
    }
    if metainfo.is_none() {
        errors.push(InvalidValue::pointer("/metainfo", "Is required."));
    }
    match metainfo {
        Some(file) if errors.is_empty() => Ok(file),
        _ => Err(Problem::invalid_body(errors)),
    }
}

/// Each tracker URL reduced to its scheme, host, and port, in order and
/// each once. The path, query, and credentials may carry a passkey.
fn tracker_origins(trackers: &[String]) -> Vec<String> {
    let mut origins: Vec<String> = Vec::new();
    for tracker in trackers {
        let Ok(url) = Url::parse(tracker) else {
            continue;
        };
        let Some(host) = url.host_str() else {
            continue;
        };
        let origin = match url.port() {
            Some(port) => format!("{}://{host}:{port}", url.scheme()),
            None => format!("{}://{host}", url.scheme()),
        };
        if !origins.contains(&origin) {
            origins.push(origin);
        }
    }
    origins
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trackers_lose_their_paths_queries_and_credentials() {
        let trackers = [
            "http://user:secret@tracker.test/announce?passkey=abc",
            "http://tracker.test/other",
            "udp://tracker.test:6969/announce",
            "https://[2001:db8::1]:8443/a/b/c/passkey/announce",
            "not a url",
            "magnet:?xt=urn:btih:x",
        ]
        .map(String::from);
        assert_eq!(
            tracker_origins(&trackers),
            [
                "http://tracker.test",
                "udp://tracker.test:6969",
                "https://[2001:db8::1]:8443",
            ]
        );
    }
}
