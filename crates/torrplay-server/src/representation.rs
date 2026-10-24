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
use torrplay_core::id::TorrentId;
use torrplay_core::lifecycle::{self, Configuration, EntityTag, ObservedState};
use torrplay_core::metadata::MetadataFile;

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
    let path = torrent_path(&record.id);
    api::Torrent {
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

/// The configuration's ETag, from its version.
pub(crate) fn configuration_etag(configuration: &Configuration) -> EntityTag {
    EntityTag::new(format!("cfg-{}", configuration.etag().opaque()))
}

pub(crate) fn configuration(id: &TorrentId, value: &Configuration) -> api::TorrentConfiguration {
    let torrent = torrent_path(id);
    api::TorrentConfiguration {
        desired_state: desired_state(value.desired_state()),
        storage_mode: storage_mode(value.storage_mode()),
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
