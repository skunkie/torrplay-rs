// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! The DLNA media server's HTTP resources, as `docs/dlna.md` describes
//! them: the descriptions, the SOAP control endpoints, and media and
//! posters, over the protocols of `torrplay-dlna`.
//!
//! They are outside the native API's authentication. While the media server
//! is off they are not-found, and they serve only clients with private
//! addresses.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex, PoisonError};

use axum::Extension;
use axum::body::Body;
use axum::extract::{ConnectInfo, Path, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use sha2::{Digest, Sha256};
use torrplay_core::engine::Engine;
use torrplay_core::library::{self, Kind};
use torrplay_core::lifecycle::ObservedState;
use torrplay_dlna::description::{self, Device};
use torrplay_dlna::dlna;
use torrplay_dlna::services::{self, Directory};
use torrplay_dlna::soap::{self, Action, Fault};
use torrplay_dlna::tree::{self, Base, Catalog};

use crate::app::App;
use crate::body::read_limited;
use crate::content;
use crate::files;
use crate::posters;
use crate::problem::{Problem, ProblemType};
use crate::representation::{self, file_name};

/// The `SOAPACTION` request header.
const SOAP_ACTION: &str = "soapaction";

/// The media server's state: its identity and its system update ID.
#[derive(Debug)]
pub(crate) struct MediaServer {
    pub(crate) device: Device,
    /// The digest of the last catalog served, and the update ID it had.
    update: Mutex<Option<([u8; 32], u32)>>,
}

impl MediaServer {
    pub(crate) fn new(device: Device) -> Self {
        Self {
            device,
            update: Mutex::new(None),
        }
    }

    /// The system update ID of `catalog`: 1 for the first one served, and
    /// one more whenever the catalog changes.
    fn update_id(&self, catalog: &Catalog) -> u32 {
        let digest = digest(catalog);
        let mut update = self.update.lock().unwrap_or_else(PoisonError::into_inner);
        let id = match *update {
            Some((last, id)) if last == digest => id,
            Some((_, id)) => id.wrapping_add(1).max(1),
            None => 1,
        };
        *update = Some((digest, id));
        id
    }
}

/// A digest of everything the catalog shows control points.
fn digest(catalog: &Catalog) -> [u8; 32] {
    let mut hasher = Sha256::new();
    let mut field = |text: &str| {
        hasher.update((text.len() as u64).to_be_bytes());
        hasher.update(text.as_bytes());
    };
    for torrent in &catalog.torrents {
        field(&torrent.id);
        field(&torrent.title);
        field(&format!("{:?}", torrent.kind));
        field(&torrent.categories.join("\n"));
        field(&torrent.genres.join("\n"));
        field(if torrent.has_poster { "poster" } else { "" });
        for file in &torrent.files {
            field(&file.id);
            field(&file.name);
            field(&file.size_bytes.to_string());
            field(file.media_type);
        }
    }
    hasher.finalize().into()
}

/// Refuses the request unless the media server is on and the client is on
/// a private network.
fn gate<E: Engine>(
    app: &App<E>,
    client: Option<Extension<ConnectInfo<SocketAddr>>>,
) -> Result<&MediaServer, Problem> {
    let server = app
        .media_server()
        .filter(|_| app.settings().explicit.effective().dlna_enabled)
        .ok_or_else(|| Problem::not_found("No resource has this path."))?;
    let private = client.is_some_and(|Extension(ConnectInfo(address))| {
        torrplay_dlna::network::is_private(address.ip())
    });
    if !private {
        return Err(Problem::new(
            ProblemType::Forbidden,
            "The media server serves only clients on a private network.",
        ));
    }
    Ok(server)
}

fn xml(status: StatusCode, body: String) -> Response {
    (
        status,
        [(header::CONTENT_TYPE, HeaderValue::from_static(soap::XML))],
        body,
    )
        .into_response()
}

pub(crate) async fn device<E: Engine>(
    State(app): State<Arc<App<E>>>,
    client: Option<Extension<ConnectInfo<SocketAddr>>>,
) -> Result<Response, Problem> {
    let server = gate(&app, client)?;
    Ok(xml(StatusCode::OK, server.device.description()))
}

pub(crate) async fn content_directory_description<E: Engine>(
    State(app): State<Arc<App<E>>>,
    client: Option<Extension<ConnectInfo<SocketAddr>>>,
) -> Result<Response, Problem> {
    gate(&app, client)?;
    Ok(xml(StatusCode::OK, description::content_directory()))
}

pub(crate) async fn connection_manager_description<E: Engine>(
    State(app): State<Arc<App<E>>>,
    client: Option<Extension<ConnectInfo<SocketAddr>>>,
) -> Result<Response, Problem> {
    gate(&app, client)?;
    Ok(xml(StatusCode::OK, description::connection_manager()))
}

/// Reads a SOAP action: its `SOAPACTION` header and its body.
async fn action<E: Engine>(
    app: &App<E>,
    headers: &HeaderMap,
    body: Body,
) -> Result<Result<Action, Fault>, Problem> {
    let bytes = read_limited(
        headers,
        body,
        soap::BODY_LIMIT_BYTES,
        soap::BODY_LIMIT_BYTES as u64,
        app.body_timeout(),
    )
    .await?;
    let Some(soap_action) = headers
        .get(SOAP_ACTION)
        .and_then(|value| value.to_str().ok())
    else {
        return Ok(Err(Fault::InvalidAction));
    };
    Ok(Action::parse(soap_action, &bytes))
}

fn answered(result: Result<String, Fault>) -> Response {
    match result {
        Ok(body) => xml(StatusCode::OK, body),
        Err(fault) => xml(StatusCode::INTERNAL_SERVER_ERROR, fault.envelope()),
    }
}

pub(crate) async fn content_directory<E: Engine>(
    State(app): State<Arc<App<E>>>,
    client: Option<Extension<ConnectInfo<SocketAddr>>>,
    headers: HeaderMap,
    body: Body,
) -> Result<Response, Problem> {
    let server = gate(&app, client)?;
    let action = match action(&app, &headers, body).await? {
        Ok(action) => action,
        Err(fault) => return Ok(answered(Err(fault))),
    };
    let catalog = catalog(&app).await?;
    let update_id = catalog
        .as_ref()
        .map_or(0, |catalog| server.update_id(catalog));
    let host = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    let base = format!("http://{host}");
    Ok(answered(services::content_directory(
        &action,
        &Directory {
            catalog: catalog.as_ref(),
            update_id,
            base: Base(&base),
        },
    )))
}

pub(crate) async fn connection_manager<E: Engine>(
    State(app): State<Arc<App<E>>>,
    client: Option<Extension<ConnectInfo<SocketAddr>>>,
    headers: HeaderMap,
    body: Body,
) -> Result<Response, Problem> {
    gate(&app, client)?;
    Ok(answered(
        action(&app, &headers, body)
            .await?
            .and_then(|action| services::connection_manager(&action)),
    ))
}

/// A playable file with what orders it: its episode number, if its name
/// has one, and its position in the metadata.
type OrderedFile = (Option<(u32, u32)>, usize, tree::File);

/// The library as the media server presents it, or `None` while the stored
/// torrents are being restored: the torrents with metadata and a playable
/// file that are neither paused nor failed, newest first.
async fn catalog<E: Engine>(app: &App<E>) -> Result<Option<Catalog>, Problem> {
    let Ok(registry) = app.registry() else {
        return Ok(None);
    };
    let mut torrents = Vec::new();
    for record in registry.list().into_iter().rev() {
        let view = app.observe(record).await?;
        if matches!(
            view.observed_state(),
            ObservedState::Paused | ObservedState::Failed
        ) {
            continue;
        }
        let record = &view.record;
        let Some(metadata) = record.metadata.as_deref() else {
            continue;
        };
        let library = record.configuration.library();
        let kind = representation::reported_kind(library, Some(metadata));
        let mut files: Vec<OrderedFile> = metadata
            .files()
            .iter()
            .enumerate()
            .filter_map(|(index, file)| {
                let name = file_name(file);
                let (media_type, _) = content::media_type(&name);
                let playable = ["audio/", "video/", "image/"]
                    .iter()
                    .any(|prefix| media_type.starts_with(prefix));
                playable.then(|| {
                    (
                        library::episode_number(&name),
                        index,
                        tree::File {
                            id: file.id().to_string(),
                            name,
                            size_bytes: file.size_bytes(),
                            media_type,
                        },
                    )
                })
            })
            .collect();
        if files.is_empty() {
            continue;
        }
        if kind == Some(Kind::Series) {
            // As up next orders episodes: by season and episode, files
            // without a pattern last, metadata order breaking ties.
            files.sort_by_key(|(number, index, _)| (number.is_none(), *number, *index));
        }
        torrents.push(tree::Torrent {
            id: record.id.to_string(),
            title: library
                .title
                .clone()
                .unwrap_or_else(|| metadata.display_name()),
            kind: kind.map(|kind| match kind {
                Kind::Movie => tree::Kind::Movie,
                Kind::Series => tree::Kind::Series,
                Kind::Other => tree::Kind::Other,
            }),
            categories: library.categories.clone(),
            genres: library.genres.clone(),
            has_poster: record.poster.is_some(),
            files: files.into_iter().map(|(_, _, file)| file).collect(),
        });
    }
    Ok(Some(Catalog { torrents }))
}

pub(crate) async fn get_media<E: Engine>(
    state: State<Arc<App<E>>>,
    client: Option<Extension<ConnectInfo<SocketAddr>>>,
    path: Path<(String, String, String)>,
    headers: HeaderMap,
) -> Result<Response, Problem> {
    media(state, client, path, headers, true).await
}

/// HEAD answers as GET would, without opening a reader.
pub(crate) async fn head_media<E: Engine>(
    state: State<Arc<App<E>>>,
    client: Option<Extension<ConnectInfo<SocketAddr>>>,
    path: Path<(String, String, String)>,
    headers: HeaderMap,
) -> Result<Response, Problem> {
    media(state, client, path, headers, false).await
}

async fn media<E: Engine>(
    State(app): State<Arc<App<E>>>,
    client: Option<Extension<ConnectInfo<SocketAddr>>>,
    Path((torrent, file, name)): Path<(String, String, String)>,
    headers: HeaderMap,
    stream: bool,
) -> Result<Response, Problem> {
    gate(&app, client)?;
    let text = |name: &str| headers.get(name).and_then(|value| value.to_str().ok());
    let negotiated = dlna::negotiate(text(dlna::GET_CONTENT_FEATURES), text(dlna::TRANSFER_MODE))
        .map_err(|_| {
        Problem::new(
            ProblemType::NotAcceptable,
            "The transfer mode must be Streaming, Interactive, or Background.",
        )
    })?;
    let (_, metadata) = files::with_metadata(&app, &torrent).await?;
    let (_, found) = metadata
        .file(&file)
        .ok_or_else(|| Problem::not_found("The torrent has no file with this ID."))?;
    if file_name(found) != name {
        return Err(Problem::not_found(
            "The path's file name is not the file's name.",
        ));
    }
    let mut response = files::content(&app, &torrent, &file, &headers, stream).await?;
    let values = response.headers_mut();
    if negotiated.content_features {
        values.insert(
            HeaderName::from_static(dlna::CONTENT_FEATURES),
            HeaderValue::from_static(dlna::FEATURES),
        );
    }
    if let Some(mode) = negotiated.transfer_mode {
        values.insert(
            HeaderName::from_static(dlna::TRANSFER_MODE),
            HeaderValue::from_static(mode.as_str()),
        );
    }
    Ok(response)
}

pub(crate) async fn get_poster<E: Engine>(
    State(app): State<Arc<App<E>>>,
    client: Option<Extension<ConnectInfo<SocketAddr>>>,
    path: Path<String>,
    headers: HeaderMap,
) -> Result<Response, Problem> {
    gate(&app, client)?;
    posters::get(State(app), path, headers).await
}

pub(crate) async fn head_poster<E: Engine>(
    State(app): State<Arc<App<E>>>,
    client: Option<Extension<ConnectInfo<SocketAddr>>>,
    path: Path<String>,
    headers: HeaderMap,
) -> Result<Response, Problem> {
    gate(&app, client)?;
    posters::head(State(app), path, headers).await
}
