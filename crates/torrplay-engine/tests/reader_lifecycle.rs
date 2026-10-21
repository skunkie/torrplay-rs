// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! The reader lifecycle through `LibrqbitEngine`: idle readers that later
//! streams continue, their timeout and limits, readahead shared across
//! torrents, the active playback signal, the cache's idle reader fallback,
//! and streams canceled by pausing and deletion.

// Helpers assert as tests do; clippy's test exemption covers only test
// functions.
#![allow(clippy::expect_used)]

mod support;

use std::io::{ErrorKind, SeekFrom};
use std::time::Duration;

use support::{
    Leecher, MIB, MemoryCache, Seeder, bounded, eventually, expected, id, range, read, start,
    start_with, upload,
};
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tokio::task::JoinHandle;
use torrplay_core::engine::{Engine, EngineError};
use torrplay_core::lifecycle::DesiredState;
use torrplay_core::metadata::FileIndex;
use torrplay_engine::playback::{Playback, PlaybackConfig, PlaybackStats};
use torrplay_engine::{LibrqbitEngine, RangeReader};
use torrplay_fixtures::catalog;

fn stats(engine: &LibrqbitEngine) -> PlaybackStats {
    engine.playback().expect("a started engine").stats()
}

/// Reads `reader` to its end on a task, returning the read's outcome.
fn drain(mut reader: RangeReader) -> JoinHandle<std::io::Result<usize>> {
    tokio::spawn(async move {
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes).await
    })
}

/// RDR-01: a player's sequential range requests continue one reader, which
/// stays idle in between.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sequential_ranges_continue_the_idle_reader() -> anyhow::Result<()> {
    let spec = catalog::single_small();
    let seeder = Seeder::start(&spec, &spec.metainfo().bytes).await?;
    let started = start(vec![seeder.addr], 8 * MIB, Duration::from_secs(30)).await?;
    let engine = &started.engine;
    engine
        .add(&id(1), &upload(&spec), DesiredState::Started)
        .await?;

    for (start, end) in [(0, 100_000), (100_000, 300_000), (300_000, 300_001)] {
        let bytes = read(engine, &id(1), 0, start, end).await?;
        assert_eq!(
            bytes,
            expected(&spec, 0, start, end),
            "bytes {start}..{end}"
        );
        assert_eq!(
            stats(engine),
            PlaybackStats {
                readers: 1,
                idle_readers: 1,
                opened_readers: 1,
            },
            "after bytes {start}..{end}"
        );
    }
    let bytes = read(engine, &id(1), 0, 10, 20).await?;
    assert_eq!(
        bytes,
        expected(&spec, 0, 10, 20),
        "a seek back continues it too"
    );
    assert_eq!(stats(engine).opened_readers, 1);

    engine.shutdown().await?;
    seeder.stop().await;
    Ok(())
}

/// RDR-02: an idle reader is released after the idle timeout with its pins
/// and priorities, and active playback ends with it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn idle_readers_expire_and_end_active_playback() -> anyhow::Result<()> {
    let spec = catalog::single_small();
    let seeder = Seeder::start(&spec, &spec.metainfo().bytes).await?;
    let started = start_with(vec![seeder.addr], |config| {
        config.idle_timeout = Duration::from_millis(300);
    })
    .await?;
    let engine = &started.engine;
    let mut active = engine.playback().expect("a started engine").subscribe();
    assert!(!*active.borrow_and_update());
    engine
        .add(&id(1), &upload(&spec), DesiredState::Started)
        .await?;

    let reader = engine
        .open_reader(&id(1), FileIndex(0), range(0, 50_000))
        .await?;
    assert!(*active.borrow_and_update(), "an active reader is playback");
    let read = bounded("reading a range", async { Ok(drain(reader).await??) }).await?;
    assert_eq!(read, 50_000);
    assert_eq!(stats(engine).idle_readers, 1);
    assert!(*active.borrow(), "an idle reader is still playback");
    assert!(engine.cache_statistics().await?.pinned_bytes > 0);

    bounded("the idle timeout", async {
        active.wait_for(|active| !*active).await?;
        Ok(())
    })
    .await?;
    assert_eq!(
        stats(engine),
        PlaybackStats {
            readers: 0,
            idle_readers: 0,
            opened_readers: 1,
        }
    );
    assert_eq!(engine.cache_statistics().await?.pinned_bytes, 0);

    engine.shutdown().await?;
    seeder.stop().await;
    Ok(())
}

/// RDR-03: past either idle reader limit, the oldest idle reader in that
/// scope is released.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn idle_reader_limits_release_the_oldest_in_their_scope() -> anyhow::Result<()> {
    let started = start_with(Vec::new(), |config| {
        config.idle_readers_per_torrent = 2;
        config.idle_readers_total = 3;
    })
    .await?;
    let engine = &started.engine;
    engine
        .add(
            &id(1),
            &upload(&catalog::single_small()),
            DesiredState::Started,
        )
        .await?;
    engine
        .add(
            &id(2),
            &upload(&catalog::multi_boundary()),
            DesiredState::Started,
        )
        .await?;

    // Readers that end before reading anything become idle all the same.
    let mut first = Vec::new();
    for start in [0, 100_000, 200_000] {
        first.push(
            engine
                .open_reader(&id(1), FileIndex(0), range(start, start + 1))
                .await?,
        );
    }
    drop(first);
    assert_eq!(
        (stats(engine).idle_readers, stats(engine).readers),
        (2, 2),
        "a third idle reader in one torrent releases its oldest"
    );
    let second = [
        engine
            .open_reader(&id(2), FileIndex(0), range(0, 1))
            .await?,
        engine
            .open_reader(&id(2), FileIndex(2), range(0, 1))
            .await?,
    ];
    drop(second);
    assert_eq!(
        (stats(engine).idle_readers, stats(engine).readers),
        (3, 3),
        "a fourth idle reader in all releases the oldest of all"
    );
    // The first torrent's remaining idle reader is the one at 200 KB: a
    // stream there continues it rather than opening a reader.
    let opened = stats(engine).opened_readers;
    drop(
        engine
            .open_reader(&id(1), FileIndex(0), range(200_000, 200_001))
            .await?,
    );
    assert_eq!(stats(engine).opened_readers, opened);

    engine.shutdown().await?;
    Ok(())
}

/// RDR-06: readahead windows shrink as readers of another torrent are
/// added and grow back as they are released.
/// A reader's window ramps up as it reads, and starts over after a seek.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn readahead_ramps_up_and_restarts_after_a_seek() -> anyhow::Result<()> {
    // 1 MiB pieces in a 32 MiB file; the reader's share is 8 pieces.
    let spec = catalog::large(8 * MIB);
    let seeder = Seeder::start(&spec, &spec.metainfo().bytes).await?;
    let memory = MemoryCache::start(16 * MIB);
    let leecher = Leecher::start().await?;
    let playback = Playback::new(PlaybackConfig::new(8 * MIB));
    let torrent = memory
        .add_scheduled_in(&leecher, &[seeder.addr], &spec, &playback, |_| {})
        .await?;
    let mut reader = bounded("opening a reader", async {
        Ok(torrent.scheduler.open(&torrent.handle, 0).await?)
    })
    .await?;
    let first_and_last = [0, 31];
    let window = |priorities: Vec<u32>| -> Vec<u32> {
        priorities
            .into_iter()
            .filter(|piece| !first_and_last.contains(piece))
            .collect()
    };
    assert_eq!(
        window(torrent.scheduler.priorities()?),
        [1],
        "a new reader reads ahead its minimum"
    );

    let mut read = vec![0; 4 * MIB as usize];
    bounded("reading 4 pieces", async {
        Ok(reader.read_exact(&mut read).await?)
    })
    .await?;
    assert_eq!(
        window(torrent.scheduler.priorities()?),
        (4..12).collect::<Vec<_>>(),
        "after 4 pieces, its whole 8-piece share"
    );

    reader.seek(SeekFrom::Start(16 * MIB)).await?;
    assert_eq!(
        window(torrent.scheduler.priorities()?),
        [16, 17],
        "a seek starts the ramp again"
    );
    reader.seek(SeekFrom::Start(16 * MIB)).await?;
    assert_eq!(
        window(torrent.scheduler.priorities()?),
        [16, 17],
        "seeking to where it is keeps the run"
    );

    drop(reader);
    leecher.session.stop().await;
    seeder.stop().await;
    memory.shutdown().await
}

/// An idle reader that played reads ahead its whole share while it waits,
/// and ramps again once taken for the next request.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_idle_reader_that_played_reads_ahead_its_share() -> anyhow::Result<()> {
    // 1 MiB pieces in a 64 MiB file; the reader's share is 16 pieces.
    let spec = catalog::large(16 * MIB);
    let seeder = Seeder::start(&spec, &spec.metainfo().bytes).await?;
    let memory = MemoryCache::start(32 * MIB);
    let leecher = Leecher::start().await?;
    let playback = Playback::new(PlaybackConfig::new(16 * MIB));
    let torrent = memory
        .add_scheduled_in(&leecher, &[seeder.addr], &spec, &playback, |_| {})
        .await?;
    let last = (spec.total_size_bytes() / MIB - 1) as u32;
    let window = |torrent: &support::ScheduledTorrent| -> anyhow::Result<usize> {
        Ok(torrent
            .scheduler
            .priorities()?
            .into_iter()
            .filter(|&piece| piece > 4 && piece != last)
            .count())
    };
    let mut reader = bounded("opening a reader", async {
        Ok(torrent.scheduler.open(&torrent.handle, 0).await?)
    })
    .await?;
    let mut read = vec![0; 4 * MIB as usize];
    bounded("reading 4 pieces", async {
        Ok(reader.read_exact(&mut read).await?)
    })
    .await?;
    // At most one live peer here, so the peer floor stays below the ramp.
    assert_eq!(
        window(&torrent)?,
        7,
        "twice the 4 pieces read, the current one included"
    );

    reader.park();
    assert_eq!(
        window(&torrent)?,
        15,
        "idle after playing, its whole 16-piece share"
    );
    let reader = torrent
        .scheduler
        .take_idle(0, 4 * MIB)
        .expect("the parked reader");
    assert_eq!(window(&torrent)?, 7, "taken again, it ramps as before");

    drop(reader);
    leecher.session.stop().await;
    seeder.stop().await;
    memory.shutdown().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn readahead_is_shared_across_torrents() -> anyhow::Result<()> {
    let spec = catalog::large(8 * MIB);
    let memory = MemoryCache::start(8 * MIB);
    // Without a ramp, so new readers take their share at once.
    let playback = Playback::new(PlaybackConfig {
        readahead_ramp_bytes: 0,
        ..PlaybackConfig::new(8 * MIB)
    });
    let (first_leecher, second_leecher) = (Leecher::start().await?, Leecher::start().await?);
    let first = memory
        .add_scheduled_in(&first_leecher, &[], &spec, &playback, |_| {})
        .await?;
    let second = memory
        .add_scheduled_in(&second_leecher, &[], &spec, &playback, |_| {})
        .await?;

    let reader = bounded("opening a reader", async {
        Ok(first.scheduler.open(&first.handle, 0).await?)
    })
    .await?;
    // The current piece, an 8-piece window, and the file's last piece.
    assert_eq!(first.scheduler.priorities()?.len(), 9);
    let other = bounded("opening a reader", async {
        Ok(second.scheduler.open(&second.handle, 0).await?)
    })
    .await?;
    assert_eq!(
        first.scheduler.priorities()?.len(),
        5,
        "half the budget: a 4-piece window"
    );
    assert_eq!(second.scheduler.priorities()?.len(), 5);
    drop(other);
    assert_eq!(
        first.scheduler.priorities()?.len(),
        9,
        "the window grows back"
    );
    assert!(second.scheduler.priorities()?.is_empty());
    drop(reader);
    assert_eq!(playback.stats().readers, 0);

    first_leecher.session.stop().await;
    second_leecher.session.stop().await;
    memory.shutdown().await
}

/// ADR 5's bounded fallback: when a reservation finds nothing evictable,
/// the oldest idle reader is released before its idle timeout so its
/// pinned piece can go.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_full_cache_releases_idle_readers() -> anyhow::Result<()> {
    // The held reader's file lies within one piece, so once idle it pins
    // that piece and needs nothing more.
    let held = catalog::many_files();
    let pressing = catalog::single_small();
    assert_eq!(held.piece_length, pressing.piece_length);
    let piece = u64::from(held.piece_length);
    let held_length = held.files()[0].size_bytes;
    assert!(held_length < piece);
    let held_seeder = Seeder::start(&held, &held.metainfo().bytes).await?;
    let pressing_seeder = Seeder::start(&pressing, &pressing.metainfo().bytes).await?;
    // One piece of cache, which the idle reader's pinned piece fills, and
    // timeouts no test waits for.
    let started = start_with(vec![held_seeder.addr, pressing_seeder.addr], |config| {
        *config = config.clone().with_cache_capacity(piece);
        config.idle_timeout = Duration::from_secs(600);
        config.piece_wait = Duration::from_secs(600);
    })
    .await?;
    let engine = &started.engine;
    engine
        .add(&id(1), &upload(&held), DesiredState::Started)
        .await?;
    engine
        .add(&id(2), &upload(&pressing), DesiredState::Started)
        .await?;
    let bytes = read(engine, &id(1), 0, 0, held_length).await?;
    assert_eq!(bytes, expected(&held, 0, 0, held_length));
    assert_eq!(stats(engine).idle_readers, 1);
    assert_eq!(engine.cache_statistics().await?.pinned_bytes, piece);

    // The read finishing at all shows the held reader was released: its
    // pinned piece filled the cache.
    let bytes = read(engine, &id(2), 0, 0, 100).await?;
    assert_eq!(bytes, expected(&pressing, 0, 0, 100));
    // The second reader, idle, still wants its two-piece minimum window,
    // which the cache cannot hold either, so the fallback may release it
    // too; wait until no reader is mid-release.
    let settled = eventually("the readers to settle", || {
        let stats = stats(engine);
        Ok((stats.readers == stats.idle_readers).then_some(stats))
    })
    .await?;
    assert!(
        settled.readers <= 1 && settled.opened_readers == 2,
        "the held reader was released: {settled:?}"
    );
    engine.cache().expect("a started cache").audit()?;

    engine.shutdown().await?;
    held_seeder.stop().await;
    pressing_seeder.stop().await;
    Ok(())
}

/// LIF-04: pausing cancels streams in progress and releases every reader,
/// idle ones included; content needs the torrent started again.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pausing_cancels_streams_and_releases_readers() -> anyhow::Result<()> {
    // Without peers a stream waits for its first piece until canceled.
    let started = start(Vec::new(), 8 * MIB, Duration::from_secs(600)).await?;
    let engine = &started.engine;
    engine
        .add(
            &id(1),
            &upload(&catalog::single_small()),
            DesiredState::Started,
        )
        .await?;
    let streaming = drain(
        engine
            .open_reader(&id(1), FileIndex(0), range(0, 1000))
            .await?,
    );
    drop(
        engine
            .open_reader(&id(1), FileIndex(0), range(500_000, 500_001))
            .await?,
    );
    assert_eq!((stats(engine).readers, stats(engine).idle_readers), (2, 1));
    let mut active = engine.playback().expect("a started engine").subscribe();
    assert!(*active.borrow_and_update());

    engine
        .set_desired_state(&id(1), DesiredState::Paused)
        .await?;
    let error = bounded("the canceled stream", async { Ok(streaming.await?) })
        .await?
        .expect_err("pausing cancels the stream");
    assert_eq!(error.kind(), ErrorKind::ConnectionAborted);
    assert_eq!((stats(engine).readers, stats(engine).idle_readers), (0, 0));
    assert!(!*active.borrow_and_update(), "no reader is left playing");
    assert_eq!(engine.cache_statistics().await?.pinned_bytes, 0);
    assert!(matches!(
        engine.open_reader(&id(1), FileIndex(0), range(0, 1)).await,
        Err(EngineError::NotStarted)
    ));
    assert!(engine.status(&id(1)).await?.metadata.is_some());

    engine
        .set_desired_state(&id(1), DesiredState::Started)
        .await?;
    drop(
        engine
            .open_reader(&id(1), FileIndex(0), range(0, 1))
            .await?,
    );
    assert_eq!(
        stats(engine).readers,
        1,
        "a started torrent opens readers again"
    );

    engine.shutdown().await?;
    Ok(())
}

/// LIF-07: deleting a torrent cancels its streams and releases its readers,
/// leaving other torrents' readers alone.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn removing_cancels_only_that_torrents_streams() -> anyhow::Result<()> {
    let started = start(Vec::new(), 8 * MIB, Duration::from_secs(600)).await?;
    let engine = &started.engine;
    engine
        .add(
            &id(1),
            &upload(&catalog::single_small()),
            DesiredState::Started,
        )
        .await?;
    engine
        .add(
            &id(2),
            &upload(&catalog::multi_boundary()),
            DesiredState::Started,
        )
        .await?;
    let removed = drain(
        engine
            .open_reader(&id(1), FileIndex(0), range(0, 1000))
            .await?,
    );
    let kept = engine
        .open_reader(&id(2), FileIndex(0), range(0, 1000))
        .await?;

    engine.remove(&id(1)).await?;
    let error = bounded("the canceled stream", async { Ok(removed.await?) })
        .await?
        .expect_err("removing cancels the stream");
    assert_eq!(error.kind(), ErrorKind::ConnectionAborted);
    assert_eq!(stats(engine).readers, 1, "the other torrent's reader stays");
    drop(kept);
    eventually("the kept reader turning idle", || {
        Ok((stats(engine).idle_readers == 1).then_some(()))
    })
    .await?;

    engine.shutdown().await?;
    assert!(
        engine.playback().is_none(),
        "a stopped engine has no playback"
    );
    Ok(())
}
