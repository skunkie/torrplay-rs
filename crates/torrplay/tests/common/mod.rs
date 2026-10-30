// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! The application for tests: `serve` on an ephemeral loopback port over an
//! engine on an isolated loopback session, with HTTP helpers.

// Each test crate uses a different subset of the helpers.
#![allow(dead_code)]
// Helpers assert as tests do; clippy's test exemption covers only test
// functions.
#![allow(clippy::expect_used)]

use std::future::Future;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use reqwest::{Client, Response, StatusCode};
use serde_json::Value;
use tempfile::TempDir;
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use torrplay::ShutdownTimings;
use torrplay_engine::playback::PlaybackStats;
use torrplay_engine::{EngineConfig, LibrqbitEngine, NetworkConfig};
use torrplay_fixtures::{FixtureSpec, generator};

pub const KIB: u64 = 1024;
pub const MIB: u64 = 1024 * KIB;

/// The longest any single step of a test may take.
pub const STEP: Duration = Duration::from_secs(30);

/// Awaits `future`, failing if it takes longer than [`STEP`].
pub async fn bounded<T>(what: &str, future: impl Future<Output = T>) -> anyhow::Result<T> {
    tokio::time::timeout(STEP, future)
        .await
        .map_err(|_| anyhow::anyhow!("{what} did not finish within {STEP:?}"))
}

/// Polls `check` every 20 ms until it returns `Some`, for at most [`STEP`].
pub async fn eventually<T>(
    what: &str,
    mut check: impl AsyncFnMut() -> anyhow::Result<Option<T>>,
) -> anyhow::Result<T> {
    bounded(what, async {
        loop {
            if let Some(value) = check().await? {
                return Ok(value);
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await?
}

/// The settings that give an engine `config`'s tunable values.
fn settings_of(config: &EngineConfig) -> torrplay_core::settings::Explicit {
    let seconds = |duration: Duration| u64::try_from(duration.as_millis().div_ceil(1000)).ok();
    let count = |count: usize| u64::try_from(count).ok();
    torrplay_core::settings::Explicit {
        piece_wait_seconds: seconds(config.piece_wait),
        idle_reader_timeout_seconds: seconds(config.idle_reader_timeout),
        idle_reader_limit_per_torrent: count(config.idle_reader_limit_per_torrent),
        idle_reader_limit: count(config.idle_reader_limit),
        drain_period_seconds: None,
        peer_limit: count(config.peer_limit),
        cache_capacity_bytes: Some(config.cache_capacity_bytes),
        temporary_idle_seconds: None,
        temporary_torrent_limit: None,
        temporary_progress_seconds: None,
        background_downloads_enabled: None,
        dlna_enabled: None,
        dlna_name: None,
        // An isolated network has no DHT, and serve applies the setting.
        dht_enabled: Some(config.network.dht),
        log_level: None,
    }
}

/// A running application with a trigger for its shutdown.
pub struct Running {
    pub engine: Arc<LibrqbitEngine>,
    pub address: SocketAddr,
    base: String,
    shutdown: Option<oneshot::Sender<()>>,
    task: JoinHandle<anyhow::Result<()>>,
    _folder: TempDir,
    /// The data directory, which a restart keeps.
    data: Arc<TempDir>,
    peers: Vec<SocketAddr>,
    timings: ShutdownTimings,
}

impl Running {
    /// Serves over an engine reaching only `peers`, with the defaults
    /// `tune` adjusts, and waits until it is ready.
    pub async fn start(
        peers: Vec<SocketAddr>,
        timings: ShutdownTimings,
        tune: impl FnOnce(&mut EngineConfig),
    ) -> anyhow::Result<Self> {
        let running = Self::spawn(peers, timings, tune).await?;
        running.ready(&Client::new()).await?;
        Ok(running)
    }

    /// Like [`start`](Self::start), without waiting until it is ready.
    pub async fn spawn(
        peers: Vec<SocketAddr>,
        timings: ShutdownTimings,
        tune: impl FnOnce(&mut EngineConfig),
    ) -> anyhow::Result<Self> {
        Self::spawn_in(Arc::new(tempfile::tempdir()?), peers, timings, tune).await
    }

    /// Stops the application and starts it again over a new engine and the
    /// same data directory, as a restart does, and waits until it is ready.
    pub async fn restart(self, tune: impl FnOnce(&mut EngineConfig)) -> anyhow::Result<Self> {
        let (data, peers, timings) = (Arc::clone(&self.data), self.peers.clone(), self.timings);
        self.stop().await?;
        let running = Self::spawn_in(data, peers, timings, tune).await?;
        running.ready(&Client::new()).await?;
        Ok(running)
    }

    /// Ends the application as a crash would, without its shutdown, so the
    /// store records no clean shutdown, and starts it again over the same
    /// data directory. The old engine is stopped only to close its files
    /// and listeners.
    pub async fn crash_and_restart(
        self,
        tune: impl Fn(&mut EngineConfig),
        between: impl AsyncFnOnce() -> anyhow::Result<()>,
    ) -> anyhow::Result<Self> {
        let (data, peers, timings) = (Arc::clone(&self.data), self.peers.clone(), self.timings);
        self.task.abort();
        let _ = self.task.await;
        self.engine.shutdown().await?;
        between().await?;
        // A crashed process loses its database lock at once, but the aborted
        // application's server and connection tasks hold its store until
        // they wind down.
        let running = eventually("the old store's release", async || {
            match Self::spawn_in(Arc::clone(&data), peers.clone(), timings, &tune).await {
                Ok(running) => Ok(Some(running)),
                Err(error)
                    if matches!(
                        error.downcast_ref(),
                        Some(torrplay_store::StoreError::InUse { .. })
                    ) =>
                {
                    Ok(None)
                }
                Err(error) => Err(error),
            }
        })
        .await?;
        running.ready(&Client::new()).await?;
        Ok(running)
    }

    /// Stops the application cleanly, runs `between` on its data
    /// directory, and starts it again over it.
    pub async fn restart_after(
        self,
        tune: impl FnOnce(&mut EngineConfig),
        between: impl AsyncFnOnce(&std::path::Path) -> anyhow::Result<()>,
    ) -> anyhow::Result<Self> {
        let (data, peers, timings) = (Arc::clone(&self.data), self.peers.clone(), self.timings);
        self.stop().await?;
        between(data.path()).await?;
        let running = Self::spawn_in(data, peers, timings, tune).await?;
        running.ready(&Client::new()).await?;
        Ok(running)
    }

    /// The data directory, which a restart keeps.
    pub fn data_dir(&self) -> &std::path::Path {
        self.data.path()
    }

    async fn spawn_in(
        data: Arc<TempDir>,
        peers: Vec<SocketAddr>,
        timings: ShutdownTimings,
        tune: impl FnOnce(&mut EngineConfig),
    ) -> anyhow::Result<Self> {
        // Server logs go to the test's captured output, which libtest shows
        // when the test fails.
        let _ = tracing_subscriber::fmt()
            .with_test_writer()
            .with_max_level(tracing::Level::INFO)
            .try_init();
        let folder = tempfile::tempdir()?;
        let mut config = EngineConfig::new(
            folder.path().to_path_buf(),
            NetworkConfig::isolated(peers.clone()),
        );
        tune(&mut config);
        // The application runs with its stored settings, so the values a
        // test tuned become settings, in whole seconds rounded up.
        let explicit = settings_of(&config);
        let engine = LibrqbitEngine::new(config);
        let (store, mut contents) = torrplay_store::Store::open(data.path())?;
        contents.settings = torrplay_store::StoredSettings {
            explicit,
            version: contents.settings.version + 1,
        };
        store.set_settings(contents.settings.clone()).await?;
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let (shutdown, triggered) = oneshot::channel::<()>();
        let task = tokio::spawn(torrplay::serve(
            listener,
            engine.clone(),
            store,
            contents,
            torrplay::ServeOptions::default(),
            timings,
            async {
                let _ = triggered.await;
            },
        ));
        Ok(Self {
            engine,
            address,
            base: format!("http://{address}"),
            shutdown: Some(shutdown),
            task,
            _folder: folder,
            data,
            peers,
            timings,
        })
    }

    pub fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base)
    }

    pub async fn ready(&self, client: &Client) -> anyhow::Result<()> {
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

    /// The engine's reader counts.
    pub fn playback(&self) -> PlaybackStats {
        self.engine.playback().expect("a started engine").stats()
    }

    /// Starts the shutdown.
    pub fn trigger(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
    }

    /// Triggers shutdown, if it has not begun, and returns the
    /// application's result.
    pub async fn stop(mut self) -> anyhow::Result<()> {
        self.trigger();
        bounded("the shutdown", self.task).await??
    }
}

/// A multipart creation request body for `metainfo`, with its media
/// type.
pub fn upload_form(metainfo: &[u8]) -> (String, Vec<u8>) {
    const BOUNDARY: &str = "torrplay-boundary";
    let mut body = format!(
        "--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"metainfo\"; \
         filename=\"upload.torrent\"\r\nContent-Type: application/x-bittorrent\r\n\r\n"
    )
    .into_bytes();
    body.extend_from_slice(metainfo);
    body.extend_from_slice(format!("\r\n--{BOUNDARY}--\r\n").as_bytes());
    (format!("multipart/form-data; boundary={BOUNDARY}"), body)
}

/// A torrent created through the API.
pub struct Created {
    /// Its ID.
    pub id: String,
    /// Its path.
    pub path: String,
    /// The content path of each file, in metadata order.
    pub content: Vec<String>,
}

pub async fn json(response: Response) -> anyhow::Result<Value> {
    Ok(serde_json::from_slice(&response.bytes().await?)?)
}

/// Uploads `spec`'s torrent file and returns the created torrent.
pub async fn upload(
    running: &Running,
    client: &Client,
    spec: &FixtureSpec,
) -> anyhow::Result<Created> {
    let (content_type, body) = upload_form(&spec.metainfo().bytes);
    let created = client
        .post(running.url("/api/v1/torrents"))
        .header("content-type", content_type)
        .body(body)
        .send()
        .await?;
    assert_eq!(created.status(), StatusCode::CREATED);
    let torrent = json(created).await?;
    let path = torrent["links"]["self"]
        .as_str()
        .expect("a self link")
        .to_owned();
    let files = json(
        client
            .get(running.url(&format!("{path}/files")))
            .send()
            .await?,
    )
    .await?;
    let content = files["items"]
        .as_array()
        .expect("file items")
        .iter()
        .map(|file| {
            file["links"]["content"]
                .as_str()
                .expect("a content link")
                .to_owned()
        })
        .collect();
    Ok(Created {
        id: torrent["id"].as_str().expect("an ID").to_owned(),
        path,
        content,
    })
}

/// Creates a torrent from `spec`'s magnet URI and returns its path.
pub async fn magnet(
    running: &Running,
    client: &Client,
    spec: &FixtureSpec,
) -> anyhow::Result<String> {
    let created = client
        .post(running.url("/api/v1/torrents"))
        .header("content-type", "application/json")
        .body(
            serde_json::json!({ "magnet_uri": spec.magnet_uri(&spec.metainfo().info_hash) })
                .to_string(),
        )
        .send()
        .await?;
    assert_eq!(created.status(), StatusCode::CREATED);
    Ok(json(created).await?["links"]["self"]
        .as_str()
        .expect("a self link")
        .to_owned())
}

/// Sets a torrent's desired state with a conditional merge patch.
pub async fn set_desired_state(
    running: &Running,
    client: &Client,
    path: &str,
    desired_state: &str,
) -> anyhow::Result<()> {
    let configuration = running.url(&format!("{path}/configuration"));
    let current = client.get(&configuration).send().await?;
    let etag = current.headers().get("etag").expect("an ETag").clone();
    let patched = client
        .patch(&configuration)
        .header("content-type", "application/merge-patch+json")
        .header("if-match", etag)
        .body(serde_json::json!({ "desired_state": desired_state }).to_string())
        .send()
        .await?;
    assert_eq!(patched.status(), StatusCode::OK);
    Ok(())
}

/// The content of bytes `start..end` of `file`.
pub fn expected(spec: &FixtureSpec, file: usize, start: u64, end: u64) -> Vec<u8> {
    let mut bytes = vec![0; usize::try_from(end - start).expect("a small range")];
    generator::fill(spec.file_seed(file), start, &mut bytes);
    bytes
}

/// Reads `response`'s body to its end or first error, returning what
/// arrived and the error.
pub async fn read_body(mut response: Response) -> (Vec<u8>, Option<reqwest::Error>) {
    let mut bytes = Vec::new();
    loop {
        match response.chunk().await {
            Ok(Some(chunk)) => bytes.extend_from_slice(&chunk),
            Ok(None) => return (bytes, None),
            Err(error) => return (bytes, Some(error)),
        }
    }
}

/// The HTTP/2 reset reason among an error's sources, if any.
pub fn reset_reason(error: &reqwest::Error) -> Option<h2::Reason> {
    let mut source: Option<&(dyn std::error::Error + 'static)> = Some(error);
    while let Some(error) = source {
        if let Some(h2) = error.downcast_ref::<h2::Error>() {
            return h2.reason();
        }
        source = error.source();
    }
    None
}

/// The tasks alive on the current runtime.
pub fn alive_tasks() -> usize {
    tokio::runtime::Handle::current()
        .metrics()
        .num_alive_tasks()
}
