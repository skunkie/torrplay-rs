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
//! librqbit's error text can quote tracker URLs or internal state, so it is
//! logged redacted and never becomes a failure detail.

use std::collections::HashMap;
use std::io::SeekFrom;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::time::Duration;

use librqbit::api::TorrentIdOrHash;
use librqbit::{
    AddTorrent, AddTorrentOptions, ListenerMode, ListenerOptions, ManagedTorrent,
    PeerConnectionOptions, Session, SessionOptions, TorrentStatsState,
};
use tokio::io::AsyncSeekExt;
use tokio::task::JoinHandle;
use torrplay_core::engine::{
    ByteRange, CacheStatistics, Engine, EngineError, Inspection, Ratio, Source, SourceRejection,
    TorrentStatistics, TorrentStatus,
};
use torrplay_core::id::{InfoHashV1, TorrentId};
use torrplay_core::lifecycle::{DesiredState, Failure, Readiness};
use torrplay_core::metadata::{FileIndex, ReportedMetadata};
use torrplay_core::redact::redact;

use crate::cache::PieceCache;
use crate::evictor::Evictor;
use crate::inspect;
use crate::reader::RangeReader;
use crate::scheduler::TorrentScheduler;
use crate::storage::{CachedTorrent, MemoryStorageFactory};

const MIB: u64 = 1024 * 1024;

/// The default cache capacity, as ADR 5's milestone 4 defaults fix it.
pub const DEFAULT_CACHE_CAPACITY_BYTES: u64 = 256 * MIB;

/// The default piece wait bound.
pub const DEFAULT_PIECE_WAIT: Duration = Duration::from_secs(60);

/// The default request window, in blocks per peer.
pub const DEFAULT_REQUEST_WINDOW_BLOCKS: usize = 32;

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
    /// Whether DHT finds peers. Its state is never persisted.
    pub dht: bool,
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
            trackers: true,
            initial_peers: Vec::new(),
        }
    }

    /// An ephemeral loopback port without DHT or trackers, reaching only
    /// `peers`, for tests that must never touch a public swarm.
    pub fn isolated(peers: Vec<SocketAddr>) -> Self {
        Self {
            listen: (std::net::Ipv4Addr::LOCALHOST, 0).into(),
            dht: false,
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
    /// The readahead budget shared by every reader of a torrent.
    pub readahead_bytes: u64,
    /// How long a read waits for missing piece data, and how long a peer
    /// may hold requests without delivering.
    pub piece_wait: Duration,
    /// The most chunk requests outstanding to one peer.
    pub request_window_blocks: usize,
    /// The session's folder. Memory storage writes nothing there.
    pub folder: PathBuf,
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
            piece_wait: DEFAULT_PIECE_WAIT,
            request_window_blocks: DEFAULT_REQUEST_WINDOW_BLOCKS,
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
    evictor: Mutex<Option<Evictor>>,
}

enum Phase {
    Starting,
    Ready(Arc<Running>),
    Stopped,
}

/// A torrent in the session, with its pieces in the cache.
struct Live {
    handle: Arc<ManagedTorrent>,
    cached: CachedTorrent,
    scheduler: Arc<TorrentScheduler>,
}

struct TorrentState {
    desired: DesiredState,
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
    phase: Mutex<Phase>,
    torrents: Mutex<HashMap<TorrentId, Arc<Torrent>>>,
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
            config,
            phase: Mutex::new(Phase::Starting),
            torrents: Mutex::new(HashMap::new()),
        })
    }

    /// Starts the session, the piece cache, and its evictor.
    pub async fn start(&self) -> anyhow::Result<()> {
        let network = &self.config.network;
        let options = SessionOptions {
            dht: network.dht.then(|| librqbit::DhtSessionConfig {
                persistence: None,
                ..Default::default()
            }),
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
        let cache = PieceCache::new(self.config.cache_capacity_bytes);
        let running = Arc::new(Running {
            factory: MemoryStorageFactory::new(cache.clone()),
            evictor: Mutex::new(Some(Evictor::spawn(cache.clone()))),
            cache,
            session,
        });
        *self.lock_phase() = Phase::Ready(running);
        Ok(())
    }

    /// Stops the engine: it reports itself unavailable, cancels metadata
    /// retrieval, stops the session, which closes every peer connection, and
    /// stops the evictor, returning its failure.
    pub async fn shutdown(&self) -> anyhow::Result<()> {
        let running = match std::mem::replace(&mut *self.lock_phase(), Phase::Stopped) {
            Phase::Ready(running) => running,
            Phase::Starting | Phase::Stopped => return Ok(()),
        };
        let torrents: Vec<_> = self.lock_torrents().values().cloned().collect();
        for torrent in torrents {
            let retrieval = torrent.state().retrieval.take();
            if let Some(retrieval) = retrieval {
                retrieval.abort();
                let _ = retrieval.await;
            }
        }
        running.session.stop().await;
        let evictor = running
            .evictor
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(evictor) = evictor {
            evictor.shutdown().await?;
        }
        Ok(())
    }

    /// The address the session's peer listener bound, once started.
    pub fn listen_addr(&self) -> Option<SocketAddr> {
        match &*self.lock_phase() {
            Phase::Ready(running) => running.session.listen_addr(),
            Phase::Starting | Phase::Stopped => None,
        }
    }

    /// The piece cache, once started, for tests that audit its accounting.
    pub fn cache(&self) -> Option<Arc<PieceCache>> {
        match &*self.lock_phase() {
            Phase::Ready(running) => Some(running.cache.clone()),
            Phase::Starting | Phase::Stopped => None,
        }
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

    fn torrent(&self, id: &TorrentId) -> Result<Arc<Torrent>, EngineError> {
        self.lock_torrents()
            .get(id)
            .cloned()
            .ok_or(EngineError::UnknownTorrent)
    }

    fn too_large(&self, piece_length: u32) -> Option<String> {
        (u64::from(piece_length) > self.config.cache_capacity_bytes).then(|| {
            format!(
                "The torrent's {piece_length}-byte pieces exceed the {}-byte cache capacity.",
                self.config.cache_capacity_bytes
            )
        })
    }

    fn inspect_file(&self, bytes: &[u8]) -> Result<inspect::InspectedFile, EngineError> {
        let inspected = inspect::torrent_file(bytes)?;
        if let Some(detail) = self.too_large(inspected.piece_length) {
            return Err(SourceRejection::Unsupported { detail }.into());
        }
        Ok(inspected)
    }

    fn add_options(
        &self,
        running: &Running,
        scheduler: &Arc<TorrentScheduler>,
    ) -> AddTorrentOptions {
        AddTorrentOptions {
            storage_factory: Some(running.factory.clone().into_boxed()),
            piece_selector: Some(scheduler.clone()),
            disable_upload: true,
            paused: true,
            overwrite: true,
            initial_peers: Some(self.config.network.initial_peers.clone()),
            peer_opts: Some(PeerConnectionOptions {
                max_request_window: Some(self.config.request_window_blocks),
                request_stall_timeout: Some(self.config.piece_wait),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    /// Adds `add` to the session, attaches it to the cache, waits for its
    /// initial check, and starts it if `desired` says so. A magnet URI
    /// waits for its metadata first.
    async fn add_to_session(
        &self,
        running: &Running,
        add: AddTorrent<'_>,
        desired: DesiredState,
    ) -> anyhow::Result<Live> {
        let scheduler = TorrentScheduler::new(self.config.readahead_bytes);
        let handle = running
            .session
            .add_torrent(add, Some(self.add_options(running, &scheduler)))
            .await?
            .into_handle()
            .ok_or_else(|| anyhow::anyhow!("the session did not add the torrent"))?;
        let cached = running.factory.attach(&handle)?;
        scheduler.bind(&cached, &handle)?;
        handle.wait_until_initialized().await?;
        let live = Live {
            handle,
            cached,
            scheduler,
        };
        match desired {
            DesiredState::Started => running.session.unpause(&live.handle).await?,
            DesiredState::Paused => live.cached.set_started(false)?,
        }
        Ok(live)
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
                .add_to_session(&running, AddTorrent::from_url(magnet_uri), desired)
                .await
            {
                Ok(live) => {
                    self.retrieved(&running, torrent, live).await;
                    return;
                }
                Err(error) => {
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

    /// Records a magnet torrent's retrieved metadata, or fails it when its
    /// pieces are longer than the cache.
    async fn retrieved(&self, running: &Running, torrent: &Torrent, live: Live) {
        let checked = live.handle.with_metadata(|metadata| {
            (
                metadata.info.info().piece_length,
                inspect::reported_metadata(&metadata.info),
            )
        });
        let (piece_length, metadata) = match checked {
            Ok(checked) => checked,
            Err(error) => {
                tracing::error!(error = %redact(&format!("{error:#}")), "retrieved metadata is unreadable");
                let _ = self.delete_live(running, live).await;
                torrent.state().failure = Some(Failure {
                    detail: ENGINE_FAILURE.to_owned(),
                });
                return;
            }
        };
        if let Some(detail) = self.too_large(piece_length) {
            let _ = self.delete_live(running, live).await;
            torrent.state().failure = Some(Failure { detail });
            return;
        }
        let mut state = torrent.state();
        state.metadata = Some(Arc::new(metadata));
        state.live = Some(live);
    }

    async fn delete_live(&self, running: &Running, live: Live) -> anyhow::Result<()> {
        let result = running
            .session
            .delete(TorrentIdOrHash::Id(live.handle.id()), false)
            .await;
        live.cached.release()?;
        result
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

    /// Reports librqbit's error for a live torrent, logged once.
    fn engine_failure(state: &mut TorrentState) -> Option<Failure> {
        let live = state.live.as_ref()?;
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
        Some(Failure {
            detail: ENGINE_FAILURE.to_owned(),
        })
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
        let bytes = live
            .handle
            .with_metadata(|metadata| metadata.torrent_bytes.clone())
            .map_err(internal)?;
        self.delete_live(running, live).await.map_err(internal)?;
        let live = self
            .add_to_session(
                running,
                AddTorrent::from_bytes(bytes),
                DesiredState::Started,
            )
            .await
            .map_err(internal)?;
        let mut state = torrent.state();
        state.live = Some(live);
        state.engine_error_logged = false;
        Ok(())
    }
}

impl Engine for LibrqbitEngine {
    type Reader = RangeReader;

    fn readiness(&self) -> Readiness {
        match &*self.lock_phase() {
            Phase::Ready(_) => Readiness::Ready,
            Phase::Starting | Phase::Stopped => Readiness::Unavailable { retry_after: None },
        }
    }

    async fn inspect(&self, source: &Source) -> Result<Inspection, EngineError> {
        self.running()?;
        match source {
            Source::TorrentFile(bytes) => {
                let inspected = self.inspect_file(bytes)?;
                Ok(Inspection {
                    info_hash: inspected.info_hash,
                    metadata: Some(Arc::new(inspected.metadata)),
                })
            }
            Source::MagnetUri(uri) => Ok(Inspection {
                info_hash: inspect::magnet_uri(uri)?,
                metadata: None,
            }),
        }
    }

    async fn add(
        &self,
        id: &TorrentId,
        source: &Source,
        desired_state: DesiredState,
    ) -> Result<(), EngineError> {
        let running = self.running()?;
        let (info_hash, metadata, magnet_uri) = match source {
            Source::TorrentFile(bytes) => {
                let inspected = self.inspect_file(bytes)?;
                (
                    inspected.info_hash,
                    Some(Arc::new(inspected.metadata)),
                    None,
                )
            }
            Source::MagnetUri(uri) => (inspect::magnet_uri(uri)?, None, Some(uri.clone())),
        };
        let torrent = Arc::new(Torrent {
            info_hash,
            magnet_uri,
            operations: tokio::sync::Mutex::new(()),
            state: Mutex::new(TorrentState {
                desired: desired_state,
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
                        AddTorrent::from_bytes(bytes.clone()),
                        desired_state,
                    )
                    .await
                {
                    Ok(live) => torrent.state().live = Some(live),
                    Err(error) => {
                        self.lock_torrents().remove(id);
                        return Err(internal(error));
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

    async fn supply_metadata(
        &self,
        id: &TorrentId,
        torrent_file: &[u8],
    ) -> Result<(), EngineError> {
        let running = self.running()?;
        let torrent = self.torrent(id)?;
        let inspected = self.inspect_file(torrent_file)?;
        if inspected.info_hash != torrent.info_hash {
            return Err(EngineError::MetadataMismatch);
        }
        let _operations = torrent.operations.lock().await;
        if torrent.state().metadata.is_some() {
            return Ok(());
        }
        self.cancel_retrieval(&running, &torrent).await;
        let desired = torrent.state().desired;
        let live = self
            .add_to_session(
                &running,
                AddTorrent::from_bytes(torrent_file.to_vec()),
                desired,
            )
            .await
            .map_err(internal)?;
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
                    let paused = state.live.as_ref().map(|live| {
                        let fetched = live
                            .handle
                            .stats()
                            .live
                            .map_or(0, |stats| stats.snapshot.fetched_bytes);
                        (live.handle.clone(), live.cached.set_started(false), fetched)
                    });
                    paused.map(|(handle, started, fetched)| {
                        state.downloaded_before += fetched;
                        (handle, started)
                    })
                };
                if let Some((handle, started)) = handle {
                    started.map_err(internal)?;
                    if !handle.is_paused()
                        && matches!(handle.stats().state, TorrentStatsState::Live)
                    {
                        running.session.pause(&handle).await.map_err(internal)?;
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
                let live = torrent
                    .state()
                    .live
                    .as_ref()
                    .map(|live| (live.handle.clone(), live.cached.set_started(true)));
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
        let live = torrent.state().live.take();
        self.lock_torrents().remove(id);
        if let Some(live) = live {
            self.delete_live(&running, live).await.map_err(internal)?;
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
        let mut reader = scheduler.open(&handle, file.0).await.map_err(internal)?;
        reader
            .seek(SeekFrom::Start(range.start()))
            .await
            .map_err(internal)?;
        Ok(RangeReader::new(
            reader,
            range.len_bytes(),
            self.config.piece_wait,
        ))
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
}
