// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! The application through its API: readiness after startup, HTTP/2, and
//! the shutdown sequence of LIF-10, on loopback sessions that reach no
//! public swarm.

// Helpers assert as tests do; clippy's test exemption covers only test
// functions.
#![allow(clippy::expect_used)]

use std::future::Future;
use std::net::SocketAddr;
use std::time::{Duration, Instant};

use reqwest::{Client, StatusCode, Version};
use tempfile::TempDir;
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use torrplay::ShutdownTimings;
use torrplay_engine::{EngineConfig, NetworkConfig};
use torrplay_fixtures::catalog;

/// The longest any single step of a test may take.
const STEP: Duration = Duration::from_secs(30);

async fn bounded<T>(what: &str, future: impl Future<Output = T>) -> anyhow::Result<T> {
    tokio::time::timeout(STEP, future)
        .await
        .map_err(|_| anyhow::anyhow!("{what} did not finish within {STEP:?}"))
}

/// A running application with a trigger for its shutdown.
struct Running {
    base: String,
    address: SocketAddr,
    shutdown: Option<oneshot::Sender<()>>,
    task: JoinHandle<anyhow::Result<()>>,
    _folder: TempDir,
}

impl Running {
    /// Serves on an ephemeral loopback port over an isolated engine whose
    /// streams wait for pieces far longer than any test.
    async fn start(timings: ShutdownTimings) -> anyhow::Result<Self> {
        let folder = tempfile::tempdir()?;
        let mut config = EngineConfig::new(
            folder.path().to_path_buf(),
            NetworkConfig::isolated(Vec::new()),
        );
        config.piece_wait = Duration::from_secs(600);
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let (shutdown, triggered) = oneshot::channel::<()>();
        let task = tokio::spawn(torrplay::serve(listener, config, timings, async {
            let _ = triggered.await;
        }));
        Ok(Self {
            base: format!("http://{address}"),
            address,
            shutdown: Some(shutdown),
            task,
            _folder: folder,
        })
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base)
    }

    async fn ready(&self, client: &Client) -> anyhow::Result<()> {
        bounded("readiness", async {
            loop {
                if let Ok(response) = client.get(self.url("/readyz")).send().await
                    && response.status() == StatusCode::OK
                {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
    }

    /// Starts the shutdown.
    fn trigger(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
    }

    /// Triggers shutdown, if it has not begun, and returns the
    /// application's result.
    async fn stop(mut self) -> anyhow::Result<()> {
        self.trigger();
        bounded("the shutdown", self.task).await??
    }
}

/// A multipart creation request body for `torrent_file`.
fn upload_form(torrent_file: &[u8]) -> (String, Vec<u8>) {
    const BOUNDARY: &str = "torrplay-boundary";
    let mut body = format!(
        "--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"torrent_file\"; \
         filename=\"upload.torrent\"\r\nContent-Type: application/x-bittorrent\r\n\r\n"
    )
    .into_bytes();
    body.extend_from_slice(torrent_file);
    body.extend_from_slice(format!("\r\n--{BOUNDARY}--\r\n").as_bytes());
    (format!("multipart/form-data; boundary={BOUNDARY}"), body)
}

/// Creates a torrent from a fixture and returns its first file's content
/// path.
async fn create(running: &Running, client: &Client) -> anyhow::Result<String> {
    let (content_type, body) = upload_form(&catalog::single_small().metainfo().bytes);
    let created = client
        .post(running.url("/api/v1/torrents"))
        .header("content-type", content_type)
        .body(body)
        .send()
        .await?;
    assert_eq!(created.status(), StatusCode::CREATED);
    let location = created
        .headers()
        .get("location")
        .and_then(|location| location.to_str().ok())
        .expect("a Location")
        .to_owned();
    let files: serde_json::Value = serde_json::from_slice(
        &client
            .get(running.url(&format!("{location}/files")))
            .send()
            .await?
            .bytes()
            .await?,
    )?;
    Ok(files["items"][0]["links"]["content"]
        .as_str()
        .expect("a content link")
        .to_owned())
}

/// LIF-10: readiness ends at once and new streams and state changes are
/// refused while reads are still served, a stream in progress gets the
/// drain period and is then canceled, and the listener closes.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn shutdown_drains_streams_then_closes_the_listener() -> anyhow::Result<()> {
    let drain = Duration::from_secs(1);
    let mut running = Running::start(ShutdownTimings {
        drain,
        limit: Duration::from_secs(20),
    })
    .await?;
    let client = Client::new();
    running.ready(&client).await?;
    let content = create(&running, &client).await?;

    // Without peers, the stream sends its headers and then waits.
    let stream = client.get(running.url(&content)).send().await?;
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
    let new_stream = client.get(running.url(&content)).send().await?;
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

/// Without streams in progress, shutdown does not wait for the drain
/// period.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn shutdown_without_streams_skips_the_drain() -> anyhow::Result<()> {
    let running = Running::start(ShutdownTimings {
        drain: Duration::from_secs(60),
        limit: Duration::from_secs(20),
    })
    .await?;
    let client = Client::new();
    running.ready(&client).await?;
    let begun = Instant::now();
    running.stop().await?;
    assert!(begun.elapsed() < Duration::from_secs(10));
    Ok(())
}

/// The listener also serves HTTP/2 over cleartext to clients with prior
/// knowledge, and serves before the engine is ready.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn http2_is_served_beside_http1() -> anyhow::Result<()> {
    let running = Running::start(ShutdownTimings::default()).await?;
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
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let result = bounded(
        "the failed start",
        torrplay::serve(
            listener,
            config,
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
