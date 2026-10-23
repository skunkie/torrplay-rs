// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! A short trace run end to end on loopback: the local tracker, a seeder,
//! and TorrPlay serving in this process, whose `/proc` entry the runner
//! samples. Small fixtures stand in for the benchmark's own.

#![cfg(target_os = "linux")]
// Helpers assert as tests do; clippy's test exemption covers only test
// functions.
#![allow(clippy::expect_used)]

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use tokio::net::TcpListener;
use tokio::sync::oneshot;
use torrplay::ShutdownTimings;
use torrplay_bench::fixtures::FixtureDir;
use torrplay_bench::target::TorrPlay;
use torrplay_bench::{runner, seed, trace, tracker};
use torrplay_engine::{EngineConfig, LibrqbitEngine, NetworkConfig};
use torrplay_fixtures::catalog;

const TRACE: &str = r#"
{"t_ms": 0, "event": "configure", "cache_capacity_bytes": 8388608}
{"t_ms": 0, "reader": "r1", "fixture": "single-small", "file": "single-small.bin", "event": "request", "range_start": 0, "range_end": null, "consume_bytes": 1048576, "consume_rate_bytes_per_second": null}
{"t_ms": 0, "reader": "r2", "fixture": "multi-boundary", "file": "part-3.bin", "event": "request", "source": "magnet", "range_start": 0, "range_end": null, "consume_bytes": 524289, "consume_rate_bytes_per_second": 2000000}
{"t_ms": 100, "reader": "r1", "fixture": "single-small", "file": "single-small.bin", "event": "request", "range_start": 500000, "range_end": 599999, "consume_bytes": 100000, "consume_rate_bytes_per_second": 1000000}
{"t_ms": 200, "reader": "r1", "fixture": "single-small", "file": "single-small.bin", "event": "head"}
"#;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_trace_runs_against_torrplay_through_the_local_tracker() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let fixtures = FixtureDir::new(dir.path());
    let specs = [catalog::single_small(), catalog::multi_boundary()];
    let tracker_listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?;
    let announce = format!("http://{}/announce", tracker_listener.local_addr()?);
    fixtures.write(&specs, &announce)?;
    let seeder = seed::start(&fixtures, &specs, (Ipv4Addr::LOCALHOST, 0).into()).await?;
    let Some(SocketAddr::V4(seeder_addr)) = seeder.listen_addr() else {
        anyhow::bail!("the seeder has no IPv4 address");
    };
    let tracking = tokio::spawn(tracker::serve(tracker_listener, vec![seeder_addr]));

    // Peers come only from the tracker, as in the benchmark.
    let session = tempfile::tempdir()?;
    let trace = trace::parse(TRACE)?;
    let config = EngineConfig::new(
        session.path().to_path_buf(),
        NetworkConfig {
            listen: (Ipv4Addr::LOCALHOST, 0).into(),
            dht: false,
            trackers: true,
            initial_peers: Vec::new(),
        },
    );
    let (store, contents) = torrplay_store::Store::open(&session.path().join("data"))?;
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?;
    let base = format!("http://{}", listener.local_addr()?);
    let (shutdown, triggered) = oneshot::channel::<()>();
    let serving = tokio::spawn(torrplay::serve(
        listener,
        LibrqbitEngine::new(config),
        store,
        contents,
        ShutdownTimings::default(),
        async {
            let _ = triggered.await;
        },
    ));
    let client = reqwest::Client::new();
    tokio::time::timeout(Duration::from_secs(30), async {
        while !client
            .get(format!("{base}/readyz"))
            .send()
            .await
            .is_ok_and(|response| response.status().is_success())
        {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await?;

    let target = Arc::new(TorrPlay::new(client.clone(), base));
    // As a benchmark run does, through the settings.
    target.configure(trace.cache_capacity_bytes).await?;
    let record = tokio::time::timeout(
        Duration::from_secs(60),
        runner::run(target, client, std::process::id(), &trace, &fixtures),
    )
    .await??;

    assert_eq!(record.cache_capacity_bytes, 8 * 1024 * 1024);
    assert_eq!(record.requests.len(), 4);
    for request in &record.requests {
        assert_eq!(request.error, None, "{request:?}");
    }
    let [first, seek, head] = [
        &record.requests[0],
        &record.requests[1],
        &record.requests[2],
    ];
    assert_eq!(
        (first.status, first.body_bytes, first.seek),
        (Some(206), 1_048_576, false)
    );
    assert!(first.throughput_bytes_per_second.is_some());
    assert!(
        first.time_to_first_playable_ms.is_none(),
        "1 MiB is short of playable"
    );
    assert_eq!((seek.body_bytes, seek.seek), (100_000, true));
    assert!(seek.time_to_first_byte_ms.is_some());
    assert_eq!((head.kind, head.status), ("head", Some(200)));
    let magnet = &record.requests[3];
    assert_eq!((magnet.reader.as_str(), magnet.body_bytes), ("r2", 524_289));

    let by_fixture = |name: &str| {
        record
            .torrents
            .iter()
            .find(|torrent| torrent.fixture == name)
            .expect("a torrent")
    };
    let retrieved = by_fixture("multi-boundary");
    assert!(
        retrieved.metadata_ms.is_some(),
        "metadata arrived through the tracker"
    );
    assert_eq!(retrieved.unique_consumed_bytes, 524_289);
    let uploaded = by_fixture("single-small");
    assert_eq!(uploaded.unique_consumed_bytes, 1_048_576);
    assert!(uploaded.downloaded_bytes >= Some(1_048_576));
    assert!(!record.samples.is_empty());
    assert!(record.summary.peak_rss_bytes > 0);
    assert!(record.summary.peak_cache_usage_bytes <= 8 * 1024 * 1024);

    let _ = shutdown.send(());
    tokio::time::timeout(Duration::from_secs(30), serving).await???;
    seeder.stop().await;
    tracking.abort();
    Ok(())
}
