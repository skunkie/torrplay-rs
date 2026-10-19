// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! E3, the feasibility gate: eviction from the piece cache with completion
//! invalidation in librqbit (patch P1). Each test names the
//! `docs/acceptance.md` scenario it covers. Without pinning, which E6 adds,
//! a reader's next pieces are ordinary eviction candidates, so these tests
//! check correctness under eviction rather than how much is re-downloaded.

use std::io::SeekFrom;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use engine_spike::{Leecher, Seeder, bounded};
use librqbit::api::{LiveStats, TorrentIdOrHash};
use librqbit::{AddTorrent, AddTorrentOptions, ManagedTorrent, TorrentStats, TorrentStatsState};
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use torrplay_engine::cache::{CacheStats, PieceCache};
use torrplay_engine::storage::{CachedTorrent, MemoryStorageFactory};
use torrplay_fixtures::{FixtureSpec, catalog, generator};

const KIB: u64 = 1024;
const MIB: u64 = 1024 * KIB;

/// Adds `spec` to the leecher with memory storage from `factory`, with the
/// seeder as its only peer, and attaches it so the cache can evict it. The
/// torrent is added paused and started once attached, as the adapter must:
/// otherwise its first writes can fill the cache before any piece is
/// evictable, and a write that fails fails the torrent. Starting it waits
/// for the initial check, because librqbit runs a second check, which fails
/// the torrent, if a start arrives as the first one ends.
async fn add_in_memory(
    leecher: &Leecher,
    seeder: &Seeder,
    spec: &FixtureSpec,
    factory: &MemoryStorageFactory,
) -> anyhow::Result<(Arc<ManagedTorrent>, CachedTorrent)> {
    let options = AddTorrentOptions {
        storage_factory: Some(factory.clone().into_boxed()),
        paused: true,
        ..leecher.add_options(&[seeder.addr])?
    };
    let handle = bounded(
        "adding a torrent with memory storage",
        leecher
            .session
            .add_torrent(AddTorrent::from_bytes(spec.metainfo().bytes), Some(options)),
    )
    .await?
    .into_handle()
    .context("the torrent was not added")?;
    let cached = factory.attach(&handle)?;
    bounded("the initial check", handle.wait_until_initialized()).await?;
    bounded("starting the torrent", leecher.session.unpause(&handle)).await?;
    Ok((handle, cached))
}

/// Reads `len` bytes of file `file_id` from `offset`, or to the end when
/// `len` is `None`.
async fn read_range(
    handle: &Arc<ManagedTorrent>,
    file_id: usize,
    offset: u64,
    len: Option<usize>,
) -> anyhow::Result<Vec<u8>> {
    let mut stream = bounded("opening a stream", handle.clone().stream(file_id)).await?;
    bounded("seeking", async {
        Ok(stream.seek(SeekFrom::Start(offset)).await?)
    })
    .await?;
    let mut bytes = Vec::new();
    match len {
        Some(len) => {
            bytes.resize(len, 0);
            bounded("reading a range", async {
                Ok(stream.read_exact(&mut bytes).await?)
            })
            .await?;
        }
        None => {
            bounded("reading to the end", async {
                Ok(stream.read_to_end(&mut bytes).await?)
            })
            .await?;
        }
    }
    Ok(bytes)
}

/// Reads every file of `spec` and checks it byte for byte.
async fn check_every_file(handle: &Arc<ManagedTorrent>, spec: &FixtureSpec) -> anyhow::Result<()> {
    for (index, file) in spec.files().iter().enumerate() {
        let bytes = read_range(handle, index, 0, None).await?;
        assert_eq!(bytes.len() as u64, file.size_bytes, "file {index}");
        assert_eq!(
            generator::first_mismatch(spec.file_seed(index), 0, &bytes),
            None,
            "file {index}"
        );
    }
    Ok(())
}

/// Waits until `condition` holds, checking every 20 ms.
async fn eventually<T>(
    what: &str,
    mut probe: impl FnMut() -> anyhow::Result<Option<T>>,
) -> anyhow::Result<T> {
    bounded(what, async {
        loop {
            if let Some(value) = probe()? {
                return Ok(value);
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
}

async fn cache_reaches(
    cache: &PieceCache,
    what: &str,
    condition: impl Fn(&CacheStats) -> bool,
) -> anyhow::Result<CacheStats> {
    eventually(what, || {
        let stats = cache.audit()?;
        Ok(condition(&stats).then_some(stats))
    })
    .await
}

fn live_stats(handle: &ManagedTorrent) -> anyhow::Result<LiveStats> {
    handle.stats().live.context("the torrent is not live")
}

/// Deletes a torrent and releases its cache usage, as the adapter will.
async fn delete(
    leecher: &Leecher,
    handle: Arc<ManagedTorrent>,
    cached: CachedTorrent,
) -> anyhow::Result<()> {
    bounded(
        "deleting a torrent",
        leecher
            .session
            .delete(TorrentIdOrHash::Id(handle.id()), false),
    )
    .await?;
    Ok(cached.release()?)
}

fn assert_not_failed(stats: &TorrentStats) {
    assert!(
        !matches!(stats.state, TorrentStatsState::Error),
        "the torrent failed: {:?}",
        stats.error
    );
}

/// CAC-01: a file four times the cache capacity streams byte for byte, and
/// the start of the file, long evicted, streams again.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn streaming_four_times_the_capacity_evicts_and_downloads_again() -> anyhow::Result<()> {
    let capacity = 8 * MIB;
    let spec = catalog::large(capacity);
    let seeder = Seeder::start(&spec, &spec.metainfo().bytes).await?;
    let leecher = Leecher::start().await?;
    let cache = PieceCache::new(capacity);
    let factory = MemoryStorageFactory::new(cache.clone());
    let (handle, cached) = add_in_memory(&leecher, &seeder, &spec, &factory).await?;

    check_every_file(&handle, &spec).await?;
    let stats = cache.audit()?;
    let pieces_beyond_capacity = (spec.total_size_bytes() - capacity) / MIB;
    assert!(
        stats.eviction_count >= pieces_beyond_capacity,
        "{} evictions for a file {pieces_beyond_capacity} pieces larger than the cache",
        stats.eviction_count
    );
    let first = read_range(&handle, 0, 0, Some(MIB as usize)).await?;
    assert_eq!(
        generator::first_mismatch(spec.file_seed(0), 0, &first),
        None
    );
    assert_not_failed(&handle.stats());
    assert_eq!(live_stats(&handle)?.snapshot.peer_stats.dead, 0);

    delete(&leecher, handle, cached).await?;
    cache_reaches(&cache, "the cache emptied", |stats| stats.usage_bytes == 0).await?;
    leecher.session.stop().await;
    seeder.session.stop().await;
    Ok(())
}

/// CAC-02 and CAC-03: evicting verified pieces of a finished torrent, whose
/// seeder was dropped as not needed, marks them missing; reading them
/// reconnects the seeder and downloads and verifies them again, and no
/// bytes are wasted on failed hash checks.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_evicted_piece_is_marked_missing_and_downloaded_again() -> anyhow::Result<()> {
    let spec = catalog::single_small();
    let piece = u64::from(spec.piece_length);
    let total = spec.total_size_bytes();
    let seeder = Seeder::start(&spec, &spec.metainfo().bytes).await?;
    let leecher = Leecher::start().await?;
    let cache = PieceCache::new(4 * MIB);
    let factory = MemoryStorageFactory::new(cache.clone());
    let (handle, cached) = add_in_memory(&leecher, &seeder, &spec, &factory).await?;
    bounded("completing the download", handle.wait_until_completed()).await?;
    eventually("the seeder dropped as not needed", || {
        let peers = live_stats(&handle)?.snapshot.peer_stats;
        Ok((peers.live == 0 && peers.not_needed == 1).then_some(()))
    })
    .await?;

    assert_eq!(cached.evict(&[5, 6])?, 2);
    assert_eq!(
        cached.evict(&[5])?,
        0,
        "a missing piece is not evicted twice"
    );
    let stats = cache.audit()?;
    assert_eq!(
        (stats.usage_bytes, stats.eviction_count),
        (total - 2 * piece, 2)
    );
    let torrent = handle.stats();
    assert_eq!(torrent.progress_bytes, total - 2 * piece);
    assert!(!torrent.finished);

    check_every_file(&handle, &spec).await?;
    bounded(
        "completing the download again",
        handle.wait_until_completed(),
    )
    .await?;
    assert_eq!(handle.stats().progress_bytes, total);
    assert_eq!(cache.audit()?.usage_bytes, total);
    let snapshot = live_stats(&handle)?.snapshot;
    assert_eq!(
        snapshot.fetched_bytes, snapshot.downloaded_and_checked_bytes,
        "every fetched byte passed its hash check"
    );
    assert_eq!(snapshot.downloaded_and_checked_bytes, total + 2 * piece);

    leecher.session.stop().await;
    seeder.session.stop().await;
    Ok(())
}

/// CAC-04: pieces evicted while a torrent is paused count as missing after
/// it starts, and are downloaded again.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pieces_evicted_while_paused_are_missing_after_starting() -> anyhow::Result<()> {
    let spec = catalog::single_small();
    let piece = u64::from(spec.piece_length);
    let total = spec.total_size_bytes();
    let seeder = Seeder::start(&spec, &spec.metainfo().bytes).await?;
    let leecher = Leecher::start().await?;
    let cache = PieceCache::new(4 * MIB);
    let factory = MemoryStorageFactory::new(cache.clone());
    let (handle, cached) = add_in_memory(&leecher, &seeder, &spec, &factory).await?;
    bounded("completing the download", handle.wait_until_completed()).await?;

    bounded("pausing", leecher.session.pause(&handle)).await?;
    assert_eq!(cached.evict(&[0, 63])?, 2);
    assert_eq!(handle.stats().progress_bytes, total - 2 * piece);
    bounded("starting", leecher.session.unpause(&handle)).await?;
    let torrent = handle.stats();
    assert_eq!(torrent.progress_bytes, total - 2 * piece);
    assert!(!torrent.finished);

    check_every_file(&handle, &spec).await?;
    bounded("completing the download", handle.wait_until_completed()).await?;
    assert_eq!(cache.audit()?.usage_bytes, total);

    leecher.session.stop().await;
    seeder.session.stop().await;
    Ok(())
}

/// CAC-08: when a started torrent needs room, the pieces of a paused torrent
/// are evicted before any of its own.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_paused_torrent_is_evicted_before_a_started_one() -> anyhow::Result<()> {
    let paused_spec = catalog::single_small();
    let started_spec = catalog::multi_boundary();
    let paused_seeder = Seeder::start(&paused_spec, &paused_spec.metainfo().bytes).await?;
    let started_seeder = Seeder::start(&started_spec, &started_spec.metainfo().bytes).await?;
    let leecher = Leecher::start().await?;
    let capacity = 2 * MIB;
    let cache = PieceCache::new(capacity);
    let factory = MemoryStorageFactory::new(cache.clone());

    let (paused, paused_cached) =
        add_in_memory(&leecher, &paused_seeder, &paused_spec, &factory).await?;
    bounded(
        "completing the first download",
        paused.wait_until_completed(),
    )
    .await?;
    bounded("pausing", leecher.session.pause(&paused)).await?;
    paused_cached.set_started(false)?;

    let (started, _) = add_in_memory(&leecher, &started_seeder, &started_spec, &factory).await?;
    check_every_file(&started, &started_spec).await?;
    bounded(
        "completing the second download",
        started.wait_until_completed(),
    )
    .await?;

    let started_stats = started.stats();
    assert_eq!(
        started_stats.progress_bytes,
        started_spec.total_size_bytes()
    );
    let lost = paused_spec.total_size_bytes() - paused.stats().progress_bytes;
    let needed =
        (paused_spec.total_size_bytes() + started_spec.total_size_bytes()).saturating_sub(capacity);
    assert!(
        lost >= needed,
        "the paused torrent lost {lost} bytes, {needed} were needed"
    );
    let stats = cache.audit()?;
    assert_eq!(
        stats.eviction_count,
        lost / u64::from(paused_spec.piece_length),
        "every eviction was a piece of the paused torrent"
    );

    leecher.session.stop().await;
    paused_seeder.session.stop().await;
    started_seeder.session.stop().await;
    Ok(())
}

/// CAC-09: opening, streaming, and deleting 100 torrents in a loop returns
/// cache usage, torrents, and peer connections to their starting levels.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn opening_and_deleting_torrents_in_a_loop_leaves_nothing_behind() -> anyhow::Result<()> {
    let spec = catalog::single_small();
    let seeder = Seeder::start(&spec, &spec.metainfo().bytes).await?;
    let leecher = Leecher::start().await?;
    let cache = PieceCache::new(512 * KIB);
    let factory = MemoryStorageFactory::new(cache.clone());

    for round in 0..100 {
        let (handle, cached) = add_in_memory(&leecher, &seeder, &spec, &factory).await?;
        let offset = (round * 37 * KIB) % (spec.total_size_bytes() - 64 * KIB);
        let bytes = read_range(&handle, 0, offset, Some(64 * KIB as usize))
            .await
            .with_context(|| {
                format!(
                    "round {round}: {:?} {:?}",
                    handle.stats().state,
                    handle.stats().error
                )
            })?;
        assert_eq!(
            generator::first_mismatch(spec.file_seed(0), offset, &bytes),
            None,
            "round {round}"
        );
        delete(&leecher, handle, cached).await?;
    }

    cache_reaches(&cache, "the cache emptied", |stats| {
        stats.usage_bytes == 0 && stats.resident_pieces == 0
    })
    .await?;
    let torrents = leecher.session.with_torrents(|torrents| torrents.count());
    assert_eq!(torrents, 0);
    eventually("the seeder's connections closed", || {
        let peers = live_stats(&seeder.handle)?.snapshot.peer_stats;
        Ok((peers.live == 0 && peers.connecting == 0).then_some(()))
    })
    .await?;

    leecher.session.stop().await;
    seeder.session.stop().await;
    Ok(())
}

/// Readers of two torrents that evict each other's pieces all finish byte
/// for byte, which a lock-order inversion between the cache and a torrent's
/// state would turn into a timeout, and a read racing an eviction into a
/// mismatch.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_readers_of_two_torrents_progress_under_eviction() -> anyhow::Result<()> {
    let capacity = 8 * MIB;
    let large = catalog::large(capacity);
    let multi = catalog::multi_boundary();
    let large_seeder = Seeder::start(&large, &large.metainfo().bytes).await?;
    let multi_seeder = Seeder::start(&multi, &multi.metainfo().bytes).await?;
    let leecher = Leecher::start().await?;
    let cache = PieceCache::new(capacity);
    let factory = MemoryStorageFactory::new(cache.clone());
    let (large_handle, _large_cached) =
        add_in_memory(&leecher, &large_seeder, &large, &factory).await?;
    let (multi_handle, _multi_cached) =
        add_in_memory(&leecher, &multi_seeder, &multi, &factory).await?;

    let half = large.total_size_bytes() / 2;
    let readers = [
        (large_handle.clone(), large.clone(), 0, 0),
        (large_handle.clone(), large.clone(), 0, half),
        (multi_handle.clone(), multi.clone(), 2, 0),
        (multi_handle.clone(), multi.clone(), 3, 0),
    ]
    .map(|(handle, spec, file_id, offset)| {
        tokio::spawn(async move {
            for _ in 0..2 {
                let bytes = read_range(&handle, file_id, offset, None).await?;
                anyhow::ensure!(
                    generator::first_mismatch(spec.file_seed(file_id), offset, &bytes).is_none(),
                    "file {file_id} from {offset} differs"
                );
            }
            anyhow::Ok(())
        })
    });
    // Each read is bounded, so a deadlock fails a read without bounding the
    // whole run, which slow runners need.
    for reader in readers {
        reader.await??;
    }
    assert!(cache.audit()?.eviction_count > 0);
    assert_not_failed(&large_handle.stats());
    assert_not_failed(&multi_handle.stats());

    leecher.session.stop().await;
    large_seeder.session.stop().await;
    multi_seeder.session.stop().await;
    Ok(())
}
