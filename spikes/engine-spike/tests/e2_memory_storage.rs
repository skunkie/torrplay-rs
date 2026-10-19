// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! E2: memory storage over the piece cache, without eviction. A leecher with
//! memory storage downloads from a local seeder with disk storage; the cache
//! accountant is audited after each phase.

use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use engine_spike::{Leecher, Seeder, bounded};
use librqbit::api::TorrentIdOrHash;
use librqbit::{AddTorrent, AddTorrentOptions, ManagedTorrent, TorrentStatsState};
use tokio::io::AsyncReadExt;
use torrplay_engine::cache::{CacheStats, PieceCache};
use torrplay_engine::storage::MemoryStorageFactory;
use torrplay_fixtures::{FixtureSpec, catalog, generator};

const MIB: u64 = 1024 * 1024;

/// Adds `spec` to the leecher with memory storage over `cache`, with the
/// seeder as its only peer.
async fn add_in_memory(
    leecher: &Leecher,
    seeder: &Seeder,
    spec: &FixtureSpec,
    cache: &Arc<PieceCache>,
) -> anyhow::Result<Arc<ManagedTorrent>> {
    let options = AddTorrentOptions {
        storage_factory: Some(MemoryStorageFactory::new(cache.clone()).into_boxed()),
        ..leecher.add_options(&[seeder.addr])?
    };
    bounded(
        "adding a torrent with memory storage",
        leecher
            .session
            .add_torrent(AddTorrent::from_bytes(spec.metainfo().bytes), Some(options)),
    )
    .await?
    .into_handle()
    .context("the torrent was not added")
}

async fn read_file(handle: &Arc<ManagedTorrent>, file_id: usize) -> anyhow::Result<Vec<u8>> {
    let mut stream = bounded("opening a stream", handle.clone().stream(file_id)).await?;
    let mut bytes = Vec::new();
    bounded("reading a file", async {
        Ok(stream.read_to_end(&mut bytes).await?)
    })
    .await?;
    Ok(bytes)
}

/// Waits until `condition` holds for the cache, checking every 20 ms.
async fn cache_reaches(
    cache: &PieceCache,
    what: &str,
    condition: impl Fn(&CacheStats) -> bool,
) -> anyhow::Result<CacheStats> {
    bounded(what, async {
        loop {
            let stats = cache.audit()?;
            if condition(&stats) {
                return Ok(stats);
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn memory_storage_streams_byte_for_byte() -> anyhow::Result<()> {
    let spec = catalog::multi_boundary();
    let seeder = Seeder::start(&spec, &spec.metainfo().bytes).await?;
    let leecher = Leecher::start().await?;
    let cache = PieceCache::new(4 * MIB);
    let handle = add_in_memory(&leecher, &seeder, &spec, &cache).await?;

    for (index, file) in spec.files().iter().enumerate() {
        let bytes = read_file(&handle, index).await?;
        assert_eq!(bytes.len() as u64, file.size_bytes, "file {index}");
        assert_eq!(
            generator::first_mismatch(spec.file_seed(index), 0, &bytes),
            None,
            "file {index}"
        );
    }
    bounded("completing the download", handle.wait_until_completed()).await?;
    let stats = cache.audit()?;
    assert_eq!(
        stats.usage_bytes,
        spec.total_size_bytes(),
        "every piece is resident once"
    );
    assert_eq!(stats.verified_pieces, spec.metainfo().piece_count);
    assert!(stats.peak_usage_bytes <= stats.capacity_bytes);

    leecher.session.stop().await;
    seeder.session.stop().await;
    Ok(())
}

/// Records ADR 2's finding that a failed write fails the whole torrent:
/// without eviction, a cache smaller than the torrent ends in `Error`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_torrent_larger_than_capacity_fails_without_eviction() -> anyhow::Result<()> {
    let spec = catalog::single_small();
    let seeder = Seeder::start(&spec, &spec.metainfo().bytes).await?;
    let leecher = Leecher::start().await?;
    let cache = PieceCache::new(4 * u64::from(spec.piece_length));
    let handle = add_in_memory(&leecher, &seeder, &spec, &cache).await?;

    bounded("the torrent failing", async {
        loop {
            if matches!(handle.stats().state, TorrentStatsState::Error) {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await?;
    eprintln!("error state: {:?}", handle.stats().error);
    let stats = cache.audit()?;
    assert!(
        stats.peak_usage_bytes <= stats.capacity_bytes,
        "the capacity holds even as the torrent fails"
    );

    leecher.session.stop().await;
    seeder.session.stop().await;
    Ok(())
}

/// The basis for LIF-07 and CAC-09: deleting a torrent returns its bytes.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn deleting_a_torrent_releases_only_its_cache_usage() -> anyhow::Result<()> {
    let small = catalog::single_small();
    let multi = catalog::multi_boundary();
    let small_seeder = Seeder::start(&small, &small.metainfo().bytes).await?;
    let multi_seeder = Seeder::start(&multi, &multi.metainfo().bytes).await?;
    let leecher = Leecher::start().await?;
    let cache = PieceCache::new(8 * MIB);
    let small_handle = add_in_memory(&leecher, &small_seeder, &small, &cache).await?;
    let multi_handle = add_in_memory(&leecher, &multi_seeder, &multi, &cache).await?;
    bounded(
        "completing the first download",
        small_handle.wait_until_completed(),
    )
    .await?;
    bounded(
        "completing the second download",
        multi_handle.wait_until_completed(),
    )
    .await?;
    let both = small.total_size_bytes() + multi.total_size_bytes();
    cache_reaches(&cache, "both torrents resident", |stats| {
        stats.usage_bytes == both
    })
    .await?;

    bounded(
        "deleting the first torrent",
        leecher
            .session
            .delete(TorrentIdOrHash::Id(small_handle.id()), false),
    )
    .await?;
    drop(small_handle);
    let stats = cache_reaches(&cache, "the first torrent's bytes released", |stats| {
        stats.usage_bytes == multi.total_size_bytes()
    })
    .await?;
    assert_eq!(stats.verified_pieces, multi.metainfo().piece_count);
    let bytes = read_file(&multi_handle, 3).await?;
    assert_eq!(
        generator::first_mismatch(multi.file_seed(3), 0, &bytes),
        None
    );

    bounded(
        "deleting the second torrent",
        leecher
            .session
            .delete(TorrentIdOrHash::Id(multi_handle.id()), false),
    )
    .await?;
    drop(multi_handle);
    cache_reaches(&cache, "the cache emptied", |stats| stats.usage_bytes == 0).await?;

    leecher.session.stop().await;
    small_seeder.session.stop().await;
    multi_seeder.session.stop().await;
    Ok(())
}

/// The basis for the lifecycle's pause rule: cached pieces survive a pause,
/// and librqbit's `take` moves them to the paused state intact.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pausing_keeps_cached_pieces_readable() -> anyhow::Result<()> {
    let spec = catalog::multi_boundary();
    let seeder = Seeder::start(&spec, &spec.metainfo().bytes).await?;
    let leecher = Leecher::start().await?;
    let cache = PieceCache::new(4 * MIB);
    let handle = add_in_memory(&leecher, &seeder, &spec, &cache).await?;
    bounded("completing the download", handle.wait_until_completed()).await?;
    let before = cache.audit()?;

    bounded("pausing", leecher.session.pause(&handle)).await?;
    let paused = cache.audit()?;
    assert_eq!(
        paused.usage_bytes, before.usage_bytes,
        "pausing keeps cached pieces"
    );
    // Stop the seeder, so reads after resuming can only come from the cache.
    seeder.session.stop().await;
    bounded("resuming", leecher.session.unpause(&handle)).await?;
    for (index, _) in spec.files().iter().enumerate() {
        let bytes = read_file(&handle, index).await?;
        assert_eq!(
            generator::first_mismatch(spec.file_seed(index), 0, &bytes),
            None,
            "file {index}"
        );
    }
    assert_eq!(cache.audit()?.usage_bytes, before.usage_bytes);

    leecher.session.stop().await;
    Ok(())
}
