// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! A scripted [`Engine`] over the `torrplay-fixtures` catalog, for tests
//! that run without a torrent engine.
//!
//! [`FakeEngine`] recognizes the exact metainfo bytes and the magnet URIs of
//! the fixtures it holds, rejects other torrent files as invalid and the
//! catalog's v2-only torrent as unsupported, and serves file content from
//! `torrplay_fixtures::generator`. It never contacts peers: a magnet torrent
//! gets its fixture's metadata only once a test marks the metadata
//! retrievable, and failures, readiness, and statistics change only when a
//! test sets them.

use std::collections::{HashMap, HashSet};
use std::io;
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::task::{Context, Poll};

use tokio::io::{AsyncRead, ReadBuf};
use torrplay_fixtures::{FixtureSpec, Layout, catalog, generator};

use crate::engine::{
    ByteRange, CacheStatistics, Engine, EngineError, Inspection, Source, SourceRejection,
    TorrentStatistics, TorrentStatus,
};
use crate::id::{InfoHashV1, PreloadId, TorrentId};
use crate::lifecycle::{DesiredState, Failure, Readiness, StorageMode};
use crate::metadata::{FileIndex, ReportedFile, ReportedMetadata};
use crate::preload::{self, PreloadRequest, PreloadState, PreloadStatus};
use crate::settings::Settings;

/// The detail of a rejected torrent file.
const INVALID_TORRENT_FILE: &str = "Must be a valid torrent file.";

/// The detail of a rejected magnet URI.
const INVALID_MAGNET_URI: &str = "Must be a magnet URI with a BitTorrent v1 info hash.";

/// The detail of a v2-only torrent.
const V2_ONLY: &str = "BitTorrent v2-only torrents are not supported.";

/// A scripted engine; see the [module documentation](self).
pub struct FakeEngine {
    state: Mutex<State>,
}

struct State {
    fixtures: HashMap<InfoHashV1, Fixture>,
    unsupported: Vec<Vec<u8>>,
    torrents: HashMap<TorrentId, Torrent>,
    retrievable: HashSet<InfoHashV1>,
    readiness: Readiness,
    /// The detail of an unexpected failure every operation reports, while
    /// one is injected.
    fault: Option<String>,
    /// The settings last applied.
    settings: Settings,
    /// The piece length a smaller cache capacity is refused below.
    longest_piece_bytes: u64,
    /// The free space a torrent in disk storage may use.
    disk_free_bytes: u64,
    /// The detail storage mode transitions fail with, while one is set.
    transition_failure: Option<String>,
    /// Holds transitions until notified, while set.
    transition_gate: Option<Arc<tokio::sync::Notify>>,
    cache_statistics: CacheStatistics,
}

struct Fixture {
    metainfo: Vec<u8>,
    metadata: Arc<ReportedMetadata>,
    /// The content of each file in the metadata's list.
    contents: Vec<Content>,
}

struct Torrent {
    info_hash: InfoHashV1,
    desired_state: DesiredState,
    storage_mode: StorageMode,
    metadata: Option<Arc<ReportedMetadata>>,
    failure: Option<Failure>,
    statistics: TorrentStatistics,
    /// Its preloads, in creation order.
    preloads: Vec<PreloadStatus>,
}

#[derive(Clone, Copy, Debug)]
enum Content {
    Generated { seed: u64 },
    Zeros,
}

impl Default for FakeEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl FakeEngine {
    /// Creates a ready engine without fixtures that rejects the catalog's
    /// v2-only torrent as unsupported.
    pub fn new() -> Self {
        Self {
            state: Mutex::new(State {
                fixtures: HashMap::new(),
                unsupported: vec![catalog::v2_only().v2_only_metainfo()],
                torrents: HashMap::new(),
                retrievable: HashSet::new(),
                readiness: Readiness::Ready,
                fault: None,
                settings: Settings::DEFAULT,
                longest_piece_bytes: 0,
                disk_free_bytes: u64::MAX,
                transition_failure: None,
                transition_gate: None,
                cache_statistics: CacheStatistics::default(),
            }),
        }
    }

    /// Creates a ready engine with the catalog's small fixtures:
    /// `single-small`, `multi-boundary`, `many-files`, and `media-names` as
    /// v1 torrents, and `hybrid` and `hybrid-equal-padding` as hybrid
    /// torrents with BEP 47 padding files.
    pub fn with_catalog() -> Self {
        let engine = Self::new();
        for spec in [
            catalog::single_small(),
            catalog::multi_boundary(),
            catalog::many_files(),
            catalog::media_names(),
        ] {
            engine.add_fixture(&spec);
        }
        for spec in [catalog::hybrid(), catalog::hybrid_equal_padding()] {
            engine.add_hybrid_fixture(&spec);
        }
        engine
    }

    /// Adds a fixture whose torrent file is [`FixtureSpec::metainfo`] and
    /// returns its info hash.
    pub fn add_fixture(&self, spec: &FixtureSpec) -> InfoHashV1 {
        let metainfo = spec.metainfo();
        let name = spec.torrent_name.as_bytes().to_vec();
        let (files, contents) = match &spec.layout {
            Layout::SingleFile { size_bytes } => (
                vec![ReportedFile {
                    path: vec![name.clone()],
                    size_bytes: *size_bytes,
                    padding: false,
                }],
                vec![Content::Generated {
                    seed: spec.file_seed(0),
                }],
            ),
            Layout::MultiFile { files } => files
                .iter()
                .enumerate()
                .map(|(index, file)| {
                    (
                        ReportedFile {
                            path: full_path(&name, &file.path),
                            size_bytes: file.size_bytes,
                            padding: false,
                        },
                        Content::Generated {
                            seed: spec.file_seed(index),
                        },
                    )
                })
                .unzip(),
        };
        self.insert_fixture(metainfo.info_hash, metainfo.bytes, name, files, contents)
    }

    /// Adds a fixture whose torrent file is
    /// [`FixtureSpec::hybrid_metainfo`], with the padding files that metainfo
    /// lists, and returns its v1 info hash.
    pub fn add_hybrid_fixture(&self, spec: &FixtureSpec) -> InfoHashV1 {
        let metainfo = spec.hybrid_metainfo();
        let name = spec.torrent_name.as_bytes().to_vec();
        let piece_length = u64::from(spec.piece_length);
        let spec_files = spec.files();
        let mut files = Vec::new();
        let mut contents = Vec::new();
        for (index, file) in spec_files.iter().enumerate() {
            files.push(ReportedFile {
                path: full_path(&name, &file.path),
                size_bytes: file.size_bytes,
                padding: false,
            });
            contents.push(Content::Generated {
                seed: spec.file_seed(index),
            });
            let remainder = file.size_bytes % piece_length;
            if index + 1 < spec_files.len() && remainder != 0 {
                let padding = piece_length - remainder;
                files.push(ReportedFile {
                    path: vec![
                        name.clone(),
                        b".pad".to_vec(),
                        padding.to_string().into_bytes(),
                    ],
                    size_bytes: padding,
                    padding: true,
                });
                contents.push(Content::Zeros);
            }
        }
        self.insert_fixture(metainfo.info_hash, metainfo.bytes, name, files, contents)
    }

    fn insert_fixture(
        &self,
        info_hash: [u8; 20],
        metainfo: Vec<u8>,
        name: Vec<u8>,
        files: Vec<ReportedFile>,
        contents: Vec<Content>,
    ) -> InfoHashV1 {
        let info_hash = InfoHashV1::from_bytes(info_hash);
        let fixture = Fixture {
            metainfo,
            metadata: Arc::new(ReportedMetadata { name, files }),
            contents,
        };
        self.lock().fixtures.insert(info_hash, fixture);
        info_hash
    }

    /// Sets whether peers supply the metadata of the fixture with
    /// `info_hash`. While it is retrievable, a started magnet torrent with
    /// that info hash has the fixture's metadata from its next status.
    pub fn set_metadata_retrievable(&self, info_hash: InfoHashV1, retrievable: bool) {
        let mut state = self.lock();
        if retrievable {
            state.retrievable.insert(info_hash);
        } else {
            state.retrievable.remove(&info_hash);
        }
    }

    /// Fails a torrent with `detail`. It stays failed until it is paused and
    /// started again.
    pub fn inject_failure(&self, id: &TorrentId, detail: &str) -> Result<(), EngineError> {
        let mut state = self.lock();
        let torrent = state
            .torrents
            .get_mut(id)
            .ok_or(EngineError::UnknownTorrent)?;
        torrent.failure = Some(Failure {
            detail: detail.to_owned(),
        });
        Ok(())
    }

    /// Makes every asynchronous operation fail with
    /// [`EngineError::Internal`] carrying `detail`, or stops doing so for
    /// `None`. Readiness is unaffected: the engine is ready but failing.
    pub fn inject_fault(&self, detail: Option<&str>) {
        self.lock().fault = detail.map(str::to_owned);
    }

    /// The settings last applied, or the defaults.
    pub fn settings(&self) -> Settings {
        self.lock().settings
    }

    /// Sets the free space torrent files added in disk storage may use;
    /// larger ones are [`EngineError::InsufficientStorage`].
    pub fn set_disk_free_bytes(&self, bytes: u64) {
        self.lock().disk_free_bytes = bytes;
    }

    /// Makes storage mode transitions fail with `detail`, or succeed again
    /// for `None`.
    pub fn fail_transitions(&self, detail: Option<&str>) {
        self.lock().transition_failure = detail.map(str::to_owned);
    }

    /// Holds every later storage mode transition until the returned gate is
    /// notified, so tests can observe one in progress.
    pub fn hold_transitions(&self) -> Arc<tokio::sync::Notify> {
        let gate = Arc::new(tokio::sync::Notify::new());
        self.lock().transition_gate = Some(gate.clone());
        gate
    }

    /// Moves a preload to `state`, as the engine would as it downloads,
    /// expires, or fails with `failure`. An ended preload records the time.
    pub fn set_preload_state(
        &self,
        id: &TorrentId,
        preload: &str,
        state: PreloadState,
        failure: Option<&str>,
    ) {
        let mut engine = self.lock();
        let Some(status) = engine.torrents.get_mut(id).and_then(|torrent| {
            torrent
                .preloads
                .iter_mut()
                .find(|status| status.id.as_str() == preload)
        }) else {
            return;
        };
        status.state = state;
        status.completed_bytes = if state == PreloadState::Ready {
            status.target_bytes
        } else {
            0
        };
        status.ended_at = state.is_ended().then(std::time::SystemTime::now);
        status.failure = failure.map(|detail| Failure {
            detail: detail.to_owned(),
        });
    }

    /// The storage mode a torrent was added in.
    pub fn storage_mode(&self, id: &TorrentId) -> Option<StorageMode> {
        self.lock()
            .torrents
            .get(id)
            .map(|torrent| torrent.storage_mode)
    }

    /// Makes the engine refuse a cache capacity smaller than its current
    /// one and below `bytes`, as if a torrent had pieces of that length.
    pub fn set_longest_piece(&self, bytes: u64) {
        self.lock().longest_piece_bytes = bytes;
    }

    /// Sets the engine's readiness.
    pub fn set_readiness(&self, readiness: Readiness) {
        self.lock().readiness = readiness;
    }

    /// Sets the statistics a torrent reports.
    pub fn set_torrent_statistics(
        &self,
        id: &TorrentId,
        statistics: TorrentStatistics,
    ) -> Result<(), EngineError> {
        let mut state = self.lock();
        let torrent = state
            .torrents
            .get_mut(id)
            .ok_or(EngineError::UnknownTorrent)?;
        torrent.statistics = statistics;
        Ok(())
    }

    /// Sets the statistics the piece cache reports.
    pub fn set_cache_statistics(&self, statistics: CacheStatistics) {
        self.lock().cache_statistics = statistics;
    }

    /// Returns the desired state the engine last accepted for a torrent, or
    /// `None` if it holds no torrent with the ID.
    pub fn desired_state(&self, id: &TorrentId) -> Option<DesiredState> {
        self.lock()
            .torrents
            .get(id)
            .map(|torrent| torrent.desired_state)
    }

    /// Locks the state. Updates leave it consistent before any point that
    /// can panic, so a poisoned lock's state is still usable.
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Locks the state if the engine is ready and no fault is injected.
    fn ready(&self) -> Result<MutexGuard<'_, State>, EngineError> {
        let state = self.lock();
        if let Some(detail) = &state.fault {
            return Err(EngineError::Internal(
                io::Error::other(detail.clone()).into(),
            ));
        }
        match state.readiness {
            Readiness::Ready => Ok(state),
            Readiness::Unavailable { retry_after } => Err(EngineError::Unavailable { retry_after }),
        }
    }
}

impl State {
    fn inspect(&self, source: &Source) -> Result<Inspection, SourceRejection> {
        match source {
            Source::TorrentFile(bytes) => {
                if let Some((info_hash, fixture)) = self
                    .fixtures
                    .iter()
                    .find(|(_, fixture)| &fixture.metainfo == bytes)
                {
                    Ok(Inspection {
                        info_hash: *info_hash,
                        metadata: Some(Arc::clone(&fixture.metadata)),
                    })
                } else if self.unsupported.contains(bytes) {
                    Err(SourceRejection::Unsupported {
                        detail: V2_ONLY.to_owned(),
                    })
                } else {
                    Err(SourceRejection::Invalid {
                        detail: INVALID_TORRENT_FILE.to_owned(),
                    })
                }
            }
            Source::MagnetUri(uri) => Ok(Inspection {
                info_hash: parse_magnet_uri(uri)?,
                metadata: None,
            }),
        }
    }

    fn torrent(&mut self, id: &TorrentId) -> Result<&mut Torrent, EngineError> {
        let State {
            fixtures,
            torrents,
            retrievable,
            ..
        } = self;
        let torrent = torrents.get_mut(id).ok_or(EngineError::UnknownTorrent)?;
        // Simulated metadata retrieval, which runs only while started.
        if torrent.metadata.is_none()
            && torrent.desired_state == DesiredState::Started
            && torrent.failure.is_none()
            && retrievable.contains(&torrent.info_hash)
        {
            torrent.metadata = fixtures
                .get(&torrent.info_hash)
                .map(|fixture| Arc::clone(&fixture.metadata));
        }
        Ok(torrent)
    }
}

impl Engine for FakeEngine {
    type Reader = FakeReader;

    fn readiness(&self) -> Readiness {
        self.lock().readiness
    }

    fn apply_settings(&self, settings: &Settings) -> Result<(), EngineError> {
        let mut state = self.lock();
        if settings.cache_capacity_bytes < state.settings.cache_capacity_bytes
            && settings.cache_capacity_bytes < state.longest_piece_bytes
        {
            return Err(EngineError::CapacityBelowPieceLength {
                piece_length_bytes: state.longest_piece_bytes,
            });
        }
        state.settings = *settings;
        Ok(())
    }

    async fn inspect(&self, source: &Source) -> Result<Inspection, EngineError> {
        Ok(self.ready()?.inspect(source)?)
    }

    async fn add_in(
        &self,
        id: &TorrentId,
        source: &Source,
        desired_state: DesiredState,
        storage_mode: StorageMode,
    ) -> Result<(), EngineError> {
        let mut state = self.ready()?;
        let inspection = state.inspect(source)?;
        if state.torrents.contains_key(id) {
            return Err(EngineError::DuplicateTorrent);
        }
        if storage_mode == StorageMode::Disk
            && let Some(metadata) = &inspection.metadata
        {
            let needed_bytes = metadata.files.iter().map(|file| file.size_bytes).sum();
            if needed_bytes > state.disk_free_bytes {
                return Err(EngineError::InsufficientStorage {
                    needed_bytes,
                    available_bytes: state.disk_free_bytes,
                });
            }
        }
        state.torrents.insert(
            id.clone(),
            Torrent {
                info_hash: inspection.info_hash,
                desired_state,
                storage_mode,
                metadata: inspection.metadata,
                failure: None,
                statistics: TorrentStatistics::default(),
                preloads: Vec::new(),
            },
        );
        Ok(())
    }

    async fn check_storage_mode(
        &self,
        id: &TorrentId,
        storage_mode: StorageMode,
    ) -> Result<(), EngineError> {
        let mut state = self.ready()?;
        let free = state.disk_free_bytes;
        let torrent = state.torrent(id)?;
        if storage_mode == StorageMode::Disk
            && let Some(metadata) = &torrent.metadata
        {
            let needed_bytes = metadata.files.iter().map(|file| file.size_bytes).sum();
            if needed_bytes > free {
                return Err(EngineError::InsufficientStorage {
                    needed_bytes,
                    available_bytes: free,
                });
            }
        }
        Ok(())
    }

    async fn change_storage_mode(
        &self,
        id: &TorrentId,
        storage_mode: StorageMode,
    ) -> Result<(), EngineError> {
        let gate = {
            let mut state = self.ready()?;
            state.torrent(id)?;
            state.transition_gate.clone()
        };
        if let Some(gate) = gate {
            gate.notified().await;
        }
        let mut state = self.ready()?;
        if let Some(detail) = state.transition_failure.clone() {
            return Err(EngineError::TransitionFailed { detail });
        }
        state.torrent(id)?.storage_mode = storage_mode;
        Ok(())
    }

    async fn supply_metadata(
        &self,
        id: &TorrentId,
        torrent_file: &[u8],
    ) -> Result<(), EngineError> {
        let mut state = self.ready()?;
        let inspection = state.inspect(&Source::TorrentFile(torrent_file.to_vec()))?;
        let torrent = state.torrent(id)?;
        if inspection.info_hash != torrent.info_hash {
            return Err(EngineError::MetadataMismatch);
        }
        if torrent.metadata.is_none() {
            torrent.metadata = inspection.metadata;
        }
        Ok(())
    }

    async fn set_desired_state(
        &self,
        id: &TorrentId,
        desired_state: DesiredState,
    ) -> Result<(), EngineError> {
        let mut state = self.ready()?;
        let torrent = state.torrent(id)?;
        if torrent.desired_state == DesiredState::Paused && desired_state == DesiredState::Started {
            torrent.failure = None;
        }
        torrent.desired_state = desired_state;
        if desired_state == DesiredState::Paused {
            fail_preloads(torrent, "The torrent was paused.");
        }
        Ok(())
    }

    async fn remove(&self, id: &TorrentId) -> Result<(), EngineError> {
        self.ready()?
            .torrents
            .remove(id)
            .map(drop)
            .ok_or(EngineError::UnknownTorrent)
    }

    async fn status(&self, id: &TorrentId) -> Result<TorrentStatus, EngineError> {
        let mut state = self.ready()?;
        let torrent = state.torrent(id)?;
        Ok(TorrentStatus {
            metadata: torrent.metadata.clone(),
            failure: torrent.failure.clone(),
        })
    }

    async fn torrent_file(&self, id: &TorrentId) -> Result<Option<Vec<u8>>, EngineError> {
        let mut state = self.ready()?;
        let torrent = state.torrent(id)?;
        if torrent.metadata.is_none() {
            return Ok(None);
        }
        let info_hash = torrent.info_hash;
        Ok(state
            .fixtures
            .get(&info_hash)
            .map(|fixture| fixture.metainfo.clone()))
    }

    async fn open_reader(
        &self,
        id: &TorrentId,
        file: FileIndex,
        range: ByteRange,
    ) -> Result<FakeReader, EngineError> {
        let mut state = self.ready()?;
        let torrent = state.torrent(id)?;
        let metadata = torrent
            .metadata
            .clone()
            .ok_or(EngineError::MetadataUnavailable)?;
        if torrent.desired_state != DesiredState::Started || torrent.failure.is_some() {
            return Err(EngineError::NotStarted);
        }
        let info_hash = torrent.info_hash;
        let size_bytes = metadata
            .files
            .get(file.0)
            .ok_or(EngineError::UnknownFile)?
            .size_bytes;
        if range.end() > size_bytes {
            return Err(EngineError::RangeOutOfBounds);
        }
        let content = state
            .fixtures
            .get(&info_hash)
            .and_then(|fixture| fixture.contents.get(file.0))
            .copied()
            .ok_or(EngineError::UnknownFile)?;
        Ok(FakeReader {
            content,
            position: range.start(),
            end: range.end(),
        })
    }

    async fn create_preload(
        &self,
        id: &TorrentId,
        file: FileIndex,
        preload: PreloadId,
        request: PreloadRequest,
    ) -> Result<PreloadStatus, EngineError> {
        let mut state = self.ready()?;
        let active = state
            .torrents
            .values()
            .flat_map(|torrent| &torrent.preloads)
            .filter(|preload| !preload.state.is_ended())
            .count();
        let torrent = state.torrent(id)?;
        let size_bytes = file_size(torrent, file)?;
        if torrent.desired_state != DesiredState::Started || torrent.failure.is_some() {
            return Err(EngineError::NotStarted);
        }
        if request
            .offset_bytes
            .is_some_and(|offset| offset >= size_bytes)
        {
            return Err(EngineError::RangeOutOfBounds);
        }
        let of_file = torrent
            .preloads
            .iter()
            .filter(|preload| preload.file == file && !preload.state.is_ended())
            .count();
        if of_file >= preload::PER_FILE_LIMIT || active >= preload::TOTAL_LIMIT {
            return Err(EngineError::TooManyPreloads);
        }
        let ranges = fake_ranges(size_bytes, request.offset_bytes);
        let created_at = std::time::SystemTime::now();
        let status = PreloadStatus {
            id: preload,
            file,
            state: PreloadState::Downloading,
            offset_bytes: request.offset_bytes,
            target_bytes: ranges.iter().map(|range| range.len_bytes()).sum(),
            ranges,
            completed_bytes: 0,
            created_at,
            expires_at: created_at + request.ttl,
            ended_at: None,
            failure: None,
        };
        torrent.preloads.push(status.clone());
        let mut excess = torrent
            .preloads
            .iter()
            .filter(|preload| preload.file == file)
            .count()
            .saturating_sub(preload::LISTED_LIMIT);
        torrent.preloads.retain(|preload| {
            if excess > 0 && preload.file == file && preload.state.is_ended() {
                excess -= 1;
                return false;
            }
            true
        });
        Ok(status)
    }

    async fn preloads(
        &self,
        id: &TorrentId,
        file: FileIndex,
    ) -> Result<Vec<PreloadStatus>, EngineError> {
        let mut state = self.ready()?;
        let torrent = state.torrent(id)?;
        file_size(torrent, file)?;
        Ok(torrent
            .preloads
            .iter()
            .filter(|preload| preload.file == file)
            .cloned()
            .collect())
    }

    async fn preload(
        &self,
        id: &TorrentId,
        file: FileIndex,
        preload: &PreloadId,
    ) -> Result<PreloadStatus, EngineError> {
        self.preloads(id, file)
            .await?
            .into_iter()
            .find(|status| &status.id == preload)
            .ok_or(EngineError::UnknownPreload)
    }

    async fn cancel_preload(
        &self,
        id: &TorrentId,
        file: FileIndex,
        preload: &PreloadId,
    ) -> Result<(), EngineError> {
        let mut state = self.ready()?;
        let torrent = state.torrent(id)?;
        file_size(torrent, file)?;
        let before = torrent.preloads.len();
        torrent
            .preloads
            .retain(|status| !(status.file == file && &status.id == preload));
        if torrent.preloads.len() == before {
            return Err(EngineError::UnknownPreload);
        }
        Ok(())
    }

    async fn torrent_statistics(&self, id: &TorrentId) -> Result<TorrentStatistics, EngineError> {
        Ok(self.ready()?.torrent(id)?.statistics)
    }

    async fn cache_statistics(&self) -> Result<CacheStatistics, EngineError> {
        Ok(self.ready()?.cache_statistics)
    }
}

/// The size of the file at `file`, with the errors preload operations
/// return for a torrent without metadata or an index outside its files.
fn file_size(torrent: &Torrent, file: FileIndex) -> Result<u64, EngineError> {
    Ok(torrent
        .metadata
        .as_ref()
        .ok_or(EngineError::MetadataUnavailable)?
        .files
        .get(file.0)
        .ok_or(EngineError::UnknownFile)?
        .size_bytes)
}

/// Ranges shaped like the engine's, without pieces: the whole file when it
/// fits, or else its head and tail, or a window at `offset`.
fn fake_ranges(size_bytes: u64, offset: Option<u64>) -> Vec<ByteRange> {
    let limit = preload::MAX_PRELOAD_BYTES;
    if size_bytes <= limit {
        return ByteRange::new(0, size_bytes).into_iter().collect();
    }
    let tail = ByteRange::new(size_bytes - preload::TAIL_BYTES, size_bytes);
    let head = match offset {
        None => ByteRange::new(0, limit - preload::TAIL_BYTES),
        Some(offset) => {
            let start = offset.min(size_bytes - limit);
            ByteRange::new(start, start + limit - preload::TAIL_BYTES)
        }
    };
    head.into_iter().chain(tail).collect()
}

/// Ends a torrent's active preloads with a failure.
fn fail_preloads(torrent: &mut Torrent, detail: &str) {
    for preload in &mut torrent.preloads {
        if !preload.state.is_ended() {
            preload.state = PreloadState::Failed;
            preload.ended_at = Some(std::time::SystemTime::now());
            preload.failure = Some(Failure {
                detail: detail.to_owned(),
            });
        }
    }
}

/// A reader over a fixture file's generated content.
#[derive(Debug)]
pub struct FakeReader {
    content: Content,
    position: u64,
    end: u64,
}

impl AsyncRead for FakeReader {
    fn poll_read(
        mut self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let remaining = self.end - self.position;
        // Bounded by the buffer's remaining length, so it fits in usize.
        let len = remaining.min(buf.remaining() as u64) as usize;
        let chunk = buf.initialize_unfilled_to(len);
        match self.content {
            Content::Generated { seed } => generator::fill(seed, self.position, chunk),
            Content::Zeros => chunk.fill(0),
        }
        buf.advance(len);
        self.position += len as u64;
        Poll::Ready(Ok(()))
    }
}

fn full_path(name: &[u8], path: &[String]) -> Vec<Vec<u8>> {
    std::iter::once(name.to_vec())
        .chain(path.iter().map(|part| part.as_bytes().to_vec()))
        .collect()
}

/// Parses a magnet URI's v1 info hash from an `xt` parameter of the form
/// `urn:btih:` followed by 40 hexadecimal or 32 base32 digits. A URI with
/// only a v2 `urn:btmh:` hash is unsupported.
fn parse_magnet_uri(uri: &str) -> Result<InfoHashV1, SourceRejection> {
    let invalid = || SourceRejection::Invalid {
        detail: INVALID_MAGNET_URI.to_owned(),
    };
    let query = uri.strip_prefix("magnet:?").ok_or_else(invalid)?;
    let mut v1 = None;
    let mut v2 = false;
    for parameter in query.split('&') {
        let Some((name, value)) = parameter.split_once('=') else {
            continue;
        };
        if name != "xt" && !name.starts_with("xt.") {
            continue;
        }
        if let Some(hash) = value.strip_prefix("urn:btih:") {
            v1 = Some(parse_btih(hash).ok_or_else(invalid)?);
        } else if value.starts_with("urn:btmh:") {
            v2 = true;
        }
    }
    match (v1, v2) {
        (Some(info_hash), _) => Ok(info_hash),
        (None, true) => Err(SourceRejection::Unsupported {
            detail: V2_ONLY.to_owned(),
        }),
        (None, false) => Err(invalid()),
    }
}

fn parse_btih(hash: &str) -> Option<InfoHashV1> {
    match hash.len() {
        40 => InfoHashV1::from_hex(hash),
        32 => crate::base32::decode(&hash.to_ascii_lowercase())
            .and_then(|bytes| <[u8; 20]>::try_from(bytes).ok())
            .map(InfoHashV1::from_bytes),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use tokio::io::AsyncReadExt;
    use torrplay_fixtures::{FileSpec, hex};

    use super::*;
    use crate::engine::Ratio;
    use crate::metadata::{Metadata, MetadataError};

    fn id(n: u8) -> TorrentId {
        TorrentId::from_random_bytes([n; 16])
    }

    fn magnet(spec: &FixtureSpec) -> Source {
        let info_hash = spec.metainfo().info_hash;
        Source::MagnetUri(spec.magnet_uri(&info_hash))
    }

    fn file(spec: &FixtureSpec) -> Source {
        Source::TorrentFile(spec.metainfo().bytes)
    }

    async fn read(reader: FakeReader) -> Vec<u8> {
        let mut reader = reader;
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes).await.unwrap();
        bytes
    }

    #[tokio::test]
    async fn recognizes_catalog_torrent_files_and_magnets() {
        let engine = FakeEngine::with_catalog();
        let spec = catalog::multi_boundary();
        let expected = InfoHashV1::from_bytes(spec.metainfo().info_hash);

        let from_file = engine.inspect(&file(&spec)).await.unwrap();
        assert_eq!(from_file.info_hash, expected);
        let metadata = Metadata::identify(&from_file.metadata.unwrap()).unwrap();
        assert_eq!(metadata.display_name(), "multi-boundary");
        assert_eq!(metadata.size_bytes(), spec.total_size_bytes());
        assert_eq!(
            metadata.files()[0].display_path(),
            ["multi-boundary", "part-1.bin"]
        );

        let from_magnet = engine.inspect(&magnet(&spec)).await.unwrap();
        assert_eq!(from_magnet.info_hash, expected);
        assert_eq!(from_magnet.metadata, None);

        let base32_magnet = format!(
            "magnet:?xt=urn:btih:{}",
            crate::base32::encode(expected.as_bytes()).to_uppercase()
        );
        assert_eq!(
            engine
                .inspect(&Source::MagnetUri(base32_magnet))
                .await
                .unwrap()
                .info_hash,
            expected
        );
    }

    #[tokio::test]
    async fn rejects_invalid_and_v2_only_sources() {
        let engine = FakeEngine::with_catalog();
        let v2_hash = format!("1220{}", "ab".repeat(32));
        let cases = [
            (
                Source::TorrentFile(catalog::invalid_metainfo()),
                SourceRejection::Invalid {
                    detail: INVALID_TORRENT_FILE.into(),
                },
            ),
            (
                Source::MagnetUri("magnet:?dn=no-hash".into()),
                SourceRejection::Invalid {
                    detail: INVALID_MAGNET_URI.into(),
                },
            ),
            (
                Source::MagnetUri("https://example.test/a.torrent".into()),
                SourceRejection::Invalid {
                    detail: INVALID_MAGNET_URI.into(),
                },
            ),
            (
                Source::MagnetUri("magnet:?xt=urn:btih:0123".into()),
                SourceRejection::Invalid {
                    detail: INVALID_MAGNET_URI.into(),
                },
            ),
            (
                Source::TorrentFile(catalog::v2_only().v2_only_metainfo()),
                SourceRejection::Unsupported {
                    detail: V2_ONLY.into(),
                },
            ),
            (
                Source::MagnetUri(format!("magnet:?xt=urn:btmh:{v2_hash}")),
                SourceRejection::Unsupported {
                    detail: V2_ONLY.into(),
                },
            ),
        ];
        for (source, expected) in cases {
            match engine.inspect(&source).await {
                Err(EngineError::Rejected(rejection)) => {
                    assert_eq!(rejection, expected, "{source:?}");
                }
                other => panic!("{source:?} must be rejected, got {other:?}"),
            }
        }
    }

    #[tokio::test]
    async fn hybrid_torrents_report_padding_files_the_api_omits() {
        let engine = FakeEngine::with_catalog();
        for spec in [catalog::hybrid(), catalog::hybrid_equal_padding()] {
            let bytes = spec.hybrid_metainfo().bytes;
            let inspection = engine.inspect(&Source::TorrentFile(bytes)).await.unwrap();
            let reported = inspection.metadata.unwrap();
            assert!(
                reported.files.iter().any(|file| file.padding),
                "{}",
                spec.id
            );
            let metadata = Metadata::identify(&reported).unwrap();
            assert_eq!(metadata.files().len(), 3, "{}", spec.id);
            assert_eq!(
                metadata.size_bytes(),
                spec.total_size_bytes(),
                "{}",
                spec.id
            );
        }
    }

    #[tokio::test]
    async fn duplicate_paths_reach_metadata_validation() {
        let engine = FakeEngine::new();
        let mut spec = catalog::single_small();
        spec.id = "duplicate-paths".into();
        spec.torrent_name = "duplicate-paths".into();
        let part = FileSpec {
            path: vec!["same.bin".into()],
            size_bytes: 100,
        };
        spec.layout = Layout::MultiFile {
            files: vec![part.clone(), part],
        };
        engine.add_fixture(&spec);
        let reported = engine
            .inspect(&file(&spec))
            .await
            .unwrap()
            .metadata
            .unwrap();
        assert!(matches!(
            Metadata::identify(&reported),
            Err(MetadataError::DuplicatePath { .. })
        ));
    }

    #[tokio::test]
    async fn magnet_metadata_arrives_only_when_retrievable_and_started() {
        let engine = FakeEngine::with_catalog();
        let spec = catalog::single_small();
        let info_hash = InfoHashV1::from_bytes(spec.metainfo().info_hash);
        engine
            .add(&id(1), &magnet(&spec), DesiredState::Started)
            .await
            .unwrap();
        engine
            .add(&id(2), &magnet(&spec), DesiredState::Paused)
            .await
            .unwrap();
        assert_eq!(engine.status(&id(1)).await.unwrap().metadata, None);

        engine.set_metadata_retrievable(info_hash, true);
        let metadata = engine.status(&id(1)).await.unwrap().metadata.unwrap();
        assert_eq!(metadata.name, b"single-small.bin");
        assert_eq!(
            engine.status(&id(2)).await.unwrap().metadata,
            None,
            "a paused torrent does not retrieve metadata"
        );
    }

    #[tokio::test]
    async fn unknown_magnets_never_get_metadata() {
        let engine = FakeEngine::with_catalog();
        let info_hash = InfoHashV1::from_bytes([0x42; 20]);
        let uri = format!(
            "magnet:?xt=urn:btih:{}&dn=unknown",
            hex(info_hash.as_bytes())
        );
        engine
            .add(&id(1), &Source::MagnetUri(uri), DesiredState::Started)
            .await
            .unwrap();
        engine.set_metadata_retrievable(info_hash, true);
        let status = engine.status(&id(1)).await.unwrap();
        assert_eq!(status.metadata, None);
        assert!(matches!(
            engine
                .open_reader(&id(1), FileIndex(0), ByteRange::new(0, 1).unwrap())
                .await,
            Err(EngineError::MetadataUnavailable)
        ));
    }

    #[tokio::test]
    async fn supplied_metadata_ends_retrieval_and_must_match() {
        let engine = FakeEngine::with_catalog();
        let spec = catalog::media_names();
        engine
            .add(&id(1), &magnet(&spec), DesiredState::Started)
            .await
            .unwrap();
        assert!(matches!(
            engine
                .supply_metadata(&id(1), &catalog::single_small().metainfo().bytes)
                .await,
            Err(EngineError::MetadataMismatch)
        ));
        engine
            .supply_metadata(&id(1), &spec.metainfo().bytes)
            .await
            .unwrap();
        let metadata = engine.status(&id(1)).await.unwrap().metadata.unwrap();
        assert_eq!(metadata.files.len(), 8);
        assert!(matches!(
            engine.supply_metadata(&id(9), &spec.metainfo().bytes).await,
            Err(EngineError::UnknownTorrent)
        ));
    }

    #[tokio::test]
    async fn failures_clear_only_after_pause_and_start() {
        let engine = FakeEngine::with_catalog();
        let spec = catalog::single_small();
        engine
            .add(&id(1), &file(&spec), DesiredState::Started)
            .await
            .unwrap();
        engine
            .inject_failure(&id(1), "A piece could not be written.")
            .unwrap();
        let failure = |engine: &FakeEngine| {
            let state = engine.lock();
            state.torrents[&id(1)].failure.clone()
        };
        engine
            .set_desired_state(&id(1), DesiredState::Started)
            .await
            .unwrap();
        assert!(
            failure(&engine).is_some(),
            "starting again keeps the failure"
        );
        assert!(matches!(
            engine
                .open_reader(&id(1), FileIndex(0), ByteRange::new(0, 1).unwrap())
                .await,
            Err(EngineError::NotStarted)
        ));
        engine
            .set_desired_state(&id(1), DesiredState::Paused)
            .await
            .unwrap();
        assert_eq!(
            engine.status(&id(1)).await.unwrap().failure,
            Some(Failure {
                detail: "A piece could not be written.".into()
            })
        );
        engine
            .set_desired_state(&id(1), DesiredState::Started)
            .await
            .unwrap();
        assert_eq!(engine.status(&id(1)).await.unwrap().failure, None);
        assert_eq!(engine.desired_state(&id(1)), Some(DesiredState::Started));
    }

    #[tokio::test]
    async fn readers_serve_generated_content() {
        let engine = FakeEngine::with_catalog();
        let spec = catalog::multi_boundary();
        engine
            .add(&id(1), &file(&spec), DesiredState::Started)
            .await
            .unwrap();
        let size = spec.files()[3].size_bytes;
        let range = ByteRange::new(1_000, size).unwrap();
        let bytes = read(
            engine
                .open_reader(&id(1), FileIndex(3), range)
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(bytes.len() as u64, range.len_bytes());
        assert_eq!(
            generator::first_mismatch(spec.file_seed(3), 1_000, &bytes),
            None
        );

        let empty = ByteRange::new(size, size).unwrap();
        assert!(
            read(
                engine
                    .open_reader(&id(1), FileIndex(3), empty)
                    .await
                    .unwrap()
            )
            .await
            .is_empty()
        );
        assert!(matches!(
            engine
                .open_reader(&id(1), FileIndex(3), ByteRange::new(0, size + 1).unwrap())
                .await,
            Err(EngineError::RangeOutOfBounds)
        ));
        assert!(matches!(
            engine
                .open_reader(&id(1), FileIndex(5), ByteRange::new(0, 1).unwrap())
                .await,
            Err(EngineError::UnknownFile)
        ));
        engine
            .set_desired_state(&id(1), DesiredState::Paused)
            .await
            .unwrap();
        assert!(matches!(
            engine
                .open_reader(&id(1), FileIndex(0), ByteRange::new(0, 1).unwrap())
                .await,
            Err(EngineError::NotStarted)
        ));
    }

    #[tokio::test]
    async fn padding_files_read_as_zeros() {
        let engine = FakeEngine::with_catalog();
        let bytes = catalog::hybrid().hybrid_metainfo().bytes;
        engine
            .add(&id(1), &Source::TorrentFile(bytes), DesiredState::Started)
            .await
            .unwrap();
        let reported = engine.status(&id(1)).await.unwrap().metadata.unwrap();
        let index = reported.files.iter().position(|file| file.padding).unwrap();
        let size = reported.files[index].size_bytes;
        let range = ByteRange::new(0, size).unwrap();
        let bytes = read(
            engine
                .open_reader(&id(1), FileIndex(index), range)
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(bytes, vec![0; size as usize]);
    }

    #[tokio::test]
    async fn readiness_gates_every_operation() {
        let engine = FakeEngine::with_catalog();
        let spec = catalog::single_small();
        engine
            .add(&id(1), &file(&spec), DesiredState::Started)
            .await
            .unwrap();
        let retry_after = Some(Duration::from_secs(5));
        engine.set_readiness(Readiness::Unavailable { retry_after });
        assert!(!engine.readiness().is_ready());
        let unavailable = |result: Result<(), EngineError>| matches!(result, Err(EngineError::Unavailable { retry_after: r }) if r == retry_after);
        assert!(unavailable(engine.inspect(&file(&spec)).await.map(drop)));
        assert!(unavailable(
            engine
                .add(&id(2), &file(&spec), DesiredState::Started)
                .await
        ));
        assert!(unavailable(engine.status(&id(1)).await.map(drop)));
        assert!(unavailable(
            engine.set_desired_state(&id(1), DesiredState::Paused).await
        ));
        assert!(unavailable(
            engine.torrent_statistics(&id(1)).await.map(drop)
        ));
        assert!(unavailable(engine.cache_statistics().await.map(drop)));
        assert!(unavailable(
            engine
                .open_reader(&id(1), FileIndex(0), ByteRange::new(0, 1).unwrap())
                .await
                .map(drop)
        ));
        assert!(unavailable(engine.remove(&id(1)).await));

        engine.set_readiness(Readiness::Ready);
        engine.remove(&id(1)).await.unwrap();
        assert!(matches!(
            engine.remove(&id(1)).await,
            Err(EngineError::UnknownTorrent)
        ));
        assert_eq!(engine.desired_state(&id(1)), None);
    }

    #[tokio::test]
    async fn injected_faults_fail_every_operation() {
        let engine = FakeEngine::with_catalog();
        let spec = catalog::single_small();
        engine
            .add(&id(1), &file(&spec), DesiredState::Started)
            .await
            .unwrap();
        engine.inject_fault(Some("disk on fire"));
        assert!(engine.readiness().is_ready(), "a failing engine is ready");
        let internal = |result: Result<(), EngineError>| match result {
            Err(EngineError::Internal(source)) => source.to_string() == "disk on fire",
            _ => false,
        };
        assert!(internal(engine.inspect(&file(&spec)).await.map(drop)));
        assert!(internal(engine.status(&id(1)).await.map(drop)));
        assert!(internal(engine.cache_statistics().await.map(drop)));
        assert!(internal(
            engine
                .open_reader(&id(1), FileIndex(0), ByteRange::new(0, 1).unwrap())
                .await
                .map(drop)
        ));
        assert!(internal(engine.remove(&id(1)).await));

        engine.inject_fault(None);
        engine.remove(&id(1)).await.unwrap();
    }

    #[tokio::test]
    async fn statistics_are_scripted() {
        let engine = FakeEngine::with_catalog();
        engine
            .add(
                &id(1),
                &file(&catalog::single_small()),
                DesiredState::Started,
            )
            .await
            .unwrap();
        assert_eq!(
            engine.torrent_statistics(&id(1)).await.unwrap(),
            TorrentStatistics::default()
        );
        let statistics = TorrentStatistics {
            downloaded_bytes: 536_870_912,
            download_rate_bytes_per_second: 4_194_304,
            peer_count: 12,
            completion_ratio: Ratio::new(0.5),
            ..TorrentStatistics::default()
        };
        engine.set_torrent_statistics(&id(1), statistics).unwrap();
        assert_eq!(engine.torrent_statistics(&id(1)).await.unwrap(), statistics);

        let cache = CacheStatistics {
            capacity_bytes: 268_435_456,
            usage_bytes: 201_326_592,
            pinned_bytes: 33_554_432,
            eviction_count: 140,
        };
        engine.set_cache_statistics(cache);
        assert_eq!(engine.cache_statistics().await.unwrap(), cache);
        assert!(matches!(
            engine.set_torrent_statistics(&id(2), statistics),
            Err(EngineError::UnknownTorrent)
        ));
    }

    #[tokio::test]
    async fn adding_a_taken_id_fails() {
        let engine = FakeEngine::with_catalog();
        let spec = catalog::single_small();
        engine
            .add(&id(1), &file(&spec), DesiredState::Started)
            .await
            .unwrap();
        assert!(matches!(
            engine
                .add(&id(1), &magnet(&spec), DesiredState::Started)
                .await,
            Err(EngineError::DuplicateTorrent)
        ));
    }
}
