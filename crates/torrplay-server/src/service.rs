// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! The entry point, the API catalog, the OpenAPI document, and the health
//! checks.

use std::sync::Arc;

use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Redirect, Response};
use torrplay_api as api;
use torrplay_core::engine::Engine;

use crate::app::App;
use crate::credentials::{access_tokens_path, password_path, playback_tokens_path};
use crate::headers::negotiate;
use crate::problem::Problem;
use crate::representation::{BASE, JSON, NO_CACHE, NO_STORE, cached, json, torrents_path};

/// The contract, served as the API's description.
const DOCUMENT: &str = include_str!("../../../api/openapi.yaml");

/// The media type of the document (RFC 9512).
const YAML: &str = "application/yaml";

pub(crate) async fn get_entry_point(headers: HeaderMap) -> Result<Response, Problem> {
    negotiate(&headers, JSON)?;
    let response = json(
        StatusCode::OK,
        &api::EntryPoint {
            links: api::EntryPointLinks {
                cache_statistics: format!("{BASE}/cache/statistics"),
                statistics: format!("{BASE}/statistics"),
                system: format!("{BASE}/system"),
                log_records: format!("{BASE}/log-records"),
                metrics: crate::system::METRICS_PATH.to_owned(),
                self_field: BASE.to_owned(),
                service_desc: format!("{BASE}/openapi.yaml"),
                service_doc: format!("{BASE}/reference"),
                profiles: format!("{BASE}/profiles"),
                settings: format!("{BASE}/settings"),
                backup: crate::backup::backup_path(),
                torrents: torrents_path(),
                torrent_previews: format!("{BASE}/torrent-previews"),
                password: password_path(),
                access_tokens: access_tokens_path(),
                playback_tokens: playback_tokens_path(),
                api_catalog: API_CATALOG_PATH.to_owned(),
            },
        },
    )?;
    Ok(cached(response, NO_CACHE))
}

/// The API catalog's path (RFC 9727).
pub(crate) const API_CATALOG_PATH: &str = "/.well-known/api-catalog";

/// The API catalog's media type: a JSON linkset (RFC 9264) of the RFC 9727
/// profile.
const LINKSET: &str = "application/linkset+json";
const API_CATALOG_TYPE: &str =
    "application/linkset+json; profile=\"https://www.rfc-editor.org/info/rfc9727\"";

/// The API catalog: this host's one API with the links that describe it.
/// axum answers HEAD from this handler without the body, as RFC 9727 asks.
pub(crate) async fn get_api_catalog(headers: HeaderMap) -> Result<Response, Problem> {
    negotiate(&headers, LINKSET)?;
    let link = |href: String, media_type: &str| api::LinkTarget {
        href,
        r#type: media_type.to_owned(),
    };
    let catalog = api::ApiCatalog {
        linkset: vec![api::ApiCatalogEntry {
            anchor: BASE.to_owned(),
            service_desc: vec![link(format!("{BASE}/openapi.yaml"), YAML)],
            service_doc: vec![link(format!("{BASE}/reference"), "text/html")],
            status: vec![link("/readyz".to_owned(), JSON)],
        }],
    };
    let mut response = json(StatusCode::OK, &catalog)?;
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(API_CATALOG_TYPE),
    );
    Ok(cached(response, NO_CACHE))
}

/// Answers `/api/v1/` with a permanent redirect to `/api/v1`, since API
/// paths never end in a slash.
pub(crate) async fn redirect_to_entry_point() -> Redirect {
    Redirect::permanent(BASE)
}

pub(crate) async fn get_openapi_document(headers: HeaderMap) -> Result<Response, Problem> {
    negotiate(&headers, YAML)?;
    Ok((
        [
            (header::CONTENT_TYPE, HeaderValue::from_static(YAML)),
            (header::CACHE_CONTROL, NO_CACHE),
        ],
        DOCUMENT,
    )
        .into_response())
}

/// A health answer, which no cache may keep: a stale one would hide a
/// change of state.
fn healthy() -> Result<Response, Problem> {
    let response = json(
        StatusCode::OK,
        &api::Health {
            status: api::HealthStatus::Ok,
        },
    )?;
    Ok(cached(response, NO_STORE))
}

/// Liveness: the process is running and serving requests.
pub(crate) async fn get_liveness(headers: HeaderMap) -> Result<Response, Problem> {
    negotiate(&headers, JSON)?;
    healthy()
}

/// Readiness: the engine is ready and the application is not shutting
/// down.
pub(crate) async fn get_readiness<E: Engine>(
    State(app): State<Arc<App<E>>>,
    headers: HeaderMap,
) -> Result<Response, Problem> {
    negotiate(&headers, JSON)?;
    app.require_engine()?;
    healthy()
}
