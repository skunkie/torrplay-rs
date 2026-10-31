// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! The TorrPlay HTTP server: the native REST API that `api/openapi.yaml`
//! describes, over the [`Engine`] facade.
//!
//! [`router`] serves every operation of the contract from an [`App`], which
//! holds the engine and the registry. The server is handwritten, as
//! `docs/adr/0006-contract-validation.md` decides: it maps domain values to
//! the generated wire types of `torrplay-api`, reads merge patches and
//! multipart forms itself, and streams file content from the engine's
//! readers. Errors are Problem Details of the types the API conventions
//! define, including `405` and `406` for every resource.
//!
//! Each response carries a request ID, which its log records name, and is
//! logged with `tracing` and counted for the metrics by method, route
//! template, and status. Logs never carry request paths, queries, headers, or bodies, and
//! engine errors are logged with URLs and magnet URIs redacted, so private
//! tracker passkeys and credentials stay out of them.
//!
//! Before any handler, a guard refuses requests that name a host the
//! application does not serve and, while a password is set, requests
//! without a credential their operation allows.
//!
//! A native handler is named for its operation's verb, with the resource
//! where its module serves several: `preloads::create` serves
//! `createFilePreload`, and `credentials::create_access_token` serves
//! `createAccessToken`. The DLNA and Stremio adapters' handlers take their
//! protocols' service and resource names, such as `content_directory` and
//! `manifest`, with a method verb where one resource answers several, as
//! `stremio::get_play` and `head_play`.
//!
//! Binding the listener and the shutdown sequence belong to the caller.

mod app;
mod authentication;
mod backup;
mod body;
mod content;
mod credentials;
mod dlna;
mod files;
mod headers;
mod images;
mod inference;
mod library;
pub mod log_files;
mod metainfo;
mod playable;
mod posters;
mod preloads;
mod previews;
mod problem;
mod profiles;
mod progress;
mod query;
mod reference;
mod representation;
mod retention;
mod service;
mod settings;
mod stremio;
mod system;
mod telemetry;
mod titles;
mod torrents;
mod up_next;
mod viewings;

use std::sync::Arc;
use std::time::Instant;

use axum::Router;
use axum::extract::{MatchedPath, Request, State};
use axum::http::{StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get, post};
use torrplay_core::engine::Engine;
use tracing::Level;

pub use crate::app::{App, Clock, FreeBytes, MediaServerSetting, SeekIndexes};
pub use crate::backup::{
    BackupCounts, BackupError, create_backup, delete_temporary_files,
    file_name as backup_file_name, restore_backup,
};
use crate::problem::{Problem, ProblemType};
pub use crate::telemetry::Run;

/// Returns the router of the API over `app`.
pub fn router<E: Engine>(app: Arc<App<E>>) -> Router {
    let torrent = "/api/v1/torrents/{torrent_id}";
    Router::new()
        .route("/api/v1", get(service::get_entry_point))
        .route("/api/v1/", any(service::redirect_to_entry_point))
        .route("/api/v1/openapi.yaml", get(service::get_openapi_document))
        .route(service::API_CATALOG_PATH, get(service::get_api_catalog))
        .route("/api/v1/reference", get(reference::get_page))
        .route("/api/v1/reference/scalar.js", get(reference::get_script))
        .route(
            "/api/v1/torrents",
            get(torrents::list::<E>).post(torrents::create::<E>),
        )
        .route("/api/v1/torrent-previews", post(previews::create::<E>))
        .route(
            torrent,
            get(torrents::get::<E>).delete(torrents::delete::<E>),
        )
        .route(
            &format!("{torrent}/configuration"),
            get(torrents::get_configuration::<E>).patch(torrents::update_configuration::<E>),
        )
        .route(
            &format!("{torrent}/metainfo"),
            get(metainfo::get::<E>).head(metainfo::head::<E>),
        )
        .route(
            &format!("{torrent}/poster"),
            get(posters::get::<E>)
                .head(posters::head::<E>)
                .put(posters::set::<E>)
                .delete(posters::delete::<E>),
        )
        .route(&format!("{torrent}/files"), get(files::list::<E>))
        .route(
            &format!("{torrent}/files/{{file_id}}"),
            get(files::get::<E>),
        )
        .route(
            &format!("{torrent}/files/{{file_id}}/content"),
            get(files::get_content::<E>).head(files::head_content::<E>),
        )
        .route(
            &format!("{torrent}/files/{{file_id}}/preloads"),
            get(preloads::list::<E>).post(preloads::create::<E>),
        )
        .route(
            &format!("{torrent}/files/{{file_id}}/preloads/{{preload_id}}"),
            get(preloads::get::<E>).delete(preloads::delete::<E>),
        )
        .route(
            &format!("{torrent}/files/{{file_id}}/viewings"),
            post(viewings::create::<E>),
        )
        .route(
            &format!("{torrent}/files/{{file_id}}/viewings/{{viewing_id}}"),
            get(viewings::get::<E>).delete(viewings::delete::<E>),
        )
        .route(
            &format!("{torrent}/statistics"),
            get(torrents::get_statistics::<E>),
        )
        .route(
            "/api/v1/cache/statistics",
            get(torrents::get_cache_statistics::<E>),
        )
        .route("/api/v1/statistics", get(system::get_statistics::<E>))
        .route("/api/v1/system", get(system::get_system::<E>))
        .route("/api/v1/log-records", get(system::list_log_records::<E>))
        .route(
            "/api/v1/settings",
            get(settings::get::<E>).patch(settings::update::<E>),
        )
        .route(
            "/api/v1/backup",
            get(backup::get::<E>).put(backup::set::<E>),
        )
        .route(
            "/api/v1/profiles",
            get(profiles::list::<E>).post(profiles::create::<E>),
        )
        .route(
            "/api/v1/profiles/{profile_id}",
            get(profiles::get::<E>)
                .patch(profiles::update::<E>)
                .delete(profiles::delete::<E>),
        )
        .route(
            "/api/v1/profiles/{profile_id}/avatar",
            get(profiles::get_avatar::<E>)
                .head(profiles::head_avatar::<E>)
                .put(profiles::set_avatar::<E>)
                .delete(profiles::delete_avatar::<E>),
        )
        .route(
            "/api/v1/profiles/{profile_id}/torrents/{torrent_id}/progress",
            get(progress::list::<E>),
        )
        .route(
            "/api/v1/profiles/{profile_id}/torrents/{torrent_id}/progress/{file_id}",
            get(progress::get::<E>)
                .put(progress::set::<E>)
                .delete(progress::delete::<E>),
        )
        .route(
            "/api/v1/profiles/{profile_id}/title-progress",
            get(titles::list::<E>),
        )
        .route(
            "/api/v1/profiles/{profile_id}/title-progress/{title_key}",
            get(titles::get::<E>)
                .put(titles::set::<E>)
                .delete(titles::delete::<E>),
        )
        .route(
            "/api/v1/profiles/{profile_id}/up-next",
            get(up_next::list::<E>),
        )
        .route(
            "/api/v1/profiles/{profile_id}/up-next/{up_next_entry_id}",
            get(up_next::get::<E>).delete(up_next::delete::<E>),
        )
        .route(
            "/api/v1/password",
            get(credentials::get_password::<E>)
                .put(credentials::set_password::<E>)
                .delete(credentials::delete_password::<E>),
        )
        .route(
            "/api/v1/access-tokens",
            get(credentials::list_access_tokens::<E>).post(credentials::create_access_token::<E>),
        )
        .route(
            "/api/v1/access-tokens/{access_token_id}",
            get(credentials::get_access_token::<E>).delete(credentials::delete_access_token::<E>),
        )
        .route(
            "/api/v1/playback-tokens",
            post(credentials::create_playback_token::<E>),
        )
        .route("/stremio/manifest.json", get(stremio::manifest::<E>))
        .route(
            "/stremio/{token}/manifest.json",
            get(stremio::manifest::<E>),
        )
        .route("/stremio/catalog/{type}/{id}", get(stremio::catalog::<E>))
        .route(
            "/stremio/catalog/{type}/{id}/{extra}",
            get(stremio::catalog::<E>),
        )
        .route(
            "/stremio/{token}/catalog/{type}/{id}",
            get(stremio::catalog::<E>),
        )
        .route(
            "/stremio/{token}/catalog/{type}/{id}/{extra}",
            get(stremio::catalog::<E>),
        )
        .route("/stremio/meta/{type}/{id}", get(stremio::meta::<E>))
        .route("/stremio/{token}/meta/{type}/{id}", get(stremio::meta::<E>))
        .route("/stremio/stream/{type}/{id}", get(stremio::stream::<E>))
        .route(
            "/stremio/{token}/stream/{type}/{id}",
            get(stremio::stream::<E>),
        )
        .route(
            "/stremio/play/{torrent}/{file}/{name}",
            get(stremio::get_play::<E>).head(stremio::head_play::<E>),
        )
        .route(
            "/stremio/{token}/play/{torrent}/{file}/{name}",
            get(stremio::get_play::<E>).head(stremio::head_play::<E>),
        )
        .route("/dlna/description.xml", get(dlna::device::<E>))
        .route("/dlna/icons/{icon}", get(dlna::get_icon::<E>))
        .route(
            "/dlna/content-directory.xml",
            get(dlna::content_directory_description::<E>),
        )
        .route(
            "/dlna/connection-manager.xml",
            get(dlna::connection_manager_description::<E>),
        )
        .route(
            "/dlna/content-directory/control",
            post(dlna::content_directory::<E>),
        )
        .route(
            "/dlna/connection-manager/control",
            post(dlna::connection_manager::<E>),
        )
        .route(
            "/dlna/content-directory/events",
            any(dlna::content_directory_events::<E>),
        )
        .route(
            "/dlna/connection-manager/events",
            any(dlna::connection_manager_events::<E>),
        )
        .route(
            "/dlna/media/{torrent_id}/{file_id}/{file_name}",
            get(dlna::get_media::<E>).head(dlna::head_media::<E>),
        )
        .route(
            "/dlna/posters/{torrent_id}",
            get(dlna::get_poster::<E>).head(dlna::head_poster::<E>),
        )
        .route("/metrics", get(system::get_metrics::<E>))
        .route("/livez", get(service::get_liveness))
        .route("/readyz", get(service::get_readiness::<E>))
        .fallback(unknown_resource)
        .layer(axum::middleware::from_fn_with_state(
            Arc::clone(&app),
            authentication::guard::<E>,
        ))
        .layer(axum::middleware::map_response(method_not_allowed))
        .layer(axum::middleware::from_fn(allow_stremio_origins))
        .layer(axum::middleware::from_fn_with_state(
            Arc::clone(&app),
            log_response::<E>,
        ))
        .layer(axum::middleware::from_fn(telemetry::assign_request_id))
        .with_state(app)
}

/// Logs each response by method, route template, and status, when its
/// headers are ready, at [`response_level`], and records it for the
/// metrics. The template names the route without the IDs, query, or
/// anything else the client sent.
async fn log_response<E: Engine>(
    State(app): State<Arc<App<E>>>,
    route: Option<MatchedPath>,
    request: Request,
    next: Next,
) -> Response {
    let method = request.method().clone();
    let started = Instant::now();
    let response = next.run(request).await;
    let route = route.as_ref().map_or("unmatched", MatchedPath::as_str);
    let status = response.status();
    let elapsed = started.elapsed();
    app.telemetry()
        .record_response(&method, route, status, elapsed);
    let elapsed_ms = u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX);
    macro_rules! log {
        ($level:expr) => {
            tracing::event!(
                $level,
                method = %method,
                route,
                status = status.as_u16(),
                elapsed_ms,
                "response",
            )
        };
    }
    if response_level(status) == Level::WARN {
        log!(Level::WARN);
    } else {
        log!(Level::DEBUG);
    }
    response
}

/// The level a response is logged at. Clients poll, browse, and fetch media
/// ranges continually, and the metrics count every response already, so
/// responses are debug messages unless the server failed them, which are
/// warnings beside the error that names the cause. A `503` reports the
/// lifecycle, as `/readyz` does while the engine starts, rather than a
/// failure.
fn response_level(status: StatusCode) -> Level {
    if status.is_server_error() && status != StatusCode::SERVICE_UNAVAILABLE {
        Level::WARN
    } else {
        Level::DEBUG
    }
}

async fn unknown_resource() -> Problem {
    Problem::not_found("No resource has this path.")
}

/// Gives the router's own `405` responses, which carry `Allow` but no body,
/// the Problem Details body every error has.
/// Lets Stremio's web client read every `/stremio` response, its refusals
/// included, as the addon protocol requires.
async fn allow_stremio_origins(request: Request, next: Next) -> Response {
    let stremio = request.uri().path().starts_with("/stremio/");
    let mut response = next.run(request).await;
    if stremio {
        response
            .headers_mut()
            .insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, stremio::ALLOW_ORIGIN);
    }
    response
}

async fn method_not_allowed(response: Response) -> Response {
    if response.status() != StatusCode::METHOD_NOT_ALLOWED
        || response.headers().contains_key(header::CONTENT_TYPE)
    {
        return response;
    }
    let (parts, _) = response.into_parts();
    let mut problem = Problem::new(
        ProblemType::MethodNotAllowed,
        "The resource does not support the method.",
    )
    .into_response();
    if let Some(allow) = parts.headers.get(header::ALLOW) {
        problem.headers_mut().insert(header::ALLOW, allow.clone());
    }
    problem
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn responses_are_debug_messages_unless_the_server_failed() {
        let cases = [
            (101, Level::DEBUG),
            (200, Level::DEBUG),
            (206, Level::DEBUG),
            (304, Level::DEBUG),
            (404, Level::DEBUG),
            (412, Level::DEBUG),
            (499, Level::DEBUG),
            (500, Level::WARN),
            (503, Level::DEBUG),
            (507, Level::WARN),
        ];
        for (status, level) in cases {
            let status = StatusCode::from_u16(status).unwrap();
            assert_eq!(response_level(status), level, "{status}");
        }
    }
}
