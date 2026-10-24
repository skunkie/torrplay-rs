// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! The application's settings: reading them, and changing them with merge
//! patches.

use std::sync::Arc;

use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode, header};
use axum::response::Response;
use serde_json::Value;
use torrplay_api as api;
use torrplay_core::engine::{Engine, EngineError};
use torrplay_core::lifecycle::EntityTag;
use torrplay_core::settings::{self, Explicit, Stored};

use crate::app::{App, store_failed};
use crate::body::{JSON_LIMIT_BYTES, read_limited};
use crate::headers::{EntityTags, MediaType, negotiate};
use crate::problem::{InvalidValue, Problem, ProblemType};
use crate::representation::{BASE, JSON, NO_CACHE, json};
use crate::torrents::{
    MERGE_PATCH, not_modified, object, parse_json, pointer, unknown_member, with_etag,
};

pub(crate) fn settings_path() -> String {
    format!("{BASE}/settings")
}

fn settings_etag(stored: &Stored) -> EntityTag {
    EntityTag::new(format!("set-{}", stored.etag().opaque()))
}

fn representation(stored: &Stored) -> api::Settings {
    let [
        piece_wait,
        idle_timeout,
        per_torrent,
        total,
        drain,
        peers,
        capacity,
    ] = stored.explicit.values();
    // Every value lies within its bounds: the counts and durations far below
    // i32::MAX, the capacity below i64::MAX.
    let int = |value: u64| i32::try_from(value).unwrap_or(i32::MAX);
    api::Settings {
        piece_wait_seconds: int(piece_wait),
        idle_timeout_seconds: int(idle_timeout),
        idle_readers_per_torrent: int(per_torrent),
        idle_readers_total: int(total),
        drain_period_seconds: int(drain),
        peer_limit: int(peers),
        cache_capacity_bytes: i64::try_from(capacity).unwrap_or(i64::MAX),
        background_downloads: stored.explicit.effective().background_downloads,
        dlna_enabled: stored.explicit.effective().dlna_enabled,
        links: api::SettingsLinks {
            self_field: settings_path(),
        },
    }
}

pub(crate) async fn get<E: Engine>(
    State(app): State<Arc<App<E>>>,
    headers: HeaderMap,
) -> Result<Response, Problem> {
    negotiate(&headers, JSON)?;
    let stored = app.settings();
    let etag = settings_etag(&stored);
    if let Some(response) = not_modified(&headers, &etag) {
        return Ok(response);
    }
    let mut response = with_etag(json(StatusCode::OK, &representation(&stored))?, &etag)?;
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, NO_CACHE);
    Ok(response)
}

pub(crate) async fn update<E: Engine>(
    State(app): State<Arc<App<E>>>,
    headers: HeaderMap,
    body: Body,
) -> Result<Response, Problem> {
    negotiate(&headers, JSON)?;
    if MediaType::of_request(&headers).is_none_or(|media| media.essence != MERGE_PATCH) {
        return Err(Problem::new(
            ProblemType::UnsupportedMediaType,
            "The operation accepts application/merge-patch+json.",
        )
        .with_header(
            HeaderName::from_static("accept-patch"),
            HeaderValue::from_static(MERGE_PATCH),
        ));
    }
    let Some(if_match) = EntityTags::parse(&headers, &header::IF_MATCH) else {
        return Err(Problem::new(
            ProblemType::PreconditionRequired,
            "Send If-Match with the settings' current ETag.",
        ));
    };
    let bytes = read_limited(
        &headers,
        body,
        JSON_LIMIT_BYTES,
        JSON_LIMIT_BYTES as u64,
        app.body_timeout(),
    )
    .await?;
    let patch = parse_patch(parse_json(&bytes)?)?;
    app.require_engine()?;

    let _lane = app.write_lane().await;
    let current = app.settings();
    if !if_match.is_ok_and(|tags| tags.matches_strongly(settings_etag(&current).opaque())) {
        return Err(Problem::new(
            ProblemType::PreconditionFailed,
            "The settings changed since the ETag was issued.",
        ));
    }
    let mut explicit = current.explicit;
    for change in patch {
        match change {
            Change::Number(name, value) => {
                if let Some(slot) = explicit.slot(name) {
                    *slot = value;
                }
            }
            Change::Switch(name, value) => {
                if name == settings::DLNA_ENABLED {
                    explicit.dlna_enabled = value;
                } else {
                    explicit.background_downloads = value;
                }
            }
        }
    }
    let effective = explicit.validate().map_err(|invalid| {
        Problem::invalid_body(
            invalid
                .into_iter()
                .map(|invalid| InvalidValue::pointer(pointer(invalid.name), invalid.detail))
                .collect(),
        )
    })?;
    let stored = if explicit == current.explicit {
        // Setting the current values again keeps the entity tag.
        current
    } else {
        let stored = Stored {
            explicit,
            version: current.version + 1,
        };
        let previous = current.explicit.effective();
        // Only a smaller cache capacity can be refused, so it is applied
        // before it is stored, and anything else after.
        let shrinks = effective.cache_capacity_bytes < previous.cache_capacity_bytes;
        if shrinks {
            apply(&app, &effective)?;
        }
        if let Err(error) = app.store().set_settings(stored).await {
            if shrinks && let Err(error) = app.engine().apply_settings(&previous) {
                tracing::error!(%error, "the previous settings could not be applied again");
            }
            return Err(store_failed(error));
        }
        app.set_settings(stored);
        if !shrinks {
            apply(&app, &effective)?;
        }
        stored
    };
    with_etag(
        json(StatusCode::OK, &representation(&stored))?,
        &settings_etag(&stored),
    )
}

/// Gives the engine `settings`, which it refuses only for a cache capacity
/// below a torrent's piece length.
fn apply<E: Engine>(app: &App<E>, settings: &settings::Settings) -> Result<(), Problem> {
    app.engine()
        .apply_settings(settings)
        .map_err(|error| match error {
            EngineError::CapacityBelowPieceLength { piece_length_bytes } => {
                Problem::invalid_body(vec![InvalidValue::pointer(
                    pointer(settings::CACHE_CAPACITY_BYTES.name),
                    format!(
                        "Must be at least {piece_length_bytes}, the piece length of a torrent \
                         the engine holds."
                    ),
                )])
            }
            error => {
                tracing::error!(%error, "the engine did not apply the settings");
                Problem::internal()
            }
        })
}

/// One member of a settings patch: its new value, or `None` to return it to
/// its default.
enum Change {
    /// A numeric setting, by name.
    Number(&'static str, Option<u64>),
    /// A switch, by name: background downloads or the DLNA media server.
    Switch(&'static str, Option<bool>),
}

/// Reads a merge patch of the settings. The patch must set at least one
/// member.
fn parse_patch(value: Value) -> Result<Vec<Change>, Problem> {
    let members = object(value)?;
    let mut errors = Vec::new();
    let mut patch = Vec::new();
    for (member, value) in &members {
        let switch = [
            (
                settings::BACKGROUND_DOWNLOADS,
                settings::BACKGROUND_DOWNLOADS_DEFAULT,
            ),
            (settings::DLNA_ENABLED, settings::DLNA_ENABLED_DEFAULT),
        ]
        .into_iter()
        .find(|(name, _)| name == member);
        if let Some((name, default)) = switch {
            match value {
                Value::Null => patch.push(Change::Switch(name, None)),
                Value::Bool(on) => patch.push(Change::Switch(name, Some(*on))),
                _ => errors.push(InvalidValue::pointer(
                    pointer(member),
                    format!("Must be true, false, or null for the default, {default}."),
                )),
            }
            continue;
        }
        let Some(bounds) = settings::ALL.iter().find(|bounds| bounds.name == member) else {
            errors.push(unknown_member(member));
            continue;
        };
        match value {
            Value::Null => patch.push(Change::Number(bounds.name, None)),
            value => match value.as_u64() {
                Some(value) => patch.push(Change::Number(bounds.name, Some(value))),
                None => errors.push(InvalidValue::pointer(
                    pointer(member),
                    format!(
                        "Must be an integer from {} to {}, or null for the default, {}.",
                        bounds.minimum, bounds.maximum, bounds.default
                    ),
                )),
            },
        }
    }
    if members.is_empty() {
        errors.push(InvalidValue::pointer("", "Must set at least one member."));
    }
    if errors.is_empty() {
        Ok(patch)
    } else {
        Err(Problem::invalid_body(errors))
    }
}

/// The explicit values to run with: stored values outside the current
/// bounds, which an earlier release may have allowed, count as unset.
pub(crate) fn usable(explicit: Explicit) -> Explicit {
    let Err(invalid) = explicit.validate() else {
        return explicit;
    };
    let mut usable = explicit;
    for invalid in invalid {
        if let Some(slot) = usable.slot(invalid.name) {
            *slot = None;
        }
    }
    if usable.validate().is_err() {
        // Only the limits' relation can still fail: both now at the defaults
        // or one of them set, so the per-torrent limit yields.
        usable.idle_readers_per_torrent = None;
    }
    if usable.validate().is_err() {
        usable.idle_readers_total = None;
    }
    usable
}
