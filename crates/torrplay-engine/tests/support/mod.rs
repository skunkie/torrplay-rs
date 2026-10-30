// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! Shared helpers for the engine tests: librqbit sessions that use only
//! loopback, local seeders and leechers, a piece cache with its evictor, and
//! waits with a bound.
//!
//! Every session disables DHT, trackers, local service discovery, UPnP port
//! forwarding, and fastresume, and listens on an ephemeral loopback port, so
//! peers reach each other only through explicit addresses.

// Each test crate uses a different subset of the helpers.
#![allow(dead_code)]
// Helpers assert as tests do; clippy's test exemption covers only test
// functions.
#![allow(clippy::expect_used)]

use std::future::Future;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use librqbit::{AddTorrent, AddTorrentOptions, ManagedTorrent, Session};
use tempfile::TempDir;
use tokio::io::AsyncReadExt;
use torrplay_core::engine::{ByteRange, Engine, Source};
use torrplay_core::id::TorrentId;
use torrplay_core::metadata::FileIndex;
use torrplay_engine::cache::PieceCache;
use torrplay_engine::evictor::Evictor;
use torrplay_engine::playback::{Playback, PlaybackConfig};
use torrplay_engine::scheduler::TorrentScheduler;
use torrplay_engine::storage::{CachedTorrent, MemoryStorageFactory};
pub use torrplay_engine::test_support::{Seeder, new_session};
use torrplay_engine::{EngineConfig, LibrqbitEngine, NetworkConfig};
use torrplay_fixtures::{FixtureSpec, generator};

/// The longest any single step of a test may take.
pub const STEP_TIMEOUT: Duration = Duration::from_secs(30);

/// Awaits `future`, failing if it takes longer than [`STEP_TIMEOUT`].
pub async fn bounded<T>(
    what: &str,
    future: impl Future<Output = anyhow::Result<T>>,
) -> anyhow::Result<T> {
    tokio::time::timeout(STEP_TIMEOUT, future)
        .await
        .with_context(|| format!("{what} did not finish within {STEP_TIMEOUT:?}"))?
}

/// Polls `check` every 20 ms until it returns `Some`, for at most
/// [`STEP_TIMEOUT`].
pub async fn eventually<T>(
    what: &str,
    mut check: impl FnMut() -> anyhow::Result<Option<T>>,
) -> anyhow::Result<T> {
    bounded(what, async {
        loop {
            if let Some(value) = check()? {
                return Ok(value);
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
}

/// A session that downloads into its own temporary folder.
pub struct Leecher {
    /// The downloading session.
    pub session: Arc<Session>,
    dir: TempDir,
}

impl Leecher {
    /// Starts a session with an empty temporary output folder.
    pub async fn start() -> anyhow::Result<Self> {
        let dir = tempfile::tempdir()?;
        let session = new_session(dir.path()).await?;
        Ok(Self { session, dir })
    }

    /// Returns options that connect only to `peers` and write below this
    /// leecher's folder.
    pub fn add_options(&self, peers: &[SocketAddr]) -> anyhow::Result<AddTorrentOptions> {
        Ok(AddTorrentOptions {
            output_folder: Some(
                self.dir
                    .path()
                    .join("download")
                    .to_str()
                    .context("temporary paths are UTF-8")?
                    .to_owned(),
            ),
            overwrite: true,
            initial_peers: Some(peers.to_vec()),
            ..Default::default()
        })
    }
}

/// A piece cache with its memory storage factory and running evictor.
pub struct MemoryCache {
    /// The cache.
    pub cache: Arc<PieceCache>,
    /// The factory for torrents in memory storage mode.
    pub factory: MemoryStorageFactory,
    evictor: Evictor,
}

impl MemoryCache {
    /// Creates a cache of `capacity_bytes` and starts its evictor.
    pub fn start(capacity_bytes: u64) -> Self {
        let cache = PieceCache::new(capacity_bytes);
        Self {
            factory: MemoryStorageFactory::new(cache.clone()),
            evictor: Evictor::spawn(cache.clone()),
            cache,
        }
    }

    /// Stops the evictor, failing if it failed.
    pub async fn shutdown(self) -> anyhow::Result<()> {
        bounded("stopping the evictor", self.evictor.shutdown()).await
    }
}

/// A torrent in memory storage mode whose readers decide what it downloads.
pub struct ScheduledTorrent {
    /// The torrent.
    pub handle: Arc<ManagedTorrent>,
    /// Its pieces in the cache.
    pub cached: CachedTorrent,
    /// Its piece selector, which opens readers.
    pub scheduler: Arc<TorrentScheduler>,
}

impl MemoryCache {
    /// Adds `spec` to `leecher` with `peers` as its only peers, as the
    /// adapter does: without upload and with a scheduler that shares
    /// `readahead_bytes` with no other torrent, paused until it is attached
    /// to the cache and its initial check has ended, and then started.
    pub async fn add_scheduled(
        &self,
        leecher: &Leecher,
        peers: &[SocketAddr],
        spec: &FixtureSpec,
        readahead_bytes: u64,
    ) -> anyhow::Result<ScheduledTorrent> {
        self.add_scheduled_with(leecher, peers, spec, readahead_bytes, |_| {})
            .await
    }

    /// Like [`add_scheduled`](Self::add_scheduled), letting `tune` adjust
    /// the options, such as peer options or rate limits.
    pub async fn add_scheduled_with(
        &self,
        leecher: &Leecher,
        peers: &[SocketAddr],
        spec: &FixtureSpec,
        readahead_bytes: u64,
        tune: impl FnOnce(&mut AddTorrentOptions),
    ) -> anyhow::Result<ScheduledTorrent> {
        let playback = Playback::new(PlaybackConfig::new(readahead_bytes));
        self.add_scheduled_in(leecher, peers, spec, &playback, tune)
            .await
    }

    /// Like [`add_scheduled_with`](Self::add_scheduled_with), with readers
    /// sharing `playback`'s readahead budget with other torrents.
    pub async fn add_scheduled_in(
        &self,
        leecher: &Leecher,
        peers: &[SocketAddr],
        spec: &FixtureSpec,
        playback: &Arc<Playback>,
        tune: impl FnOnce(&mut AddTorrentOptions),
    ) -> anyhow::Result<ScheduledTorrent> {
        let scheduler = TorrentScheduler::new(playback);
        let mut options = AddTorrentOptions {
            storage_factory: Some(self.factory.clone().into_boxed()),
            piece_selector: Some(scheduler.clone()),
            disable_upload: true,
            paused: true,
            ..leecher.add_options(peers)?
        };
        tune(&mut options);
        let handle = bounded(
            "adding a torrent",
            leecher
                .session
                .add_torrent(AddTorrent::from_bytes(spec.metainfo().bytes), Some(options)),
        )
        .await?
        .into_handle()
        .context("the torrent was not added")?;
        let cached = self.factory.attach(&handle)?;
        scheduler.bind(&cached, &handle)?;
        bounded("the initial check", handle.wait_until_initialized()).await?;
        bounded("starting the torrent", leecher.session.unpause(&handle)).await?;
        Ok(ScheduledTorrent {
            handle,
            cached,
            scheduler,
        })
    }
}

pub const KIB: u64 = 1024;
pub const MIB: u64 = 1024 * KIB;

/// A started engine with its session folder.
pub struct Started {
    pub engine: Arc<LibrqbitEngine>,
    _folder: TempDir,
}

/// Starts an engine reaching only `peers`, with `capacity` of cache and
/// `piece_wait` as its piece wait bound.
pub async fn start(
    peers: Vec<SocketAddr>,
    capacity: u64,
    piece_wait: Duration,
) -> anyhow::Result<Started> {
    start_with(peers, |config| {
        *config = config.clone().with_cache_capacity(capacity);
        config.piece_wait = piece_wait;
    })
    .await
}

/// Starts an engine reaching only `peers`, with the defaults `tune`
/// adjusts.
pub async fn start_with(
    peers: Vec<SocketAddr>,
    tune: impl FnOnce(&mut EngineConfig),
) -> anyhow::Result<Started> {
    let folder = tempfile::tempdir()?;
    let mut config = EngineConfig::new(folder.path().to_path_buf(), NetworkConfig::isolated(peers));
    tune(&mut config);
    let engine = LibrqbitEngine::new(config);
    bounded("starting the engine", engine.start()).await?;
    Ok(Started {
        engine,
        _folder: folder,
    })
}

pub fn id(n: u8) -> TorrentId {
    TorrentId::from_random_bytes([n; 16])
}

pub fn upload(spec: &FixtureSpec) -> Source {
    Source::Metainfo(spec.metainfo().bytes)
}

pub fn magnet(spec: &FixtureSpec) -> Source {
    Source::MagnetUri(spec.magnet_uri(&spec.metainfo().info_hash))
}

pub fn range(start: u64, end: u64) -> ByteRange {
    ByteRange::new(start, end).expect("a range")
}

/// Reads bytes `start..end` of `file` through the engine.
pub async fn read(
    engine: &LibrqbitEngine,
    id: &TorrentId,
    file: usize,
    start: u64,
    end: u64,
) -> anyhow::Result<Vec<u8>> {
    let mut reader = engine
        .open_reader(id, FileIndex(file), range(start, end))
        .await?;
    let mut bytes = Vec::new();
    bounded("reading a range", async {
        Ok(reader.read_to_end(&mut bytes).await?)
    })
    .await?;
    Ok(bytes)
}

/// The content of bytes `start..end` of `file`.
pub fn expected(spec: &FixtureSpec, file: usize, start: u64, end: u64) -> Vec<u8> {
    let mut bytes = vec![0; usize::try_from(end - start).expect("a small range")];
    generator::fill(spec.file_seed(file), start, &mut bytes);
    bytes
}

/// Polls a future that the adapter completes without waiting.
pub fn futures_now<T>(
    future: impl std::future::Future<Output = Result<T, torrplay_core::engine::EngineError>>,
) -> anyhow::Result<T> {
    let mut future = std::pin::pin!(future);
    let waker = std::task::Waker::noop();
    match future
        .as_mut()
        .poll(&mut std::task::Context::from_waker(waker))
    {
        std::task::Poll::Ready(result) => Ok(result?),
        std::task::Poll::Pending => anyhow::bail!("the status was not immediate"),
    }
}
