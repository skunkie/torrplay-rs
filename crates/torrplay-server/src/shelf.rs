// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! The torrents the protocol adapters present, DLNA's media server and the
//! Stremio addon: those with metadata and a playable file, neither paused,
//! failed, nor temporary, with their playable files in the order a player
//! lists them.

use std::time::SystemTime;

use torrplay_core::engine::Engine;
use torrplay_core::id::{FileId, TorrentId};
use torrplay_core::library::{self, Kind};
use torrplay_core::lifecycle::{ObservedState, Retention};

use crate::app::App;
use crate::content;
use crate::problem::Problem;
use crate::representation::{self, file_name};

/// A torrent an adapter may present.
pub(crate) struct Shelved {
    pub(crate) id: TorrentId,
    /// Its library title, if it has one.
    pub(crate) title: Option<String>,
    /// Its name from its metadata.
    pub(crate) name: String,
    pub(crate) kind: Option<Kind>,
    /// Its library IMDb ID, if it has one.
    pub(crate) imdb_id: Option<String>,
    pub(crate) categories: Vec<String>,
    pub(crate) genres: Vec<String>,
    pub(crate) has_poster: bool,
    pub(crate) created_at: SystemTime,
    /// Its files served with an `audio/`, `video/`, or `image/` media type,
    /// at least one: in metadata order, or a series' in episode order.
    pub(crate) files: Vec<ShelvedFile>,
}

/// A playable file.
pub(crate) struct ShelvedFile {
    pub(crate) id: FileId,
    pub(crate) name: String,
    pub(crate) size_bytes: u64,
    pub(crate) media_type: &'static str,
    /// Its season and episode, from its name's episode pattern.
    pub(crate) episode: Option<(u32, u32)>,
}

/// The torrents adapters present, newest first, or `None` while the stored
/// torrents are being restored.
pub(crate) async fn shelved<E: Engine>(app: &App<E>) -> Result<Option<Vec<Shelved>>, Problem> {
    let Ok(registry) = app.registry() else {
        return Ok(None);
    };
    let mut torrents = Vec::new();
    for record in registry.list().into_iter().rev() {
        // Temporary torrents are never shown to other players.
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
        let record = view.record;
        let Some(metadata) = record.metadata.as_deref() else {
            continue;
        };
        let library = record.configuration.library();
        let kind = representation::reported_kind(library, Some(metadata));
        let mut files: Vec<(usize, ShelvedFile)> = metadata
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
                        index,
                        ShelvedFile {
                            id: file.id().clone(),
                            episode: library::episode_number(&name),
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
            files.sort_by_key(|(index, file)| (file.episode.is_none(), file.episode, *index));
        }
        torrents.push(Shelved {
            title: library.title.clone(),
            name: metadata.display_name(),
            kind,
            imdb_id: library.imdb_id.clone(),
            categories: library.categories.clone(),
            genres: library.genres.clone(),
            has_poster: record.poster.is_some(),
            created_at: record.created_at,
            files: files.into_iter().map(|(_, file)| file).collect(),
            id: record.id,
        });
    }
    Ok(Some(torrents))
}
