// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! The Go TorrPlay adapter against a fake of the endpoints it uses, which
//! records what it receives, so the adapter's requests and parsing are
//! checked without a Go toolchain.

// Helpers assert as tests do; clippy's test exemption covers only test
// functions.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{get, patch, post};
use serde_json::{Value, json};
use tokio::net::TcpListener;
use torrplay_bench::target::{CacheSample, GoTorrPlay, Target};
use torrplay_bench::trace::Source;

#[derive(Default)]
struct Received {
    settings: Option<Value>,
    creations: Vec<(String, Vec<u8>)>,
    max_memory: u64,
    // A capacity to report instead of the one set, as a server that clamps
    // it would.
    reported_max_memory: Option<u64>,
}

type Shared = Arc<Mutex<Received>>;

fn json_body(value: Value) -> impl IntoResponse {
    ([("content-type", "application/json")], value.to_string())
}

async fn settings(State(received): State<Shared>, body: Bytes) -> StatusCode {
    let body: Value = serde_json::from_slice(&body).unwrap();
    let mut received = received.lock().unwrap();
    received.max_memory = body["max_memory"].as_u64().unwrap();
    received.settings = Some(body);
    StatusCode::NO_CONTENT
}

async fn memory(State(received): State<Shared>) -> impl IntoResponse {
    let received = received.lock().unwrap();
    let max_memory = received.reported_max_memory.unwrap_or(received.max_memory);
    json_body(json!({
        "active_torrents": 1, "max_memory": max_memory, "total_pieces": 3, "used_memory": 4096,
    }))
}

async fn create(
    State(received): State<Shared>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    let content_type = headers["content-type"].to_str().unwrap().to_owned();
    received
        .lock()
        .unwrap()
        .creations
        .push((content_type, body.to_vec()));
    (
        StatusCode::CREATED,
        json_body(json!({
            "hash": "ab".repeat(20), "magnet": "magnet:?", "name": "bench-episodes",
            "total_size": 10, "piece_count": 1,
            "files": [
                { "name": "episode-1.mkv", "path": "bench-episodes/episode-1.mkv", "length": 5 },
                { "name": "episode 2.mkv", "path": "bench-episodes/episode 2.mkv", "length": 5 },
            ],
        })),
    )
}

async fn stats() -> impl IntoResponse {
    json_body(json!({ "bytes_read_data": 123_456, "active_peers": 1 }))
}

async fn fake() -> (String, Shared) {
    let received = Shared::default();
    let router = Router::new()
        .route("/api/v1/settings", patch(settings))
        .route("/api/stats/memory", get(memory))
        .route("/api/v1/torrents", post(create))
        .route("/api/stats/torrents/{hash}", get(stats))
        .with_state(received.clone());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, router).await });
    (base, received)
}

#[tokio::test]
async fn the_go_adapter_speaks_go_torrplays_api() -> anyhow::Result<()> {
    let (base, received) = fake().await;
    let go = GoTorrPlay::new(reqwest::Client::new(), base.clone());

    go.configure(64 * 1024 * 1024).await?;
    assert_eq!(
        received.lock().unwrap().settings,
        Some(json!({
            "max_memory": 67_108_864,
            "torrent_client": { "disable_dht": true, "seed": false },
        }))
    );

    let uploaded = go.create(Source::Upload, b"d4:infoe", "").await?;
    let magnet = go
        .create(Source::Magnet, b"", "magnet:?xt=urn:btih:x")
        .await?;
    {
        let received = received.lock().unwrap();
        let (content_type, body) = &received.creations[0];
        assert!(content_type.starts_with("multipart/form-data; boundary="));
        let body = String::from_utf8_lossy(body);
        assert!(
            body.contains("name=\"storage\"\r\n\r\nmemory\r\n"),
            "{body}"
        );
        assert!(
            body.contains("name=\"file\"; filename=\"bench.torrent\""),
            "{body}"
        );
        assert!(body.contains("d4:infoe"));
        let (content_type, body) = &received.creations[1];
        assert_eq!(content_type, "application/json");
        assert_eq!(
            serde_json::from_slice::<Value>(body)?,
            json!({ "magnet": "magnet:?xt=urn:btih:x", "storage": "memory" })
        );
    }

    assert!(
        go.has_metadata(&magnet).await?,
        "creation waited for metadata"
    );
    assert_eq!(
        go.content_url(&uploaded, "episode 2.mkv").await?,
        format!(
            "{base}/api/v1/stream/{}?path=bench-episodes%2Fepisode+2.mkv",
            "ab".repeat(20)
        )
    );
    assert!(go.content_url(&uploaded, "episode-3.mkv").await.is_err());
    assert_eq!(go.downloaded_bytes(&uploaded).await?, 123_456);
    assert_eq!(
        go.cache().await?,
        CacheSample {
            usage_bytes: 4096,
            eviction_count: None,
        }
    );
    Ok(())
}

#[tokio::test]
async fn a_capacity_go_torrplay_did_not_take_is_an_error() {
    let (base, received) = fake().await;
    received.lock().unwrap().reported_max_memory = Some(64 * 1024 * 1024);
    let go = GoTorrPlay::new(reqwest::Client::new(), base);
    let error = go.configure(1024 * 1024 * 1024).await.unwrap_err();
    assert!(error.to_string().contains("rather than"), "{error:#}");
}
