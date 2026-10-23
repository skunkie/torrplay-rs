// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! `LibrqbitEngine` through the engine facade, against local seeders on
//! loopback sessions that reach no public swarm.

mod support;

use std::time::Duration;

use support::{
    MIB, Seeder, bounded, eventually, expected, futures_now, id, magnet, read, start, upload,
};
use tokio::io::AsyncReadExt;
use torrplay_core::engine::{ByteRange, Engine, EngineError, Source, SourceRejection};
use torrplay_core::lifecycle::{DesiredState, Readiness};
use torrplay_core::metadata::FileIndex;
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
    engine
        .add(&id(1), &with_tracker, DesiredState::Started)
        .await?;
    assert_eq!(
        engine.torrent_file(&id(1)).await?,
        None,
        "no torrent file before metadata"
    );
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

    match engine.inspect(&upload(&spec)).await {
        Err(EngineError::Rejected(SourceRejection::Unsupported { detail })) => {
            assert!(detail.contains("8388608-byte cache capacity"), "{detail}");
        }
        other => anyhow::bail!("the upload was not refused: {other:?}"),
    }
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

/// A paused torrent opens no reader, and a reader whose piece never comes
/// fails after the piece wait bound.
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

    engine.shutdown().await?;
    Ok(())
}

/// Removing a torrent releases its cache usage before returning, and the
/// engine no longer knows it; statistics count what was downloaded.
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
