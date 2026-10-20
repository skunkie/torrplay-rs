// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! LIF-06 in the engine: a storage write failure fails the torrent and
//! cancels its streams at once, and pausing and starting it again restarts
//! it. Needs the `faults` feature, which injects the failure.

// Helpers assert as tests do; clippy's test exemption covers only test
// functions.
#![allow(clippy::expect_used)]

mod support;

use std::io::ErrorKind;
use std::time::Duration;

use support::{MIB, Seeder, bounded, expected, id, range, read, start, upload};
use tokio::io::AsyncReadExt;
use torrplay_core::engine::{Engine, EngineError};
use torrplay_core::lifecycle::DesiredState;
use torrplay_core::metadata::FileIndex;
use torrplay_fixtures::catalog;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_failed_write_fails_the_torrent_and_cancels_its_streams() -> anyhow::Result<()> {
    let spec = catalog::single_small();
    let seeder = Seeder::start(&spec, &spec.metainfo().bytes).await?;
    // A piece wait far beyond the test's bounds: only cancellation can end
    // the stream in time.
    let started = start(vec![seeder.addr], 8 * MIB, Duration::from_secs(600)).await?;
    let engine = &started.engine;
    engine
        .add(&id(1), &upload(&spec), DesiredState::Started)
        .await?;
    engine.fail_storage_writes(id(1).as_str())?;

    let mut reader = engine
        .open_reader(&id(1), FileIndex(0), range(0, 1000))
        .await?;
    let mut bytes = Vec::new();
    let error = bounded("the canceled stream", async {
        Ok(reader.read_to_end(&mut bytes).await)
    })
    .await?
    .expect_err("the failed write cancels the stream");
    assert_eq!(error.kind(), ErrorKind::ConnectionAborted);
    assert!(bytes.is_empty(), "no byte of the failed piece is sent");
    drop(reader);

    let failure = engine.status(&id(1)).await?.failure;
    assert!(failure.is_some(), "the torrent failed");
    let playback = engine.playback().expect("a started engine").stats();
    assert_eq!(
        (playback.readers, playback.idle_readers),
        (0, 0),
        "a canceled reader is released, not kept idle"
    );
    assert!(matches!(
        engine.open_reader(&id(1), FileIndex(0), range(0, 1)).await,
        Err(EngineError::NotStarted)
    ));

    engine
        .set_desired_state(&id(1), DesiredState::Paused)
        .await?;
    engine
        .set_desired_state(&id(1), DesiredState::Started)
        .await?;
    assert_eq!(
        engine.status(&id(1)).await?.failure,
        None,
        "starting the paused torrent restarted it"
    );
    let size = spec.total_size_bytes();
    let bytes = read(engine, &id(1), 0, 0, size).await?;
    assert_eq!(bytes, expected(&spec, 0, 0, size));

    engine.shutdown().await?;
    seeder.stop().await;
    Ok(())
}
