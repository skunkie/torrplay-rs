// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! The Stremio addon's routes, as `docs/stremio.md` specifies them: the
//! manifest, catalogs, metadata, and streams `torrplay-stremio` answers
//! from the library, and the play URLs, which serve files as the native
//! content operation does. The authentication guard checks the credential
//! a path or query carries before these run.

use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use serde::Serialize;
use torrplay_core::engine::Engine;
use torrplay_core::library::Kind;
use torrplay_core::lifecycle::Retention;
use torrplay_stremio::protocol::{MetaResponse, Metas, Streams};
use torrplay_stremio::{Base, Extra, Library};

use crate::app::App;
use crate::files::{self, Watcher};
use crate::problem::{InvalidValue, Problem};
use crate::query::Query;
use crate::representation::file_name;
use crate::shelf;

/// The response header that lets Stremio's web client read the addon.
pub(crate) const ALLOW_ORIGIN: HeaderValue = HeaderValue::from_static("*");

/// The library as the addon presents it: the shelved movies and series with
/// their video files.
async fn library<E: Engine>(app: &App<E>) -> Result<Library, Problem> {
    let shelved = shelf::shelved(app).await?.unwrap_or_default();
    let torrents = shelved
        .into_iter()
        .filter_map(|torrent| {
            let kind = match torrent.kind? {
                Kind::Movie => torrplay_stremio::Kind::Movie,
                Kind::Series => torrplay_stremio::Kind::Series,
                Kind::Other => return None,
            };
            let files: Vec<torrplay_stremio::File> = torrent
                .files
                .into_iter()
                .filter(|file| file.media_type.starts_with("video/"))
                .map(|file| torrplay_stremio::File {
                    id: file.id.to_string(),
                    name: file.name,
                    media_type: file.media_type,
                    episode: file.episode,
                })
                .collect();
            (!files.is_empty()).then(|| torrplay_stremio::Torrent {
                id: torrent.id.to_string(),
                title: torrent.title,
                name: torrent.name,
                kind,
                imdb_id: torrent.imdb_id,
                genres: torrent.genres,
                has_poster: torrent.has_poster,
                files,
            })
        })
        .collect();
    Ok(Library { torrents })
}

/// The origin the request named, and the token its path or query carried.
fn origin(headers: &HeaderMap) -> String {
    let host = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    format!("http://{host}")
}

fn token<'a>(params: &'a HashMap<String, String>, query: &'a Query) -> Option<&'a str> {
    params
        .get("token")
        .map(String::as_str)
        .or_else(|| query.first("token"))
}

/// A JSON document of the protocol.
fn document(value: &impl Serialize) -> Result<Response, Problem> {
    let body = serde_json::to_vec(value).map_err(|_| Problem::internal())?;
    Ok((
        StatusCode::OK,
        [(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        )],
        body,
    )
        .into_response())
}

/// A path segment's name without `.json`, `None` without it.
fn resource(segment: &str) -> Option<&str> {
    segment.strip_suffix(".json")
}

pub(crate) async fn manifest<E: Engine>(
    State(app): State<Arc<App<E>>>,
) -> Result<Response, Problem> {
    let library = library(&app).await?;
    document(&torrplay_stremio::manifest(
        env!("CARGO_PKG_VERSION"),
        library.genres(),
    ))
}

pub(crate) async fn catalog<E: Engine>(
    State(app): State<Arc<App<E>>>,
    Path(params): Path<HashMap<String, String>>,
    uri: Uri,
    headers: HeaderMap,
) -> Result<Response, Problem> {
    let extra = match params.get("extra") {
        Some(segment) => {
            let segment = resource(segment)
                .ok_or_else(|| Problem::not_found("A catalog's path ends in .json."))?;
            Extra::parse(segment).map_err(|invalid| {
                Problem::malformed(vec![InvalidValue::parameter("extra", invalid.0)])
            })?
        }
        None => Extra::default(),
    };
    let kind = params
        .get("type")
        .and_then(|kind| torrplay_stremio::Kind::parse(kind));
    // Only the path's last segment ends in .json.
    let catalog = params.get("id").and_then(|id| {
        if params.contains_key("extra") {
            Some(id.as_str())
        } else {
            resource(id)
        }
    });
    let query = Query::parse(uri.query());
    let origin = origin(&headers);
    let base = Base {
        origin: &origin,
        token: token(&params, &query),
    };
    let metas = match (kind, catalog) {
        (Some(kind), Some(catalog)) if catalog == kind.catalog_id() => {
            library(&app).await?.catalog(kind, &extra, base)
        }
        // An unknown catalog lists nothing, as the protocol expects.
        _ => Vec::new(),
    };
    document(&Metas { metas })
}

pub(crate) async fn meta<E: Engine>(
    State(app): State<Arc<App<E>>>,
    Path(params): Path<HashMap<String, String>>,
    uri: Uri,
    headers: HeaderMap,
) -> Result<Response, Problem> {
    let id = params
        .get("id")
        .and_then(|id| resource(id))
        .ok_or_else(no_item)?;
    let query = Query::parse(uri.query());
    let origin = origin(&headers);
    let base = Base {
        origin: &origin,
        token: token(&params, &query),
    };
    let meta = library(&app).await?.meta(id, base).ok_or_else(no_item)?;
    document(&MetaResponse { meta })
}

fn no_item() -> Problem {
    Problem::not_found("The addon has no item with this ID.")
}

pub(crate) async fn stream<E: Engine>(
    State(app): State<Arc<App<E>>>,
    Path(params): Path<HashMap<String, String>>,
    uri: Uri,
    headers: HeaderMap,
) -> Result<Response, Problem> {
    let query = Query::parse(uri.query());
    let origin = origin(&headers);
    let base = Base {
        origin: &origin,
        token: token(&params, &query),
    };
    let streams = match params.get("id").and_then(|id| resource(id)) {
        Some(id) => library(&app).await?.streams(id, base),
        None => Vec::new(),
    };
    document(&Streams { streams })
}

pub(crate) async fn get_play<E: Engine>(
    State(app): State<Arc<App<E>>>,
    Path(params): Path<HashMap<String, String>>,
    headers: HeaderMap,
) -> Result<Response, Problem> {
    play(&app, &params, &headers, true).await
}

/// HEAD answers as GET would, without opening a reader.
pub(crate) async fn head_play<E: Engine>(
    State(app): State<Arc<App<E>>>,
    Path(params): Path<HashMap<String, String>>,
    headers: HeaderMap,
) -> Result<Response, Problem> {
    play(&app, &params, &headers, false).await
}

async fn play<E: Engine>(
    app: &App<E>,
    params: &HashMap<String, String>,
    headers: &HeaderMap,
    stream: bool,
) -> Result<Response, Problem> {
    let param = |name: &str| params.get(name).map(String::as_str).unwrap_or_default();
    let (torrent, file, name) = (param("torrent"), param("file"), param("name"));
    let (view, metadata) = files::with_metadata(app, torrent).await?;
    if view.record.configuration.retention() == Retention::Temporary {
        return Err(Problem::not_found("The addon has no such torrent."));
    }
    let (_, found) = metadata
        .file(file)
        .ok_or_else(|| Problem::not_found("The torrent has no file with this ID."))?;
    if file_name(found) != name {
        return Err(Problem::not_found(
            "The path's file name is not the file's name.",
        ));
    }
    files::content(app, torrent, file, headers, stream, Watcher::Nobody).await
}
