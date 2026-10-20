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
        torrent_file: &[u8],
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
    pub eviction_count: u64,
}

/// TorrPlay's native API.
pub struct TorrPlay {
    client: Client,
    base: String,
}

impl TorrPlay {
    /// Talks to the API at `base`, such as `http://127.0.0.1:8080`.
    pub fn new(client: Client, base: String) -> Self {
        Self { client, base }
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

/// The multipart body of a creation request for `torrent_file`, with its
/// media type.
fn upload_form(torrent_file: &[u8]) -> (String, Vec<u8>) {
    const BOUNDARY: &str = "torrplay-bench-boundary";
    let mut body = format!(
        "--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"torrent_file\"; \
         filename=\"bench.torrent\"\r\nContent-Type: application/x-bittorrent\r\n\r\n"
    )
    .into_bytes();
    body.extend_from_slice(torrent_file);
    body.extend_from_slice(format!("\r\n--{BOUNDARY}--\r\n").as_bytes());
    (format!("multipart/form-data; boundary={BOUNDARY}"), body)
}

impl Target for TorrPlay {
    /// The torrent's path, from its `self` link.
    type Torrent = String;

    async fn create(
        &self,
        source: Source,
        torrent_file: &[u8],
        magnet_uri: &str,
    ) -> anyhow::Result<String> {
        let request = self.client.post(format!("{}/api/v1/torrents", self.base));
        let request = match source {
            Source::Upload => {
                let (content_type, body) = upload_form(torrent_file);
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
            eviction_count: cache["eviction_count"]
                .as_u64()
                .context("no eviction_count")?,
        })
    }
}
