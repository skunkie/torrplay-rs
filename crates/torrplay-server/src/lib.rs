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
//! Each response is logged with `tracing` by method, route template, and
//! status. Logs never carry request paths, queries, headers, or bodies, and
//! engine errors are logged with URLs and magnet URIs redacted, so private
//! tracker passkeys and credentials stay out of them.
//!
//! Before any handler, a guard refuses requests that name a host the
//! application does not serve and, while a password is set, requests
//! without a credential their operation allows.
//!
//! Binding the listener and the shutdown sequence belong to the caller.

mod app;
mod authentication;
mod body;
mod content;
mod credentials;
mod dlna;
mod files;
mod headers;
mod images;
mod library;
mod posters;
mod preloads;
mod problem;
mod profiles;
mod progress;
mod query;
mod representation;
mod service;
mod settings;
mod torrents;
mod up_next;

use std::sync::Arc;
use std::time::Instant;

use axum::Router;
use axum::extract::{MatchedPath, Request};
use axum::http::{StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get, post};
use torrplay_core::engine::Engine;

pub use crate::app::{App, Clock};
use crate::problem::{Problem, ProblemType};

/// Returns the router of the API over `app`.
pub fn router<E: Engine>(app: Arc<App<E>>) -> Router {
    let torrent = "/api/v1/torrents/{torrent_id}";
    Router::new()
        .route("/api/v1", get(service::entry_point))
        .route("/api/v1/", any(service::entry_point_with_slash))
        .route("/api/v1/openapi.yaml", get(service::document))
        .route(
            "/api/v1/torrents",
            get(torrents::list::<E>).post(torrents::create::<E>),
        )
        .route(
            torrent,
            get(torrents::get::<E>).delete(torrents::delete::<E>),
        )
        .route(
            &format!("{torrent}/configuration"),
            get(torrents::get_configuration::<E>).patch(torrents::update_configuration::<E>),
        )
        .route(
            &format!("{torrent}/poster"),
            get(posters::get::<E>)
                .head(posters::head::<E>)
                .put(posters::put::<E>)
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
            &format!("{torrent}/statistics"),
            get(torrents::statistics::<E>),
        )
        .route(
            "/api/v1/cache/statistics",
            get(torrents::cache_statistics::<E>),
        )
        .route(
            "/api/v1/settings",
            get(settings::get::<E>).patch(settings::update::<E>),
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
                .put(profiles::put_avatar::<E>)
                .delete(profiles::delete_avatar::<E>),
        )
        .route(
            "/api/v1/profiles/{profile_id}/torrents/{torrent_id}/progress",
            get(progress::list::<E>),
        )
        .route(
            "/api/v1/profiles/{profile_id}/torrents/{torrent_id}/progress/{file_id}",
            get(progress::get::<E>)
                .put(progress::put::<E>)
                .delete(progress::delete::<E>),
        )
        .route(
            "/api/v1/profiles/{profile_id}/up-next",
            get(up_next::list::<E>),
        )
        .route(
            "/api/v1/profiles/{profile_id}/up-next/{entry_id}",
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
        .route("/dlna/description.xml", get(dlna::device::<E>))
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
        .route("/livez", get(service::liveness))
        .route("/readyz", get(service::readiness::<E>))
        .fallback(unknown_resource)
        .layer(axum::middleware::from_fn_with_state(
            Arc::clone(&app),
            authentication::guard::<E>,
        ))
        .layer(axum::middleware::map_response(method_not_allowed))
        .layer(axum::middleware::from_fn(log_response))
        .with_state(app)
}

/// Logs each response by method, route template, and status, when its
/// headers are ready. The template names the route without the IDs, query,
/// or anything else the client sent.
async fn log_response(route: Option<MatchedPath>, request: Request, next: Next) -> Response {
    let method = request.method().clone();
    let started = Instant::now();
    let response = next.run(request).await;
    tracing::info!(
        method = %method,
        route = route.as_ref().map_or("unmatched", MatchedPath::as_str),
        status = response.status().as_u16(),
        elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        "response",
    );
    response
}

async fn unknown_resource() -> Problem {
    Problem::not_found("No resource has this path.")
}

/// Gives the router's own `405` responses, which carry `Allow` but no body,
/// the Problem Details body every error has.
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
