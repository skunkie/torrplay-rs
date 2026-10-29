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
use torrplay_core::settings::{self, Explicit, LogLevel, LogOverride, Overrides, Stored};

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

/// The settings' entity tag, which also changes with the overrides, since
/// they are part of the representation.
fn settings_etag(stored: &Stored, overrides: &Overrides) -> EntityTag {
    let mut tag = format!("set-{}", stored.etag().opaque());
    if overrides.dht_enabled == Some(false) {
        tag.push_str("-nodht");
    }
    match overrides.log_level {
        Some(LogOverride::Level(level)) => {
            tag.push_str("-log.");
            tag.push_str(level.as_str());
        }
        Some(LogOverride::Custom) => tag.push_str("-log.custom"),
        None => {}
    }
    EntityTag::new(tag)
}

fn log_level(level: LogLevel) -> api::SettingsLogLevel {
    match level {
        LogLevel::Error => api::SettingsLogLevel::Error,
        LogLevel::Warn => api::SettingsLogLevel::Warn,
        LogLevel::Info => api::SettingsLogLevel::Info,
        LogLevel::Debug => api::SettingsLogLevel::Debug,
        LogLevel::Trace => api::SettingsLogLevel::Trace,
    }
}

fn log_override(level: LogOverride) -> api::SettingsOverridesLogLevel {
    match level {
        LogOverride::Level(LogLevel::Error) => api::SettingsOverridesLogLevel::Error,
        LogOverride::Level(LogLevel::Warn) => api::SettingsOverridesLogLevel::Warn,
        LogOverride::Level(LogLevel::Info) => api::SettingsOverridesLogLevel::Info,
        LogOverride::Level(LogLevel::Debug) => api::SettingsOverridesLogLevel::Debug,
        LogOverride::Level(LogLevel::Trace) => api::SettingsOverridesLogLevel::Trace,
        LogOverride::Custom => api::SettingsOverridesLogLevel::Custom,
    }
}

fn representation(stored: &Stored, overrides: &Overrides) -> api::Settings {
    let [
        piece_wait,
        idle_timeout,
        per_torrent,
        total,
        drain,
        peers,
        capacity,
        temporary_idle,
        temporary_max,
        temporary_progress,
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
        temporary_idle_seconds: int(temporary_idle),
        temporary_torrents_max: int(temporary_max),
        temporary_progress_seconds: int(temporary_progress),
        background_downloads: stored.explicit.effective().background_downloads,
        dlna_enabled: stored.explicit.effective().dlna_enabled,
        dlna_name: stored.explicit.dlna_name().to_owned(),
        dht_enabled: stored.explicit.effective().dht_enabled,
        log_level: log_level(stored.explicit.effective().log_level),
        overrides: api::SettingsOverrides {
            dht_enabled: overrides.dht_enabled,
            log_level: overrides.log_level.map(log_override),
        },
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
    let etag = settings_etag(&stored, app.overrides());
    if let Some(response) = not_modified(&headers, &etag) {
        return Ok(response);
    }
    let mut response = with_etag(
        json(StatusCode::OK, &representation(&stored, app.overrides()))?,
        &etag,
    )?;
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
    let overrides = *app.overrides();
    if !if_match
        .is_ok_and(|tags| tags.matches_strongly(settings_etag(&current, &overrides).opaque()))
    {
        return Err(Problem::new(
            ProblemType::PreconditionFailed,
            "The settings changed since the ETag was issued.",
        ));
    }
    let mut explicit = current.explicit.clone();
    for change in patch {
        match change {
            Change::Number(name, value) => {
                if let Some(slot) = explicit.slot(name) {
                    *slot = value;
                }
            }
            Change::Switch(name, value) => match name {
                settings::DLNA_ENABLED => explicit.dlna_enabled = value,
                settings::DHT_ENABLED => explicit.dht_enabled = value,
                _ => explicit.background_downloads = value,
            },
            Change::Name(value) => explicit.dlna_name = value,
            Change::LogLevel(value) => explicit.log_level = value,
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
        // Only a smaller cache capacity and starting the DHT can fail, so
        // they are applied before the change is stored, and anything else
        // after.
        let shrinks = effective.cache_capacity_bytes < previous.cache_capacity_bytes;
        if shrinks {
            apply(&app, &effective)?;
        }
        let dht = effective.overridden(&overrides).dht_enabled;
        let dht_before = previous.overridden(&overrides).dht_enabled;
        if dht != dht_before
            && let Err(error) = app.engine().set_dht(dht).await
        {
            tracing::error!(%error, "the DHT could not be turned {}", on_off(dht));
            if shrinks && let Err(error) = app.engine().apply_settings(&previous) {
                tracing::error!(%error, "the previous settings could not be applied again");
            }
            return Err(Problem::internal());
        }
        if let Err(error) = app.store().set_settings(stored.clone()).await {
            if shrinks && let Err(error) = app.engine().apply_settings(&previous) {
                tracing::error!(%error, "the previous settings could not be applied again");
            }
            if dht != dht_before
                && let Err(error) = app.engine().set_dht(dht_before).await
            {
                tracing::error!(%error, "the DHT could not be turned {} again", on_off(dht_before));
            }
            return Err(store_failed(error));
        }
        app.set_settings(stored.clone());
        if !shrinks {
            apply(&app, &effective)?;
        }
        stored
    };
    with_etag(
        json(StatusCode::OK, &representation(&stored, &overrides))?,
        &settings_etag(&stored, &overrides),
    )
}

fn on_off(on: bool) -> &'static str {
    if on { "on" } else { "off" }
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
    /// A switch, by name: background downloads, the DLNA media server, or
    /// the DHT.
    Switch(&'static str, Option<bool>),
    /// The DLNA media server's friendly name, validated with the others.
    Name(Option<String>),
    /// The log level.
    LogLevel(Option<LogLevel>),
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
            (settings::DHT_ENABLED, settings::DHT_ENABLED_DEFAULT),
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
        if member == settings::DLNA_NAME {
            match value {
                Value::Null => patch.push(Change::Name(None)),
                Value::String(name) => patch.push(Change::Name(Some(name.clone()))),
                _ => errors.push(InvalidValue::pointer(
                    pointer(member),
                    format!(
                        "Must be a string, or null for the default, {}.",
                        settings::DLNA_NAME_DEFAULT
                    ),
                )),
            }
            continue;
        }
        if member == settings::LOG_LEVEL {
            match value {
                Value::Null => patch.push(Change::LogLevel(None)),
                Value::String(name) if LogLevel::parse(name).is_some() => {
                    patch.push(Change::LogLevel(LogLevel::parse(name)));
                }
                _ => errors.push(InvalidValue::pointer(
                    pointer(member),
                    "Must be error, warn, info, debug, trace, or null for the default, info.",
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
        if invalid.name == settings::DLNA_NAME {
            usable.dlna_name = None;
        } else if let Some(slot) = usable.slot(invalid.name) {
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
