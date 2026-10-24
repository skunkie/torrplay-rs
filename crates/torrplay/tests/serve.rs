// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! The application through its API: readiness after startup, HTTP/2, and
//! the shutdown sequence of LIF-10, on loopback sessions that reach no
//! public swarm.

mod common;

use std::time::{Duration, Instant};

use common::{Running, alive_tasks, bounded, magnet, upload, upload_form};
use reqwest::{Client, StatusCode, Version};
use tokio::net::TcpListener;
use torrplay::ShutdownTimings;
use torrplay_engine::{EngineConfig, LibrqbitEngine, NetworkConfig};
use torrplay_fixtures::catalog;

/// Streams wait for pieces far longer than any test.
fn patient(config: &mut EngineConfig) {
    config.piece_wait = Duration::from_secs(600);
}

/// LIF-10: readiness ends at once and new streams and state changes are
/// refused while reads are still served, a stream in progress gets the
/// drain period and is then canceled, and the listener closes.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn shutdown_drains_streams_then_closes_the_listener() -> anyhow::Result<()> {
    let drain = Duration::from_secs(1);
    let timings = ShutdownTimings {
        drain: Some(drain),
        limit: Duration::from_secs(20),
    };
    let mut running = Running::start(Vec::new(), timings, patient).await?;
    let client = Client::new();
    let created = upload(&running, &client, &catalog::single_small()).await?;
    let content = &created.content[0];

    // Without peers, the stream sends its headers and then waits.
    let stream = client.get(running.url(content)).send().await?;
    assert_eq!(stream.status(), StatusCode::OK);
    let streaming = tokio::spawn(stream.bytes());

    let begun = Instant::now();
    running.trigger();
    bounded("readiness ending", async {
        loop {
            let response = client.get(running.url("/readyz")).send().await;
            if response.is_ok_and(|response| response.status() == StatusCode::SERVICE_UNAVAILABLE) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await?;
    let (content_type, body) = upload_form(&catalog::multi_boundary().metainfo().bytes);
    let refused = client
        .post(running.url("/api/v1/torrents"))
        .header("content-type", content_type)
        .body(body)
        .send()
        .await?;
    assert_eq!(refused.status(), StatusCode::SERVICE_UNAVAILABLE);
    let new_stream = client.get(running.url(content)).send().await?;
    assert_eq!(new_stream.status(), StatusCode::SERVICE_UNAVAILABLE);
    let listed = client.get(running.url("/api/v1/torrents")).send().await?;
    assert_eq!(listed.status(), StatusCode::OK, "reads are still served");
    assert!(
        begun.elapsed() < drain,
        "all of this happened while draining"
    );

    let ended = bounded("the canceled stream", streaming).await??;
    assert!(ended.is_err(), "the stream is canceled, not completed");
    assert!(
        begun.elapsed() >= drain,
        "the stream ran until the drain period ended"
    );
    let address = running.address;
    running.stop().await?;
    assert!(
        tokio::net::TcpStream::connect(address).await.is_err(),
        "the listener is closed"
    );
    Ok(())
}

/// LIF-10's task cleanup: once `serve` returns, every task the application
/// started has ended, those of librqbit's session included, though a stream
/// and a metadata retrieval were in progress when shutdown began.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn shutdown_joins_every_task() -> anyhow::Result<()> {
    let before = alive_tasks();
    let timings = ShutdownTimings {
        drain: Some(Duration::from_millis(500)),
        limit: Duration::from_secs(20),
    };
    let running = Running::start(Vec::new(), timings, patient).await?;
    let client = Client::new();
    let created = upload(&running, &client, &catalog::single_small()).await?;
    magnet(&running, &client, &catalog::multi_boundary()).await?;
    let stream = client.get(running.url(&created.content[0])).send().await?;
    assert_eq!(stream.status(), StatusCode::OK);
    assert!(alive_tasks() > before);

    running.stop().await?;
    // The client's own connection tasks end once it is dropped.
    drop((stream, client));
    bounded("the client's tasks ending", async {
        while alive_tasks() > before {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .map_err(|error| anyhow::anyhow!("{error}: {} tasks alive, {before} before", alive_tasks()))?;
    Ok(())
}

/// Without streams in progress, shutdown does not wait for the drain
/// period.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn shutdown_without_streams_skips_the_drain() -> anyhow::Result<()> {
    let timings = ShutdownTimings {
        drain: Some(Duration::from_secs(60)),
        limit: Duration::from_secs(20),
    };
    let running = Running::start(Vec::new(), timings, |_| {}).await?;
    let begun = Instant::now();
    running.stop().await?;
    assert!(begun.elapsed() < Duration::from_secs(10));
    Ok(())
}

/// The listener also serves HTTP/2 over cleartext to clients with prior
/// knowledge, and serves before the engine is ready.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn http2_is_served_beside_http1() -> anyhow::Result<()> {
    let running = Running::spawn(Vec::new(), ShutdownTimings::default(), |_| {}).await?;
    let client = Client::builder().http2_prior_knowledge().build()?;
    let live = client.get(running.url("/livez")).send().await?;
    assert_eq!(live.version(), Version::HTTP_2);
    assert_eq!(live.status(), StatusCode::OK);
    running.ready(&client).await?;
    let listed = client.get(running.url("/api/v1/torrents")).send().await?;
    assert_eq!(
        (listed.version(), listed.status()),
        (Version::HTTP_2, StatusCode::OK)
    );
    running.stop().await
}

/// An engine that cannot start stops the application with an error.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_engine_that_cannot_start_stops_the_application() -> anyhow::Result<()> {
    let taken = std::net::TcpListener::bind("127.0.0.1:0")?;
    let folder = tempfile::tempdir()?;
    let mut network = NetworkConfig::isolated(Vec::new());
    network.listen = taken.local_addr()?;
    let config = EngineConfig::new(folder.path().to_path_buf(), network);
    let (store, contents) = torrplay_store::Store::open(&folder.path().join("data"))?;
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let result = bounded(
        "the failed start",
        torrplay::serve(
            listener,
            LibrqbitEngine::new(config),
            store,
            contents,
            torrplay::ServeOptions::default(),
            ShutdownTimings::default(),
            std::future::pending(),
        ),
    )
    .await?;
    let error = result.expect_err("the peer port is taken");
    assert!(
        format!("{error:#}").contains("the engine did not start"),
        "{error:#}"
    );
    Ok(())
}
