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
//! Binding the listener and the shutdown sequence belong to the caller; the
//! prototype listens on loopback only.

mod app;
mod body;
mod content;
mod files;
mod headers;
mod problem;
mod query;
mod representation;
mod service;
mod torrents;

use std::sync::Arc;

use axum::Router;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get};
use torrplay_core::engine::Engine;

pub use crate::app::App;
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
            &format!("{torrent}/statistics"),
            get(torrents::statistics::<E>),
        )
        .route(
            "/api/v1/cache/statistics",
            get(torrents::cache_statistics::<E>),
        )
        .route("/livez", get(service::liveness))
        .route("/readyz", get(service::readiness::<E>))
        .fallback(unknown_resource)
        .layer(axum::middleware::map_response(method_not_allowed))
        .with_state(app)
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
