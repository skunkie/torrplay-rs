// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! The implementations under test, behind one interface. Requests are
//! adapted to each API; wire compatibility is not expected.

use std::future::Future;

use anyhow::Context;
use reqwest::{Client, StatusCode};
use serde_json::Value;

use crate::trace::Source;

/// What a benchmark needs from an implementation.
pub trait Target: Send + Sync {
    /// A torrent the target created.
    type Torrent: Clone + Send + Sync;

    /// Creates a torrent from a torrent file or a magnet URI.
    fn create(
        &self,
        source: Source,
        metainfo: &[u8],
        magnet_uri: &str,
    ) -> impl Future<Output = anyhow::Result<Self::Torrent>> + Send;

    /// Whether the torrent's metadata is available.
    fn has_metadata(
        &self,
        torrent: &Self::Torrent,
    ) -> impl Future<Output = anyhow::Result<bool>> + Send;

    /// The URL serving the content of the file named `file`, the last
    /// component of its path. Needs metadata.
    fn content_url(
        &self,
        torrent: &Self::Torrent,
        file: &str,
    ) -> impl Future<Output = anyhow::Result<String>> + Send;

    /// Bytes the torrent received from peers.
    fn downloaded_bytes(
        &self,
        torrent: &Self::Torrent,
    ) -> impl Future<Output = anyhow::Result<u64>> + Send;

    /// The cache's usage and eviction count.
    fn cache(&self) -> impl Future<Output = anyhow::Result<CacheSample>> + Send;
}

/// A sample of a target's cache.
#[derive(Clone, Copy, Debug, Default, serde::Serialize, PartialEq, Eq)]
pub struct CacheSample {
    pub usage_bytes: u64,
    /// Evictions so far, when the target reports them.
    pub eviction_count: Option<u64>,
}

/// TorrPlay's native API.
pub struct TorrPlay {
    client: Client,
    base: String,
}

impl TorrPlay {
    /// Talks to the API at `base`, such as `http://127.0.0.1:8642`.
    pub fn new(client: Client, base: String) -> Self {
        Self { client, base }
    }

    /// Sets the cache capacity setting and checks that the cache took it.
    pub async fn configure(&self, cache_capacity_bytes: u64) -> anyhow::Result<()> {
        let path = "/api/v1/settings";
        let current = self
            .client
            .get(format!("{}{path}", self.base))
            .send()
            .await?;
        let status = current.status();
        anyhow::ensure!(status == StatusCode::OK, "GET {path}: {status}");
        let etag = current
            .headers()
            .get(reqwest::header::ETAG)
            .context("the settings have no ETag")?
            .clone();
        let response = self
            .client
            .patch(format!("{}{path}", self.base))
            .header(
                reqwest::header::CONTENT_TYPE,
                "application/merge-patch+json",
            )
            .header(reqwest::header::IF_MATCH, etag)
            .body(serde_json::json!({ "cache_capacity_bytes": cache_capacity_bytes }).to_string())
            .send()
            .await?;
        let status = response.status();
        anyhow::ensure!(
            status == StatusCode::OK,
            "updating settings: {status}: {}",
            response.text().await?
        );
        let cache = self.json("/api/v1/cache/statistics").await?;
        anyhow::ensure!(
            cache["capacity_bytes"].as_u64() == Some(cache_capacity_bytes),
            "the cache capacity is {} rather than {cache_capacity_bytes}",
            cache["capacity_bytes"]
        );
        Ok(())
    }

    async fn json(&self, path: &str) -> anyhow::Result<Value> {
        let response = self
            .client
            .get(format!("{}{path}", self.base))
            .send()
            .await?;
        let status = response.status();
        let body = response.bytes().await?;
        anyhow::ensure!(status == StatusCode::OK, "GET {path}: {status}");
        Ok(serde_json::from_slice(&body)?)
    }
}

/// The multipart body of a creation request for `metainfo`, with its
/// media type.
fn upload_form(metainfo: &[u8]) -> (String, Vec<u8>) {
    const BOUNDARY: &str = "torrplay-bench-boundary";
    let mut body = format!(
        "--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"metainfo\"; \
         filename=\"bench.torrent\"\r\nContent-Type: application/x-bittorrent\r\n\r\n"
    )
    .into_bytes();
    body.extend_from_slice(metainfo);
    body.extend_from_slice(format!("\r\n--{BOUNDARY}--\r\n").as_bytes());
    (format!("multipart/form-data; boundary={BOUNDARY}"), body)
}

impl Target for TorrPlay {
    /// The torrent's path, from its `self` link.
    type Torrent = String;

    async fn create(
        &self,
        source: Source,
        metainfo: &[u8],
        magnet_uri: &str,
    ) -> anyhow::Result<String> {
        let request = self.client.post(format!("{}/api/v1/torrents", self.base));
        let request = match source {
            Source::Upload => {
                let (content_type, body) = upload_form(metainfo);
                request.header("content-type", content_type).body(body)
            }
            Source::Magnet => request
                .header("content-type", "application/json")
                .body(serde_json::json!({ "magnet_uri": magnet_uri }).to_string()),
        };
        let response = request.send().await?;
        let status = response.status();
        let body: Value = serde_json::from_slice(&response.bytes().await?)?;
        anyhow::ensure!(
            status == StatusCode::CREATED,
            "creating a torrent: {status}: {body}"
        );
        body["links"]["self"]
            .as_str()
            .map(str::to_owned)
            .context("the created torrent has no self link")
    }

    async fn has_metadata(&self, torrent: &String) -> anyhow::Result<bool> {
        let torrent = self.json(torrent).await?;
        anyhow::ensure!(
            torrent["observed_state"] != "failed",
            "the torrent failed: {}",
            torrent["failure"]
        );
        Ok(torrent["observed_state"] != "retrieving_metadata")
    }

    async fn content_url(&self, torrent: &String, file: &str) -> anyhow::Result<String> {
        let files = self.json(&format!("{torrent}/files")).await?;
        let content = files["items"]
            .as_array()
            .context("the file list has no items")?
            .iter()
            .find(|item| {
                item["path"]
                    .as_array()
                    .and_then(|path| path.last())
                    .and_then(Value::as_str)
                    == Some(file)
            })
            .and_then(|item| item["links"]["content"].as_str())
            .with_context(|| format!("the torrent has no file named {file}"))?;
        Ok(format!("{}{content}", self.base))
    }

    async fn downloaded_bytes(&self, torrent: &String) -> anyhow::Result<u64> {
        self.json(&format!("{torrent}/statistics")).await?["downloaded_bytes"]
            .as_u64()
            .context("the statistics have no downloaded_bytes")
    }

    async fn cache(&self) -> anyhow::Result<CacheSample> {
        let cache = self.json("/api/v1/cache/statistics").await?;
        Ok(CacheSample {
            usage_bytes: cache["usage_bytes"].as_u64().context("no usage_bytes")?,
            eviction_count: Some(
                cache["eviction_count"]
                    .as_u64()
                    .context("no eviction_count")?,
            ),
        })
    }
}

/// Go TorrPlay's API, the baseline.
///
/// Creating a torrent waits for its metadata, so the creation time is the
/// time to metadata. Go TorrPlay reports no eviction count.
pub struct GoTorrPlay {
    client: Client,
    base: String,
}

/// A torrent Go TorrPlay created.
#[derive(Clone, Debug)]
pub struct GoTorrent {
    hash: String,
    /// Each file's name and full path within the torrent.
    files: Vec<(String, String)>,
}

impl GoTorrPlay {
    /// Talks to the API at `base`, such as `http://127.0.0.1:8090`.
    pub fn new(client: Client, base: String) -> Self {
        Self { client, base }
    }

    /// Sets the cache capacity and turns DHT off, so peers come only from
    /// the tracker, and checks that the cache took the capacity.
    pub async fn configure(&self, cache_capacity_bytes: u64) -> anyhow::Result<()> {
        let response = self
            .client
            .patch(format!("{}/api/v1/settings", self.base))
            .header("content-type", "application/json")
            .body(
                serde_json::json!({
                    "max_memory": cache_capacity_bytes,
                    "torrent_client": { "disable_dht": true, "seed": false },
                })
                .to_string(),
            )
            .send()
            .await?;
        let status = response.status();
        anyhow::ensure!(
            status.is_success(),
            "updating settings: {status}: {}",
            response.text().await?
        );
        let memory = self.json("/api/stats/memory").await?;
        anyhow::ensure!(
            memory["max_memory"].as_u64() == Some(cache_capacity_bytes),
            "the cache capacity is {} rather than {cache_capacity_bytes}",
            memory["max_memory"]
        );
        Ok(())
    }

    async fn json(&self, path: &str) -> anyhow::Result<Value> {
        let response = self
            .client
            .get(format!("{}{path}", self.base))
            .send()
            .await?;
        let status = response.status();
        let body = response.bytes().await?;
        anyhow::ensure!(status == StatusCode::OK, "GET {path}: {status}");
        Ok(serde_json::from_slice(&body)?)
    }
}

impl Target for GoTorrPlay {
    type Torrent = GoTorrent;

    async fn create(
        &self,
        source: Source,
        metainfo: &[u8],
        magnet_uri: &str,
    ) -> anyhow::Result<GoTorrent> {
        let request = self.client.post(format!("{}/api/v1/torrents", self.base));
        let request = match source {
            Source::Upload => {
                const BOUNDARY: &str = "torrplay-bench-boundary";
                let mut body = format!(
                    "--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"storage\"\r\n\r\n\
                     memory\r\n--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"file\"; \
                     filename=\"bench.torrent\"\r\nContent-Type: application/x-bittorrent\r\n\r\n"
                )
                .into_bytes();
                body.extend_from_slice(metainfo);
                body.extend_from_slice(format!("\r\n--{BOUNDARY}--\r\n").as_bytes());
                request
                    .header(
                        "content-type",
                        format!("multipart/form-data; boundary={BOUNDARY}"),
                    )
                    .body(body)
            }
            Source::Magnet => request
                .header("content-type", "application/json")
                .body(serde_json::json!({ "magnet": magnet_uri, "storage": "memory" }).to_string()),
        };
        let response = request.send().await?;
        let status = response.status();
        let body: Value = serde_json::from_slice(&response.bytes().await?)?;
        anyhow::ensure!(
            status == StatusCode::CREATED,
            "creating a torrent: {status}: {body}"
        );
        let files = body["files"]
            .as_array()
            .context("the created torrent has no files")?
            .iter()
            .filter_map(|file| {
                Some((
                    file["name"].as_str()?.to_owned(),
                    file["path"].as_str()?.to_owned(),
                ))
            })
            .collect();
        Ok(GoTorrent {
            hash: body["hash"]
                .as_str()
                .context("the created torrent has no hash")?
                .to_owned(),
            files,
        })
    }

    async fn has_metadata(&self, _torrent: &GoTorrent) -> anyhow::Result<bool> {
        // Creation answered only once metadata had arrived.
        Ok(true)
    }

    async fn content_url(&self, torrent: &GoTorrent, file: &str) -> anyhow::Result<String> {
        let (_, path) = torrent
            .files
            .iter()
            .find(|(name, _)| name == file)
            .with_context(|| format!("the torrent has no file named {file}"))?;
        Ok(reqwest::Url::parse_with_params(
            &format!("{}/api/v1/stream/{}", self.base, torrent.hash),
            [("path", path)],
        )?
        .into())
    }

    async fn downloaded_bytes(&self, torrent: &GoTorrent) -> anyhow::Result<u64> {
        self.json(&format!("/api/stats/torrents/{}", torrent.hash))
            .await?["bytes_read_data"]
            .as_u64()
            .context("the statistics have no bytes_read_data")
    }

    async fn cache(&self) -> anyhow::Result<CacheSample> {
        let memory = self.json("/api/stats/memory").await?;
        Ok(CacheSample {
            usage_bytes: memory["used_memory"].as_u64().context("no used_memory")?,
            eviction_count: None,
        })
    }
}
