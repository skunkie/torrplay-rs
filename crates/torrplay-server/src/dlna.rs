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

use std::collections::{BTreeMap, HashMap};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use axum::Extension;
use axum::body::Body;
use axum::extract::{ConnectInfo, Path, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use sha2::{Digest, Sha256};
use torrplay_core::engine::Engine;
use torrplay_core::id::{FileId, TorrentId};
use torrplay_core::library::{self, Kind};
use torrplay_core::lifecycle::{ObservedState, Retention};
use torrplay_core::profiles::Progress;
use torrplay_dlna::description::{self, Device};
use torrplay_dlna::dlna;
use torrplay_dlna::gena::{self, Refusal, Subscribe, Subscriptions};
use torrplay_dlna::services::{self, Directory};
use torrplay_dlna::soap::{self, Action, Fault};
use torrplay_dlna::tree::{self, Base, Catalog};

use crate::app::App;
use crate::body::read_limited;
use crate::content;
use crate::files::{self, Watcher};
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
    pub(crate) subscriptions: Subscriptions,
}

impl MediaServer {
    pub(crate) fn new(device: Device) -> Self {
        Self {
            device,
            update: Mutex::new(None),
            subscriptions: Subscriptions::default(),
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

/// Sends the events due to the media server's subscribers: initial events,
/// and `SystemUpdateID` when the content tree changed. Deliveries run at
/// once, each bounded by [`gena::DELIVERY_TIMEOUT`].
pub(crate) async fn notify<E: Engine>(app: &App<E>) {
    let Some(server) = app.media_server() else {
        return;
    };
    if !app.settings().explicit.effective().dlna_enabled
        || !server.subscriptions.any(std::time::Instant::now())
    {
        return;
    }
    let catalog = match catalog(app).await {
        Ok(Some(catalog)) => catalog,
        Ok(None) => return,
        Err(_) => {
            tracing::debug!("the content tree could not be read for events");
            return;
        }
    };
    let update_id = server.update_id(&catalog);
    let due = server.subscriptions.due(
        update_id,
        &services::source_protocol_info(),
        std::time::Instant::now(),
    );
    let mut deliveries = tokio::task::JoinSet::new();
    for notification in due {
        deliveries.spawn(async move { notification.deliver().await });
    }
    while deliveries.join_next().await.is_some() {}
}

pub(crate) async fn content_directory_events<E: Engine>(
    state: State<Arc<App<E>>>,
    client: Option<Extension<ConnectInfo<SocketAddr>>>,
    method: axum::http::Method,
    headers: HeaderMap,
) -> Result<Response, Problem> {
    events(
        state,
        client,
        method,
        headers,
        gena::Service::ContentDirectory,
    )
    .await
}

pub(crate) async fn connection_manager_events<E: Engine>(
    state: State<Arc<App<E>>>,
    client: Option<Extension<ConnectInfo<SocketAddr>>>,
    method: axum::http::Method,
    headers: HeaderMap,
) -> Result<Response, Problem> {
    events(
        state,
        client,
        method,
        headers,
        gena::Service::ConnectionManager,
    )
    .await
}

/// Answers `SUBSCRIBE` and `UNSUBSCRIBE`; other methods are `405`.
async fn events<E: Engine>(
    State(app): State<Arc<App<E>>>,
    client: Option<Extension<ConnectInfo<SocketAddr>>>,
    method: axum::http::Method,
    headers: HeaderMap,
    service: gena::Service,
) -> Result<Response, Problem> {
    let subscriber = client
        .as_ref()
        .map(|Extension(ConnectInfo(address))| address.ip());
    let server = gate(&app, client)?;
    let subscriber = subscriber.ok_or_else(Problem::internal)?;
    let text = |name: &str| headers.get(name).and_then(|value| value.to_str().ok());
    let refused = |refusal: Refusal| {
        let status = match refusal {
            Refusal::BadRequest => StatusCode::BAD_REQUEST,
            Refusal::PreconditionFailed => StatusCode::PRECONDITION_FAILED,
            Refusal::Unavailable => StatusCode::SERVICE_UNAVAILABLE,
        };
        status.into_response()
    };
    match method.as_str() {
        "SUBSCRIBE" => {
            let request = Subscribe {
                sid: text("sid"),
                callback: text("callback"),
                nt: text("nt"),
                timeout: text("timeout"),
            };
            let granted = server.subscriptions.subscribe(
                service,
                request,
                subscriber,
                std::time::Instant::now(),
                || {
                    torrplay_core::id::random_uuid()
                        .ok()
                        .map(|uuid| format!("uuid:{uuid}"))
                },
            );
            Ok(match granted {
                Ok(granted) => {
                    let mut response = StatusCode::OK.into_response();
                    let values = response.headers_mut();
                    values.insert(
                        HeaderName::from_static("sid"),
                        HeaderValue::try_from(granted.sid.as_str())
                            .map_err(|_| Problem::internal())?,
                    );
                    values.insert(
                        HeaderName::from_static("timeout"),
                        HeaderValue::try_from(granted.timeout_header())
                            .map_err(|_| Problem::internal())?,
                    );
                    values.insert(header::CONTENT_LENGTH, HeaderValue::from(0));
                    response
                }
                Err(refusal) => refused(refusal),
            })
        }
        "UNSUBSCRIBE" => Ok(
            match server.subscriptions.unsubscribe(service, text("sid")) {
                Ok(()) => StatusCode::OK.into_response(),
                Err(refusal) => refused(refusal),
            },
        ),
        _ => Ok((
            StatusCode::METHOD_NOT_ALLOWED,
            [(
                header::ALLOW,
                HeaderValue::from_static("SUBSCRIBE, UNSUBSCRIBE"),
            )],
        )
            .into_response()),
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
            field(&format!("{:?}", file.resume_position));
        }
    }
    for (torrent, file) in &catalog.viewed {
        field(torrent);
        field(file);
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
    let name = app.settings().explicit.dlna_name().to_owned();
    Ok(xml(StatusCode::OK, server.device.description(&name)))
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
    let progress = device_progress(app);
    let mut torrents = Vec::new();
    for record in registry.list().into_iter().rev() {
        // Temporary torrents are never shown to televisions.
        if record.configuration.retention() == Retention::Temporary {
            continue;
        }
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
                let resume_position = progress
                    .get(&record.id)
                    .and_then(|files| files.get(file.id()))
                    .filter(|progress| progress.in_progress() && !progress.viewed())
                    .map(|progress| Duration::from_millis(progress.position_ms));
                playable.then(|| {
                    (
                        library::episode_number(&name),
                        index,
                        tree::File {
                            id: file.id().to_string(),
                            name,
                            size_bytes: file.size_bytes(),
                            media_type,
                            resume_position,
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
            created_at: record.created_at,
            files: files.into_iter().map(|(_, _, file)| file).collect(),
        });
    }
    Ok(Some(Catalog {
        viewed: viewed(progress),
        torrents,
    }))
}

/// The device profile's progress, or none without the profile.
fn device_progress<E: Engine>(app: &App<E>) -> HashMap<TorrentId, BTreeMap<FileId, Progress>> {
    app.viewing()
        .ok()
        .and_then(|viewing| {
            let device = viewing.device()?;
            Some(viewing.profile_progress(&device.id))
        })
        .unwrap_or_default()
}

/// The device profile's files in progress and viewed, most recently saved
/// first, then by ID.
fn viewed(device: HashMap<TorrentId, BTreeMap<FileId, Progress>>) -> Vec<(String, String)> {
    let mut files: Vec<_> = device
        .into_iter()
        .flat_map(|(torrent, files)| {
            files
                .into_iter()
                .filter(|(_, progress)| progress.in_progress() || progress.viewed())
                .map(move |(file, progress)| {
                    (progress.updated_at, torrent.to_string(), file.to_string())
                })
        })
        .collect();
    files.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| (&a.1, &a.2).cmp(&(&b.1, &b.2))));
    files
        .into_iter()
        .map(|(_, torrent, file)| (torrent, file))
        .collect()
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
    let (view, metadata) = files::with_metadata(&app, &torrent).await?;
    if view.record.configuration.retention() == Retention::Temporary {
        return Err(Problem::not_found("The media server has no such torrent."));
    }
    let (_, found) = metadata
        .file(&file)
        .ok_or_else(|| Problem::not_found("The torrent has no file with this ID."))?;
    if file_name(found) != name {
        return Err(Problem::not_found(
            "The path's file name is not the file's name.",
        ));
    }
    let mut response =
        files::content(&app, &torrent, &file, &headers, stream, Watcher::Device).await?;
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

/// The bytes of each icon `torrplay_dlna::description::ICONS` lists, which
/// `scripts/render-logo.py` draws.
fn icon_bytes(name: &str) -> Option<&'static [u8]> {
    Some(match name {
        "icon-48.png" => include_bytes!("../assets/dlna/icon-48.png"),
        "icon-120.png" => include_bytes!("../assets/dlna/icon-120.png"),
        "icon-48.jpg" => include_bytes!("../assets/dlna/icon-48.jpg"),
        "icon-120.jpg" => include_bytes!("../assets/dlna/icon-120.jpg"),
        _ => return None,
    })
}

pub(crate) async fn get_icon<E: Engine>(
    State(app): State<Arc<App<E>>>,
    client: Option<Extension<ConnectInfo<SocketAddr>>>,
    Path(name): Path<String>,
) -> Result<Response, Problem> {
    gate(&app, client)?;
    let icon = torrplay_dlna::description::ICONS
        .iter()
        .find(|icon| icon.name == name)
        .zip(icon_bytes(&name));
    let Some((icon, bytes)) = icon else {
        return Err(Problem::not_found("The media server has no such icon."));
    };
    Ok((
        [(
            header::CONTENT_TYPE,
            HeaderValue::from_static(icon.media_type),
        )],
        bytes,
    )
        .into_response())
}
