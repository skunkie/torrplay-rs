// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! Preloads through the engine facade, against local seeders: what they
//! cover, pin, and queue for, and how they end.

mod support;

use std::time::Duration;

use support::{
    MIB, Seeder, bounded, eventually, expected, futures_now, id, range, start, start_with, upload,
};
use tokio::io::AsyncReadExt;
use torrplay_core::engine::{Engine, EngineError};
use torrplay_core::id::{PreloadId, TorrentId};
use torrplay_core::lifecycle::{DesiredState, StorageMode};
use torrplay_core::metadata::FileIndex;
use torrplay_core::preload::{PreloadRequest, PreloadState, PreloadStatus};
use torrplay_engine::LibrqbitEngine;
use torrplay_fixtures::catalog;

fn preload_id(n: u8) -> PreloadId {
    PreloadId::from_random_bytes([n; 16])
}

fn request(offset_bytes: Option<u64>) -> PreloadRequest {
    PreloadRequest {
        offset_bytes,
        ttl: Duration::from_secs(120),
    }
}

async fn status(
    engine: &LibrqbitEngine,
    torrent: &TorrentId,
    n: u8,
) -> Result<PreloadStatus, EngineError> {
    engine.preload(torrent, FileIndex(0), &preload_id(n)).await
}

/// Waits until preload `n` is in `state`.
async fn reaches(
    engine: &LibrqbitEngine,
    torrent: &TorrentId,
    n: u8,
    state: PreloadState,
) -> anyhow::Result<PreloadStatus> {
    eventually(&format!("preload {n} becoming {}", state.as_str()), || {
        let status = futures_now(status(engine, torrent, n))?;
        Ok((status.state == state).then_some(status))
    })
    .await
}

fn pinned_bytes(engine: &LibrqbitEngine) -> anyhow::Result<u64> {
    Ok(futures_now(engine.cache_statistics())?.pinned_bytes)
}

fn spans(status: &PreloadStatus) -> Vec<(u64, u64)> {
    status
        .ranges
        .iter()
        .map(|range| (range.start(), range.end()))
        .collect()
}

/// PRL-01 and part of PRL-03: a memory torrent's preload without readers
/// fetches its file's head and tail, which stay pinned until it is
/// canceled.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_preload_pins_its_head_and_tail_until_canceled() -> anyhow::Result<()> {
    let spec = catalog::large(64 * MIB);
    let seeder = Seeder::start(&spec, &spec.metainfo().bytes).await?;
    let started = start(vec![seeder.addr], 64 * MIB, Duration::from_secs(30)).await?;
    let engine = &started.engine;
    engine
        .add(&id(1), &upload(&spec), DesiredState::Started)
        .await?;

    let created = engine
        .create_preload(&id(1), FileIndex(0), preload_id(1), request(None))
        .await?;
    // Half of the 32 MiB readahead budget, the last 4 MiB as the tail.
    let size = spec.files()[0].size_bytes;
    assert_eq!(spans(&created), [(0, 12 * MIB), (size - 4 * MIB, size)]);
    assert_eq!(created.target_bytes, 16 * MIB);
    let ready = reaches(engine, &id(1), 1, PreloadState::Ready).await?;
    assert_eq!(ready.completed_bytes, ready.target_bytes);
    assert!(pinned_bytes(engine)? >= 16 * MIB, "its pieces are pinned");
    let statistics = engine.torrent_statistics(&id(1)).await?;
    assert!(
        statistics.downloaded_bytes < 32 * MIB,
        "it fetches its ranges, not the file"
    );

    engine
        .cancel_preload(&id(1), FileIndex(0), &preload_id(1))
        .await?;
    assert!(engine.preloads(&id(1), FileIndex(0)).await?.is_empty());
    assert!(matches!(
        engine.preload(&id(1), FileIndex(0), &preload_id(1)).await,
        Err(EngineError::UnknownPreload)
    ));
    eventually("the pins going", || {
        Ok((pinned_bytes(engine)? == 0).then_some(()))
    })
    .await?;

    engine.shutdown().await?;
    seeder.stop().await;
    Ok(())
}

/// PRL-02: a preload at an offset covers the first and last pieces and a
/// window there, which a stream then reads without a peer.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_preload_at_an_offset_serves_a_stream_there() -> anyhow::Result<()> {
    let spec = catalog::large(64 * MIB);
    let seeder = Seeder::start(&spec, &spec.metainfo().bytes).await?;
    let started = start(vec![seeder.addr], 64 * MIB, Duration::from_secs(2)).await?;
    let engine = &started.engine;
    engine
        .add(&id(1), &upload(&spec), DesiredState::Started)
        .await?;

    let offset = 100 * MIB;
    let created = engine
        .create_preload(&id(1), FileIndex(0), preload_id(1), request(Some(offset)))
        .await?;
    let size = spec.files()[0].size_bytes;
    assert_eq!(
        spans(&created),
        [(0, MIB), (98 * MIB, 112 * MIB), (size - MIB, size)],
        "a 14-piece window starting an eighth of itself before the offset"
    );
    reaches(engine, &id(1), 1, PreloadState::Ready).await?;

    // Without the seeder, only the preload's pieces can serve the stream.
    seeder.stop().await;
    let mut reader = engine
        .open_reader(&id(1), FileIndex(0), range(offset, offset + MIB))
        .await?;
    let mut bytes = Vec::new();
    bounded("reading the preloaded window", async {
        Ok(reader.read_to_end(&mut bytes).await?)
    })
    .await?;
    assert!(bytes == expected(&spec, 0, offset, offset + MIB));

    drop(reader);
    engine.shutdown().await?;
    Ok(())
}

/// PRL-05: preloads of memory torrents queue, in creation order, while the
/// pins of earlier ones fill the preloads' share of the readahead budget.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn preloads_queue_for_their_share_of_the_budget() -> anyhow::Result<()> {
    let spec = catalog::large(8 * MIB);
    let seeder = Seeder::start(&spec, &spec.metainfo().bytes).await?;
    // A 4 MiB budget leaves preloads 2 MiB: two pieces each, one at a time.
    let started = start(vec![seeder.addr], 8 * MIB, Duration::from_secs(30)).await?;
    let engine = &started.engine;
    engine
        .add(&id(1), &upload(&spec), DesiredState::Started)
        .await?;

    for n in 1..=3 {
        let created = engine
            .create_preload(
                &id(1),
                FileIndex(0),
                preload_id(n),
                request(Some(u64::from(n) * 8 * MIB)),
            )
            .await?;
        assert_eq!(created.target_bytes, 2 * MIB);
    }
    reaches(engine, &id(1), 1, PreloadState::Ready).await?;
    assert_eq!(status(engine, &id(1), 2).await?.state, PreloadState::Queued);
    assert_eq!(status(engine, &id(1), 3).await?.state, PreloadState::Queued);
    assert!(
        pinned_bytes(engine)? <= 2 * MIB,
        "pins stay within the share"
    );

    engine
        .cancel_preload(&id(1), FileIndex(0), &preload_id(1))
        .await?;
    reaches(engine, &id(1), 2, PreloadState::Ready).await?;
    assert_eq!(
        status(engine, &id(1), 3).await?.state,
        PreloadState::Queued,
        "the next waits behind the one that left the queue first"
    );
    assert!(pinned_bytes(engine)? <= 2 * MIB);

    engine.shutdown().await?;
    seeder.stop().await;
    Ok(())
}

/// PRL-03: a preload expires at its time to live, releasing its pins, and
/// stays listed as expired.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_preload_expires_at_its_time_to_live() -> anyhow::Result<()> {
    let spec = catalog::single_small();
    let seeder = Seeder::start(&spec, &spec.metainfo().bytes).await?;
    let started = start(vec![seeder.addr], 8 * MIB, Duration::from_secs(30)).await?;
    let engine = &started.engine;
    engine
        .add(&id(1), &upload(&spec), DesiredState::Started)
        .await?;
    let created = engine
        .create_preload(
            &id(1),
            FileIndex(0),
            preload_id(1),
            PreloadRequest {
                offset_bytes: None,
                ttl: Duration::from_secs(10),
            },
        )
        .await?;
    reaches(engine, &id(1), 1, PreloadState::Ready).await?;
    assert!(pinned_bytes(engine)? > 0);

    let expired = reaches(engine, &id(1), 1, PreloadState::Expired).await?;
    let ended = expired.ended_at.expect("an end time");
    assert!(ended >= created.expires_at);
    assert!(expired.failure.is_none());
    eventually("the pins going", || {
        Ok((pinned_bytes(engine)? == 0).then_some(()))
    })
    .await?;
    assert_eq!(engine.preloads(&id(1), FileIndex(0)).await?.len(), 1);

    engine.shutdown().await?;
    seeder.stop().await;
    Ok(())
}

/// PRL-07: a paused torrent takes no preload, pausing fails a torrent's
/// preloads, and a preload without peers fails after the piece wait bound.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn preloads_fail_when_paused_or_stalled() -> anyhow::Result<()> {
    let spec = catalog::single_small();
    let started = start(Vec::new(), 8 * MIB, Duration::from_secs(1)).await?;
    let engine = &started.engine;
    engine
        .add(&id(1), &upload(&spec), DesiredState::Paused)
        .await?;
    assert!(matches!(
        engine
            .create_preload(&id(1), FileIndex(0), preload_id(1), request(None))
            .await,
        Err(EngineError::NotStarted)
    ));

    engine
        .set_desired_state(&id(1), DesiredState::Started)
        .await?;
    engine
        .create_preload(&id(1), FileIndex(0), preload_id(1), request(None))
        .await?;
    let stalled = reaches(engine, &id(1), 1, PreloadState::Failed).await?;
    assert_eq!(
        stalled.failure.map(|failure| failure.detail).as_deref(),
        Some("No piece of the preload arrived within the piece wait bound.")
    );

    engine
        .create_preload(&id(1), FileIndex(0), preload_id(2), request(None))
        .await?;
    engine
        .set_desired_state(&id(1), DesiredState::Paused)
        .await?;
    let paused = reaches(engine, &id(1), 2, PreloadState::Failed).await?;
    assert_eq!(
        paused.failure.map(|failure| failure.detail).as_deref(),
        Some("The torrent was paused.")
    );

    engine.shutdown().await?;
    Ok(())
}

/// PRL-06: when active readers' current pieces need the cache, its bounded
/// fallback ends the preload that pins pieces, and the readers read.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_cache_fallback_ends_preloads_for_active_readers() -> anyhow::Result<()> {
    let preloaded = catalog::single_small();
    let streamed = catalog::large(4 * MIB);
    let first = Seeder::start(&preloaded, &preloaded.metainfo().bytes).await?;
    let second = Seeder::start(&streamed, &streamed.metainfo().bytes).await?;
    let started = start(
        vec![first.addr, second.addr],
        4 * MIB,
        Duration::from_secs(30),
    )
    .await?;
    let engine = &started.engine;
    engine
        .add(&id(1), &upload(&preloaded), DesiredState::Started)
        .await?;
    engine
        .add(&id(2), &upload(&streamed), DesiredState::Started)
        .await?;
    engine
        .create_preload(&id(1), FileIndex(0), preload_id(1), request(None))
        .await?;
    reaches(engine, &id(1), 1, PreloadState::Ready).await?;

    // Four readers' current pieces fill the 4 MiB cache.
    let mut readers = Vec::new();
    for n in 0..4 {
        let start = n * 4 * MIB;
        let mut reader = engine
            .open_reader(&id(2), FileIndex(0), range(start, start + 4 * MIB))
            .await?;
        let mut first_bytes = vec![0; 1000];
        bounded("a reader's first bytes", async {
            Ok(reader.read_exact(&mut first_bytes).await?)
        })
        .await?;
        assert!(first_bytes == expected(&streamed, 0, start, start + 1000));
        readers.push(reader);
    }
    let ended = status(engine, &id(1), 1).await?;
    assert_eq!(ended.state, PreloadState::Failed);
    assert_eq!(
        ended.failure.map(|failure| failure.detail).as_deref(),
        Some("The piece cache needed the preload's pins for active readers.")
    );

    drop(readers);
    engine.shutdown().await?;
    first.stop().await;
    second.stop().await;
    Ok(())
}

/// PRL-09: a disk torrent's preload never queues and pins nothing; a
/// transition keeps it, and deleting the torrent ends it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_disk_preload_pins_nothing_and_survives_a_transition() -> anyhow::Result<()> {
    let spec = catalog::large(8 * MIB);
    let seeder = Seeder::start(&spec, &spec.metainfo().bytes).await?;
    let downloads = tempfile::tempdir()?;
    let started = start_with(vec![seeder.addr], |config| {
        config.download_dir = downloads.path().to_path_buf();
    })
    .await?;
    let engine = &started.engine;
    engine
        .add_in(
            &id(1),
            &upload(&spec),
            DesiredState::Started,
            StorageMode::Disk,
        )
        .await?;
    for n in 1..=3 {
        let created = engine
            .create_preload(&id(1), FileIndex(0), preload_id(n), request(None))
            .await?;
        assert_eq!(created.state, PreloadState::Downloading, "it never queues");
    }
    reaches(engine, &id(1), 3, PreloadState::Ready).await?;
    assert_eq!(pinned_bytes(engine)?, 0, "disk storage pins nothing");

    engine
        .change_storage_mode(&id(1), StorageMode::Memory)
        .await?;
    let kept = engine.preloads(&id(1), FileIndex(0)).await?;
    assert_eq!(kept.len(), 3);
    assert!(kept.iter().all(|preload| !preload.state.is_ended()));

    engine.remove(&id(1)).await?;
    assert!(matches!(
        engine.preloads(&id(1), FileIndex(0)).await,
        Err(EngineError::UnknownTorrent)
    ));

    engine.shutdown().await?;
    seeder.stop().await;
    Ok(())
}
