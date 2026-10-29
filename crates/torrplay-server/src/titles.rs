// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! A profile's title progress: progress of titles and episodes in external
//! catalogs, named by title keys, which outlives the torrents that played
//! them. It lives in the store and needs no engine.

use std::sync::Arc;
use std::time::SystemTime;

use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use serde_json::Value;
use torrplay_api as api;
use torrplay_core::engine::Engine;
use torrplay_core::id::{FileId, ProfileId, TorrentId};
use torrplay_core::pagination::{
    CURSOR_PARAMETER, FilterFingerprint, PAGE_SIZE_PARAMETER, PageRequest, TitleProgressSortKey,
    paginate,
};
use torrplay_core::profiles::{self, ProfileKind, TitleKey, TitleProgress};
use torrplay_store::StoredTitleProgress;

use crate::app::App;
use crate::body::{JSON_LIMIT_BYTES, read_limited};
use crate::headers::{MediaType, negotiate};
use crate::problem::{InvalidValue, Problem, ProblemType};
use crate::profiles as profile_routes;
use crate::progress::{stored, update_of};
use crate::query::{Paging, Query, page_error, reject};
use crate::representation::{JSON, NO_CACHE, json, profile_path, torrent_path};
use crate::torrents::{object, parse_json};

/// The query parameter that narrows the listing to keys with a prefix.
const PREFIX_PARAMETER: &str = "title_key_prefix";

/// The longest prefix the listing takes.
const PREFIX_MAX_CHARS: usize = 64;

fn titles_path(profile: &ProfileId) -> String {
    format!("{}/title-progress", profile_path(profile))
}

fn no_progress() -> Problem {
    Problem::not_found("The profile has no progress of this title.")
}

/// The path's title key, which must follow the pattern.
fn key(text: &str) -> Result<TitleKey, Problem> {
    TitleKey::parse(text).ok_or_else(|| {
        Problem::malformed(vec![InvalidValue::parameter(
            "title_key",
            "Must be a title key: tmdb-movie-{id}, tmdb-tv-{id}-s{season}e{episode}, \
             imdb-{IMDb ID}, or imdb-{IMDb ID}-s{season}e{episode}.",
        )])
    })
}

/// Whether the torrent is registered with the file in its metadata.
fn file_exists<E: Engine>(app: &App<E>, torrent: &str, file: &str) -> bool {
    app.registry()
        .ok()
        .and_then(|registry| registry.get(torrent))
        .is_some_and(|record| {
            record
                .metadata
                .as_deref()
                .is_some_and(|metadata| metadata.file(file).is_some())
        })
}

fn representation<E: Engine>(
    app: &App<E>,
    profile: &ProfileId,
    key: &TitleKey,
    title: &TitleProgress,
) -> api::TitleProgress {
    let value = &title.progress;
    let file = title
        .played
        .as_ref()
        .filter(|(torrent, file)| file_exists(app, torrent.as_str(), file.as_str()))
        .map(|(torrent, file)| format!("{}/files/{file}", torrent_path(torrent)));
    api::TitleProgress {
        title_key: key.to_string(),
        position_seconds: value.position_ms as f64 / 1000.0,
        duration_seconds: value.duration_ms as f64 / 1000.0,
        completion_ratio: value.completion_ratio(),
        viewed: value.viewed(),
        viewed_at: value.viewed_at.map(Into::into),
        updated_at: value.updated_at.into(),
        torrent_id: title
            .played
            .as_ref()
            .map(|(torrent, _)| torrent.to_string()),
        file_id: title.played.as_ref().map(|(_, file)| file.to_string()),
        links: api::TitleProgressLinks {
            self_field: format!("{}/{key}", titles_path(profile)),
            profile: profile_path(profile),
            content: file.as_ref().map(|file| format!("{file}/content")),
            file,
        },
    }
}

pub(crate) async fn list<E: Engine>(
    State(app): State<Arc<App<E>>>,
    Path(profile): Path<String>,
    uri: Uri,
    headers: HeaderMap,
) -> Result<Response, Problem> {
    negotiate(&headers, JSON)?;
    let query = Query::parse(uri.query());
    let parameters = [PAGE_SIZE_PARAMETER, CURSOR_PARAMETER, PREFIX_PARAMETER];
    let mut errors = query.unexpected(&parameters, &parameters);
    let paging = Paging::parse(&query, &mut errors);
    let prefix = query.first(PREFIX_PARAMETER);
    if prefix.is_some_and(|prefix| prefix.is_empty() || prefix.chars().count() > PREFIX_MAX_CHARS) {
        errors.push(InvalidValue::parameter(
            PREFIX_PARAMETER,
            format!("Must have 1 to {PREFIX_MAX_CHARS} characters."),
        ));
    }
    reject(errors)?;
    let filters: Vec<(&str, &str)> = prefix
        .map(|prefix| (PREFIX_PARAMETER, prefix))
        .into_iter()
        .collect();
    let fingerprint = if filters.is_empty() {
        FilterFingerprint::none()
    } else {
        FilterFingerprint::of(filters.iter().copied())
    };
    let request =
        PageRequest::<TitleProgressSortKey>::new(paging.page_size, paging.cursor, fingerprint)
            .map_err(|error| Problem::malformed(vec![page_error(error)]))?;
    let profile = profile_routes::find(&app, &profile)?;
    let titles = app.viewing()?.titles(&profile.id);
    let page = paginate(
        titles
            .iter()
            .filter(|(key, _)| prefix.is_none_or(|prefix| key.as_str().starts_with(prefix))),
        |(key, title)| TitleProgressSortKey::of(key.as_str(), title.progress.updated_at),
        &request,
    );
    let path = titles_path(&profile.id);
    let body = api::TitleProgressPage {
        items: page
            .items
            .iter()
            .map(|(key, title)| representation(&app, &profile.id, key, title))
            .collect(),
        links: api::TitleProgressPageLinks {
            next: page
                .next_cursor
                .as_deref()
                .map(|cursor| paging.link(&path, &filters, Some(cursor))),
            self_field: paging.link(&path, &filters, paging.cursor),
        },
    };
    let mut response = json(StatusCode::OK, &body)?;
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, NO_CACHE);
    Ok(response)
}

pub(crate) async fn get<E: Engine>(
    State(app): State<Arc<App<E>>>,
    Path((profile, title)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<Response, Problem> {
    negotiate(&headers, JSON)?;
    let key = key(&title)?;
    let profile = profile_routes::find(&app, &profile)?;
    let title = app
        .viewing()?
        .title(&profile.id, key.as_str())
        .ok_or_else(no_progress)?;
    let mut response = json(
        StatusCode::OK,
        &representation(&app, &profile.id, &key, &title),
    )?;
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, NO_CACHE);
    Ok(response)
}

/// Reads a title progress update: a file's progress update, and optionally
/// the file that played, which must exist.
fn update<E: Engine>(
    app: &App<E>,
    value: Value,
) -> Result<(profiles::ProgressUpdate, Option<(TorrentId, FileId)>), Problem> {
    let members = object(value)?;
    let mut errors = Vec::new();
    let update = update_of(&members, &["torrent_id", "file_id"], &mut errors);
    let text = |member: &str| members.get(member).map(Value::as_str);
    let played = match (text("torrent_id"), text("file_id")) {
        (None, None) => None,
        (Some(Some(torrent)), Some(Some(file))) => {
            if file_exists(app, torrent, file) {
                TorrentId::from_stored(torrent).zip(FileId::from_stored(file))
            } else {
                errors.push(InvalidValue::pointer(
                    "/file_id",
                    "Must name a file of the torrent torrent_id names.",
                ));
                None
            }
        }
        (torrent, file) => {
            for (member, value) in [("torrent_id", torrent), ("file_id", file)] {
                if !matches!(value, Some(Some(_))) {
                    errors.push(InvalidValue::pointer(
                        format!("/{member}"),
                        "Must be a string, given with torrent_id and file_id together.",
                    ));
                }
            }
            None
        }
    };
    match update {
        Some(update) if errors.is_empty() => Ok((update, played)),
        _ => Err(Problem::invalid_body(errors)),
    }
}

pub(crate) async fn put<E: Engine>(
    State(app): State<Arc<App<E>>>,
    Path((profile, title)): Path<(String, String)>,
    headers: HeaderMap,
    body: Body,
) -> Result<Response, Problem> {
    negotiate(&headers, JSON)?;
    let key = key(&title)?;
    let profile = profile_routes::find(&app, &profile)?;
    if profile.kind == ProfileKind::Device {
        return Err(profile_routes::device_conflict());
    }
    if MediaType::of_request(&headers).is_none_or(|media| media.essence != JSON) {
        return Err(Problem::new(
            ProblemType::UnsupportedMediaType,
            "The operation accepts application/json.",
        )
        .with_header(header::ACCEPT, HeaderValue::from_static(JSON)));
    }
    let bytes = read_limited(
        &headers,
        body,
        JSON_LIMIT_BYTES,
        JSON_LIMIT_BYTES as u64,
        app.body_timeout(),
    )
    .await?;
    let (update, played) = update(&app, parse_json(&bytes)?)?;
    app.require_running()?;
    let (title, created) = {
        let _lane = app.write_lane().await;
        let viewing = app.viewing()?;
        let previous = viewing.title(&profile.id, key.as_str());
        let title = TitleProgress {
            progress: profiles::apply_update(
                previous.as_ref().map(|previous| &previous.progress),
                update,
                SystemTime::now(),
            ),
            played,
        };
        let replaced = viewing.title_to_make_room(&profile.id, key.as_str());
        app.store()
            .set_title_progress(
                &StoredTitleProgress {
                    profile: profile.id.clone(),
                    key: key.clone(),
                    progress: title.clone(),
                },
                replaced.as_ref(),
            )
            .await
            .map_err(stored)?;
        if let Some(replaced) = replaced {
            viewing.remove_title(&profile.id, replaced.as_str());
        }
        viewing.set_title(&profile.id, key.clone(), title.clone());
        (title, previous.is_none())
    };
    let body = representation(&app, &profile.id, &key, &title);
    if !created {
        return json(StatusCode::OK, &body);
    }
    let location =
        HeaderValue::try_from(body.links.self_field.clone()).map_err(|_| Problem::internal())?;
    let mut response = json(StatusCode::CREATED, &body)?;
    response.headers_mut().insert(header::LOCATION, location);
    Ok(response)
}

pub(crate) async fn delete<E: Engine>(
    State(app): State<Arc<App<E>>>,
    Path((profile, title)): Path<(String, String)>,
) -> Result<Response, Problem> {
    let key = key(&title)?;
    let profile = profile_routes::find(&app, &profile)?;
    app.require_running()?;
    let _lane = app.write_lane().await;
    let viewing = app.viewing()?;
    if viewing.title(&profile.id, key.as_str()).is_none() {
        return Err(no_progress());
    }
    app.store()
        .delete_title_progress(&profile.id, &key)
        .await
        .map_err(stored)?;
    viewing.remove_title(&profile.id, key.as_str());
    Ok(StatusCode::NO_CONTENT.into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_keys_outside_the_pattern_are_malformed() {
        assert_eq!(
            key("tmdb-tv-1399-s1e2").unwrap().as_str(),
            "tmdb-tv-1399-s1e2"
        );
        for invalid in ["tmdb-tv-1399", "tmdb-movie-0603", "imdb-603"] {
            assert_eq!(
                key(invalid).unwrap_err().pointers(),
                Vec::<&str>::new(),
                "a parameter, not a body member: {invalid}"
            );
        }
    }
}
