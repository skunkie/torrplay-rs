// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! The per-torrent peer limit: a torrent offered more peers than its limit
//! connects to no more than the limit at any time.

mod support;

use std::time::Duration;

use support::{MIB, Seeder, bounded, expected, id, read, start_with, upload};
use torrplay_core::engine::Engine;
use torrplay_core::lifecycle::DesiredState;
use torrplay_engine::LibrqbitEngine;
use torrplay_fixtures::catalog;

async fn peers(engine: &LibrqbitEngine) -> anyhow::Result<u64> {
    Ok(engine.torrent_statistics(&id(1)).await?.peer_count)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_torrent_connects_to_at_most_its_peer_limit() -> anyhow::Result<()> {
    let spec = catalog::single_small();
    let mut seeders = Vec::new();
    for _ in 0..3 {
        seeders.push(Seeder::start(&spec, &spec.metainfo().bytes).await?);
    }
    let started = start_with(seeders.iter().map(|s| s.addr).collect(), |config| {
        *config = config.clone().with_cache_capacity(8 * MIB);
        config.peer_limit = 2;
    })
    .await?;
    let engine = &started.engine;
    engine
        .add(&id(1), &upload(&spec), DesiredState::Started)
        .await?;
    let bytes = read(engine, &id(1), 0, 0, 1000).await?;
    assert_eq!(bytes, expected(&spec, 0, 0, 1000));

    bounded("peers connecting", async {
        while peers(engine).await? < 2 {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        Ok(())
    })
    .await?;
    // Three seeders keep offering themselves; the limit holds throughout.
    for _ in 0..20 {
        let count = peers(engine).await?;
        assert!(count <= 2, "{count} peers connected, beyond the limit of 2");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    engine.shutdown().await?;
    for seeder in seeders {
        seeder.stop().await;
    }
    Ok(())
}
