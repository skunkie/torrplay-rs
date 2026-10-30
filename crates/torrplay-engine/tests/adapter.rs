// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! `LibrqbitEngine` through the engine facade, against local seeders on
//! loopback sessions that reach no public swarm.

mod support;

use std::time::Duration;

use librqbit::AddTorrent;
use support::{
    Leecher, MIB, Seeder, bounded, eventually, expected, futures_now, id, magnet, range, read,
    start, start_with, upload,
};
use tokio::io::AsyncReadExt;
use torrplay_core::engine::{ByteRange, Engine, EngineError, HashChecks, Source, SourceRejection};
use torrplay_core::lifecycle::{DesiredState, Readiness, StorageMode};
use torrplay_core::metadata::FileIndex;
use torrplay_core::settings::Settings;
use torrplay_engine::{EngineConfig, LibrqbitEngine, NetworkConfig};
use torrplay_fixtures::catalog;

/// STR-01 and STR-02 through the facade: every file of `multi-boundary`
/// whole, and ranges across piece boundaries, byte for byte.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn uploads_stream_every_file_and_range_byte_for_byte() -> anyhow::Result<()> {
    let spec = catalog::multi_boundary();
    let seeder = Seeder::start(&spec, &spec.metainfo().bytes).await?;
    let started = start(vec![seeder.addr], 8 * MIB, Duration::from_secs(30)).await?;
    let engine = &started.engine;

    let inspection = engine.inspect(&upload(&spec)).await?;
    assert_eq!(inspection.info_hash.as_bytes(), &spec.metainfo().info_hash);
    let metadata = inspection.metadata.expect("an upload has metadata");
    assert_eq!(metadata.files.len(), spec.files().len());

    engine
        .add(&id(1), &upload(&spec), DesiredState::Started)
        .await?;
    let status = engine.status(&id(1)).await?;
    assert!(status.metadata.is_some() && status.failure.is_none());
    assert_eq!(
        engine.torrent_file(&id(1)).await?,
        Some(spec.metainfo().bytes),
        "an upload's torrent file is its original bytes"
    );
    for (index, file) in spec.files().iter().enumerate() {
        let bytes = read(engine, &id(1), index, 0, file.size_bytes).await?;
        assert_eq!(
            bytes,
            expected(&spec, index, 0, file.size_bytes),
            "file {index}"
        );
    }
    // File 2 starts inside a piece; read across the next boundary.
    let piece = u64::from(spec.piece_length);
    let start: u64 = spec.files()[..2].iter().map(|file| file.size_bytes).sum();
    let boundary = start.div_ceil(piece) * piece - start;
    let bytes = read(engine, &id(1), 2, boundary - 1000, boundary + 1000).await?;
    assert_eq!(bytes, expected(&spec, 2, boundary - 1000, boundary + 1000));
    assert!(matches!(
        engine
            .open_reader(&id(1), FileIndex(1), ByteRange::new(0, 2).expect("a range"))
            .await,
        Err(EngineError::RangeOutOfBounds)
    ));
    assert!(matches!(
        engine
            .open_reader(&id(1), FileIndex(9), ByteRange::new(0, 1).expect("a range"))
            .await,
        Err(EngineError::UnknownFile)
    ));
    engine.cache().expect("a started cache").audit()?;

    engine.shutdown().await?;
    seeder.stop().await;
    Ok(())
}

/// SET-06 and SET-07 through the facade: applied settings release idle
/// readers over a new limit at once, and a wait that begins afterward uses
/// the new piece wait bound.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn applied_settings_reach_idle_readers_and_later_waits() -> anyhow::Result<()> {
    let small = catalog::single_small();
    let seeder = Seeder::start(&small, &small.metainfo().bytes).await?;
    let started = start(vec![seeder.addr], 8 * MIB, Duration::from_secs(30)).await?;
    let engine = &started.engine;
    engine
        .add(&id(1), &upload(&small), DesiredState::Started)
        .await?;
    let size = small.total_size_bytes();
    assert_eq!(
        read(engine, &id(1), 0, 0, size).await?,
        expected(&small, 0, 0, size)
    );
    let playback = engine.playback().expect("a started engine");
    eventually("the reader becoming idle", || {
        Ok((playback.stats().idle_readers == 1).then_some(()))
    })
    .await?;

    let mut settings = Settings::DEFAULT;
    settings.idle_readers_per_torrent = 0;
    settings.idle_readers_total = 0;
    settings.piece_wait = Duration::from_secs(1);
    engine.apply_settings(&settings)?;
    assert_eq!(
        playback.stats().idle_readers,
        0,
        "the idle reader over the new limits is released at once"
    );

    // No peer has this torrent, so its first read waits for the bound.
    let multi = catalog::multi_boundary();
    engine
        .add(&id(2), &upload(&multi), DesiredState::Started)
        .await?;
    let waited = std::time::Instant::now();
    let result = read(engine, &id(2), 0, 0, 1).await;
    assert!(result.is_err(), "no piece arrives");
    assert!(
        waited.elapsed() < Duration::from_secs(10),
        "the wait took {:?}, not the new bound of one second",
        waited.elapsed()
    );

    engine.shutdown().await?;
    seeder.stop().await;
    Ok(())
}

/// SET-08 through the facade: shrinking the cache below its usage while a
/// reader streams evicts down to the new capacity, which no reservation
/// exceeds meanwhile, and the stream stays byte-correct; growing it again
/// lets the cache hold more than the smaller capacity.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_cache_capacity_shrinks_and_grows_during_a_stream() -> anyhow::Result<()> {
    let spec = catalog::large(8 * MIB);
    let seeder = Seeder::start(&spec, &spec.metainfo().bytes).await?;
    let started = start(vec![seeder.addr], 16 * MIB, Duration::from_secs(30)).await?;
    let engine = &started.engine;
    let cache = engine.cache().expect("a started cache");
    engine
        .add(&id(1), &upload(&spec), DesiredState::Started)
        .await?;
    let size = spec.total_size_bytes();
    let half = size / 2;
    let mut reader = engine
        .open_reader(&id(1), FileIndex(0), range(0, size))
        .await?;
    let mut first = vec![0; usize::try_from(half)?];
    bounded("reading the first half", async {
        Ok(reader.read_exact(&mut first).await?)
    })
    .await?;
    assert!(first == expected(&spec, 0, 0, half), "the first half");
    let before = cache.audit()?;
    assert!(
        before.usage_bytes > 6 * MIB,
        "{} bytes in use before the shrink",
        before.usage_bytes
    );

    let mut settings = Settings::DEFAULT;
    settings.cache_capacity_bytes = 6 * MIB;
    engine.apply_settings(&settings)?;
    assert_eq!(engine.cache_statistics().await?.capacity_bytes, 6 * MIB);
    eventually("eviction down to the new capacity", || {
        let stats = cache.audit()?;
        Ok((stats.usage_bytes <= 6 * MIB).then_some(()))
    })
    .await?;
    let mut rest = Vec::new();
    bounded("reading the second half", async {
        Ok(reader.read_to_end(&mut rest).await?)
    })
    .await?;
    assert!(rest == expected(&spec, 0, half, size), "the second half");
    drop(reader);
    let shrunk = cache.audit()?;
    assert!(shrunk.usage_bytes <= 6 * MIB);
    assert!(shrunk.eviction_count > before.eviction_count);

    settings.cache_capacity_bytes = 32 * MIB;
    engine.apply_settings(&settings)?;
    let bytes = read(engine, &id(1), 0, 0, size).await?;
    assert!(bytes == expected(&spec, 0, 0, size), "the whole file again");
    let grown = cache.audit()?;
    assert!(
        grown.usage_bytes > 6 * MIB && grown.usage_bytes <= 32 * MIB,
        "{} bytes in use after growing",
        grown.usage_bytes
    );

    engine.shutdown().await?;
    seeder.stop().await;
    Ok(())
}

/// SET-09 through the facade: a cache capacity below a torrent's piece
/// length is refused and changes nothing. Once that torrent is gone it
/// applies, and a later upload is checked against it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_capacity_below_a_torrents_piece_length_is_refused() -> anyhow::Result<()> {
    let spec = catalog::large(4 * MIB);
    let started = start(Vec::new(), 8 * MIB, Duration::from_secs(30)).await?;
    let engine = &started.engine;
    engine
        .add(&id(1), &upload(&spec), DesiredState::Paused)
        .await?;

    let mut settings = Settings::DEFAULT;
    settings.cache_capacity_bytes = MIB / 2;
    assert!(matches!(
        engine.apply_settings(&settings),
        Err(EngineError::CapacityBelowPieceLength { piece_length_bytes }) if piece_length_bytes == MIB
    ));
    assert_eq!(engine.cache_statistics().await?.capacity_bytes, 8 * MIB);

    engine.remove(&id(1)).await?;
    engine.apply_settings(&settings)?;
    assert_eq!(engine.cache_statistics().await?.capacity_bytes, MIB / 2);
    match engine
        .add(&id(2), &upload(&spec), DesiredState::Started)
        .await
    {
        Err(EngineError::Rejected(SourceRejection::Unsupported { detail })) => {
            assert!(detail.contains("524288-byte cache capacity"), "{detail}");
        }
        other => anyhow::bail!("the upload was not refused: {other:?}"),
    }

    engine.shutdown().await?;
    Ok(())
}

/// LIF-02 through the facade: a magnet torrent's metadata arrives from a
/// seeder, and its file then streams. Its torrent file, built from the
/// retrieved metadata, keeps the magnet URI's trackers.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn magnet_metadata_arrives_from_a_seeder() -> anyhow::Result<()> {
    let spec = catalog::single_small();
    let seeder = Seeder::start(&spec, &spec.metainfo().bytes).await?;
    let started = start(vec![seeder.addr], 8 * MIB, Duration::from_secs(30)).await?;
    let engine = &started.engine;

    // A tracker nothing answers on: peers come from the seeder.
    let tracker = "http://127.0.0.1:9/announce";
    let Source::MagnetUri(uri) = magnet(&spec) else {
        unreachable!("a magnet URI");
    };
    let with_tracker = Source::MagnetUri(format!("{uri}&tr=http%3A%2F%2F127.0.0.1%3A9%2Fannounce"));
    let inspection = engine.inspect(&with_tracker).await?;
    assert!(inspection.metadata.is_none());
    assert_eq!(engine.torrent_file(&id(1)).await.ok(), None);
    // Added paused, so a fast local seeder cannot deliver the metadata
    // before the torrent file is checked.
    engine
        .add(&id(1), &with_tracker, DesiredState::Paused)
        .await?;
    assert_eq!(
        engine.torrent_file(&id(1)).await?,
        None,
        "no torrent file before metadata"
    );
    engine
        .set_desired_state(&id(1), DesiredState::Started)
        .await?;
    let metadata = eventually("metadata arriving", || {
        let status = futures_now(engine.status(&id(1)))?;
        Ok(status.metadata)
    })
    .await?;
    assert_eq!(metadata.name, b"single-small.bin");
    let built = engine
        .torrent_file(&id(1))
        .await?
        .expect("a torrent file once metadata arrived");
    assert_eq!(
        engine
            .inspect(&Source::TorrentFile(built.clone()))
            .await?
            .info_hash,
        inspection.info_hash
    );
    assert!(
        built
            .windows(tracker.len())
            .any(|window| window == tracker.as_bytes()),
        "the built torrent file keeps the magnet URI's tracker"
    );
    let size = spec.total_size_bytes();
    assert_eq!(
        read(engine, &id(1), 0, 0, size).await?,
        expected(&spec, 0, 0, size)
    );

    engine.shutdown().await?;
    seeder.stop().await;
    Ok(())
}

/// LIF-01 and LIF-03 through the facade: without peers, retrieval keeps
/// waiting without failing, pausing and starting it changes nothing else,
/// and an uploaded torrent file supplies the metadata.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn retrieval_without_peers_waits_until_metadata_is_supplied() -> anyhow::Result<()> {
    let spec = catalog::single_small();
    let started = start(Vec::new(), 8 * MIB, Duration::from_secs(30)).await?;
    let engine = &started.engine;
    engine
        .add(&id(1), &magnet(&spec), DesiredState::Started)
        .await?;
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let status = engine.status(&id(1)).await?;
    assert!(status.metadata.is_none() && status.failure.is_none());
    assert!(matches!(
        engine
            .open_reader(&id(1), FileIndex(0), ByteRange::new(0, 1).expect("a range"))
            .await,
        Err(EngineError::MetadataUnavailable)
    ));

    engine
        .set_desired_state(&id(1), DesiredState::Paused)
        .await?;
    engine
        .set_desired_state(&id(1), DesiredState::Started)
        .await?;
    let status = engine.status(&id(1)).await?;
    assert!(status.metadata.is_none() && status.failure.is_none());

    let other = catalog::multi_boundary().metainfo().bytes;
    assert!(matches!(
        engine.supply_metadata(&id(1), &other).await,
        Err(EngineError::MetadataMismatch)
    ));
    engine
        .supply_metadata(&id(1), &spec.metainfo().bytes)
        .await?;
    let status = engine.status(&id(1)).await?;
    assert_eq!(status.metadata.expect("supplied metadata").files.len(), 1);

    engine.shutdown().await?;
    Ok(())
}

/// CAC-06 through the facade: an upload whose pieces are longer than the
/// cache is unsupported, and a magnet torrent with such pieces fails once
/// its metadata arrives, holding no cache usage.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pieces_longer_than_the_cache_are_refused() -> anyhow::Result<()> {
    let spec = catalog::oversized_piece();
    let seeder = Seeder::start(&spec, &spec.metainfo().bytes).await?;
    let started = start(vec![seeder.addr], 8 * MIB, Duration::from_secs(30)).await?;
    let engine = &started.engine;

    match engine
        .add(&id(2), &upload(&spec), DesiredState::Started)
        .await
    {
        Err(EngineError::Rejected(SourceRejection::Unsupported { detail })) => {
            assert!(detail.contains("8388608-byte cache capacity"), "{detail}");
        }
        other => anyhow::bail!("the upload was not refused: {other:?}"),
    }
    // ADR 9: the cache never holds a disk torrent's pieces.
    engine
        .add_in(
            &id(3),
            &upload(&spec),
            DesiredState::Paused,
            StorageMode::Disk,
        )
        .await?;
    let directory = engine.download_dir().join(id(3).as_str());
    assert!(directory.is_dir());
    engine.remove(&id(3)).await?;
    assert!(
        !directory.exists(),
        "deleting a disk torrent deletes its data"
    );
    assert!(!directory.with_extension("have").exists());
    engine
        .add(&id(1), &magnet(&spec), DesiredState::Started)
        .await?;
    let failure = eventually("the magnet torrent failing", || {
        Ok(futures_now(engine.status(&id(1)))?.failure)
    })
    .await?;
    assert!(
        failure.detail.contains("16777216-byte pieces"),
        "{}",
        failure.detail
    );
    let stats = engine.cache().expect("a started cache").audit()?;
    assert_eq!(stats.usage_bytes - stats.retained_bytes, 0);

    engine.shutdown().await?;
    seeder.stop().await;
    Ok(())
}

/// DSK-01 through the facade: a torrent in disk storage streams every file
/// byte for byte, and after a restart streams them again from its files
/// with no peer reachable.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_disk_torrent_streams_again_from_its_files_after_a_restart() -> anyhow::Result<()> {
    let spec = catalog::multi_boundary();
    let seeder = Seeder::start(&spec, &spec.metainfo().bytes).await?;
    let downloads = tempfile::tempdir()?;
    let in_downloads = |config: &mut EngineConfig| {
        config.download_dir = downloads.path().to_path_buf();
    };
    let read_all = async |engine: &LibrqbitEngine| -> anyhow::Result<()> {
        for (index, file) in spec.files().iter().enumerate() {
            let bytes = read(engine, &id(1), index, 0, file.size_bytes).await?;
            assert!(
                bytes == expected(&spec, index, 0, file.size_bytes),
                "file {index}"
            );
        }
        Ok(())
    };

    let started = start_with(vec![seeder.addr], in_downloads).await?;
    let engine = &started.engine;
    engine
        .add_in(
            &id(1),
            &upload(&spec),
            DesiredState::Started,
            StorageMode::Disk,
        )
        .await?;
    read_all(engine).await?;
    assert_eq!(
        engine
            .cache()
            .expect("a started cache")
            .audit()?
            .usage_bytes,
        0,
        "disk pieces never enter the cache"
    );
    engine.shutdown().await?;
    seeder.stop().await;

    let started = start_with(Vec::new(), in_downloads).await?;
    let engine = &started.engine;
    engine
        .add_in(
            &id(1),
            &upload(&spec),
            DesiredState::Started,
            StorageMode::Disk,
        )
        .await?;
    read_all(engine).await?;
    engine.shutdown().await?;
    Ok(())
}

/// DSK-02 through the facade: a stream continues byte for byte while its
/// torrent moves from memory to disk storage and back; the move back
/// deletes the torrent's directory.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_stream_continues_across_storage_mode_transitions() -> anyhow::Result<()> {
    let spec = catalog::large(4 * MIB);
    let seeder = Seeder::start(&spec, &spec.metainfo().bytes).await?;
    let started = start(vec![seeder.addr], 8 * MIB, Duration::from_secs(30)).await?;
    let engine = &started.engine;
    engine
        .add(&id(1), &upload(&spec), DesiredState::Started)
        .await?;
    let size = spec.total_size_bytes();
    let third = size / 3;
    let mut reader = engine
        .open_reader(&id(1), FileIndex(0), range(0, size))
        .await?;
    let mut read_until = async |end: u64, from: u64| -> anyhow::Result<()> {
        let mut bytes = vec![0; usize::try_from(end - from)?];
        bounded("reading", async {
            Ok(reader.read_exact(&mut bytes).await?)
        })
        .await?;
        assert!(
            bytes == expected(&spec, 0, from, end),
            "bytes {from}..{end}"
        );
        Ok(())
    };
    read_until(third, 0).await?;

    let directory = engine.download_dir().join(id(1).as_str());
    // The reader reads while the torrent moves, waiting through the swap.
    let (moved, read) = tokio::join!(
        bounded("moving to disk", async {
            Ok(engine
                .change_storage_mode(&id(1), StorageMode::Disk)
                .await?)
        }),
        read_until(2 * third, third),
    );
    moved?;
    read?;
    assert!(directory.is_dir());

    bounded("moving to memory", async {
        Ok(engine
            .change_storage_mode(&id(1), StorageMode::Memory)
            .await?)
    })
    .await?;
    assert!(!directory.exists(), "memory storage keeps nothing on disk");
    read_until(size, 2 * third).await?;
    drop(read_until);
    engine.cache().expect("a started cache").audit()?;

    engine.shutdown().await?;
    seeder.stop().await;
    Ok(())
}

/// DSK-03 through the facade: a move to disk that cannot create the
/// torrent's directory fails, the torrent stays in memory storage, and its
/// stream continues.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_failed_transition_leaves_the_torrent_and_its_stream() -> anyhow::Result<()> {
    let spec = catalog::single_small();
    let seeder = Seeder::start(&spec, &spec.metainfo().bytes).await?;
    let blocked = tempfile::NamedTempFile::new()?;
    let blocked_path = blocked.path().to_path_buf();
    let started = start_with(vec![seeder.addr], |config| {
        // A file where the download directory should be.
        config.download_dir = blocked_path.clone();
    })
    .await?;
    let engine = &started.engine;
    engine
        .add(&id(1), &upload(&spec), DesiredState::Started)
        .await?;
    let size = spec.total_size_bytes();
    let mut reader = engine
        .open_reader(&id(1), FileIndex(0), range(0, size))
        .await?;
    let mut first = vec![0; 1000];
    bounded("reading the first bytes", async {
        Ok(reader.read_exact(&mut first).await?)
    })
    .await?;

    let result = engine.change_storage_mode(&id(1), StorageMode::Disk).await;
    assert!(
        matches!(result, Err(EngineError::TransitionFailed { .. })),
        "{result:?}"
    );
    let mut rest = Vec::new();
    bounded("reading the rest", async {
        Ok(reader.read_to_end(&mut rest).await?)
    })
    .await?;
    first.extend(rest);
    assert!(first == expected(&spec, 0, 0, size), "the whole file");
    assert!(blocked_path.is_file(), "the blocking file is untouched");

    engine.shutdown().await?;
    seeder.stop().await;
    Ok(())
}

/// DSK-09 through the facade: a disk torrent uploads to a leecher, as ADR 3
/// decides, and once it moves to memory storage it uploads nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_disk_torrent_uploads_until_it_moves_to_memory() -> anyhow::Result<()> {
    let spec = catalog::single_small();
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
    let size = spec.total_size_bytes();
    read(engine, &id(1), 0, 0, size).await?;
    // Only the engine has the torrent now.
    seeder.stop().await;
    let ours = engine.listen_addr().expect("a peer listener");

    let leecher = Leecher::start().await?;
    let handle = leecher
        .session
        .add_torrent(
            AddTorrent::from_bytes(spec.metainfo().bytes),
            Some(leecher.add_options(&[ours])?),
        )
        .await?
        .into_handle()
        .expect("a torrent");
    bounded(
        "downloading from the disk torrent",
        handle.wait_until_completed(),
    )
    .await?;

    bounded("moving to memory", async {
        Ok(engine
            .change_storage_mode(&id(1), StorageMode::Memory)
            .await?)
    })
    .await?;
    let late = Leecher::start().await?;
    let handle = late
        .session
        .add_torrent(
            AddTorrent::from_bytes(spec.metainfo().bytes),
            Some(late.add_options(&[ours])?),
        )
        .await?
        .into_handle()
        .expect("a torrent");
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(
        handle.stats().progress_bytes,
        0,
        "a memory torrent uploads nothing"
    );

    engine.shutdown().await?;
    Ok(())
}

/// Waits until torrent `n` holds every piece.
async fn completed(engine: &LibrqbitEngine, n: u8) -> anyhow::Result<()> {
    eventually("the whole torrent downloading", || {
        let ratio = futures_now(engine.torrent_statistics(&id(n)))?.completion_ratio;
        Ok(ratio.is_some_and(|ratio| ratio.get() >= 1.0).then_some(()))
    })
    .await
}

async fn downloaded(engine: &LibrqbitEngine, n: u8) -> anyhow::Result<u64> {
    Ok(engine.torrent_statistics(&id(n)).await?.downloaded_bytes)
}

/// BGD-01: a started disk torrent downloads every piece without a reader;
/// a memory torrent without readers downloads nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_disk_torrent_downloads_in_the_background_and_a_memory_one_does_not() -> anyhow::Result<()>
{
    let large = catalog::large(4 * MIB);
    let small = catalog::single_small();
    let large_seeder = Seeder::start(&large, &large.metainfo().bytes).await?;
    let small_seeder = Seeder::start(&small, &small.metainfo().bytes).await?;
    let downloads = tempfile::tempdir()?;
    let started = start_with(vec![large_seeder.addr, small_seeder.addr], |config| {
        config.download_dir = downloads.path().to_path_buf();
    })
    .await?;
    let engine = &started.engine;
    engine
        .add_in(
            &id(1),
            &upload(&large),
            DesiredState::Started,
            StorageMode::Disk,
        )
        .await?;
    engine
        .add(&id(2), &upload(&small), DesiredState::Started)
        .await?;
    completed(engine, 1).await?;
    assert!(downloaded(engine, 1).await? >= large.total_size_bytes());
    assert_eq!(
        downloaded(engine, 2).await?,
        0,
        "a memory torrent fetches only what readers need"
    );

    engine.shutdown().await?;
    large_seeder.stop().await;
    small_seeder.stop().await;
    Ok(())
}

/// BGD-02: any reader pauses background downloads, an idle one included,
/// and releasing the last resumes them.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn readers_pause_background_downloads_until_the_last_is_released() -> anyhow::Result<()> {
    let large = catalog::large(4 * MIB);
    let small = catalog::single_small();
    let large_seeder = Seeder::start(&large, &large.metainfo().bytes).await?;
    let small_seeder = Seeder::start(&small, &small.metainfo().bytes).await?;
    let downloads = tempfile::tempdir()?;
    let started = start_with(vec![large_seeder.addr, small_seeder.addr], |config| {
        config.download_dir = downloads.path().to_path_buf();
    })
    .await?;
    let engine = &started.engine;
    engine
        .add(&id(2), &upload(&small), DesiredState::Started)
        .await?;
    let mut reader = engine
        .open_reader(&id(2), FileIndex(0), range(0, 1000))
        .await?;
    let mut bytes = Vec::new();
    bounded("reading", async {
        Ok(reader.read_to_end(&mut bytes).await?)
    })
    .await?;

    engine
        .add_in(
            &id(1),
            &upload(&large),
            DesiredState::Started,
            StorageMode::Disk,
        )
        .await?;
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(
        downloaded(engine, 1).await?,
        0,
        "an active reader pauses it"
    );
    // The stream ends; its reader stays idle.
    drop(reader);
    let playback = engine.playback().expect("a started engine");
    assert_eq!(playback.stats().idle_readers, 1);
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(
        downloaded(engine, 1).await?,
        0,
        "an idle reader pauses it too"
    );

    playback.release_idle_readers();
    completed(engine, 1).await?;

    engine.shutdown().await?;
    large_seeder.stop().await;
    small_seeder.stop().await;
    Ok(())
}

/// BGD-03: a paused disk torrent downloads nothing; starting it starts its
/// background download.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_paused_disk_torrent_downloads_once_started() -> anyhow::Result<()> {
    let spec = catalog::large(4 * MIB);
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
            DesiredState::Paused,
            StorageMode::Disk,
        )
        .await?;
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(downloaded(engine, 1).await?, 0);
    engine
        .set_desired_state(&id(1), DesiredState::Started)
        .await?;
    completed(engine, 1).await?;

    engine.shutdown().await?;
    seeder.stop().await;
    Ok(())
}

/// BGD-04: turned off, background downloads stop; turned on, they resume.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn turning_background_downloads_off_stops_them_until_turned_on() -> anyhow::Result<()> {
    let spec = catalog::large(4 * MIB);
    let seeder = Seeder::start(&spec, &spec.metainfo().bytes).await?;
    let downloads = tempfile::tempdir()?;
    let started = start_with(vec![seeder.addr], |config| {
        config.download_dir = downloads.path().to_path_buf();
    })
    .await?;
    let engine = &started.engine;
    let mut settings = Settings::DEFAULT;
    settings.background_downloads = false;
    engine.apply_settings(&settings)?;
    engine
        .add_in(
            &id(1),
            &upload(&spec),
            DesiredState::Started,
            StorageMode::Disk,
        )
        .await?;
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(
        downloaded(engine, 1).await?,
        0,
        "nothing downloads in the background while they are off"
    );

    settings.background_downloads = true;
    engine.apply_settings(&settings)?;
    completed(engine, 1).await?;

    engine.shutdown().await?;
    seeder.stop().await;
    Ok(())
}

/// A paused torrent opens no reader, and a reader whose piece never comes
/// fails after the piece wait bound, which the engine counts as a wait that
/// timed out (OBS-07).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn readers_need_a_started_torrent_and_wait_a_bounded_time() -> anyhow::Result<()> {
    let spec = catalog::single_small();
    let started = start(Vec::new(), 8 * MIB, Duration::from_secs(1)).await?;
    let engine = &started.engine;
    engine
        .add(&id(1), &upload(&spec), DesiredState::Paused)
        .await?;
    assert!(matches!(
        engine
            .open_reader(
                &id(1),
                FileIndex(0),
                ByteRange::new(0, 10).expect("a range")
            )
            .await,
        Err(EngineError::NotStarted)
    ));
    engine
        .set_desired_state(&id(1), DesiredState::Started)
        .await?;
    let mut reader = engine
        .open_reader(
            &id(1),
            FileIndex(0),
            ByteRange::new(0, 10).expect("a range"),
        )
        .await?;
    let mut bytes = Vec::new();
    let error = bounded("a read without peers", async {
        Ok(reader.read_to_end(&mut bytes).await)
    })
    .await?
    .expect_err("no peer can supply the piece");
    assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
    let statistics = engine.engine_statistics().await?;
    let waits = &statistics.piece_waits;
    assert_eq!(waits.waiting_reader_count, 0, "the wait has ended");
    assert_eq!(waits.timed_out.count, 1);
    assert!(waits.timed_out.sum >= Duration::from_secs(1), "{waits:?}");
    assert_eq!((waits.ready.count, waits.canceled.count), (0, 0));

    engine.shutdown().await?;
    Ok(())
}

/// Removing a torrent releases its cache usage before returning, and the
/// engine no longer knows it; statistics count what was downloaded, for the
/// torrent and across the engine, and the connection to the seeder
/// (OBS-07).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn removing_releases_the_torrent_and_its_cache_usage() -> anyhow::Result<()> {
    let spec = catalog::single_small();
    let seeder = Seeder::start(&spec, &spec.metainfo().bytes).await?;
    let started = start(vec![seeder.addr], 8 * MIB, Duration::from_secs(30)).await?;
    let engine = &started.engine;
    engine
        .add(&id(1), &upload(&spec), DesiredState::Started)
        .await?;
    let size = spec.total_size_bytes();
    read(engine, &id(1), 0, 0, size).await?;

    let statistics = engine.torrent_statistics(&id(1)).await?;
    assert!(statistics.downloaded_bytes >= size);
    assert_eq!(statistics.uploaded_bytes, 0);
    assert!(
        statistics
            .completion_ratio
            .is_some_and(|ratio| ratio.get() > 0.0)
    );
    let cache = engine.cache_statistics().await?;
    assert!(cache.usage_bytes > 0 && cache.usage_bytes <= cache.capacity_bytes);
    let totals = engine.engine_statistics().await?;
    assert!(totals.downloaded_bytes >= size, "{totals:?}");
    assert_eq!(totals.uploaded_bytes, 0);
    assert!(totals.peer_connections.tcp.succeeded >= 1, "{totals:?}");
    assert_eq!(
        totals.hash_checks,
        HashChecks {
            passed: size.div_ceil(u64::from(spec.piece_length)),
            failed: 0,
        }
    );
    assert_eq!(totals.piece_waits.waiting_reader_count, 0);

    engine.remove(&id(1)).await?;
    let stats = engine.cache().expect("a started cache").audit()?;
    assert_eq!(stats.usage_bytes - stats.retained_bytes, 0);
    assert!(matches!(
        engine.status(&id(1)).await,
        Err(EngineError::UnknownTorrent)
    ));
    assert!(matches!(
        engine.remove(&id(1)).await,
        Err(EngineError::UnknownTorrent)
    ));

    engine.shutdown().await?;
    seeder.stop().await;
    Ok(())
}

/// The DHT starts with the engine only when turned on beforehand, and
/// turning it on or off later starts or stops it without restarting the
/// engine.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_dht_turns_on_and_off_while_the_engine_runs() -> anyhow::Result<()> {
    let folder = tempfile::tempdir()?;
    let engine = LibrqbitEngine::new(EngineConfig::new(
        folder.path().to_path_buf(),
        NetworkConfig::isolated(Vec::new()),
    ));
    engine.set_dht(true).await?;
    bounded("starting the engine", engine.start()).await?;
    let first = engine
        .dht_address()
        .expect("the DHT started with the engine");
    engine.set_dht(true).await?;
    assert_eq!(
        engine.dht_address(),
        Some(first),
        "unchanged, it keeps running"
    );
    engine.set_dht(false).await?;
    assert_eq!(engine.dht_address(), None);
    assert_eq!(engine.readiness(), Readiness::Ready);
    engine.set_dht(true).await?;
    assert!(engine.dht_address().is_some(), "started again");
    bounded("stopping the engine", engine.shutdown()).await?;
    Ok(())
}

/// The engine is unavailable until it starts and after it shuts down.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn readiness_follows_start_and_shutdown() -> anyhow::Result<()> {
    let folder = tempfile::tempdir()?;
    let engine = LibrqbitEngine::new(EngineConfig::new(
        folder.path().to_path_buf(),
        NetworkConfig::isolated(Vec::new()),
    ));
    assert!(!engine.readiness().is_ready());
    let spec = catalog::single_small();
    assert!(matches!(
        engine.inspect(&upload(&spec)).await,
        Err(EngineError::Unavailable { .. })
    ));
    bounded("starting the engine", engine.start()).await?;
    assert_eq!(engine.readiness(), Readiness::Ready);
    engine
        .add(&id(1), &magnet(&spec), DesiredState::Started)
        .await?;
    bounded("stopping the engine", engine.shutdown()).await?;
    assert!(!engine.readiness().is_ready());
    assert!(matches!(
        engine.status(&id(1)).await,
        Err(EngineError::Unavailable { .. })
    ));
    Ok(())
}
