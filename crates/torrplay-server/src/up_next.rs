// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! A profile's up next: what it can resume or watch next, derived from its
//! progress, and its dismissals.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::SystemTime;

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use torrplay_api as api;
use torrplay_core::engine::Engine;
use torrplay_core::id::{TorrentId, UpNextEntryId};
use torrplay_core::library::Kind;
use torrplay_core::pagination::{
    CURSOR_PARAMETER, FilterFingerprint, PAGE_SIZE_PARAMETER, PageRequest, UpNextSortKey, paginate,
};
use torrplay_core::profiles::{self, Profile, Reason, VideoFiles};
use torrplay_store::{StoreError, StoredDismissal};

use crate::app::{App, TorrentView, store_failed};
use crate::content;
use crate::headers::negotiate;
use crate::problem::Problem;
use crate::profiles as profile_routes;
use crate::query::{Paging, Query, page_error, reject};
use crate::representation::{self, JSON, NO_CACHE, body_etag, json, profile_path};
use crate::torrents::{not_modified, with_etag};

fn not_found() -> Problem {
    Problem::not_found("The profile's up next lists no entry with this ID.")
}

/// The profile's entries, in order, with their representations.
async fn entries<E: Engine>(
    app: &App<E>,
    profile: &Profile,
) -> Result<Vec<(UpNextSortKey, api::UpNextEntry)>, Problem> {
    let viewing = app.viewing()?;
    let progress = viewing.profile_progress(&profile.id);
    let dismissals = viewing.dismissals(&profile.id);
    let registry = app.registry()?;
    let mut views: HashMap<_, TorrentView> = HashMap::new();
    for torrent in progress.keys() {
        if let Some(record) = registry.get(torrent.as_str()) {
            views.insert(torrent.clone(), app.observe(record).await?);
        }
    }
    let names: HashMap<_, Vec<(_, String)>> = views
        .iter()
        .filter_map(|(torrent, view)| {
            let metadata = view.record.metadata.as_deref()?;
            let videos = metadata
                .files()
                .iter()
                .map(|file| (file.id(), representation::file_name(file)))
                .filter(|(_, name)| content::media_type(name).0.starts_with("video/"))
                .collect();
            Some((torrent, videos))
        })
        .collect();
    let torrents = names.iter().map(|(torrent, videos)| {
        let view = &views[*torrent];
        let kind = representation::reported_kind(
            view.record.configuration.library(),
            view.record.metadata.as_deref(),
        );
        VideoFiles {
            torrent,
            series: kind == Some(Kind::Series),
            videos: videos
                .iter()
                .map(|(file, name)| (*file, name.as_str()))
                .collect(),
        }
    });
    let found = profiles::up_next(torrents, &progress, &dismissals);
    let path = format!("{}/up-next", profile_path(&profile.id));
    let mut entries = Vec::with_capacity(found.len());
    for entry in found {
        let view = &views[&entry.torrent];
        let Some((_, file)) = view
            .record
            .metadata
            .as_deref()
            .and_then(|metadata| metadata.file(entry.file.as_str()))
        else {
            continue;
        };
        let saved = progress
            .get(&entry.torrent)
            .and_then(|files| files.get(&entry.file));
        entries.push((
            UpNextSortKey::of(&entry.id, entry.last_active_at),
            api::UpNextEntry {
                id: entry.id.to_string(),
                reason: match entry.reason {
                    Reason::Resume => api::UpNextEntryReason::Resume,
                    Reason::NextEpisode => api::UpNextEntryReason::NextEpisode,
                },
                last_active_at: entry.last_active_at.into(),
                torrent: representation::torrent(view),
                file: representation::file(&entry.torrent, file),
                progress: saved.map(|saved| {
                    representation::progress(&profile.id, &entry.torrent, &entry.file, saved)
                }),
                links: api::UpNextEntryLinks {
                    self_field: format!("{path}/{}", entry.id),
                },
            },
        ));
    }
    Ok(entries)
}

pub(crate) async fn list<E: Engine>(
    State(app): State<Arc<App<E>>>,
    Path(profile): Path<String>,
    uri: Uri,
    headers: HeaderMap,
) -> Result<Response, Problem> {
    negotiate(&headers, JSON)?;
    let query = Query::parse(uri.query());
    let parameters = [PAGE_SIZE_PARAMETER, CURSOR_PARAMETER];
    let mut errors = query.unexpected(&parameters, &parameters);
    let paging = Paging::parse(&query, &mut errors);
    reject(errors)?;
    let request = PageRequest::<UpNextSortKey>::new(
        paging.page_size,
        paging.cursor,
        FilterFingerprint::none(),
    )
    .map_err(|error| Problem::malformed(vec![page_error(error)]))?;
    let profile = profile_routes::find(&app, &profile)?;
    let page = paginate(
        entries(&app, &profile).await?,
        |(key, _)| key.clone(),
        &request,
    );
    let path = format!("{}/up-next", profile_path(&profile.id));
    let body = api::UpNextPage {
        items: page.items.into_iter().map(|(_, entry)| entry).collect(),
        links: api::UpNextPageLinks {
            next: page
                .next_cursor
                .as_deref()
                .map(|cursor| paging.link(&path, &[], Some(cursor))),
            self_field: paging.link(&path, &[], paging.cursor),
        },
    };
    let etag = body_etag("upn", &body)?;
    if let Some(response) = not_modified(&headers, &etag) {
        return Ok(response);
    }
    let mut response = with_etag(json(StatusCode::OK, &body)?, &etag)?;
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, NO_CACHE);
    Ok(response)
}

pub(crate) async fn get<E: Engine>(
    State(app): State<Arc<App<E>>>,
    Path((profile, entry)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<Response, Problem> {
    negotiate(&headers, JSON)?;
    let profile = profile_routes::find(&app, &profile)?;
    let (_, found) = entries(&app, &profile)
        .await?
        .into_iter()
        .find(|(_, found)| found.id == entry)
        .ok_or_else(not_found)?;
    let mut response = json(StatusCode::OK, &found)?;
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, NO_CACHE);
    Ok(response)
}

pub(crate) async fn delete<E: Engine>(
    State(app): State<Arc<App<E>>>,
    Path((profile, entry)): Path<(String, String)>,
) -> Result<Response, Problem> {
    let profile = profile_routes::find(&app, &profile)?;
    app.require_running()?;
    let _lane = app.write_lane().await;
    let viewing = app.viewing()?;
    // The listed entry with the ID, found as listing finds it.
    let (_, found) = entries(&app, &profile)
        .await?
        .into_iter()
        .find(|(_, found)| found.id == entry)
        .ok_or_else(not_found)?;
    let torrent = TorrentId::from_stored(&found.torrent.id).ok_or_else(Problem::internal)?;
    let entry = UpNextEntryId::from_stored(&found.id).ok_or_else(Problem::internal)?;
    let dismissal = StoredDismissal {
        profile: profile.id.clone(),
        entry,
        torrent,
        at: SystemTime::now(),
    };
    app.store()
        .set_dismissal(&dismissal)
        .await
        .map_err(|error| match error {
            StoreError::UnknownProfile => profile_routes::not_found(),
            StoreError::UnknownTorrent => not_found(),
            error => store_failed(error),
        })?;
    viewing.set_dismissal(&profile.id, &dismissal.entry, dismissal.at);
    Ok(StatusCode::NO_CONTENT.into_response())
}
