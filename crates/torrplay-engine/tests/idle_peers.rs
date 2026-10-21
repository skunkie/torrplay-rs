// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! Idle peers: a torrent without readers keeps its peers, so its first
//! stream does not wait for librqbit to reconnect them after a backoff.

mod support;

use std::time::{Duration, Instant};

use librqbit::PeerConnectionOptions;
use support::{MIB, Seeder, expected, id, read, start_with, upload};
use torrplay_core::engine::Engine;
use torrplay_core::lifecycle::DesiredState;
use torrplay_fixtures::catalog;

/// How long the seeder lets a peer send nothing before dropping it.
const SEEDER_IDLE_TIMEOUT: Duration = Duration::from_secs(1);

/// Keep-alives keep the seeder connected; without them, as with librqbit's
/// two-minute default, the seeder drops the idle torrent, and opening a
/// reader reconnects it at once.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_stream_after_an_idle_period_starts_promptly() -> anyhow::Result<()> {
    for keep_alive_interval in [SEEDER_IDLE_TIMEOUT / 4, Duration::from_secs(120)] {
        first_read_after_idling(keep_alive_interval).await?;
    }
    Ok(())
}

async fn first_read_after_idling(keep_alive_interval: Duration) -> anyhow::Result<()> {
    let spec = catalog::single_small();
    let seeder = Seeder::start_with_peer_options(
        &spec,
        &spec.metainfo().bytes,
        PeerConnectionOptions {
            read_write_timeout: Some(SEEDER_IDLE_TIMEOUT),
            ..Default::default()
        },
    )
    .await?;
    let started = start_with(vec![seeder.addr], |config| {
        *config = config.clone().with_cache_capacity(8 * MIB);
        config.peer_keep_alive_interval = keep_alive_interval;
    })
    .await?;
    let engine = &started.engine;
    engine
        .add(&id(1), &upload(&spec), DesiredState::Started)
        .await?;

    // Long enough for the seeder to drop a peer that sends nothing.
    tokio::time::sleep(SEEDER_IDLE_TIMEOUT * 3).await;
    let requested = Instant::now();
    let bytes = read(engine, &id(1), 0, 0, 1000).await?;
    let elapsed = requested.elapsed();
    assert_eq!(bytes, expected(&spec, 0, 0, 1000));
    assert!(
        elapsed < Duration::from_secs(2),
        "with keep-alives every {keep_alive_interval:?}, the first read waited \
         {elapsed:?}, as for a reconnect after a backoff"
    );

    engine.shutdown().await?;
    seeder.stop().await;
    Ok(())
}
