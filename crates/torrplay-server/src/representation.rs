// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! Mapping domain values to the contract's representations, and the links
//! between resources.

use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::Serialize;
use sha2::{Digest, Sha256};
use torrplay_api as api;
use torrplay_core::engine;
use torrplay_core::id::{FileId, ProfileId, TorrentId};
use torrplay_core::image::{ImageInfo, ImageType};
use torrplay_core::library::{self, Kind, Library};
use torrplay_core::lifecycle::{self, Configuration, EntityTag, ObservedState};
use torrplay_core::metadata::{Metadata, MetadataFile};
use torrplay_core::profiles::{Profile, Progress};

use crate::app::TorrentView;
use crate::content;
use crate::problem::Problem;

/// The media type of JSON representations.
pub(crate) const JSON: &str = "application/json";

/// The API's base path.
pub(crate) const BASE: &str = "/api/v1";

/// `Cache-Control` of state-bearing responses.
pub(crate) const NO_CACHE: HeaderValue = HeaderValue::from_static("no-cache");

pub(crate) fn torrents_path() -> String {
    format!("{BASE}/torrents")
}

pub(crate) fn torrent_path(id: &TorrentId) -> String {
    format!("{BASE}/torrents/{id}")
}

fn file_path(torrent: &TorrentId, file: &MetadataFile) -> String {
    format!("{}/files/{}", torrent_path(torrent), file.id())
}

/// Serializes `value` as a JSON response.
pub(crate) fn json(status: StatusCode, value: &impl Serialize) -> Result<Response, Problem> {
    let body = serde_json::to_vec(value).map_err(|_| Problem::internal())?;
    Ok((
        status,
        [(header::CONTENT_TYPE, HeaderValue::from_static(JSON))],
        body,
    )
        .into_response())
}

/// Returns the API's value for a byte count or counter. Counts above
/// 2^63 - 1 cannot occur, and saturate if they did.
pub(crate) fn count(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

fn desired_state(value: lifecycle::DesiredState) -> api::DesiredState {
    match value {
        lifecycle::DesiredState::Started => api::DesiredState::Started,
        lifecycle::DesiredState::Paused => api::DesiredState::Paused,
    }
}

fn storage_mode(value: lifecycle::StorageMode) -> api::StorageMode {
    match value {
        lifecycle::StorageMode::Memory => api::StorageMode::Memory,
        lifecycle::StorageMode::Disk => api::StorageMode::Disk,
    }
}

fn kind(value: Kind) -> api::TorrentKind {
    match value {
        Kind::Movie => api::TorrentKind::Movie,
        Kind::Series => api::TorrentKind::Series,
        Kind::Other => api::TorrentKind::Other,
    }
}

pub(crate) fn image_type(value: ImageType) -> api::ImageMediaType {
    match value {
        ImageType::Avif => api::ImageMediaType::ImageAvif,
        ImageType::Gif => api::ImageMediaType::ImageGif,
        ImageType::Jpeg => api::ImageMediaType::ImageJpeg,
        ImageType::Png => api::ImageMediaType::ImagePng,
        ImageType::Webp => api::ImageMediaType::ImageWebp,
    }
}

/// The kind a torrent reports: the configured one, or the one its video
/// files' names suggest once metadata is available.
pub(crate) fn reported_kind(library: &Library, metadata: Option<&Metadata>) -> Option<Kind> {
    library.kind.or_else(|| {
        let metadata = metadata?;
        let names: Vec<String> = metadata
            .files()
            .iter()
            .map(file_name)
            .filter(|name| content::media_type(name).0.starts_with("video/"))
            .collect();
        Some(library::infer_kind(names.iter().map(String::as_str)))
    })
}

fn observed_state(value: ObservedState) -> api::ObservedState {
    match value {
        ObservedState::RetrievingMetadata => api::ObservedState::RetrievingMetadata,
        ObservedState::Downloading => api::ObservedState::Downloading,
        ObservedState::Paused => api::ObservedState::Paused,
        ObservedState::Failed => api::ObservedState::Failed,
    }
}

pub(crate) fn torrent(view: &TorrentView) -> api::Torrent {
    let record = &view.record;
    let metadata = record.metadata.as_deref();
    let library = record.configuration.library();
    let path = torrent_path(&record.id);
    api::Torrent {
        title: library.title.clone(),
        kind: reported_kind(library, metadata).map(kind),
        year: library.year.map(i32::from),
        description: library.description.clone(),
        categories: library.categories.clone(),
        genres: library.genres.clone(),
        poster: record.poster.as_ref().map(|poster| api::TorrentPoster {
            media_type: image_type(poster.image_type),
            size_bytes: count(poster.size_bytes),
            updated_at: poster.updated_at.into(),
        }),
        created_at: record.created_at.into(),
        failure: view.failure.as_ref().map(|failure| api::TorrentFailure {
            detail: failure.detail.clone(),
        }),
        file_count: metadata.map(|metadata| count(metadata.files().len() as u64)),
        id: record.id.to_string(),
        info_hashes: vec![api::InfoHash {
            algorithm: api::InfoHashAlgorithm::Sha1,
            value: record.info_hash.to_string(),
        }],
        links: api::TorrentLinks {
            configuration: format!("{path}/configuration"),
            files: format!("{path}/files"),
            statistics: format!("{path}/statistics"),
            poster: format!("{path}/poster"),
            self_field: path,
        },
        name: metadata.map(|metadata| metadata.display_name()),
        observed_state: observed_state(view.observed_state()),
        size_bytes: metadata.map(|metadata| count(metadata.size_bytes())),
        storage_mode: storage_mode(record.storage_mode),
        storage_transition: view.transition.map(|target| api::TorrentStorageTransition {
            target_storage_mode: storage_mode(target),
        }),
        storage_failure: view.storage_failure.as_ref().map(|(target, detail)| {
            api::TorrentStorageFailure {
                target_storage_mode: storage_mode(*target),
                detail: detail.clone(),
            }
        }),
    }
}

/// The torrent's state ETag, a digest of its representation, so it
/// changes whenever any member does.
pub(crate) fn torrent_etag(torrent: &api::Torrent) -> Result<EntityTag, Problem> {
    let body = serde_json::to_vec(torrent).map_err(|_| Problem::internal())?;
    let digest = Sha256::digest(&body);
    let hex: String = digest[..12]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    Ok(EntityTag::new(format!("st-{hex}")))
}

/// Returns the first 12 bytes of a SHA-256 digest in hexadecimal.
fn short_hex(digest: &[u8]) -> String {
    digest[..12]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// The listing version of `torrents`, in creation order: a digest of their
/// representations, so it changes when one is added, removed, or changes.
pub(crate) fn listing_version(torrents: &[&api::Torrent]) -> Result<String, Problem> {
    let mut hasher = Sha256::new();
    for torrent in torrents {
        let body = serde_json::to_vec(torrent).map_err(|_| Problem::internal())?;
        hasher.update((body.len() as u64).to_be_bytes());
        hasher.update(&body);
    }
    Ok(format!("lv-{}", short_hex(&hasher.finalize())))
}

/// The ETag of a page of the torrents collection: a digest of the listing
/// version and every query parameter, in a canonical order, since the page
/// follows from those alone.
pub(crate) fn listing_etag(version: &str, query: &crate::query::Query) -> EntityTag {
    let mut pairs = query.pairs().to_vec();
    pairs.sort();
    let mut hasher = Sha256::new();
    for part in std::iter::once(version).chain(
        pairs
            .iter()
            .flat_map(|(name, value)| [name.as_str(), value.as_str()]),
    ) {
        hasher.update((part.len() as u64).to_be_bytes());
        hasher.update(part);
    }
    EntityTag::new(format!("tl-{}", short_hex(&hasher.finalize())))
}

/// The configuration's ETag, from its version.
pub(crate) fn configuration_etag(configuration: &Configuration) -> EntityTag {
    EntityTag::new(format!("cfg-{}", configuration.etag().opaque()))
}

/// An uploaded image's ETag, from its digest.
pub(crate) fn image_etag(image: &ImageInfo) -> EntityTag {
    let hex: String = image.digest[..12]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    EntityTag::new(format!("pst-{hex}"))
}

pub(crate) fn configuration(id: &TorrentId, value: &Configuration) -> api::TorrentConfiguration {
    let torrent = torrent_path(id);
    let library = value.library();
    api::TorrentConfiguration {
        desired_state: desired_state(value.desired_state()),
        storage_mode: storage_mode(value.storage_mode()),
        title: library.title.clone(),
        kind: library.kind.map(kind),
        year: library.year.map(i32::from),
        description: library.description.clone(),
        categories: library.categories.clone(),
        genres: library.genres.clone(),
        links: api::TorrentConfigurationLinks {
            self_field: format!("{torrent}/configuration"),
            torrent,
        },
    }
}

/// Returns a file's name: the last component of its path.
pub(crate) fn file_name(file: &MetadataFile) -> String {
    file.display_path().pop().unwrap_or_default()
}

pub(crate) fn file(torrent: &TorrentId, file: &MetadataFile) -> api::TorrentFile {
    let path = file_path(torrent, file);
    api::TorrentFile {
        id: file.id().to_string(),
        links: api::TorrentFileLinks {
            content: format!("{path}/content"),
            preloads: format!("{path}/preloads"),
            self_field: path,
            torrent: torrent_path(torrent),
        },
        media_type: content::media_type(&file_name(file)).0.to_owned(),
        path: file.display_path(),
        size_bytes: count(file.size_bytes()),
    }
}

pub(crate) fn torrent_statistics(
    id: &TorrentId,
    value: &engine::TorrentStatistics,
) -> api::TorrentStatistics {
    let torrent = torrent_path(id);
    api::TorrentStatistics {
        completion_ratio: value.completion_ratio.map(engine::Ratio::get),
        download_rate_bytes_per_second: count(value.download_rate_bytes_per_second),
        downloaded_bytes: count(value.downloaded_bytes),
        links: api::TorrentStatisticsLinks {
            self_field: format!("{torrent}/statistics"),
            torrent,
        },
        peer_count: count(value.peer_count),
        upload_rate_bytes_per_second: count(value.upload_rate_bytes_per_second),
        uploaded_bytes: count(value.uploaded_bytes),
    }
}

pub(crate) fn cache_statistics(value: &engine::CacheStatistics) -> api::CacheStatistics {
    api::CacheStatistics {
        capacity_bytes: count(value.capacity_bytes),
        eviction_count: count(value.eviction_count),
        links: api::CacheStatisticsLinks {
            self_field: format!("{BASE}/cache/statistics"),
        },
        pinned_bytes: count(value.pinned_bytes),
        usage_bytes: count(value.usage_bytes),
    }
}

pub(crate) fn profiles_path() -> String {
    format!("{BASE}/profiles")
}

pub(crate) fn profile_path(id: &ProfileId) -> String {
    format!("{BASE}/profiles/{id}")
}

pub(crate) fn profile(value: &Profile) -> api::Profile {
    let path = profile_path(&value.id);
    api::Profile {
        id: value.id.to_string(),
        name: value.name.clone(),
        created_at: value.created_at.into(),
        avatar: value.avatar.as_ref().map(|avatar| api::ProfileAvatar {
            media_type: image_type(avatar.image_type),
            size_bytes: count(avatar.size_bytes),
            updated_at: avatar.updated_at.into(),
        }),
        links: api::ProfileLinks {
            avatar: format!("{path}/avatar"),
            up_next: format!("{path}/up-next"),
            self_field: path,
        },
    }
}

/// The profile's ETag, a digest of its representation, so it changes
/// whenever any member does.
pub(crate) fn profile_etag(profile: &api::Profile) -> Result<EntityTag, Problem> {
    let body = serde_json::to_vec(profile).map_err(|_| Problem::internal())?;
    Ok(EntityTag::new(format!(
        "prf-{}",
        short_hex(&Sha256::digest(&body))
    )))
}

/// The path of a profile's progress of a torrent's files.
pub(crate) fn progress_path(profile: &ProfileId, torrent: &TorrentId) -> String {
    format!("{}/torrents/{torrent}/progress", profile_path(profile))
}

/// Seconds from milliseconds.
fn seconds(milliseconds: u64) -> f64 {
    milliseconds as f64 / 1000.0
}

pub(crate) fn progress(
    profile: &ProfileId,
    torrent: &TorrentId,
    file: &FileId,
    value: &Progress,
) -> api::Progress {
    let file_path = format!("{}/files/{file}", torrent_path(torrent));
    api::Progress {
        file_id: file.to_string(),
        position_seconds: seconds(value.position_ms),
        duration_seconds: seconds(value.duration_ms),
        completion_ratio: value.completion_ratio(),
        viewed: value.viewed(),
        viewed_at: value.viewed_at.map(Into::into),
        updated_at: value.updated_at.into(),
        links: api::ProgressLinks {
            self_field: format!("{}/{file}", progress_path(profile, torrent)),
            profile: profile_path(profile),
            torrent: torrent_path(torrent),
            content: format!("{file_path}/content"),
            file: file_path,
        },
    }
}

/// A digest of a JSON body as a strong entity tag with `prefix`.
pub(crate) fn body_etag(prefix: &str, value: &impl Serialize) -> Result<EntityTag, Problem> {
    let body = serde_json::to_vec(value).map_err(|_| Problem::internal())?;
    Ok(EntityTag::new(format!(
        "{prefix}-{}",
        short_hex(&Sha256::digest(&body))
    )))
}
