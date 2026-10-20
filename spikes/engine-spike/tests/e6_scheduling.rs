// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! E6: scheduling. Torrents use the engine's scheduler as their piece
//! selector (patch P4), so they download only what their readers need, and
//! reserve each piece's room before requesting it. The scheduler's own unit
//! tests cover highest-wins priorities (RDR-05) and pin tiers.

use std::io::SeekFrom;
use std::num::NonZeroU32;
use std::time::{Duration, Instant};

use engine_spike::{Leecher, MemoryCache, ScheduledTorrent, Seeder, bounded};
use librqbit::{PeerConnectionOptions, TorrentStatsState};
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use torrplay_engine::scheduler::Reader;
use torrplay_fixtures::{FixtureSpec, catalog, generator};

const KIB: u64 = 1024;
const MIB: u64 = 1024 * KIB;

async fn read_at(reader: &mut Reader, offset: u64, len: u64) -> anyhow::Result<Vec<u8>> {
    let mut bytes = vec![0; usize::try_from(len)?];
    bounded("reading a range", async {
        reader.seek(SeekFrom::Start(offset)).await?;
        Ok(reader.read_exact(&mut bytes).await?)
    })
    .await?;
    Ok(bytes)
}

fn check(spec: &FixtureSpec, file: usize, offset: u64, bytes: &[u8]) {
    assert_eq!(
        generator::first_mismatch(spec.file_seed(file), offset, bytes),
        None,
        "file {file} from {offset}"
    );
}

fn downloaded_bytes(torrent: &ScheduledTorrent) -> u64 {
    torrent
        .handle
        .stats()
        .live
        .map(|live| live.snapshot.downloaded_and_checked_bytes)
        .unwrap_or_default()
}

/// CAC-01 with the scheduler: a file four times the cache capacity streams
/// byte for byte, and the torrent downloads each piece once, because it
/// requests only what the reader needs.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_scheduled_torrent_downloads_only_what_its_reader_needs() -> anyhow::Result<()> {
    let capacity = 8 * MIB;
    let spec = catalog::large(capacity);
    let seeder = Seeder::start(&spec, &spec.metainfo().bytes).await?;
    let leecher = Leecher::start().await?;
    let memory = MemoryCache::start(capacity);
    let torrent = memory
        .add_scheduled(&leecher, &[seeder.addr], &spec, capacity / 2)
        .await?;

    let mut reader = bounded(
        "opening a reader",
        torrent.scheduler.open(&torrent.handle, 0),
    )
    .await?;
    let mut bytes = Vec::new();
    bounded("reading the file", async {
        Ok(reader.read_to_end(&mut bytes).await?)
    })
    .await?;
    check(&spec, 0, 0, &bytes);
    let stats = memory.cache.audit()?;
    assert!(stats.eviction_count >= (spec.total_size_bytes() - capacity) / MIB);
    assert_eq!(
        downloaded_bytes(&torrent),
        spec.total_size_bytes(),
        "every piece downloaded once"
    );

    drop(reader);
    leecher.session.stop().await;
    seeder.session.stop().await;
    memory.shutdown().await?;
    Ok(())
}

/// CAC-10: while a reader of a `multi-boundary` file exists, the first and
/// last pieces of its file stay resident through sustained eviction by
/// another torrent's reader, while an unpinned piece it read is evicted.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn file_boundary_pieces_stay_resident_while_read() -> anyhow::Result<()> {
    let multi = catalog::multi_boundary();
    let other = catalog::single_small();
    let multi_seeder = Seeder::start(&multi, &multi.metainfo().bytes).await?;
    let other_seeder = Seeder::start(&other, &other.metainfo().bytes).await?;
    let leecher = Leecher::start().await?;
    let memory = MemoryCache::start(1536 * KIB);
    let held = memory
        .add_scheduled(&leecher, &[multi_seeder.addr], &multi, MIB)
        .await?;
    let pressing = memory
        .add_scheduled(&leecher, &[other_seeder.addr], &other, 256 * KIB)
        .await?;

    // part-4.bin starts in piece 3 and ends in piece 6; stop the reader in
    // piece 5, past piece 4.
    let file = 3;
    let mut reader = bounded("opening a reader", held.scheduler.open(&held.handle, file)).await?;
    let bytes = read_at(&mut reader, 0, 500_000).await?;
    check(&multi, file, 0, &bytes);
    bounded("the last piece arriving", async {
        while !held.cached.is_resident(6)? {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        Ok(())
    })
    .await?;

    let mut other_reader = bounded(
        "opening another reader",
        pressing.scheduler.open(&pressing.handle, 0),
    )
    .await?;
    for _ in 0..3 {
        let bytes = read_at(&mut other_reader, 0, other.total_size_bytes()).await?;
        check(&other, 0, 0, &bytes);
    }

    let stats = memory.cache.audit()?;
    assert!(stats.eviction_count > 0, "the other reader evicted pieces");
    assert!(
        !held.cached.is_resident(4)?,
        "the unpinned piece read earlier was evicted"
    );
    for piece in [3, 5, 6] {
        assert!(
            held.cached.is_resident(piece)?,
            "pinned piece {piece} was evicted"
        );
    }
    let more = read_at(&mut reader, 500_000, 100_000).await?;
    check(&multi, file, 500_000, &more);

    drop((reader, other_reader));
    leecher.session.stop().await;
    multi_seeder.session.stop().await;
    other_seeder.session.stop().await;
    memory.shutdown().await?;
    Ok(())
}

/// CAC-05: six readers whose minimum windows need more than the cache
/// capacity all finish byte for byte, and cache usage never exceeds
/// capacity.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn readers_whose_windows_exceed_capacity_all_progress() -> anyhow::Result<()> {
    let capacity = 4 * MIB;
    let spec = catalog::large(capacity);
    let seeder = Seeder::start(&spec, &spec.metainfo().bytes).await?;
    let leecher = Leecher::start().await?;
    let memory = MemoryCache::start(capacity);
    let torrent = memory
        .add_scheduled(&leecher, &[seeder.addr], &spec, 2 * MIB)
        .await?;

    let readers: Vec<_> = (0..6u64)
        .map(|index| {
            let scheduler = torrent.scheduler.clone();
            let handle = torrent.handle.clone();
            let spec = spec.clone();
            tokio::spawn(async move {
                let mut reader = bounded("opening a reader", scheduler.open(&handle, 0)).await?;
                let offset = index * 2 * MIB + 1000;
                let bytes = read_at(&mut reader, offset, 2 * MIB).await?;
                anyhow::ensure!(
                    generator::first_mismatch(spec.file_seed(0), offset, &bytes).is_none(),
                    "reader {index} read wrong bytes"
                );
                anyhow::Ok(())
            })
        })
        .collect();
    for reader in readers {
        reader.await??;
    }
    memory.cache.audit()?;

    leecher.session.stop().await;
    seeder.session.stop().await;
    memory.shutdown().await?;
    Ok(())
}

/// CAC-06: with a piece longer than the cache capacity, nothing is
/// requested: no write fails the torrent, a reader waits, and cache usage
/// stays at zero.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_piece_longer_than_the_capacity_is_never_requested() -> anyhow::Result<()> {
    let spec = catalog::oversized_piece();
    let seeder = Seeder::start(&spec, &spec.metainfo().bytes).await?;
    let leecher = Leecher::start().await?;
    let memory = MemoryCache::start(8 * MIB);
    let torrent = memory
        .add_scheduled(&leecher, &[seeder.addr], &spec, 4 * MIB)
        .await?;

    let mut reader = bounded(
        "opening a reader",
        torrent.scheduler.open(&torrent.handle, 0),
    )
    .await?;
    let mut byte = [0];
    let read = tokio::time::timeout(Duration::from_secs(3), reader.read_exact(&mut byte)).await;
    assert!(read.is_err(), "the reader waits");
    let stats = memory.cache.audit()?;
    assert_eq!(
        (
            stats.usage_bytes - stats.retained_bytes,
            stats.waiting_pieces
        ),
        (0, 0)
    );
    assert!(!matches!(
        torrent.handle.stats().state,
        TorrentStatsState::Error
    ));
    assert_eq!(downloaded_bytes(&torrent), 0);

    drop(reader);
    leecher.session.stop().await;
    seeder.session.stop().await;
    memory.shutdown().await?;
    Ok(())
}

/// Records seek latency: time to first byte after a seek far ahead, with
/// the download rate limited so that requests already outstanding take a
/// while, for librqbit's default request window of 128 blocks and for 16.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn records_seek_latency_by_request_window() -> anyhow::Result<()> {
    let capacity = 16 * MIB;
    let spec = catalog::large(capacity);
    let seeder = Seeder::start(&spec, &spec.metainfo().bytes).await?;
    for window in [None, Some(16)] {
        let leecher = Leecher::start().await?;
        let memory = MemoryCache::start(capacity);
        let torrent = memory
            .add_scheduled_with(&leecher, &[seeder.addr], &spec, 8 * MIB, |options| {
                options.peer_opts = Some(PeerConnectionOptions {
                    max_request_window: window,
                    ..Default::default()
                });
                options.ratelimits.download_bps = NonZeroU32::new(16 * MIB as u32);
            })
            .await?;
        let mut reader = bounded(
            "opening a reader",
            torrent.scheduler.open(&torrent.handle, 0),
        )
        .await?;
        let mut latencies = Vec::new();
        for seek in 0..4u64 {
            // Read a little so the window fills with requests, then jump.
            let _ = read_at(&mut reader, seek * 13 * MIB % spec.total_size_bytes(), 1).await?;
            let target = (seek * 13 + 29) * MIB % spec.total_size_bytes() + 777;
            let started = Instant::now();
            let byte = read_at(&mut reader, target, 1).await?;
            latencies.push(started.elapsed());
            check(&spec, 0, target, &byte);
        }
        latencies.sort();
        eprintln!("seek time to first byte, request window {window:?}: {latencies:?}");
        drop(reader);
        leecher.session.stop().await;
        memory.shutdown().await?;
    }
    seeder.session.stop().await;
    Ok(())
}
