// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! [`LibrqbitEngine`], the [`Engine`] facade over librqbit in memory storage
//! mode.
//!
//! Every torrent is added the way ADR 2's E3 found necessary: paused, with
//! its scheduler as the piece selector and memory storage over the piece
//! cache, attached to the cache, through its initial check, and only then
//! started. Torrents never upload, request at most a small window from each
//! peer, and drop peers that stop delivering after the piece wait bound
//! (fork patch P6).
//!
//! A magnet torrent's metadata is retrieved by a task the adapter owns:
//! librqbit adds the torrent only once its metadata has arrived. Pausing
//! cancels the task and starting runs it again, and a retrieval that fails,
//! for example without any known peer, is retried with a growing delay, so a
//! lack of peers never fails the torrent. Retrieved metadata whose pieces are
//! longer than the cache capacity fails the torrent instead, as ADR 5
//! decides.
//!
//! The cache capacity changes with settings. A torrent's piece length is
//! checked against the capacity in effect once its storage is registered
//! with the cache, which refuses to shrink below a registered piece length,
//! so a concurrent shrink and addition can never leave a torrent whose
//! pieces exceed the capacity.
//!
//! Readers share one [`Playback`]: readahead is split across every torrent's
//! readers, a stream that ends leaves its reader idle for the next stream of
//! the file, and active playback is published as readers come and go.
//! Pausing, failing, or deleting a torrent closes its scheduler, which
//! cancels its streams and releases its readers at once. A storage write
//! failure reaches the engine through a channel the storage reports to, so
//! the streams of a failed torrent end without waiting for the piece wait
//! bound.
//!
//! librqbit's error text can quote tracker URLs or internal state, so it is
//! logged redacted and never becomes a failure detail.

use std::collections::{HashMap, HashSet};
use std::io::SeekFrom;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::time::Duration;

use librqbit::api::TorrentIdOrHash;
use librqbit::{
    AddTorrent, AddTorrentOptions, ListenerMode, ListenerOptions, ManagedTorrent,
    PeerConnectionOptions, Session, SessionOptions, TorrentStatsState,
};
use tokio::io::AsyncSeekExt;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use torrplay_core::engine::{
    ByteRange, CacheStatistics, ConnectionOutcomes, Engine, EngineError, EngineStatistics,
    HashChecks, Inspection, PeerConnections, Ratio, Source, SourceRejection, TorrentStatistics,
    TorrentStatus,
};
use torrplay_core::id::{InfoHashV1, PreloadId, TorrentId};
use torrplay_core::lifecycle::{DesiredState, Failure, Readiness, StorageMode};
use torrplay_core::metadata::{FileIndex, ReportedMetadata};
use torrplay_core::preload::{PreloadRequest, PreloadStatus};
use torrplay_core::redact::redact;
use torrplay_core::settings::Settings;
use torrplay_core::telemetry::PieceWaitRecorder;

use crate::cache::TorrentKey;
use crate::cache::{CacheError, PieceCache};
use crate::disk::{self, DiskFiles, DiskRefusal, DiskStorageFactory, bitfield};
use crate::evictor::Evictor;
use crate::inspect;
use crate::playback::{
    DEFAULT_IDLE_READER_LIMIT, DEFAULT_IDLE_READER_LIMIT_PER_TORRENT, DEFAULT_IDLE_READER_TIMEOUT,
    DEFAULT_READAHEAD_RAMP_BYTES, Playback, PlaybackConfig, Reaper,
};
use crate::preload::{PreloadTimer, Preloads};
use crate::reader::RangeReader;
use crate::scheduler::{OpenError, TorrentScheduler};
use crate::storage::{CachedTorrent, Layout, MemoryStorageFactory};

const MIB: u64 = 1024 * 1024;

/// The default cache capacity, as ADR 5's milestone 4 defaults fix it.
pub const DEFAULT_CACHE_CAPACITY_BYTES: u64 = 256 * MIB;

/// The default piece wait bound.
pub const DEFAULT_PIECE_WAIT: Duration = Duration::from_secs(60);

/// The default request window, in blocks per peer.
pub const DEFAULT_REQUEST_WINDOW_BLOCKS: usize = 32;

/// The default number of peers one torrent connects to at a time, connecting
/// ones included, as Go TorrPlay's engine keeps about 50 established
/// connections per torrent. Each peer holds a socket and some memory, and a
/// stream rarely needs more than a few dozen good peers; librqbit's default
/// of 128 let a few busy torrents exhaust the 256 open files a macOS
/// terminal allows.
pub const DEFAULT_PEER_LIMIT: usize = 50;

/// The default interval between keep-alive messages to an idle peer. It is
/// shorter than any peer's idle timeout we know of: librqbit drops a peer
/// that sends nothing for 10 seconds, so a torrent without readers would
/// otherwise lose its peers and make its next stream wait for a reconnect.
pub const DEFAULT_PEER_KEEP_ALIVE_INTERVAL: Duration = Duration::from_secs(5);

/// The default time a peer may send nothing before it is dropped. It is
/// longer than the two-minute keep-alive interval BEP 3 suggests, which
/// librqbit and other clients use, so idle peers are kept.
pub const DEFAULT_PEER_READ_TIMEOUT: Duration = Duration::from_secs(150);

/// The default peer listen port, as ADR 2's network configuration fixes it.
pub const DEFAULT_PEER_PORT: u16 = 4240;

/// The first delay before a failed metadata retrieval runs again; it
/// doubles up to [`MAX_RETRIEVAL_DELAY`].
const FIRST_RETRIEVAL_DELAY: Duration = Duration::from_secs(1);

/// The longest delay between metadata retrieval attempts.
const MAX_RETRIEVAL_DELAY: Duration = Duration::from_secs(60);

/// The failure detail of a torrent librqbit stopped with an error.
const ENGINE_FAILURE: &str = "The torrent engine stopped the torrent after an error.";

/// How the engine reaches peers.
#[derive(Clone, Debug)]
pub struct NetworkConfig {
    /// The address the peer listener binds.
    pub listen: SocketAddr,
    /// Whether DHT finds peers when the engine starts;
    /// [`Engine::set_dht`] changes it. Its state is never persisted.
    pub dht: bool,
    /// The nodes the DHT bootstraps from, or `None` for librqbit's public
    /// routers.
    pub dht_bootstrap: Option<Vec<String>>,
    /// Whether trackers named by torrents and magnet URIs are contacted.
    pub trackers: bool,
    /// Peers every torrent connects to, besides those DHT and trackers
    /// find. Tests use it to reach local seeders.
    pub initial_peers: Vec<SocketAddr>,
}

impl NetworkConfig {
    /// ADR 2's network configuration: TCP on port 4240 on every interface,
    /// with DHT and trackers on.
    pub fn public() -> Self {
        Self {
            listen: (std::net::Ipv6Addr::UNSPECIFIED, DEFAULT_PEER_PORT).into(),
            dht: true,
            dht_bootstrap: None,
            trackers: true,
            initial_peers: Vec::new(),
        }
    }

    /// An ephemeral loopback port without DHT or trackers, reaching only
    /// `peers`, for tests that must never touch a public swarm. A DHT
    /// turned on later bootstraps from no node.
    pub fn isolated(peers: Vec<SocketAddr>) -> Self {
        Self {
            listen: (std::net::Ipv4Addr::LOCALHOST, 0).into(),
            dht: false,
            dht_bootstrap: Some(Vec::new()),
            trackers: false,
            initial_peers: peers,
        }
    }
}

/// The engine's configuration.
#[derive(Clone, Debug)]
pub struct EngineConfig {
    /// The global cache capacity.
    pub cache_capacity_bytes: u64,
    /// The readahead budget shared by every reader of every torrent.
    pub readahead_bytes: u64,
    /// The least a new reader, or one after a seek, reads ahead while it
    /// ramps up; zero gives every reader its share at once.
    pub readahead_ramp_bytes: u64,
    /// How long a read waits for missing piece data, and how long a peer
    /// may hold requests without delivering.
    pub piece_wait: Duration,
    /// How long a reader stays idle after its stream ends.
    pub idle_reader_timeout: Duration,
    /// The most idle readers one torrent keeps.
    pub idle_reader_limit_per_torrent: usize,
    /// The most idle readers all torrents keep together.
    pub idle_reader_limit: usize,
    /// The most chunk requests outstanding to one peer.
    pub request_window_blocks: usize,
    /// The most peers one torrent connects to at a time.
    pub peer_limit: usize,
    /// How often an idle peer is sent a keep-alive message.
    pub peer_keep_alive_interval: Duration,
    /// How long a peer may send nothing before it is dropped.
    pub peer_read_timeout: Duration,
    /// The session's folder. Memory storage writes nothing there.
    pub folder: PathBuf,
    /// Where torrents in disk storage keep their files, one directory per
    /// torrent named by its ID; ADR 9 gives TorrPlay every entry named like
    /// a torrent ID.
    pub download_dir: PathBuf,
    /// How the engine reaches peers.
    pub network: NetworkConfig,
}

impl EngineConfig {
    /// The milestone 4 defaults with `network`, using `folder` as the
    /// session's folder.
    pub fn new(folder: PathBuf, network: NetworkConfig) -> Self {
        Self {
            cache_capacity_bytes: DEFAULT_CACHE_CAPACITY_BYTES,
            readahead_bytes: DEFAULT_CACHE_CAPACITY_BYTES / 2,
            readahead_ramp_bytes: DEFAULT_READAHEAD_RAMP_BYTES,
            piece_wait: DEFAULT_PIECE_WAIT,
            idle_reader_timeout: DEFAULT_IDLE_READER_TIMEOUT,
            idle_reader_limit_per_torrent: DEFAULT_IDLE_READER_LIMIT_PER_TORRENT,
            idle_reader_limit: DEFAULT_IDLE_READER_LIMIT,
            request_window_blocks: DEFAULT_REQUEST_WINDOW_BLOCKS,
            peer_limit: DEFAULT_PEER_LIMIT,
            peer_keep_alive_interval: DEFAULT_PEER_KEEP_ALIVE_INTERVAL,
            peer_read_timeout: DEFAULT_PEER_READ_TIMEOUT,
            download_dir: folder.join("downloads"),
            folder,
            network,
        }
    }

    /// Sets the cache capacity and a readahead budget of half of it.
    pub fn with_cache_capacity(mut self, capacity_bytes: u64) -> Self {
        self.cache_capacity_bytes = capacity_bytes;
        self.readahead_bytes = capacity_bytes / 2;
        self
    }
}

/// What the running engine holds.
struct Running {
    session: Arc<Session>,
    cache: Arc<PieceCache>,
    factory: MemoryStorageFactory,
    disk: DiskStorageFactory,
    playback: Arc<Playback>,
    evictor: Mutex<Option<Evictor>>,
    reaper: Mutex<Option<Reaper>>,
    preloads: Arc<Preloads>,
    preload_timer: Mutex<Option<PreloadTimer>>,
    // Cancels the streams of torrents whose storage failed.
    failure_monitor: Mutex<Option<JoinHandle<()>>>,
}

enum Phase {
    Starting,
    Ready(Arc<Running>),
    Stopped,
}

/// A torrent in the session, with its pieces in the cache or on disk.
struct Live {
    handle: Arc<ManagedTorrent>,
    // Its pieces in the cache; `None` in disk storage.
    cached: Option<CachedTorrent>,
    // Its files; `None` in memory storage.
    files: Option<Arc<DiskFiles>>,
    scheduler: Arc<TorrentScheduler>,
}

struct TorrentState {
    desired: DesiredState,
    storage_mode: StorageMode,
    metadata: Option<Arc<ReportedMetadata>>,
    // A failure the adapter decided, such as pieces longer than the cache.
    failure: Option<Failure>,
    // Whether librqbit's error was logged, so it is logged once.
    engine_error_logged: bool,
    live: Option<Live>,
    retrieval: Option<JoinHandle<()>>,
    // Bytes fetched by earlier live states, which pausing ends.
    downloaded_before: u64,
}

/// One torrent the engine holds.
struct Torrent {
    info_hash: InfoHashV1,
    magnet_uri: Option<String>,
    // Its directory in the download directory, in disk storage.
    directory: PathBuf,
    // Serializes operations on the torrent; held across awaits.
    operations: tokio::sync::Mutex<()>,
    state: Mutex<TorrentState>,
}

impl Torrent {
    fn state(&self) -> MutexGuard<'_, TorrentState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// The [`Engine`] facade over librqbit in memory storage mode.
///
/// Torrents are added paused with their scheduler and attached to the piece
/// cache before they start; they never upload, and peers that stop
/// delivering are dropped after the piece wait bound. Metadata of magnet
/// URIs is retrieved by tasks the engine owns, retried without a deadline,
/// and torrents whose pieces are longer than the cache are refused or failed
/// as ADR 5 decides. librqbit's error text is logged redacted and never
/// becomes a failure detail.
pub struct LibrqbitEngine {
    // The engine itself, for the retrieval tasks it spawns.
    this: Weak<LibrqbitEngine>,
    config: EngineConfig,
    /// The configuration's values that settings change while the engine
    /// runs.
    tunables: Mutex<Tunables>,
    phase: Mutex<Phase>,
    torrents: Mutex<HashMap<TorrentId, Arc<Torrent>>>,
    // Whether disk torrents start from their resume bitfields; not after an
    // unclean shutdown, when a bitfield may be newer than its data.
    trust_bitfields: std::sync::atomic::AtomicBool,
    /// Whether the session runs a DHT, which `start` reads and `set_dht`
    /// changes; the lock serializes them, since both await the session.
    dht_enabled: tokio::sync::Mutex<bool>,
    /// Every reader's waits for missing pieces.
    piece_waits: Arc<PieceWaitRecorder>,
}

/// The values [`Engine::apply_settings`] changes.
#[derive(Clone, Copy, Debug)]
struct Tunables {
    piece_wait: Duration,
    idle_reader_timeout: Duration,
    idle_reader_limit_per_torrent: usize,
    idle_reader_limit: usize,
    peer_limit: usize,
    cache_capacity_bytes: u64,
    readahead_bytes: u64,
    background_downloads_enabled: bool,
}

/// Why a torrent did not join the session.
enum AddFailure {
    /// Its storage cannot hold it: in memory, pieces longer than the cache
    /// capacity in effect; on disk, files that do not map to file names.
    /// The detail names the cause.
    Unsupported(String),
    /// Its files need more free space than their filesystem has.
    InsufficientStorage {
        needed_bytes: u64,
        available_bytes: u64,
    },
    /// librqbit or the cache failed.
    Engine(anyhow::Error),
}

impl AddFailure {
    /// The failure for `error` from adding a torrent, which names a disk
    /// refusal when a magnet URI's storage opened only once its metadata
    /// arrived.
    fn from_engine(error: anyhow::Error) -> Self {
        match error.downcast_ref::<DiskRefusal>() {
            Some(DiskRefusal::Unmappable(paths)) => Self::Unsupported(paths.to_string()),
            Some(&DiskRefusal::InsufficientStorage {
                needed_bytes,
                available_bytes,
            }) => Self::InsufficientStorage {
                needed_bytes,
                available_bytes,
            },
            Some(DiskRefusal::Io(_)) | None => Self::Engine(error),
        }
    }

    /// The error for an addition the API requested.
    fn into_engine_error(self) -> EngineError {
        match self {
            Self::Unsupported(detail) => SourceRejection::Unsupported { detail }.into(),
            Self::InsufficientStorage {
                needed_bytes,
                available_bytes,
            } => EngineError::InsufficientStorage {
                needed_bytes,
                available_bytes,
            },
            Self::Engine(error) => internal(error),
        }
    }

    /// The failure detail of a torrent that cannot continue, for a magnet
    /// URI whose metadata showed its storage cannot hold it.
    fn detail(&self) -> Option<String> {
        match self {
            Self::Unsupported(detail) => Some(detail.clone()),
            Self::InsufficientStorage {
                needed_bytes,
                available_bytes,
            } => Some(format!(
                "The torrent's files need {needed_bytes} bytes, and the download directory has {available_bytes} free."
            )),
            Self::Engine(_) => None,
        }
    }
}

/// The resume bitfield a torrent file in disk storage starts from: the
/// recorded one, or none for a new directory, or `None` for a directory
/// whose pieces are unknown, which the full check finds.
fn initial_pieces(directory: &Path, piece_count: u32) -> anyhow::Result<Option<Vec<u8>>> {
    if let Some(bits) = bitfield::load(&bitfield::path_for(directory), piece_count)? {
        return Ok(Some(bits));
    }
    Ok((!directory.exists()).then(|| vec![0; bitfield::bitfield_bytes(piece_count)]))
}

/// Deletes a disk torrent's directory and resume bitfield. What it cannot
/// delete, such as a file another process holds open on Windows, is left
/// for the startup sweep.
async fn delete_disk_data(directory: PathBuf) {
    let deleted = tokio::task::spawn_blocking(move || {
        let bitfield = bitfield::path_for(&directory);
        for result in [
            std::fs::remove_dir_all(&directory),
            std::fs::remove_file(&bitfield),
        ] {
            match result {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
        }
        Ok(())
    })
    .await;
    match deleted {
        Ok(Ok(())) => {}
        Ok(Err(error)) => tracing::error!(%error, "a deleted torrent's data could not be deleted"),
        Err(error) => tracing::error!(%error, "deleting a torrent's data panicked"),
    }
}

/// The DHT's configuration: an ephemeral routing table, since torrents
/// persist in the store and the DHT bootstraps within seconds.
fn dht_config(network: &NetworkConfig) -> librqbit::DhtSessionConfig {
    librqbit::DhtSessionConfig {
        bootstrap_addrs: network.dht_bootstrap.clone(),
        persistence: None,
        ..Default::default()
    }
}

fn internal(error: impl Into<anyhow::Error>) -> EngineError {
    let error: anyhow::Error = error.into();
    EngineError::Internal(error.into())
}

impl LibrqbitEngine {
    /// Creates an engine that reports itself unavailable until
    /// [`start`](Self::start) has finished.
    pub fn new(config: EngineConfig) -> Arc<Self> {
        Arc::new_cyclic(|this| Self {
            this: this.clone(),
            tunables: Mutex::new(Tunables {
                piece_wait: config.piece_wait,
                idle_reader_timeout: config.idle_reader_timeout,
                idle_reader_limit_per_torrent: config.idle_reader_limit_per_torrent,
                idle_reader_limit: config.idle_reader_limit,
                peer_limit: config.peer_limit,
                cache_capacity_bytes: config.cache_capacity_bytes,
                readahead_bytes: config.readahead_bytes,
                background_downloads_enabled: true,
            }),
            phase: Mutex::new(Phase::Starting),
            torrents: Mutex::new(HashMap::new()),
            trust_bitfields: std::sync::atomic::AtomicBool::new(true),
            dht_enabled: tokio::sync::Mutex::new(config.network.dht),
            piece_waits: Arc::default(),
            config,
        })
    }

    /// The DHT's local address while it runs.
    pub fn dht_address(&self) -> Option<SocketAddr> {
        match &*self.lock_phase() {
            Phase::Ready(running) => running.session.get_dht().map(|dht| dht.listen_addr()),
            Phase::Starting | Phase::Stopped => None,
        }
    }

    /// Starts the session, the piece cache, and its evictor.
    pub async fn start(&self) -> anyhow::Result<()> {
        let network = &self.config.network;
        // Held until the session exists, so set_dht waits for it.
        let dht_enabled = self.dht_enabled.lock().await;
        let options = SessionOptions {
            dht: dht_enabled.then(|| dht_config(network)),
            disable_trackers: !network.trackers,
            disable_local_service_discovery: true,
            fastresume: false,
            persistence: None,
            ipv4_only: network.listen.is_ipv4(),
            listen: Some(ListenerOptions {
                mode: ListenerMode::TcpOnly,
                listen_addr: network.listen,
                enable_upnp_port_forwarding: false,
                ipv4_only: network.listen.is_ipv4(),
                ..Default::default()
            }),
            ..Default::default()
        };
        let session = Session::new_with_opts(self.config.folder.clone(), options).await?;
        // Held until the engine is ready, so settings applied meanwhile
        // reach the cache and playback built here.
        let tunables = self.tunables.lock().unwrap_or_else(PoisonError::into_inner);
        let cache = PieceCache::new(tunables.cache_capacity_bytes);
        let playback = Playback::new(PlaybackConfig {
            readahead_bytes: tunables.readahead_bytes,
            readahead_ramp_bytes: self.config.readahead_ramp_bytes,
            idle_reader_timeout: tunables.idle_reader_timeout,
            idle_reader_limit_per_torrent: tunables.idle_reader_limit_per_torrent,
            idle_reader_limit: tunables.idle_reader_limit,
        });
        playback.set_background_downloads(tunables.background_downloads_enabled);
        let preloads = Preloads::new(&playback, tunables.piece_wait);
        let (failures, failed) = mpsc::unbounded_channel();
        let running = Arc::new(Running {
            factory: MemoryStorageFactory::new(cache.clone()).with_failure_reports(failures),
            disk: DiskStorageFactory::new(),
            evictor: Mutex::new(Some(Evictor::spawn_with_fallback(
                cache.clone(),
                playback.clone(),
            ))),
            reaper: Mutex::new(Some(playback.spawn_reaper())),
            preload_timer: Mutex::new(Some(preloads.spawn_timer())),
            preloads,
            failure_monitor: Mutex::new(Some(tokio::spawn(monitor_failures(
                self.this.clone(),
                failed,
            )))),
            playback,
            cache,
            session,
        });
        *self.lock_phase() = Phase::Ready(running);
        drop(tunables);
        Ok(())
    }

    /// Stops the engine: it reports itself unavailable, cancels the streams
    /// still running and releases every reader, idle ones included, cancels
    /// metadata retrieval, stops the session, which closes every peer
    /// connection, and stops the engine's tasks, returning the first
    /// failure.
    pub async fn shutdown(&self) -> anyhow::Result<()> {
        let running = match std::mem::replace(&mut *self.lock_phase(), Phase::Stopped) {
            Phase::Ready(running) => running,
            Phase::Starting | Phase::Stopped => return Ok(()),
        };
        let torrents: Vec<_> = self.lock_torrents().values().cloned().collect();
        for torrent in torrents {
            let (retrieval, scheduler) = {
                let mut state = torrent.state();
                (
                    state.retrieval.take(),
                    state.live.as_ref().map(|live| live.scheduler.clone()),
                )
            };
            if let Some(scheduler) = scheduler {
                scheduler.close()?;
            }
            if let Some(retrieval) = retrieval {
                retrieval.abort();
                let _ = retrieval.await;
            }
        }
        running.playback.release_idle_readers();
        // Disk torrents' data and bitfields reach stable storage before the
        // application records a clean shutdown.
        let flushed = self.flush_disk_data().await;
        let monitor = take(&running.failure_monitor);
        if let Some(monitor) = monitor {
            monitor.abort();
            let _ = monitor.await;
        }
        running.session.stop().await;
        let reaper = take(&running.reaper).map(Reaper::shutdown);
        let evictor = take(&running.evictor).map(Evictor::shutdown);
        let preload_timer = take(&running.preload_timer).map(PreloadTimer::shutdown);
        if let Some(reaper) = reaper {
            reaper.await?;
        }
        if let Some(preload_timer) = preload_timer {
            preload_timer.await?;
        }
        if let Some(evictor) = evictor {
            evictor.await?;
        }
        flushed
    }

    /// Makes every disk torrent's data and resume bitfield durable.
    async fn flush_disk_data(&self) -> anyhow::Result<()> {
        let files: Vec<Arc<DiskFiles>> = self
            .lock_torrents()
            .values()
            .filter_map(|torrent| torrent.state().live.as_ref()?.files.clone())
            .collect();
        use anyhow::Context as _;
        tokio::task::spawn_blocking(move || {
            for files in files {
                files.flush().context("flushing a disk torrent's data")?;
            }
            Ok(())
        })
        .await
        .context("flushing disk data panicked")?
    }

    /// Makes disk torrents added from now on check their data in full
    /// instead of starting from their resume bitfields, as after an unclean
    /// shutdown, which can leave a bitfield newer than the data it records.
    pub fn distrust_resume_bitfields(&self) {
        self.trust_bitfields
            .store(false, std::sync::atomic::Ordering::Release);
    }

    /// Deletes every entry of the download directory named like a torrent
    /// ID, with or without the resume bitfield's extension, except those of
    /// `keep`, the torrents that use disk storage or move to or from it.
    /// Other entries stay. Returns the paths deleted. Run it before the
    /// engine adds any torrent.
    pub fn sweep_download_dir(&self, keep: &HashSet<TorrentId>) -> std::io::Result<Vec<PathBuf>> {
        let entries = match std::fs::read_dir(&self.config.download_dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error),
        };
        let mut deleted = Vec::new();
        for entry in entries {
            let entry = entry?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            let stem = name
                .strip_suffix(&format!(".{}", bitfield::EXTENSION))
                .unwrap_or(name);
            let Some(id) = TorrentId::from_stored(stem) else {
                continue;
            };
            if keep.contains(&id) {
                continue;
            }
            let path = entry.path();
            if entry.file_type()?.is_dir() {
                std::fs::remove_dir_all(&path)?;
            } else {
                std::fs::remove_file(&path)?;
            }
            deleted.push(path);
        }
        Ok(deleted)
    }

    /// Cancels every stream in progress and releases every reader, idle
    /// ones included, as shutdown does once its drain period ends. Torrents
    /// open no readers afterward.
    pub fn cancel_streams(&self) -> anyhow::Result<()> {
        let torrents: Vec<_> = self.lock_torrents().values().cloned().collect();
        for torrent in torrents {
            let scheduler = torrent
                .state()
                .live
                .as_ref()
                .map(|live| live.scheduler.clone());
            if let Some(scheduler) = scheduler {
                scheduler.close()?;
            }
        }
        if let Ok(running) = self.running() {
            running.playback.release_idle_readers();
        }
        Ok(())
    }

    /// The readers across torrents, once started: their counts and the
    /// active playback signal background work waits on.
    pub fn playback(&self) -> Option<Arc<Playback>> {
        match &*self.lock_phase() {
            Phase::Ready(running) => Some(running.playback.clone()),
            Phase::Starting | Phase::Stopped => None,
        }
    }

    /// Makes every later storage write of the torrent with ID `id` fail, as
    /// LIF-06's injected storage failure does. It takes the ID as text, as
    /// tests read it from API responses.
    #[cfg(feature = "faults")]
    pub fn fail_storage_writes(&self, id: &str) -> Result<(), EngineError> {
        let torrent = self
            .lock_torrents()
            .get(id)
            .cloned()
            .ok_or(EngineError::UnknownTorrent)?;
        let state = torrent.state();
        let cached = state
            .live
            .as_ref()
            .and_then(|live| live.cached.as_ref())
            .ok_or(EngineError::MetadataUnavailable)?;
        cached.fail_writes();
        Ok(())
    }

    /// The address the session's peer listener bound, once started.
    pub fn listen_addr(&self) -> Option<SocketAddr> {
        match &*self.lock_phase() {
            Phase::Ready(running) => running.session.listen_addr(),
            Phase::Starting | Phase::Stopped => None,
        }
    }

    /// The download directory, where torrents in disk storage keep their
    /// files.
    pub fn download_dir(&self) -> &Path {
        &self.config.download_dir
    }

    /// The piece cache, once started, for tests that audit its accounting.
    pub fn cache(&self) -> Option<Arc<PieceCache>> {
        match &*self.lock_phase() {
            Phase::Ready(running) => Some(running.cache.clone()),
            Phase::Starting | Phase::Stopped => None,
        }
    }

    fn tunables(&self) -> Tunables {
        *self.tunables.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn lock_phase(&self) -> MutexGuard<'_, Phase> {
        self.phase.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn lock_torrents(&self) -> MutexGuard<'_, HashMap<TorrentId, Arc<Torrent>>> {
        self.torrents.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn running(&self) -> Result<Arc<Running>, EngineError> {
        match &*self.lock_phase() {
            Phase::Ready(running) => Ok(running.clone()),
            Phase::Starting | Phase::Stopped => Err(EngineError::Unavailable { retry_after: None }),
        }
    }

    /// Checks that the torrent's metadata lists a file at `file`.
    fn check_file(&self, id: &TorrentId, file: FileIndex) -> Result<(), EngineError> {
        let torrent = self.torrent(id)?;
        let state = torrent.state();
        let metadata = state
            .metadata
            .as_ref()
            .ok_or(EngineError::MetadataUnavailable)?;
        metadata
            .files
            .get(file.0)
            .map(drop)
            .ok_or(EngineError::UnknownFile)
    }

    fn torrent(&self, id: &TorrentId) -> Result<Arc<Torrent>, EngineError> {
        self.lock_torrents()
            .get(id)
            .cloned()
            .ok_or(EngineError::UnknownTorrent)
    }

    /// The cache capacity in effect.
    fn capacity_bytes(&self) -> u64 {
        let cache = match &*self.lock_phase() {
            Phase::Ready(running) => Some(running.cache.clone()),
            Phase::Starting | Phase::Stopped => None,
        };
        cache
            .and_then(|cache| cache.capacity_bytes().ok())
            .unwrap_or_else(|| self.tunables().cache_capacity_bytes)
    }

    fn too_large(&self, piece_length: u32) -> Option<String> {
        let capacity = self.capacity_bytes();
        (u64::from(piece_length) > capacity).then(|| {
            format!(
                "The torrent's {piece_length}-byte pieces exceed the {capacity}-byte cache capacity."
            )
        })
    }

    /// Checks that a torrent file's torrent can use `storage_mode`, in
    /// `directory` for disk storage.
    fn check_storage(
        &self,
        inspected: &inspect::InspectedFile,
        storage_mode: StorageMode,
        directory: &Path,
    ) -> Result<(), EngineError> {
        match storage_mode {
            StorageMode::Memory => match self.too_large(inspected.piece_length) {
                Some(detail) => Err(SourceRejection::Unsupported { detail }.into()),
                None => Ok(()),
            },
            StorageMode::Disk => match disk::check(directory, &inspected.metadata) {
                Ok(()) => Ok(()),
                Err(DiskRefusal::Unmappable(paths)) => Err(SourceRejection::Unsupported {
                    detail: paths.to_string(),
                }
                .into()),
                Err(DiskRefusal::InsufficientStorage {
                    needed_bytes,
                    available_bytes,
                }) => Err(EngineError::InsufficientStorage {
                    needed_bytes,
                    available_bytes,
                }),
                Err(DiskRefusal::Io(error)) => Err(internal(error)),
            },
        }
    }

    /// The options that add a torrent with `scheduler`, in memory storage,
    /// or in disk storage starting from `initial_pieces`.
    fn add_options(
        &self,
        running: &Running,
        scheduler: &Arc<TorrentScheduler>,
        storage_mode: StorageMode,
        initial_pieces: Option<Vec<u8>>,
    ) -> AddTorrentOptions {
        let tunables = self.tunables();
        let disk = storage_mode == StorageMode::Disk;
        AddTorrentOptions {
            storage_factory: Some(if disk {
                running.disk.clone().into_boxed()
            } else {
                running.factory.clone().into_boxed()
            }),
            piece_selector: Some(scheduler.clone()),
            // ADR 3: only disk torrents upload, since memory evicts.
            disable_upload: !disk,
            initial_pieces,
            paused: true,
            overwrite: true,
            initial_peers: Some(self.config.network.initial_peers.clone()),
            peer_limit: Some(tunables.peer_limit),
            peer_opts: Some(PeerConnectionOptions {
                max_request_window: Some(self.config.request_window_blocks),
                request_stall_timeout: Some(tunables.piece_wait),
                keep_alive_interval: Some(self.config.peer_keep_alive_interval),
                read_write_timeout: Some(self.config.peer_read_timeout),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    /// Adds `add` to the session, attaches it to the cache, waits for its
    /// initial check, and starts it if `desired` says so, unless its pieces
    /// are longer than the cache capacity in effect once the cache has
    /// registered them. A magnet URI waits for its metadata first.
    async fn add_to_session(
        &self,
        running: &Running,
        torrent: &Torrent,
        add: AddTorrent<'_>,
        desired: DesiredState,
        initial_pieces: Option<Vec<u8>>,
    ) -> Result<Live, AddFailure> {
        self.add_with_scheduler(running, torrent, add, desired, initial_pieces, None)
            .await
    }

    /// Adds a torrent as [`add_to_session`](Self::add_to_session) does, with
    /// `scheduler` and its readers when a transition keeps them.
    async fn add_with_scheduler(
        &self,
        running: &Running,
        torrent: &Torrent,
        add: AddTorrent<'_>,
        desired: DesiredState,
        initial_pieces: Option<Vec<u8>>,
        scheduler: Option<Arc<TorrentScheduler>>,
    ) -> Result<Live, AddFailure> {
        let storage_mode = torrent.state().storage_mode;
        let live = self
            .join_session(
                running,
                torrent,
                add,
                storage_mode,
                initial_pieces,
                scheduler,
            )
            .await
            .map_err(AddFailure::from_engine)?;
        let piece_length = live
            .handle
            .with_metadata(|metadata| metadata.info.info().piece_length)
            .map_err(AddFailure::Engine)?;
        if storage_mode == StorageMode::Memory
            && let Some(detail) = self.too_large(piece_length)
        {
            self.delete_live(running, live)
                .await
                .map_err(AddFailure::Engine)?;
            return Err(AddFailure::Unsupported(detail));
        }
        let started = match desired {
            DesiredState::Started => running.session.unpause(&live.handle).await,
            DesiredState::Paused => live.scheduler.close().and_then(|()| match &live.cached {
                Some(cached) => cached.set_started(false).map_err(Into::into),
                None => Ok(()),
            }),
        };
        started.map_err(AddFailure::Engine)?;
        Ok(live)
    }

    /// Adds `add` to the session paused, in `storage_mode`, binds its
    /// scheduler, and waits for its initial check. A disk torrent's resume
    /// bitfield then records what the check found.
    async fn join_session(
        &self,
        running: &Running,
        torrent: &Torrent,
        add: AddTorrent<'_>,
        storage_mode: StorageMode,
        initial_pieces: Option<Vec<u8>>,
        scheduler: Option<Arc<TorrentScheduler>>,
    ) -> anyhow::Result<Live> {
        let info_hash = librqbit_core::hash_id::Id20::new(*torrent.info_hash.as_bytes());
        if storage_mode == StorageMode::Disk {
            running
                .disk
                .expect(info_hash, torrent.directory.clone(), initial_pieces.clone())?;
        }
        let scheduler = scheduler.unwrap_or_else(|| TorrentScheduler::new(&running.playback));
        let options = self.add_options(running, &scheduler, storage_mode, initial_pieces);
        let handle = running
            .session
            .add_torrent(add, Some(options))
            .await?
            .into_handle()
            .ok_or_else(|| anyhow::anyhow!("the session did not add the torrent"))?;
        let (cached, files): (_, Option<Arc<DiskFiles>>) = match storage_mode {
            StorageMode::Memory => {
                let cached = running.factory.attach(&handle)?;
                scheduler.bind(&cached, &handle)?;
                (Some(cached), None)
            }
            StorageMode::Disk => {
                let files = running
                    .disk
                    .take_opened(info_hash)?
                    .ok_or_else(|| anyhow::anyhow!("the torrent's disk storage did not open"))?;
                let layout = handle.with_metadata(|metadata| Layout::of(metadata))?;
                scheduler.bind_disk(layout, &handle)?;
                (None, Some(files))
            }
        };
        handle.wait_until_initialized().await?;
        if let Some(files) = &files {
            files.bitfield().replace(&handle.have_pieces()?)?;
            files.arm_failures();
        }
        Ok(Live {
            handle,
            cached,
            files,
            scheduler,
        })
    }

    /// Spawns metadata retrieval for a magnet torrent.
    fn spawn_retrieval(&self, torrent: &Arc<Torrent>) {
        let (Some(magnet_uri), Some(engine)) = (torrent.magnet_uri.clone(), self.this.upgrade())
        else {
            return;
        };
        let task_torrent = Arc::clone(torrent);
        let task = tokio::spawn(async move {
            engine.retrieve(&task_torrent, &magnet_uri).await;
        });
        torrent.state().retrieval = Some(task);
    }

    async fn retrieve(&self, torrent: &Torrent, magnet_uri: &str) {
        let mut delay = FIRST_RETRIEVAL_DELAY;
        loop {
            let Ok(running) = self.running() else {
                return;
            };
            let desired = torrent.state().desired;
            match self
                .add_to_session(
                    &running,
                    torrent,
                    AddTorrent::from_url(magnet_uri),
                    desired,
                    None,
                )
                .await
            {
                Ok(live) => {
                    self.retrieved(&running, torrent, live).await;
                    return;
                }
                Err(failure) if failure.detail().is_some() => {
                    // Peers would send the same metadata again.
                    torrent.state().failure = failure.detail().map(|detail| Failure { detail });
                    return;
                }
                Err(AddFailure::Unsupported(_) | AddFailure::InsufficientStorage { .. }) => return,
                Err(AddFailure::Engine(error)) => {
                    tracing::debug!(
                        error = %redact(&format!("{error:#}")),
                        "metadata retrieval failed; retrying"
                    );
                    tokio::time::sleep(delay).await;
                    delay = (delay * 2).min(MAX_RETRIEVAL_DELAY);
                }
            }
        }
    }

    /// Records a magnet torrent's retrieved metadata.
    async fn retrieved(&self, running: &Running, torrent: &Torrent, live: Live) {
        let checked = live
            .handle
            .with_metadata(|metadata| inspect::reported_metadata(&metadata.info));
        let metadata = match checked {
            Ok(metadata) => metadata,
            Err(error) => {
                tracing::error!(error = %redact(&format!("{error:#}")), "retrieved metadata is unreadable");
                let _ = self.delete_live(running, live).await;
                torrent.state().failure = Some(Failure {
                    detail: ENGINE_FAILURE.to_owned(),
                });
                return;
            }
        };
        let mut state = torrent.state();
        state.metadata = Some(Arc::new(metadata));
        state.live = Some(live);
    }

    async fn delete_live(&self, running: &Running, live: Live) -> anyhow::Result<()> {
        live.scheduler.close()?;
        self.leave_session(running, live).await
    }

    /// Deletes a live torrent from the session, releasing its cache usage
    /// and closing its files, but keeps its scheduler and readers, for a
    /// transition that adds it again.
    async fn leave_session(&self, running: &Running, live: Live) -> anyhow::Result<()> {
        let result = running
            .session
            .delete(TorrentIdOrHash::Id(live.handle.id()), false)
            .await;
        if let Some(cached) = &live.cached {
            cached.release()?;
        }
        result
    }

    /// Moves a live torrent into disk storage: copies the cache's verified
    /// pieces of it into its files, then adds it again with them, its
    /// readers waiting meanwhile. A failure returns it to memory storage,
    /// with its directory deleted.
    async fn move_to_disk(
        &self,
        running: &Running,
        torrent: &Torrent,
        live: Live,
        metadata: &ReportedMetadata,
    ) -> Result<(), EngineError> {
        let failed = |detail: String| EngineError::TransitionFailed { detail };
        let (bytes, layout, pieces) = live
            .handle
            .with_metadata(|metadata| {
                (
                    metadata.torrent_bytes.to_vec(),
                    Layout::of(metadata),
                    metadata.lengths().total_pieces(),
                )
            })
            .map_err(internal)?;
        let directory = torrent.directory.clone();
        let copied = match disk::check(&directory, metadata) {
            Err(refusal) => Err(anyhow::Error::new(refusal)),
            Ok(()) => {
                let cached = live
                    .cached
                    .as_ref()
                    .map(|cached| (cached.cache.clone(), cached.key));
                let metadata = metadata.clone();
                let directory = directory.clone();
                let layout = layout.clone();
                tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<u8>> {
                    let zeros = vec![0; bitfield::bitfield_bytes(pieces)];
                    let files = DiskFiles::open(&directory, &metadata, &zeros)?;
                    if let Some((cache, key)) = cached {
                        for piece in cache.verified_pieces(key)? {
                            let Some(length) = layout.piece_length(piece) else {
                                continue;
                            };
                            let mut buf = vec![0; length];
                            // A piece evicted meanwhile, even one requested
                            // again since, is downloaded again.
                            if cache.read_verified(key, piece, &mut buf).is_ok() {
                                files.write_piece(&layout, piece, &buf)?;
                            }
                        }
                    }
                    files.flush()?;
                    Ok(files.bitfield().bits())
                })
                .await
                .map_err(anyhow::Error::new)
                .and_then(|copied| copied)
            }
        };
        let bits = match copied {
            Ok(bits) => bits,
            Err(error) => {
                torrent.state().live = Some(live);
                delete_disk_data(directory).await;
                return Err(failed(format!(
                    "The torrent's files could not be prepared: {}",
                    redact(&format!("{error:#}"))
                )));
            }
        };
        self.swap(
            running,
            torrent,
            live,
            bytes,
            StorageMode::Disk,
            Some(bits),
            Some(directory),
        )
        .await
    }

    /// Moves a live torrent into memory storage: seeds the cache with the
    /// pieces its readers need first, then adds it again, its readers
    /// waiting meanwhile, and deletes its directory once it is in memory. A
    /// failure returns it to disk storage, its files kept.
    async fn move_to_memory(
        &self,
        running: &Running,
        torrent: &Torrent,
        live: Live,
    ) -> Result<(), EngineError> {
        let (bytes, layout, piece_length, info_hash) = live
            .handle
            .with_metadata(|metadata| {
                (
                    metadata.torrent_bytes.to_vec(),
                    Layout::of(metadata),
                    metadata.info.info().piece_length,
                    live.handle.info_hash(),
                )
            })
            .map_err(internal)?;
        if let Some(detail) = self.too_large(piece_length) {
            torrent.state().live = Some(live);
            return Err(EngineError::TransitionFailed { detail });
        }
        let Some(files) = live.files.clone() else {
            torrent.state().live = Some(live);
            return Err(internal(anyhow::anyhow!("a disk torrent has no files")));
        };
        // The pieces the readers need first, as far as the readahead budget
        // goes and the files hold them.
        let have = files.bitfield().bits();
        let budget = running.playback.readahead_bytes();
        let priorities = live.scheduler.priorities().map_err(internal)?;
        let seeded = {
            let files = files.clone();
            let layout = layout.clone();
            tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<(u32, Vec<u8>)>> {
                let mut seeds = Vec::new();
                let mut bytes = 0;
                for piece in priorities {
                    let held = have
                        .get((piece / 8) as usize)
                        .is_some_and(|byte| byte & (0x80 >> (piece % 8)) != 0);
                    let Some(length) = layout.piece_length(piece) else {
                        continue;
                    };
                    if !held || bytes + length as u64 > budget {
                        continue;
                    }
                    let mut buf = vec![0; length];
                    files.read_piece(&layout, piece, &mut buf)?;
                    bytes += length as u64;
                    seeds.push((piece, buf));
                }
                Ok(seeds)
            })
            .await
            .map_err(anyhow::Error::new)
            .and_then(|seeds| seeds)
        };
        let seeds = match seeded {
            Ok(seeds) => seeds,
            Err(error) => {
                torrent.state().live = Some(live);
                return Err(EngineError::TransitionFailed {
                    detail: format!(
                        "The torrent's files could not be read: {}",
                        redact(&format!("{error:#}"))
                    ),
                });
            }
        };
        let mut bits = vec![0; files.bitfield().bits().len()];
        for (piece, _) in &seeds {
            bits[(*piece / 8) as usize] |= 0x80 >> (piece % 8);
        }
        running.factory.seed(info_hash, seeds).map_err(internal)?;
        drop(files);
        let directory = torrent.directory.clone();
        let result = self
            .swap(
                running,
                torrent,
                live,
                bytes,
                StorageMode::Memory,
                Some(bits),
                None,
            )
            .await;
        // A failed move to memory keeps the torrent's files, and drops the
        // seeds a failed addition left unused.
        match &result {
            Ok(()) => delete_disk_data(directory).await,
            Err(_) => running
                .factory
                .seed(info_hash, Vec::new())
                .map_err(internal)?,
        }
        result
    }

    /// Deletes a live torrent from the session and adds it again from
    /// `bytes` in `storage_mode`, starting from `initial_pieces`, with its
    /// scheduler, whose readers wait meanwhile and then continue. If that
    /// fails, it adds the torrent again in the storage mode it had, with the
    /// pieces that storage records, and deletes `discard`, the directory a
    /// failed move to disk prepared. If that fails too, the torrent fails.
    #[allow(clippy::too_many_arguments)]
    async fn swap(
        &self,
        running: &Running,
        torrent: &Torrent,
        live: Live,
        bytes: Vec<u8>,
        storage_mode: StorageMode,
        initial_pieces: Option<Vec<u8>>,
        discard: Option<PathBuf>,
    ) -> Result<(), EngineError> {
        let scheduler = live.scheduler.clone();
        let previous = torrent.state().storage_mode;
        let previous_bits = live.files.as_ref().map(|files| files.bitfield().bits());
        let desired = torrent.state().desired;
        scheduler.begin_swap().map_err(internal)?;
        let result = async {
            self.leave_session(running, live)
                .await
                .map_err(|error| redact(&format!("{error:#}")))?;
            torrent.state().storage_mode = storage_mode;
            match self
                .add_with_scheduler(
                    running,
                    torrent,
                    AddTorrent::from_bytes(bytes.clone()),
                    desired,
                    initial_pieces,
                    Some(scheduler.clone()),
                )
                .await
            {
                Ok(added) => {
                    torrent.state().live = Some(added);
                    Ok(())
                }
                Err(failure) => {
                    let detail = failure
                        .detail()
                        .unwrap_or_else(|| "The engine could not add the torrent again.".into());
                    torrent.state().storage_mode = previous;
                    let restored = self
                        .add_with_scheduler(
                            running,
                            torrent,
                            AddTorrent::from_bytes(bytes.clone()),
                            desired,
                            previous_bits,
                            Some(scheduler.clone()),
                        )
                        .await;
                    match restored {
                        Ok(added) => torrent.state().live = Some(added),
                        Err(_) => {
                            let _ = scheduler.close();
                            torrent.state().failure = Some(Failure {
                                detail: format!(
                                    "The torrent could not return to {} storage.",
                                    previous.as_str()
                                ),
                            });
                        }
                    }
                    if let Some(directory) = discard {
                        delete_disk_data(directory).await;
                    }
                    Err(detail)
                }
            }
        }
        .await;
        scheduler.end_swap().map_err(internal)?;
        result.map_err(|detail| EngineError::TransitionFailed { detail })
    }

    /// Cancels a torrent's metadata retrieval and removes anything it added
    /// to the session but did not record.
    async fn cancel_retrieval(&self, running: &Running, torrent: &Torrent) {
        let retrieval = torrent.state().retrieval.take();
        let Some(retrieval) = retrieval else {
            return;
        };
        retrieval.abort();
        let _ = retrieval.await;
        if torrent.state().live.is_none()
            && let Some(orphan) =
                running
                    .session
                    .get(TorrentIdOrHash::Hash(librqbit_core::hash_id::Id20::new(
                        *torrent.info_hash.as_bytes(),
                    )))
        {
            let _ = running
                .session
                .delete(TorrentIdOrHash::Id(orphan.id()), false)
                .await;
        }
    }

    /// Reports a live torrent's storage failure or librqbit's error, which
    /// is logged once.
    fn engine_failure(state: &mut TorrentState) -> Option<Failure> {
        let live = state.live.as_ref()?;
        let failure = Failure {
            detail: ENGINE_FAILURE.to_owned(),
        };
        if live.cached.as_ref().is_some_and(CachedTorrent::has_failed)
            || live.files.as_ref().is_some_and(|files| files.has_failed())
        {
            return Some(failure);
        }
        let stats = live.handle.stats();
        if !matches!(stats.state, TorrentStatsState::Error) {
            return None;
        }
        if !state.engine_error_logged {
            state.engine_error_logged = true;
            tracing::error!(
                error = %redact(stats.error.as_deref().unwrap_or("unknown")),
                "the torrent engine stopped a torrent"
            );
        }
        Some(failure)
    }

    /// Cancels the streams of the torrent whose storage, cache key `key`,
    /// failed a write.
    fn storage_failed(&self, key: TorrentKey) {
        let torrents: Vec<_> = self
            .lock_torrents()
            .iter()
            .map(|(id, torrent)| (id.clone(), torrent.clone()))
            .collect();
        let found = torrents.iter().find_map(|(id, torrent)| {
            let state = torrent.state();
            let live = state.live.as_ref()?;
            (live.cached.as_ref()?.key == key).then(|| (id.clone(), live.scheduler.clone()))
        });
        if let Some((id, scheduler)) = found {
            if let Ok(running) = self.running() {
                running
                    .preloads
                    .fail_torrent(&id, "The torrent's storage failed a write.");
            }
            tracing::error!("a torrent's storage failed a write; its streams are canceled");
            if let Err(error) = scheduler.close() {
                tracing::error!(error = %redact(&format!("{error:#}")), "canceling the streams failed");
            }
        }
    }

    /// Restarts a live torrent that librqbit stopped with an error by adding
    /// it again from its metainfo, since starting it in place would create
    /// storage the cache has not attached.
    async fn restart_failed(
        &self,
        running: &Running,
        torrent: &Torrent,
    ) -> Result<(), EngineError> {
        let live = torrent.state().live.take();
        let Some(live) = live else {
            return Ok(());
        };
        let (bytes, piece_length) = live
            .handle
            .with_metadata(|metadata| {
                (
                    metadata.torrent_bytes.clone(),
                    metadata.info.info().piece_length,
                )
            })
            .map_err(internal)?;
        // A failed torrent's storage left the cache, so the capacity may
        // have shrunk below its pieces; it stays failed until it grows.
        let storage_mode = torrent.state().storage_mode;
        if storage_mode == StorageMode::Memory
            && let Some(detail) = self.too_large(piece_length)
        {
            let mut state = torrent.state();
            state.live = Some(live);
            state.failure = Some(Failure { detail });
            return Ok(());
        }
        self.delete_live(running, live).await.map_err(internal)?;
        // A disk torrent checks its data in full, since a failed read or
        // write may have left it unlike its bitfield.
        let live = self
            .add_to_session(
                running,
                torrent,
                AddTorrent::from_bytes(bytes),
                DesiredState::Started,
                None,
            )
            .await
            .map_err(AddFailure::into_engine_error)?;
        let mut state = torrent.state();
        state.live = Some(live);
        state.engine_error_logged = false;
        Ok(())
    }
}

impl Engine for LibrqbitEngine {
    type Reader = RangeReader;

    fn apply_settings(&self, settings: &Settings) -> Result<(), EngineError> {
        let (tunables, running, resized) = {
            let mut current = self.tunables.lock().unwrap_or_else(PoisonError::into_inner);
            let running = match &*self.lock_phase() {
                Phase::Ready(running) => Some(Arc::clone(running)),
                Phase::Starting | Phase::Stopped => None,
            };
            // An unchanged capacity keeps a readahead budget the
            // configuration set apart from it.
            let resized = settings.cache_capacity_bytes != current.cache_capacity_bytes;
            if resized && let Some(running) = &running {
                running
                    .cache
                    .set_capacity(settings.cache_capacity_bytes)
                    .map_err(|error| match error {
                        CacheError::CapacityBelowPieceLength {
                            piece_length_bytes, ..
                        } => EngineError::CapacityBelowPieceLength { piece_length_bytes },
                        error => internal(error),
                    })?;
            }
            *current = Tunables {
                piece_wait: settings.piece_wait,
                idle_reader_timeout: settings.idle_reader_timeout,
                idle_reader_limit_per_torrent: settings.idle_reader_limit_per_torrent,
                idle_reader_limit: settings.idle_reader_limit,
                peer_limit: settings.peer_limit,
                cache_capacity_bytes: settings.cache_capacity_bytes,
                readahead_bytes: if resized {
                    settings.cache_capacity_bytes / 2
                } else {
                    current.readahead_bytes
                },
                background_downloads_enabled: settings.background_downloads_enabled,
            };
            (*current, running, resized)
        };
        // A starting engine builds its cache and playback from the
        // tunables. Replanning takes scheduler locks, so it runs unlocked.
        if let Some(running) = running {
            running.playback.set_idle_limits(
                tunables.idle_reader_timeout,
                tunables.idle_reader_limit_per_torrent,
                tunables.idle_reader_limit,
            );
            if resized {
                running
                    .playback
                    .set_readahead_bytes(tunables.readahead_bytes);
            }
            running
                .playback
                .set_background_downloads(tunables.background_downloads_enabled);
            running.preloads.set_piece_wait(tunables.piece_wait);
        }
        Ok(())
    }

    fn readiness(&self) -> Readiness {
        match &*self.lock_phase() {
            Phase::Ready(_) => Readiness::Ready,
            Phase::Starting | Phase::Stopped => Readiness::Unavailable { retry_after: None },
        }
    }

    async fn set_dht(&self, enabled: bool) -> Result<(), EngineError> {
        let mut current = self.dht_enabled.lock().await;
        if *current == enabled {
            return Ok(());
        }
        let running = match &*self.lock_phase() {
            Phase::Ready(running) => Some(Arc::clone(running)),
            Phase::Starting | Phase::Stopped => None,
        };
        // Before the engine starts, start reads the switch.
        if let Some(running) = running {
            running
                .session
                .set_dht(enabled.then(|| dht_config(&self.config.network)))
                .await
                .map_err(internal)?;
        }
        *current = enabled;
        Ok(())
    }

    async fn inspect(&self, source: &Source) -> Result<Inspection, EngineError> {
        self.running()?;
        match source {
            Source::TorrentFile(bytes) => {
                let inspected = inspect::torrent_file(bytes)?;
                Ok(Inspection {
                    info_hash: inspected.info_hash,
                    metadata: Some(Arc::new(inspected.metadata)),
                    trackers: inspected.trackers,
                })
            }
            Source::MagnetUri(uri) => Ok(Inspection {
                info_hash: inspect::magnet_uri(uri)?,
                metadata: None,
                trackers: inspect::magnet_trackers(uri),
            }),
        }
    }

    async fn add_in(
        &self,
        id: &TorrentId,
        source: &Source,
        desired_state: DesiredState,
        storage_mode: StorageMode,
    ) -> Result<(), EngineError> {
        let running = self.running()?;
        let directory = self.config.download_dir.join(id.as_str());
        let (info_hash, metadata, magnet_uri, initial) = match source {
            Source::TorrentFile(bytes) => {
                let inspected = inspect::torrent_file(bytes)?;
                self.check_storage(&inspected, storage_mode, &directory)?;
                let initial = match storage_mode {
                    StorageMode::Memory => None,
                    StorageMode::Disk
                        if self
                            .trust_bitfields
                            .load(std::sync::atomic::Ordering::Acquire) =>
                    {
                        initial_pieces(&directory, inspected.piece_count).map_err(internal)?
                    }
                    // A new directory holds nothing; an existing one is
                    // checked in full.
                    StorageMode::Disk => (!directory.exists())
                        .then(|| vec![0; bitfield::bitfield_bytes(inspected.piece_count)]),
                };
                (
                    inspected.info_hash,
                    Some(Arc::new(inspected.metadata)),
                    None,
                    initial,
                )
            }
            Source::MagnetUri(uri) => (inspect::magnet_uri(uri)?, None, Some(uri.clone()), None),
        };
        let torrent = Arc::new(Torrent {
            info_hash,
            magnet_uri,
            directory,
            operations: tokio::sync::Mutex::new(()),
            state: Mutex::new(TorrentState {
                desired: desired_state,
                storage_mode,
                metadata,
                failure: None,
                engine_error_logged: false,
                live: None,
                retrieval: None,
                downloaded_before: 0,
            }),
        });
        {
            let mut torrents = self.lock_torrents();
            if torrents.contains_key(id) {
                return Err(EngineError::DuplicateTorrent);
            }
            torrents.insert(id.clone(), Arc::clone(&torrent));
        }
        let _operations = torrent.operations.lock().await;
        match source {
            Source::TorrentFile(bytes) => {
                match self
                    .add_to_session(
                        &running,
                        &torrent,
                        AddTorrent::from_bytes(bytes.clone()),
                        desired_state,
                        initial,
                    )
                    .await
                {
                    Ok(live) => torrent.state().live = Some(live),
                    Err(failure) => {
                        self.lock_torrents().remove(id);
                        return Err(failure.into_engine_error());
                    }
                }
            }
            Source::MagnetUri(_) => {
                if desired_state == DesiredState::Started {
                    self.spawn_retrieval(&torrent);
                }
            }
        }
        Ok(())
    }

    async fn check_storage_mode(
        &self,
        id: &TorrentId,
        storage_mode: StorageMode,
    ) -> Result<(), EngineError> {
        self.running()?;
        let torrent = self.torrent(id)?;
        let bytes = {
            let state = torrent.state();
            match &state.live {
                Some(live) => live
                    .handle
                    .with_metadata(|metadata| metadata.torrent_bytes.to_vec())
                    .map_err(internal)?,
                None => return Ok(()),
            }
        };
        let inspected = inspect::torrent_file(&bytes)?;
        self.check_storage(&inspected, storage_mode, &torrent.directory)
    }

    async fn change_storage_mode(
        &self,
        id: &TorrentId,
        storage_mode: StorageMode,
    ) -> Result<(), EngineError> {
        let running = self.running()?;
        let torrent = self.torrent(id)?;
        let _operations = torrent.operations.lock().await;
        let (live, metadata) = {
            let mut state = torrent.state();
            if state.storage_mode == storage_mode {
                return Ok(());
            }
            if state.live.is_none() {
                // Metadata retrieval adds it in the new storage mode.
                state.storage_mode = storage_mode;
                return Ok(());
            }
            (state.live.take(), state.metadata.clone())
        };
        let (Some(live), Some(metadata)) = (live, metadata) else {
            return Err(internal(anyhow::anyhow!("a live torrent has no metadata")));
        };
        match storage_mode {
            StorageMode::Disk => self.move_to_disk(&running, &torrent, live, &metadata).await,
            StorageMode::Memory => self.move_to_memory(&running, &torrent, live).await,
        }
    }

    async fn supply_metadata(
        &self,
        id: &TorrentId,
        torrent_file: &[u8],
    ) -> Result<(), EngineError> {
        let running = self.running()?;
        let torrent = self.torrent(id)?;
        let inspected = inspect::torrent_file(torrent_file)?;
        if inspected.info_hash != torrent.info_hash {
            return Err(EngineError::MetadataMismatch);
        }
        let storage_mode = torrent.state().storage_mode;
        self.check_storage(&inspected, storage_mode, &torrent.directory)?;
        let _operations = torrent.operations.lock().await;
        if torrent.state().metadata.is_some() {
            return Ok(());
        }
        self.cancel_retrieval(&running, &torrent).await;
        let desired = torrent.state().desired;
        let live = self
            .add_to_session(
                &running,
                &torrent,
                AddTorrent::from_bytes(torrent_file.to_vec()),
                desired,
                None,
            )
            .await
            .map_err(AddFailure::into_engine_error)?;
        let mut state = torrent.state();
        state.metadata = Some(Arc::new(inspected.metadata));
        state.live = Some(live);
        Ok(())
    }

    async fn set_desired_state(
        &self,
        id: &TorrentId,
        desired_state: DesiredState,
    ) -> Result<(), EngineError> {
        let running = self.running()?;
        let torrent = self.torrent(id)?;
        let _operations = torrent.operations.lock().await;
        let previous = torrent.state().desired;
        if previous == desired_state {
            return Ok(());
        }
        match desired_state {
            DesiredState::Paused => {
                self.cancel_retrieval(&running, &torrent).await;
                let handle = {
                    let mut state = torrent.state();
                    state.desired = DesiredState::Paused;
                    // librqbit drops a failed torrent's storage, which
                    // leaves the cache, so there is nothing to mark paused.
                    let failed = Self::engine_failure(&mut state).is_some();
                    let paused = state.live.as_ref().map(|live| {
                        let fetched = live
                            .handle
                            .stats()
                            .live
                            .map_or(0, |stats| stats.snapshot.fetched_bytes);
                        let mut closed = live.scheduler.close();
                        if !failed && let Some(cached) = &live.cached {
                            closed = closed.and(cached.set_started(false).map_err(Into::into));
                        }
                        (live.handle.clone(), closed, fetched)
                    });
                    paused.map(|(handle, started, fetched)| {
                        state.downloaded_before += fetched;
                        (handle, started, failed)
                    })
                };
                if let Some((handle, started, failed)) = handle {
                    started.map_err(internal)?;
                    // librqbit stops a failed torrent itself, and a pause
                    // racing that stop fails, so a torrent that failed, or
                    // fails while it is paused, counts as paused.
                    if !failed
                        && !handle.is_paused()
                        && matches!(handle.stats().state, TorrentStatsState::Live)
                        && let Err(error) = running.session.pause(&handle).await
                        && Self::engine_failure(&mut torrent.state()).is_none()
                    {
                        return Err(internal(error));
                    }
                }
            }
            DesiredState::Started => {
                let failed = {
                    let mut state = torrent.state();
                    state.desired = DesiredState::Started;
                    state.failure = None;
                    Self::engine_failure(&mut state).is_some()
                };
                if failed {
                    self.restart_failed(&running, &torrent).await?;
                    return Ok(());
                }
                let live = torrent.state().live.as_ref().map(|live| {
                    let reopened = live.scheduler.reopen();
                    (
                        live.handle.clone(),
                        reopened.and(match &live.cached {
                            Some(cached) => cached.set_started(true).map_err(Into::into),
                            None => Ok(()),
                        }),
                    )
                });
                match live {
                    Some((handle, started)) => {
                        started.map_err(internal)?;
                        running.session.unpause(&handle).await.map_err(internal)?;
                    }
                    None => self.spawn_retrieval(&torrent),
                }
            }
        }
        Ok(())
    }

    async fn remove(&self, id: &TorrentId) -> Result<(), EngineError> {
        let running = self.running()?;
        let torrent = self.torrent(id)?;
        let _operations = torrent.operations.lock().await;
        self.cancel_retrieval(&running, &torrent).await;
        let (live, storage_mode) = {
            let mut state = torrent.state();
            (state.live.take(), state.storage_mode)
        };
        self.lock_torrents().remove(id);
        running.preloads.remove_torrent(id);
        if let Some(live) = live {
            self.delete_live(&running, live).await.map_err(internal)?;
        }
        if storage_mode == StorageMode::Disk {
            delete_disk_data(torrent.directory.clone()).await;
        }
        Ok(())
    }

    async fn status(&self, id: &TorrentId) -> Result<TorrentStatus, EngineError> {
        self.running()?;
        let torrent = self.torrent(id)?;
        let mut state = torrent.state();
        let failure = state
            .failure
            .clone()
            .or_else(|| Self::engine_failure(&mut state));
        Ok(TorrentStatus {
            metadata: state.metadata.clone(),
            failure,
        })
    }

    async fn torrent_file(&self, id: &TorrentId) -> Result<Option<Vec<u8>>, EngineError> {
        self.running()?;
        let torrent = self.torrent(id)?;
        let state = torrent.state();
        if state.metadata.is_none() {
            return Ok(None);
        }
        // librqbit builds a magnet URI's torrent file from the retrieved info
        // dictionary and the URI's trackers when the metadata arrives; while a
        // failed torrent is being restarted it has no live handle.
        let Some(live) = &state.live else {
            return Ok(None);
        };
        live.handle
            .with_metadata(|metadata| metadata.torrent_bytes.to_vec())
            .map(Some)
            .map_err(internal)
    }

    async fn open_reader(
        &self,
        id: &TorrentId,
        file: FileIndex,
        range: ByteRange,
    ) -> Result<RangeReader, EngineError> {
        self.running()?;
        let torrent = self.torrent(id)?;
        let (handle, scheduler) = {
            let mut state = torrent.state();
            let metadata = state
                .metadata
                .clone()
                .ok_or(EngineError::MetadataUnavailable)?;
            if state.desired != DesiredState::Started
                || state.failure.is_some()
                || Self::engine_failure(&mut state).is_some()
            {
                return Err(EngineError::NotStarted);
            }
            let size_bytes = metadata
                .files
                .get(file.0)
                .ok_or(EngineError::UnknownFile)?
                .size_bytes;
            if range.end() > size_bytes {
                return Err(EngineError::RangeOutOfBounds);
            }
            let live = state.live.as_ref().ok_or(EngineError::NotStarted)?;
            (live.handle.clone(), live.scheduler.clone())
        };
        // A torrent without streams may have lost its peers, and librqbit
        // would reconnect them only after a backoff; this stream needs data
        // now.
        handle.reconnect_peers();
        let mut reader = match scheduler.take_idle(file.0, range.start()) {
            Some(reader) => reader,
            None => scheduler
                .open(&handle, file.0)
                .await
                .map_err(|error| match error {
                    OpenError::Closed => EngineError::NotStarted,
                    OpenError::Engine(error) => internal(error),
                })?,
        };
        reader
            .seek(SeekFrom::Start(range.start()))
            .await
            .map_err(internal)?;
        Ok(RangeReader::new(
            reader,
            range.len_bytes(),
            self.tunables().piece_wait,
            self.piece_waits.clone(),
        ))
    }

    async fn create_preload(
        &self,
        id: &TorrentId,
        file: FileIndex,
        preload: PreloadId,
        request: PreloadRequest,
    ) -> Result<PreloadStatus, EngineError> {
        let running = self.running()?;
        let torrent = self.torrent(id)?;
        let scheduler = {
            let mut state = torrent.state();
            let metadata = state
                .metadata
                .clone()
                .ok_or(EngineError::MetadataUnavailable)?;
            let size_bytes = metadata
                .files
                .get(file.0)
                .ok_or(EngineError::UnknownFile)?
                .size_bytes;
            if state.desired != DesiredState::Started
                || state.failure.is_some()
                || Self::engine_failure(&mut state).is_some()
            {
                return Err(EngineError::NotStarted);
            }
            if request
                .offset_bytes
                .is_some_and(|offset| offset >= size_bytes)
            {
                return Err(EngineError::RangeOutOfBounds);
            }
            let live = state.live.as_ref().ok_or(EngineError::NotStarted)?;
            live.scheduler.clone()
        };
        // A transition may be adding the torrent again; its layout stays.
        let layout = scheduler.layout().ok_or(EngineError::NotStarted)?;
        if let Some(handle) = scheduler.torrent() {
            // As for a stream: the preload needs peers now.
            handle.reconnect_peers();
        }
        running
            .preloads
            .create(id, &scheduler, &layout, file, preload, request)
    }

    async fn preloads(
        &self,
        id: &TorrentId,
        file: FileIndex,
    ) -> Result<Vec<PreloadStatus>, EngineError> {
        let running = self.running()?;
        self.check_file(id, file)?;
        Ok(running.preloads.of_file(id, file))
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
        let running = self.running()?;
        self.check_file(id, file)?;
        running.preloads.cancel(id, file, preload)
    }

    async fn torrent_statistics(&self, id: &TorrentId) -> Result<TorrentStatistics, EngineError> {
        self.running()?;
        let torrent = self.torrent(id)?;
        let state = torrent.state();
        let mut statistics = TorrentStatistics {
            downloaded_bytes: state.downloaded_before,
            ..TorrentStatistics::default()
        };
        if let Some(live) = &state.live {
            let stats = live.handle.stats();
            if stats.total_bytes > 0 && state.metadata.is_some() {
                statistics.completion_ratio =
                    Ratio::new(stats.progress_bytes as f64 / stats.total_bytes as f64);
            }
            if let Some(live_stats) = stats.live {
                statistics.downloaded_bytes += live_stats.snapshot.fetched_bytes;
                statistics.download_rate_bytes_per_second = live_stats.download_speed.as_bytes();
                statistics.peer_count = u64::from(live_stats.snapshot.peer_stats.live);
            }
        }
        Ok(statistics)
    }

    async fn cache_statistics(&self) -> Result<CacheStatistics, EngineError> {
        let running = self.running()?;
        let stats = running.cache.stats().map_err(internal)?;
        Ok(CacheStatistics {
            capacity_bytes: stats.capacity_bytes,
            usage_bytes: stats.usage_bytes,
            pinned_bytes: stats.pinned_bytes,
            eviction_count: stats.eviction_count,
        })
    }

    async fn engine_statistics(&self) -> Result<EngineStatistics, EngineError> {
        let running = self.running()?;
        let session = running.session.stats_snapshot();
        let readers = running.playback.stats();
        // librqbit does not export the snapshot types, so their fields are
        // read by name.
        macro_rules! outcomes {
            ($family:expr) => {
                ConnectionOutcomes {
                    succeeded: $family.v4.successes + $family.v6.successes,
                    failed: $family.v4.errors + $family.v6.errors,
                }
            };
        }
        Ok(EngineStatistics {
            active_reader_count: readers.readers.saturating_sub(readers.idle_readers),
            idle_reader_count: readers.idle_readers,
            peer_count: u64::from(session.peers.live),
            downloaded_bytes: session.counters.fetched_bytes,
            uploaded_bytes: session.counters.uploaded_bytes,
            download_rate_bytes_per_second: session.download_speed.as_bytes(),
            upload_rate_bytes_per_second: session.upload_speed.as_bytes(),
            peer_connections: PeerConnections {
                tcp: outcomes!(session.connections.tcp),
                utp: outcomes!(session.connections.utp),
                socks: outcomes!(session.connections.socks),
            },
            hash_checks: HashChecks {
                passed: session.counters.hash_checks_passed,
                failed: session.counters.hash_checks_failed,
            },
            piece_waits: self.piece_waits.snapshot(),
        })
    }
}

fn take<T>(slot: &Mutex<Option<T>>) -> Option<T> {
    slot.lock().unwrap_or_else(PoisonError::into_inner).take()
}

/// Cancels the streams of each torrent the storage reports failed.
async fn monitor_failures(
    engine: Weak<LibrqbitEngine>,
    mut failed: mpsc::UnboundedReceiver<TorrentKey>,
) {
    while let Some(key) = failed.recv().await {
        let Some(engine) = engine.upgrade() else {
            return;
        };
        engine.storage_failed(key);
    }
}
