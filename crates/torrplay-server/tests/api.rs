// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! The API's behavior over the fake engine, with every exchange checked
//! against the contract. Each scenario starts its own server; the coverage
//! test runs them all in one process and requires every documented
//! response to have been produced.

// Scenarios and their helpers assert as tests do; clippy's test exemption
// covers only test functions.
#![allow(clippy::expect_used, clippy::unwrap_used)]

// Each test binary uses only part of the shared harness.
#[allow(dead_code)]
mod common;

use std::collections::HashSet;
use std::time::Duration;

use common::{
    BOUNDARY, Exchange, LogCapture, TestServer, film, film_indexes, form, form_request,
    json_request, request,
};
use http::{Method, StatusCode, header};
use serde_json::{Value, json};
use torrplay_core::engine::{
    CacheStatistics, ConnectionOutcomes, EngineStatistics, PeerConnections, Ratio,
    TorrentStatistics,
};
use torrplay_core::id::FileId;
use torrplay_core::lifecycle::{Readiness, StorageMode};
use torrplay_core::preload::PreloadState;
use torrplay_core::settings::{Explicit, LogLevel, LogOverride, Overrides, Settings};
use torrplay_fixtures::{FixtureSpec, catalog, generator, hex};
use torrplay_store::StoredSettings;

fn magnet(spec: &FixtureSpec) -> String {
    spec.magnet_uri(&spec.metainfo().info_hash)
}

/// A magnet URI with an info hash no fixture has.
fn unknown_magnet(n: u8) -> String {
    format!("magnet:?xt=urn:btih:{}", hex(&[n; 20]))
}

fn empty(method: Method, path: &str) -> http::Request<Vec<u8>> {
    request(method, path).body(Vec::new()).unwrap()
}

fn with_header(
    method: Method,
    path: &str,
    name: header::HeaderName,
    value: &str,
) -> http::Request<Vec<u8>> {
    request(method, path)
        .header(name, value)
        .body(Vec::new())
        .unwrap()
}

fn patch(
    path: &str,
    if_match: Option<&str>,
    body: &[u8],
    media_type: &str,
) -> http::Request<Vec<u8>> {
    let mut builder = request(Method::PATCH, path).header(header::CONTENT_TYPE, media_type);
    if let Some(tag) = if_match {
        builder = builder.header(header::IF_MATCH, tag);
    }
    builder.body(body.to_vec()).unwrap()
}

fn merge_patch(path: &str, if_match: &str, body: &Value) -> http::Request<Vec<u8>> {
    patch(
        path,
        Some(if_match),
        &serde_json::to_vec(body).unwrap(),
        "application/merge-patch+json",
    )
}

/// Polls torrent `path` until `done` holds for its representation.
async fn torrent_until(
    server: &TestServer,
    path: &str,
    what: &str,
    done: impl Fn(&Value) -> bool,
) -> Value {
    for _ in 0..500 {
        let torrent = server.get(path).await.json();
        if done(&torrent) {
            return torrent;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("the torrent never showed {what}");
}

fn expected_bytes(spec: &FixtureSpec, index: usize, start: u64, end: u64) -> Vec<u8> {
    let mut bytes = vec![0; usize::try_from(end - start).unwrap()];
    generator::fill(spec.file_seed(index), start, &mut bytes);
    bytes
}

fn assert_problem(exchange: &Exchange, status: StatusCode, problem: &str) {
    assert_eq!(
        exchange.status,
        status,
        "{}",
        String::from_utf8_lossy(&exchange.body)
    );
    assert_eq!(exchange.problem(), problem);
}

fn errors(exchange: &Exchange, key: &str) -> Vec<String> {
    exchange.json()["errors"]
        .as_array()
        .map(|errors| {
            errors
                .iter()
                .map(|error| error[key].as_str().unwrap().to_owned())
                .collect()
        })
        .unwrap_or_default()
}

/// The torrent at `path` and its files, with metadata available.
async fn files_of(server: &TestServer, path: &str) -> Vec<Value> {
    let page = server.get(&format!("{path}/files?page_size=200")).await;
    assert_eq!(page.status, StatusCode::OK);
    page.json()["items"].as_array().unwrap().clone()
}

/// A poster upload of `bytes` as `media_type`.
fn put_poster(path: &str, media_type: &str, bytes: &[u8]) -> http::Request<Vec<u8>> {
    request(Method::PUT, path)
        .header(header::CONTENT_TYPE, media_type)
        .body(bytes.to_vec())
        .unwrap()
}

/// The start of a PNG image, which is all the server checks.
const PNG: &[u8] = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR\0\0\0\x01\0\0\0\x01";

/// The start of a JPEG image.
const JPEG: &[u8] = &[0xff, 0xd8, 0xff, 0xe0, 0, 0x10, b'J', b'F', b'I', b'F', 0];

/// The IDs of the torrents a listing at `path` returns, all pages followed.
async fn listed_ids(server: &TestServer, path: &str) -> Vec<String> {
    let mut ids = Vec::new();
    let mut next = Some(path.to_owned());
    while let Some(path) = next {
        let page = server.get(&path).await;
        assert_eq!(page.status, StatusCode::OK, "{path}");
        let page = page.json();
        ids.extend(
            page["items"]
                .as_array()
                .unwrap()
                .iter()
                .map(|torrent| torrent["id"].as_str().unwrap().to_owned()),
        );
        next = page["links"]["next"].as_str().map(str::to_owned);
    }
    ids
}

/// The IDs of every item a collection at `path` lists, all pages followed.
async fn listed_ids_of(server: &TestServer, path: &str) -> Vec<String> {
    listed_ids(server, path).await
}

/// A fixture whose video files name episodes, for an inferred series.
fn episodes() -> FixtureSpec {
    FixtureSpec {
        id: "episodes".into(),
        torrent_name: "Show".into(),
        seed: 40,
        piece_length: 16 * 1024,
        layout: torrplay_fixtures::Layout::MultiFile {
            files: ["Show.S01E01.mkv", "Show.S01E02.mkv", "notes.txt"]
                .into_iter()
                .map(|name| torrplay_fixtures::FileSpec {
                    path: vec![name.into()],
                    size_bytes: 20_000,
                })
                .collect(),
        },
    }
}

/// Creates a torrent from an unknown magnet URI with library `members`, and
/// returns its ID.
async fn create_with(server: &TestServer, n: u8, members: Value) -> String {
    let mut body = members;
    body["magnet_uri"] = json!(unknown_magnet(n));
    let created = server
        .send(json_request(Method::POST, "/api/v1/torrents", &body))
        .await;
    assert_eq!(created.status, StatusCode::CREATED);
    created.json()["id"].as_str().unwrap().to_owned()
}

/// The path of the first profile, the default one on a new data directory.
/// The path of the first listed profile of `kind`.
async fn profile_of_kind(server: &TestServer, kind: &str) -> String {
    let listed = server.get("/api/v1/profiles").await.json();
    listed["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["kind"] == kind)
        .unwrap_or_else(|| panic!("a {kind} profile is listed"))["links"]["self"]
        .as_str()
        .unwrap()
        .to_owned()
}

async fn default_profile(server: &TestServer) -> String {
    profile_of_kind(server, "person").await
}

async fn device_profile(server: &TestServer) -> String {
    profile_of_kind(server, "device").await
}

/// A request to save progress at `path`.
fn put_progress(path: &str, position: f64, duration: f64, viewed: bool) -> http::Request<Vec<u8>> {
    json_request(
        Method::PUT,
        path,
        &json!({ "position_seconds": position, "duration_seconds": duration, "viewed": viewed }),
    )
}

/// A single file large enough that a stream of it outlasts the socket
/// buffers of a client that stops reading.
fn large(id: &str, seed: u64) -> FixtureSpec {
    FixtureSpec {
        id: id.into(),
        torrent_name: format!("{id}.mkv"),
        seed,
        piece_length: 1 << 20,
        layout: torrplay_fixtures::Layout::SingleFile {
            size_bytes: 48 << 20,
        },
    }
}

/// Creates a temporary torrent from `spec`'s magnet URI.
async fn create_temporary(server: &TestServer, spec: &FixtureSpec) -> Exchange {
    server
        .send(json_request(
            Method::POST,
            "/api/v1/torrents",
            &json!({ "magnet_uri": magnet(spec), "retention": "temporary" }),
        ))
        .await
}

/// A series whose episodes come out of order in its metadata.
fn series() -> FixtureSpec {
    FixtureSpec {
        id: "series".into(),
        torrent_name: "Series".into(),
        seed: 41,
        piece_length: 16 * 1024,
        layout: torrplay_fixtures::Layout::MultiFile {
            files: [
                "Series.S01E02.mkv",
                "Series.S01E01.mkv",
                "Series.S01E03.mkv",
                "notes.txt",
            ]
            .into_iter()
            .map(|name| torrplay_fixtures::FileSpec {
                path: vec![name.into()],
                size_bytes: 20_000,
            })
            .collect(),
        },
    }
}

/// The IDs of a torrent's files by name.
async fn file_ids(server: &TestServer, torrent: &str) -> std::collections::HashMap<String, String> {
    files_of(server, torrent)
        .await
        .iter()
        .map(|file| {
            (
                file["path"]
                    .as_array()
                    .unwrap()
                    .last()
                    .unwrap()
                    .as_str()
                    .unwrap()
                    .to_owned(),
                file["id"].as_str().unwrap().to_owned(),
            )
        })
        .collect()
}

/// The password the authentication scenarios set.
const PASSWORD: &str = "correct horse battery staple";

/// A request builder that carries `secret` as a bearer token.
fn bearer(method: Method, path: &str, secret: &str) -> http::request::Builder {
    request(method, path).header(header::AUTHORIZATION, format!("Bearer {secret}"))
}

/// A request without a body that carries `secret` as a bearer token.
fn bearer_empty(method: Method, path: &str, secret: &str) -> http::Request<Vec<u8>> {
    bearer(method, path, secret).body(Vec::new()).unwrap()
}

/// A JSON request that carries `secret` as a bearer token.
fn bearer_json(method: Method, path: &str, secret: &str, body: &Value) -> http::Request<Vec<u8>> {
    bearer(method, path, secret)
        .header(header::CONTENT_TYPE, "application/json")
        .body(serde_json::to_vec(body).unwrap())
        .unwrap()
}

/// Sets the first password.
async fn set_first_password(server: &TestServer) {
    let set = server
        .send(json_request(
            Method::PUT,
            "/api/v1/password",
            &json!({ "password": PASSWORD }),
        ))
        .await;
    assert_eq!(set.status, StatusCode::CREATED, "{set:?}");
    assert_eq!(set.header(header::LOCATION), Some("/api/v1/password"));
}

/// Signs in with `scope`, returning the new token's ID and secret.
async fn sign_in(server: &TestServer, name: &str, scope: &str) -> (String, String) {
    let created = server
        .send(json_request(
            Method::POST,
            "/api/v1/access-tokens",
            &json!({ "password": PASSWORD, "name": name, "scope": scope }),
        ))
        .await;
    assert_eq!(created.status, StatusCode::CREATED, "{created:?}");
    assert_eq!(created.header(header::CACHE_CONTROL), Some("no-store"));
    let body = created.json();
    (
        body["id"].as_str().unwrap().to_owned(),
        body["secret"].as_str().unwrap().to_owned(),
    )
}

/// Issues a playback token with `secret` for `torrent`, or for every
/// torrent.
async fn playback_token(server: &TestServer, secret: &str, torrent: Option<&str>) -> String {
    let issued = server
        .send(bearer_json(
            Method::POST,
            "/api/v1/playback-tokens",
            secret,
            &json!({ "torrent_id": torrent }),
        ))
        .await;
    assert_eq!(issued.status, StatusCode::OK, "{issued:?}");
    assert_eq!(issued.header(header::CACHE_CONTROL), Some("no-store"));
    assert_eq!(issued.json()["torrent_id"].as_str(), torrent);
    issued.json()["token"].as_str().unwrap().to_owned()
}

/// The content path of a torrent's first file, uploaded from `spec`.
async fn uploaded_content(server: &TestServer, spec: &FixtureSpec) -> (String, String) {
    let torrent = server.upload(&spec.metainfo().bytes, None).await.json();
    let path = torrent["links"]["self"].as_str().unwrap().to_owned();
    let file = files_of(server, &path).await[0]["links"]["self"]
        .as_str()
        .unwrap()
        .to_owned();
    (
        torrent["id"].as_str().unwrap().to_owned(),
        format!("{file}/content"),
    )
}

/// Asserts that `text` follows the Prometheus text exposition format: each
/// family's `HELP` and `TYPE` come once, before its samples, and each sample
/// is a name, optional labels, and a number.
fn assert_exposition(text: &str) {
    let mut typed = HashSet::new();
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("# TYPE ") {
            let (name, kind) = rest.split_once(' ').unwrap();
            assert!(["counter", "gauge", "histogram"].contains(&kind), "{line}");
            assert!(typed.insert(name.to_owned()), "{name} typed twice");
            continue;
        }
        if line.starts_with("# HELP ") {
            continue;
        }
        let (series, value) = line.rsplit_once(' ').unwrap();
        assert!(value.parse::<f64>().is_ok(), "{line}");
        let name = series.split('{').next().unwrap();
        assert!(
            name.bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_'),
            "{line}"
        );
        let family = ["_bucket", "_sum", "_count"]
            .iter()
            .find_map(|suffix| {
                name.strip_suffix(suffix)
                    .filter(|base| typed.contains(*base))
            })
            .unwrap_or(name);
        assert!(typed.contains(family), "{name} has no TYPE before it");
    }
}

/// Asserts a `401` whose challenge names `error`, if any.
fn assert_unauthenticated(exchange: &Exchange, error: Option<&str>) {
    assert_problem(
        exchange,
        StatusCode::UNAUTHORIZED,
        "authentication-required",
    );
    let challenge = exchange.header(header::WWW_AUTHENTICATE).unwrap();
    assert!(
        challenge.starts_with(r#"Bearer realm="TorrPlay""#),
        "{challenge}"
    );
    match error {
        Some(error) => assert!(
            challenge.ends_with(&format!(r#"error="{error}""#)),
            "{challenge}"
        ),
        None => assert!(!challenge.contains("error="), "{challenge}"),
    }
}

mod scenario {
    use super::*;

    /// API-01, API-02, API-19.
    pub async fn service_endpoints() {
        let server = TestServer::start().await;
        let entry = server.get("/api/v1").await;
        assert_eq!(entry.status, StatusCode::OK);
        assert_eq!(
            entry.json()["links"]["service-desc"],
            "/api/v1/openapi.yaml"
        );
        assert_eq!(entry.json()["links"]["service-doc"], "/api/v1/reference");
        let redirect = server.send_raw(empty(Method::GET, "/api/v1/")).await;
        assert_eq!(redirect.status, StatusCode::PERMANENT_REDIRECT);
        assert_eq!(redirect.header(header::LOCATION), Some("/api/v1"));
        let document = server.get("/api/v1/openapi.yaml").await;
        assert_eq!(
            document.header(header::CONTENT_TYPE),
            Some("application/yaml")
        );
        assert_eq!(document.body, torrplay_contract::DOCUMENT.as_bytes());
        for path in ["/livez", "/readyz"] {
            let health = server.get(path).await;
            assert_eq!(health.status, StatusCode::OK);
            assert_eq!(health.json(), json!({ "status": "ok" }));
        }
        let unknown = server.send_raw(empty(Method::GET, "/api/v2")).await;
        assert_eq!(unknown.status, StatusCode::NOT_FOUND);
        server.stop().await;
    }

    /// API-22.
    pub async fn api_reference() {
        let server = TestServer::start().await;
        let page = server.get("/api/v1/reference").await;
        assert_eq!(page.status, StatusCode::OK);
        assert_eq!(
            page.header(header::CONTENT_TYPE),
            Some("text/html; charset=utf-8")
        );
        let policy = page.header(header::CONTENT_SECURITY_POLICY).unwrap();
        for directive in [
            "default-src 'none'",
            "script-src 'self'",
            "connect-src 'self'",
        ] {
            assert!(policy.contains(directive), "{directive}");
        }
        let html = String::from_utf8(page.body).unwrap();
        assert!(html.contains(r#"src="/api/v1/reference/scalar.js""#));

        let script = "/api/v1/reference/scalar.js";
        let compressed = server.get(script).await;
        assert_eq!(compressed.status, StatusCode::OK);
        assert_eq!(
            compressed.header(header::CONTENT_TYPE),
            Some("text/javascript; charset=utf-8")
        );
        assert_eq!(compressed.header(header::CONTENT_ENCODING), Some("gzip"));
        assert_eq!(compressed.header(header::VARY), Some("Accept-Encoding"));
        let mut decompressed = Vec::new();
        std::io::Read::read_to_end(
            &mut flate2::read::GzDecoder::new(compressed.body.as_slice()),
            &mut decompressed,
        )
        .expect("a gzip body");

        let identity = server
            .send(with_header(
                Method::GET,
                script,
                header::ACCEPT_ENCODING,
                "identity",
            ))
            .await;
        assert_eq!(identity.status, StatusCode::OK);
        assert_eq!(identity.header(header::CONTENT_ENCODING), None);
        assert_eq!(identity.body, decompressed, "one bundle in both codings");
        let gzip_tag = compressed.header(header::ETAG).unwrap();
        let identity_tag = identity.header(header::ETAG).unwrap();
        assert_ne!(gzip_tag, identity_tag, "codings have their own tags");

        let mut revalidation = with_header(Method::GET, script, header::IF_NONE_MATCH, gzip_tag);
        revalidation
            .headers_mut()
            .insert(header::ACCEPT_ENCODING, "gzip".parse().unwrap());
        let unchanged = server.send(revalidation).await;
        assert_eq!(unchanged.status, StatusCode::NOT_MODIFIED);
        assert_eq!(unchanged.header(header::ETAG), Some(gzip_tag));
        assert!(unchanged.body.is_empty());
        let other_coding = server
            .send(with_header(
                Method::GET,
                script,
                header::IF_NONE_MATCH,
                identity_tag,
            ))
            .await;
        assert_eq!(
            other_coding.status,
            StatusCode::OK,
            "the identity tag does not match the gzip representation"
        );
        server.stop().await;
    }

    /// API-04, LIF-01: magnet torrents retrieve metadata in the background.
    pub async fn magnet_creation() {
        let server = TestServer::start().await;
        let spec = catalog::single_small();
        let created = server.create_magnet(&magnet(&spec)).await;
        let id = server.id_of(&created);
        let body = created.json();
        let path = body["links"]["self"].as_str().unwrap().to_owned();
        assert_eq!(created.header(header::LOCATION), Some(path.as_str()));
        assert_eq!(body["observed_state"], "retrieving_metadata");
        assert_eq!(body["name"], Value::Null);
        assert_eq!(
            body["info_hashes"][0]["value"],
            hex(&spec.metainfo().info_hash)
        );

        let files = server.get(&format!("{path}/files")).await;
        assert_problem(&files, StatusCode::CONFLICT, "lifecycle-conflict");
        let file = format!("{path}/files/fil_aaaaaaaaaaaaaaaaaaaaaaaaaa");
        assert_problem(
            &server.get(&file).await,
            StatusCode::CONFLICT,
            "lifecycle-conflict",
        );
        let content = format!("{file}/content");
        assert_problem(
            &server.get(&content).await,
            StatusCode::CONFLICT,
            "lifecycle-conflict",
        );
        let head = server.send(empty(Method::HEAD, &content)).await;
        assert_eq!(head.status, StatusCode::CONFLICT);
        assert!(head.body.is_empty());

        server.engine.set_metadata_retrievable(
            torrplay_core::id::InfoHashV1::from_bytes(spec.metainfo().info_hash),
            true,
        );
        let torrent = server.get(&path).await.json();
        assert_eq!(torrent["observed_state"], "downloading");
        assert_eq!(torrent["name"], "single-small.bin");
        assert_eq!(torrent["file_count"], 1);
        assert_eq!(files_of(&server, &path).await.len(), 1);
        assert!(server.engine.desired_state(&id).is_some());
        server.stop().await;
    }

    /// API-04: uploaded metadata is used at once.
    pub async fn upload_creation() {
        let server = TestServer::start().await;
        let spec = catalog::single_small();
        let created = server.upload(&spec.metainfo().bytes, None).await;
        assert_eq!(created.status, StatusCode::CREATED);
        let body = created.json();
        assert_eq!(body["observed_state"], "downloading");
        assert_eq!(body["size_bytes"], 1_048_576);
        assert_eq!(
            created.header(header::LOCATION),
            body["links"]["self"].as_str()
        );
        let paused = server
            .upload(&catalog::multi_boundary().metainfo().bytes, Some("paused"))
            .await;
        assert_eq!(paused.json()["observed_state"], "paused");
        let magnet_paused = server
            .send(json_request(
                Method::POST,
                "/api/v1/torrents",
                &json!({ "magnet_uri": unknown_magnet(1), "desired_state": "paused" }),
            ))
            .await;
        assert_eq!(magnet_paused.json()["observed_state"], "paused");
        server.stop().await;
    }

    /// DSK-01 and DSK-05 at creation: a torrent created in disk storage
    /// reports it, in the torrent and its configuration, and keeps it across
    /// a restart; too little free space is `507`, and nothing is created.
    pub async fn disk_creation() {
        let server = TestServer::start().await;
        let spec = catalog::single_small();
        let created = server
            .send(form(&[
                (
                    "torrent_file",
                    Some("application/x-bittorrent"),
                    spec.metainfo().bytes,
                ),
                ("storage_mode", None, b"disk".to_vec()),
            ]))
            .await;
        assert_eq!(created.status, StatusCode::CREATED);
        assert_eq!(created.json()["storage_mode"], "disk");
        assert_eq!(created.json()["storage_transition"], Value::Null);
        let id = server.id_of(&created);
        assert_eq!(server.engine.storage_mode(&id), Some(StorageMode::Disk));
        let path = created.json()["links"]["configuration"]
            .as_str()
            .unwrap()
            .to_owned();
        assert_eq!(server.get(&path).await.json()["storage_mode"], "disk");

        let magnet = server
            .send(json_request(
                Method::POST,
                "/api/v1/torrents",
                &json!({ "magnet_uri": unknown_magnet(1), "storage_mode": "disk" }),
            ))
            .await;
        assert_eq!(magnet.json()["storage_mode"], "disk");
        let invalid = server
            .send_unchecked(json_request(
                Method::POST,
                "/api/v1/torrents",
                &json!({ "magnet_uri": unknown_magnet(2), "storage_mode": "tape" }),
            ))
            .await;
        assert_eq!(errors(&invalid, "pointer"), ["/storage_mode"]);

        server.engine.set_disk_free_bytes(1);
        let full = server
            .send(form(&[
                (
                    "torrent_file",
                    Some("application/x-bittorrent"),
                    catalog::multi_boundary().metainfo().bytes,
                ),
                ("storage_mode", None, b"disk".to_vec()),
            ]))
            .await;
        assert_problem(
            &full,
            StatusCode::INSUFFICIENT_STORAGE,
            "insufficient-storage",
        );
        assert_eq!(listed_len(&server).await, 2, "nothing was created");

        let server = server.restart().await;
        assert_eq!(server.get(&path).await.json()["storage_mode"], "disk");
        assert_eq!(
            server.engine.storage_mode(&id),
            Some(StorageMode::Disk),
            "restored in disk storage"
        );
        server.stop().await;
    }

    /// DSK-02, DSK-03, and DSK-05 through the API: a change of storage mode
    /// is answered at once and completes in the background; one in
    /// progress shows in the torrent and refuses another; one that fails
    /// reverts the configuration with a new ETag and reports
    /// `storage_failure`; too little free space is `507`.
    pub async fn storage_transitions() {
        let server = TestServer::start().await;
        let spec = catalog::single_small();
        let created = server.upload(&spec.metainfo().bytes, None).await;
        let id = server.id_of(&created);
        let path = created.json()["links"]["self"].as_str().unwrap().to_owned();
        let configuration = format!("{path}/configuration");
        let tag = |exchange: &Exchange| exchange.header(header::ETAG).unwrap().to_owned();
        let current = tag(&server.get(&configuration).await);

        let disk = server
            .send(merge_patch(
                &configuration,
                &current,
                &json!({ "storage_mode": "disk" }),
            ))
            .await;
        assert_eq!(disk.status, StatusCode::OK);
        assert_eq!(disk.json()["storage_mode"], "disk");
        let torrent = torrent_until(&server, &path, "disk storage", |torrent| {
            torrent["storage_mode"] == "disk"
        })
        .await;
        assert_eq!(torrent["storage_transition"], Value::Null);
        assert_eq!(server.engine.storage_mode(&id), Some(StorageMode::Disk));

        server.engine.fail_transitions(Some("The disk filled up."));
        let failing = server
            .send(merge_patch(
                &configuration,
                &tag(&disk),
                &json!({ "storage_mode": "memory" }),
            ))
            .await;
        assert_eq!(failing.json()["storage_mode"], "memory");
        let torrent = torrent_until(&server, &path, "a storage failure", |torrent| {
            !torrent["storage_failure"].is_null()
        })
        .await;
        assert_eq!(
            torrent["storage_failure"],
            json!({ "target_storage_mode": "memory", "detail": "The disk filled up." })
        );
        assert_eq!(torrent["storage_mode"], "disk");
        let reverted = server.get(&configuration).await;
        assert_eq!(
            reverted.json()["storage_mode"],
            "disk",
            "the configuration reverts"
        );
        assert_ne!(tag(&reverted), tag(&failing), "with a new ETag");
        server.engine.fail_transitions(None);

        let gate = server.engine.hold_transitions();
        let held = server
            .send(merge_patch(
                &configuration,
                &tag(&reverted),
                &json!({ "storage_mode": "memory" }),
            ))
            .await;
        assert_eq!(held.status, StatusCode::OK);
        let torrent = server.get(&path).await.json();
        assert_eq!(
            torrent["storage_transition"],
            json!({ "target_storage_mode": "memory" })
        );
        assert_eq!(
            torrent["storage_mode"], "disk",
            "the mode in effect until it completes"
        );
        assert_eq!(
            torrent["storage_failure"],
            Value::Null,
            "a new change clears the failure"
        );
        assert_problem(
            &server
                .send(merge_patch(
                    &configuration,
                    &tag(&held),
                    &json!({ "storage_mode": "disk" }),
                ))
                .await,
            StatusCode::CONFLICT,
            "lifecycle-conflict",
        );
        gate.notify_one();
        torrent_until(&server, &path, "memory storage", |torrent| {
            torrent["storage_mode"] == "memory" && torrent["storage_transition"].is_null()
        })
        .await;

        server.engine.set_disk_free_bytes(1);
        let full = server
            .send(merge_patch(
                &configuration,
                &tag(&held),
                &json!({ "storage_mode": "disk" }),
            ))
            .await;
        assert_problem(
            &full,
            StatusCode::INSUFFICIENT_STORAGE,
            "insufficient-storage",
        );
        assert_eq!(
            server.get(&configuration).await.json()["storage_mode"],
            "memory"
        );
        server.stop().await;
    }

    /// API-05, and a duplicate upload supplying metadata to a magnet
    /// torrent.
    pub async fn duplicates() {
        let server = TestServer::start().await;
        let spec = catalog::single_small();
        let created = server.create_magnet(&magnet(&spec)).await;
        let path = created.json()["links"]["self"].as_str().unwrap().to_owned();
        let again = server.create_magnet(&magnet(&spec)).await;
        assert_problem(&again, StatusCode::CONFLICT, "duplicate-torrent");
        assert_eq!(again.json()["torrent"], path.as_str());
        let upload = server.upload(&spec.metainfo().bytes, None).await;
        assert_problem(&upload, StatusCode::CONFLICT, "duplicate-torrent");
        assert_eq!(upload.json()["torrent"], path.as_str());
        let torrent = server.get(&path).await.json();
        assert_eq!(
            torrent["name"], "single-small.bin",
            "the upload's metadata is used"
        );
        assert_eq!(torrent["observed_state"], "downloading");

        // A torrent created from an upload is a duplicate by either input,
        // and duplicates leave it unchanged.
        let other = catalog::multi_boundary();
        let uploaded = server.upload(&other.metainfo().bytes, Some("paused")).await;
        let uploaded_path = uploaded.json()["links"]["self"]
            .as_str()
            .unwrap()
            .to_owned();
        for duplicate in [
            server.upload(&other.metainfo().bytes, None).await,
            server.create_magnet(&magnet(&other)).await,
        ] {
            assert_problem(&duplicate, StatusCode::CONFLICT, "duplicate-torrent");
            assert_eq!(duplicate.json()["torrent"], uploaded_path.as_str());
        }
        let unchanged = server.get(&uploaded_path).await.json();
        assert_eq!(unchanged["observed_state"], "paused");
        let listed = server.get("/api/v1/torrents").await.json();
        assert_eq!(listed["items"].as_array().unwrap().len(), 2);
        server.stop().await;
    }

    /// API-06.
    pub async fn rejected_sources() {
        let server = TestServer::start().await;
        let invalid = server.upload(&catalog::invalid_metainfo(), None).await;
        assert_problem(
            &invalid,
            StatusCode::UNPROCESSABLE_ENTITY,
            "validation-failed",
        );
        assert_eq!(errors(&invalid, "pointer"), ["/torrent_file"]);
        let magnet = server.create_magnet("magnet:?xt=urn:btih:zz").await;
        assert_problem(
            &magnet,
            StatusCode::UNPROCESSABLE_ENTITY,
            "validation-failed",
        );
        assert_eq!(errors(&magnet, "pointer"), ["/magnet_uri"]);
        let v2 = server
            .upload(&catalog::v2_only().v2_only_metainfo(), None)
            .await;
        assert_problem(&v2, StatusCode::UNPROCESSABLE_ENTITY, "unsupported-torrent");
        assert!(
            server.get("/api/v1/torrents").await.json()["items"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        server.stop().await;
    }

    /// API-07, API-08, and malformed and unsupported bodies.
    pub async fn invalid_creation_bodies() {
        let server = TestServer::start().await;
        let large_json = json!({ "magnet_uri": format!("magnet:?{}", "a".repeat(16_400)) });
        let too_large = server
            .send_unchecked(json_request(Method::POST, "/api/v1/torrents", &large_json))
            .await;
        assert_problem(
            &too_large,
            StatusCode::PAYLOAD_TOO_LARGE,
            "content-too-large",
        );
        let large_file = vec![b'd'; 10 * 1024 * 1024 + 1];
        let too_large = server.send_unchecked(form_request(&large_file, None)).await;
        assert_problem(
            &too_large,
            StatusCode::PAYLOAD_TOO_LARGE,
            "content-too-large",
        );

        let members = server
            .send_unchecked(json_request(
                Method::POST,
                "/api/v1/torrents",
                &json!({ "magnet_uri": unknown_magnet(2), "id": "trr_x", "extra": 1 }),
            ))
            .await;
        assert_problem(
            &members,
            StatusCode::UNPROCESSABLE_ENTITY,
            "validation-failed",
        );
        assert_eq!(errors(&members, "pointer"), ["/extra", "/id"]);
        let form_member = server
            .send_unchecked(form(&[
                (
                    "torrent_file",
                    Some("application/x-bittorrent"),
                    catalog::single_small().metainfo().bytes,
                ),
                ("name", None, b"x".to_vec()),
            ]))
            .await;
        assert_eq!(errors(&form_member, "pointer"), ["/name"]);

        let not_json = request(Method::POST, "/api/v1/torrents")
            .header(header::CONTENT_TYPE, "application/json")
            .body(b"{".to_vec())
            .unwrap();
        let malformed = server.send_unchecked(not_json).await;
        assert_problem(&malformed, StatusCode::BAD_REQUEST, "malformed-request");
        let not_a_form = request(Method::POST, "/api/v1/torrents")
            .header(
                header::CONTENT_TYPE,
                format!("multipart/form-data; boundary={BOUNDARY}"),
            )
            .body(b"nothing".to_vec())
            .unwrap();
        assert_problem(
            &server.send_unchecked(not_a_form).await,
            StatusCode::BAD_REQUEST,
            "malformed-request",
        );
        let text = request(Method::POST, "/api/v1/torrents")
            .header(header::CONTENT_TYPE, "text/plain")
            .body(b"magnet".to_vec())
            .unwrap();
        let unsupported = server.send_unchecked(text).await;
        assert_problem(
            &unsupported,
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "unsupported-media-type",
        );
        assert_eq!(
            unsupported.header(header::ACCEPT),
            Some("application/json, multipart/form-data")
        );
        server.stop().await;
    }

    /// API-09 and API-10.
    pub async fn torrent_pages() {
        let server = TestServer::start().await;
        for n in 0..7 {
            server.create_magnet(&unknown_magnet(10 + n)).await;
        }
        for query in [
            "page_size=0",
            "page_size=201",
            "page_size=x",
            "sort=name",
            "cursor=AAAA",
            "observed_state=done",
        ] {
            let exchange = server
                .send_unchecked(empty(Method::GET, &format!("/api/v1/torrents?{query}")))
                .await;
            assert_problem(&exchange, StatusCode::BAD_REQUEST, "malformed-request");
            let parameter = query.split('=').next().unwrap();
            assert_eq!(errors(&exchange, "parameter"), [parameter], "{query}");
        }

        let first = server.get("/api/v1/torrents?page_size=3").await.json();
        let next = first["links"]["next"].as_str().unwrap().to_owned();
        let cursor = next.split("cursor=").nth(1).unwrap().to_owned();
        let filtered = server
            .send_unchecked(empty(
                Method::GET,
                &format!("/api/v1/torrents?observed_state=paused&cursor={cursor}"),
            ))
            .await;
        assert_problem(&filtered, StatusCode::BAD_REQUEST, "malformed-request");
        assert_eq!(errors(&filtered, "parameter"), ["cursor"]);

        // Pages stay consistent while torrents are created and deleted.
        let mut seen: Vec<String> = Vec::new();
        let mut page = first;
        let mut deleted = HashSet::new();
        let mut round = 0;
        loop {
            for item in page["items"].as_array().unwrap() {
                seen.push(item["id"].as_str().unwrap().to_owned());
            }
            server.create_magnet(&unknown_magnet(100 + round)).await;
            if round == 0 {
                // Deletes a torrent not yet listed.
                let all = server.get("/api/v1/torrents?page_size=200").await.json();
                let last = all["items"].as_array().unwrap().last().unwrap().clone();
                let path = last["links"]["self"].as_str().unwrap();
                let removed = server.send(empty(Method::DELETE, path)).await;
                assert_eq!(removed.status, StatusCode::NO_CONTENT);
                deleted.insert(last["id"].as_str().unwrap().to_owned());
            }
            round += 1;
            let Some(next) = page["links"]["next"].as_str() else {
                break;
            };
            page = server.get(next).await.json();
        }
        let unique: HashSet<_> = seen.iter().collect();
        assert_eq!(unique.len(), seen.len(), "no torrent repeats");
        assert_eq!(seen.len(), 6, "the seven originals but the deleted one");
        assert!(seen.iter().all(|id| !deleted.contains(id)));

        let paused = server
            .get("/api/v1/torrents?observed_state=paused")
            .await
            .json();
        assert!(paused["items"].as_array().unwrap().is_empty());
        assert_eq!(
            paused["links"]["self"],
            "/api/v1/torrents?observed_state=paused"
        );
        server.stop().await;
    }

    /// API-11, API-12, API-13, LIF-05.
    pub async fn configuration_changes() {
        let server = TestServer::start().await;
        let created = server
            .upload(&catalog::single_small().metainfo().bytes, None)
            .await;
        let id = server.id_of(&created);
        let path = created.json()["links"]["self"].as_str().unwrap().to_owned();
        let configuration_path = format!("{path}/configuration");

        let torrent = server.get(&path).await;
        let state_tag = torrent.header(header::ETAG).unwrap().to_owned();
        let configuration = server.get(&configuration_path).await;
        let tag = configuration.header(header::ETAG).unwrap().to_owned();
        assert_eq!(
            configuration.header(header::CACHE_CONTROL),
            Some("no-cache")
        );
        for (resource, etag) in [(&path, &state_tag), (&configuration_path, &tag)] {
            let cached = server
                .send(with_header(
                    Method::GET,
                    resource,
                    header::IF_NONE_MATCH,
                    etag,
                ))
                .await;
            assert_eq!(cached.status, StatusCode::NOT_MODIFIED);
            assert_eq!(cached.header(header::ETAG), Some(etag.as_str()));
        }

        server
            .engine
            .inject_failure(&id, "A piece could not be written to storage.")
            .unwrap();
        let failed = server
            .send(with_header(
                Method::GET,
                &path,
                header::IF_NONE_MATCH,
                &state_tag,
            ))
            .await;
        assert_eq!(failed.status, StatusCode::OK, "the observed state changed");
        assert_eq!(failed.json()["observed_state"], "failed");
        let unchanged = server
            .send(with_header(
                Method::GET,
                &configuration_path,
                header::IF_NONE_MATCH,
                &tag,
            ))
            .await;
        assert_eq!(unchanged.status, StatusCode::NOT_MODIFIED);

        let pause = json!({ "desired_state": "paused" });
        let body = serde_json::to_vec(&pause).unwrap();
        let missing = server
            .send_unchecked(patch(
                &configuration_path,
                None,
                &body,
                "application/merge-patch+json",
            ))
            .await;
        assert_problem(
            &missing,
            StatusCode::PRECONDITION_REQUIRED,
            "precondition-required",
        );
        let stale = server
            .send(merge_patch(&configuration_path, "\"cfg-0\"", &pause))
            .await;
        assert_problem(
            &stale,
            StatusCode::PRECONDITION_FAILED,
            "precondition-failed",
        );
        let paused = server
            .send(merge_patch(&configuration_path, &tag, &pause))
            .await;
        assert_eq!(paused.status, StatusCode::OK);
        assert_eq!(paused.json()["desired_state"], "paused");
        let paused_tag = paused.header(header::ETAG).unwrap().to_owned();
        assert_ne!(paused_tag, tag);
        let again = server
            .send(merge_patch(&configuration_path, &paused_tag, &pause))
            .await;
        assert_eq!(
            again.header(header::ETAG),
            Some(paused_tag.as_str()),
            "LIF-05"
        );

        let start = json!({ "desired_state": "started" });
        let started = server
            .send(merge_patch(&configuration_path, &paused_tag, &start))
            .await;
        assert_eq!(started.status, StatusCode::OK);
        let torrent = server.get(&path).await.json();
        assert_eq!(
            torrent["observed_state"], "downloading",
            "starting clears the failure"
        );
        assert_eq!(torrent["failure"], Value::Null);
        let current = started.header(header::ETAG).unwrap().to_owned();

        // API-08: every unknown or read-only member is named.
        let read_only = json!({
            "desired_state": "paused",
            "links": { "self": "/x" },
            "observed_state": "paused",
        });
        for (body, pointers) in [
            (json!(null), vec![""]),
            (json!({}), vec![""]),
            (read_only, vec!["/links", "/observed_state"]),
        ] {
            let invalid = server
                .send_unchecked(merge_patch(&configuration_path, &current, &body))
                .await;
            assert_problem(
                &invalid,
                StatusCode::UNPROCESSABLE_ENTITY,
                "validation-failed",
            );
            assert_eq!(errors(&invalid, "pointer"), pointers, "{body}");
        }
        let kept = server.get(&configuration_path).await;
        assert_eq!(
            kept.json()["desired_state"],
            "started",
            "invalid patches change nothing"
        );
        let as_json = server
            .send_unchecked(patch(
                &configuration_path,
                Some(&current),
                &body,
                "application/json",
            ))
            .await;
        assert_problem(
            &as_json,
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "unsupported-media-type",
        );
        assert_eq!(
            as_json.header("accept-patch"),
            Some("application/merge-patch+json")
        );
        let not_json = server
            .send_unchecked(patch(
                &configuration_path,
                Some(&current),
                b"{",
                "application/merge-patch+json",
            ))
            .await;
        assert_problem(&not_json, StatusCode::BAD_REQUEST, "malformed-request");
        let large = vec![b' '; 16 * 1024 + 1];
        let too_large = server
            .send_unchecked(patch(
                &configuration_path,
                Some(&current),
                &large,
                "application/merge-patch+json",
            ))
            .await;
        assert_problem(
            &too_large,
            StatusCode::PAYLOAD_TOO_LARGE,
            "content-too-large",
        );
        server.stop().await;
    }

    /// API-14 and API-15.
    pub async fn methods_and_media_types() {
        let server = TestServer::start().await;
        let put = server
            .send_unchecked(empty(Method::PUT, "/api/v1/torrents"))
            .await;
        assert_problem(&put, StatusCode::METHOD_NOT_ALLOWED, "method-not-allowed");
        let allow = put.header(header::ALLOW).unwrap();
        assert!(allow.contains("GET") && allow.contains("POST"), "{allow}");
        let text = server
            .send(with_header(
                Method::GET,
                "/api/v1",
                header::ACCEPT,
                "text/plain",
            ))
            .await;
        assert_problem(&text, StatusCode::NOT_ACCEPTABLE, "not-acceptable");
        let any = server
            .send(with_header(
                Method::GET,
                "/api/v1",
                header::ACCEPT,
                "text/plain, */*;q=0.1",
            ))
            .await;
        assert_eq!(any.status, StatusCode::OK);
        server.stop().await;
    }

    /// API-16, API-17.
    pub async fn file_pages() {
        let server = TestServer::start().await;
        let spec = catalog::many_files();
        let created = server.upload(&spec.metainfo().bytes, None).await;
        let path = created.json()["links"]["self"].as_str().unwrap().to_owned();
        let mut listed = Vec::new();
        let mut next = Some(format!("{path}/files?page_size=150"));
        while let Some(link) = next {
            let page = server.get(&link).await.json();
            listed.extend(page["items"].as_array().unwrap().iter().cloned());
            next = page["links"]["next"].as_str().map(str::to_owned);
        }
        let expected = spec.files();
        assert_eq!(listed.len(), expected.len());
        for (file, spec_file) in listed.iter().zip(&expected) {
            assert_eq!(file["path"][1], spec_file.path[0].as_str());
            assert_eq!(file["size_bytes"], spec_file.size_bytes);
            assert_eq!(file["media_type"], "application/octet-stream");
        }
        // File IDs derive from the paths, so they are stable across
        // listings and match the manifest.
        let again = server
            .get(&format!("{path}/files?page_size=150"))
            .await
            .json();
        assert_eq!(again["items"].as_array().unwrap(), &listed[..150]);
        for (file, spec_file) in listed.iter().zip(&expected) {
            let components = [spec.torrent_name.as_str(), spec_file.path[0].as_str()];
            assert_eq!(file["id"], FileId::derive(&components).as_str());
        }
        let link = listed[42]["links"]["self"].as_str().unwrap();
        assert_eq!(server.get(link).await.json(), listed[42]);
        let missing = server
            .get(&format!("{path}/files/fil_aaaaaaaaaaaaaaaaaaaaaaaaaa"))
            .await;
        assert_problem(&missing, StatusCode::NOT_FOUND, "not-found");
        let bad_size = server
            .send_unchecked(empty(Method::GET, &format!("{path}/files?page_size=0")))
            .await;
        assert_problem(&bad_size, StatusCode::BAD_REQUEST, "malformed-request");
        server.stop().await;
    }

    /// STR-01 to STR-04, header parts.
    pub async fn content_ranges() {
        let server = TestServer::start().await;
        let spec = catalog::multi_boundary();
        let created = server.upload(&spec.metainfo().bytes, None).await;
        let path = created.json()["links"]["self"].as_str().unwrap().to_owned();
        let files = files_of(&server, &path).await;
        for (index, file) in files.iter().enumerate() {
            let content = file["links"]["content"].as_str().unwrap();
            let size = spec.files()[index].size_bytes;
            let full = server.get(content).await;
            assert_eq!(full.status, StatusCode::OK);
            assert_eq!(
                full.body,
                expected_bytes(&spec, index, 0, size),
                "{content}"
            );
            assert_eq!(
                full.header(header::CONTENT_LENGTH),
                Some(size.to_string().as_str())
            );
            assert_eq!(full.header(header::ACCEPT_RANGES), Some("bytes"));
            let etag = full.header(header::ETAG).unwrap().to_owned();
            assert_eq!(
                etag,
                format!("\"sha1-{}-{index}\"", hex(&spec.metainfo().info_hash))
            );
        }

        let index = 2;
        let size = spec.files()[index].size_bytes;
        let content = files[index]["links"]["content"]
            .as_str()
            .unwrap()
            .to_owned();
        let range = |value: &str| with_header(Method::GET, &content, header::RANGE, value);
        // The file starts inside a piece, so its first bytes share a piece
        // with the files before it; `boundary` is the offset, within the
        // file, of the next piece boundary.
        let piece = u64::from(spec.piece_length);
        let file_start: u64 = spec.files()[..index]
            .iter()
            .map(|file| file.size_bytes)
            .sum();
        let boundary = file_start.div_ceil(piece) * piece - file_start;
        assert!(boundary > 100 && boundary + 100 < size);
        let across = format!("bytes={}-{}", boundary - 100, boundary + 99);
        let cases = [
            ("bytes=0-99", 0, 100),
            (across.as_str(), boundary - 100, boundary + 100),
            ("bytes=500000-", 500_000, size),
            ("bytes=-10", size - 10, size),
            ("bytes=0-0", 0, 1),
            (&format!("bytes={}-{}", size - 1, size - 1), size - 1, size),
        ];
        for (value, start, end) in cases {
            let partial = server.send(range(value)).await;
            assert_eq!(partial.status, StatusCode::PARTIAL_CONTENT, "{value}");
            assert_eq!(
                partial.body,
                expected_bytes(&spec, index, start, end),
                "{value}"
            );
            assert_eq!(
                partial.header(header::CONTENT_RANGE),
                Some(format!("bytes {start}-{}/{size}", end - 1).as_str())
            );
        }
        let past_end = server.send(range(&format!("bytes={size}-"))).await;
        assert_problem(
            &past_end,
            StatusCode::RANGE_NOT_SATISFIABLE,
            "range-not-satisfiable",
        );
        assert_eq!(
            past_end.header(header::CONTENT_RANGE),
            Some(format!("bytes */{size}").as_str())
        );
        let multiple = server.send(range("bytes=0-1,10-20")).await;
        assert_eq!(multiple.status, StatusCode::OK);
        assert_eq!(multiple.body.len() as u64, size);

        let etag = multiple.header(header::ETAG).unwrap().to_owned();
        let conditional = |if_range: &str| {
            request(Method::GET, &content)
                .header(header::RANGE, "bytes=0-9")
                .header(header::IF_RANGE, if_range)
                .body(Vec::new())
                .unwrap()
        };
        assert_eq!(
            server.send(conditional(&etag)).await.status,
            StatusCode::PARTIAL_CONTENT
        );
        let stale = server.send(conditional("\"sha1-0-0\"")).await;
        assert_eq!(stale.status, StatusCode::OK);
        assert_eq!(stale.body.len() as u64, size);
        let cached = server
            .send(with_header(
                Method::GET,
                &content,
                header::IF_NONE_MATCH,
                &etag,
            ))
            .await;
        assert_eq!(cached.status, StatusCode::NOT_MODIFIED);

        // HEAD answers as GET does, without a body.
        let head = server.send(empty(Method::HEAD, &content)).await;
        assert_eq!(head.status, StatusCode::OK);
        assert!(head.body.is_empty());
        assert_eq!(
            head.header(header::CONTENT_LENGTH),
            Some(size.to_string().as_str())
        );
        let head_range = server
            .send(with_header(
                Method::HEAD,
                &content,
                header::RANGE,
                "bytes=0-9",
            ))
            .await;
        assert_eq!(head_range.status, StatusCode::PARTIAL_CONTENT);
        assert_eq!(head_range.header(header::CONTENT_LENGTH), Some("10"));
        let head_past = server
            .send(with_header(
                Method::HEAD,
                &content,
                header::RANGE,
                &format!("bytes={size}-"),
            ))
            .await;
        assert_eq!(head_past.status, StatusCode::RANGE_NOT_SATISFIABLE);
        let head_cached = server
            .send(with_header(
                Method::HEAD,
                &content,
                header::IF_NONE_MATCH,
                &etag,
            ))
            .await;
        assert_eq!(head_cached.status, StatusCode::NOT_MODIFIED);
        let other = format!("{path}/files/fil_aaaaaaaaaaaaaaaaaaaaaaaaaa/content");
        assert_eq!(
            server.send(empty(Method::HEAD, &other)).await.status,
            StatusCode::NOT_FOUND
        );
        assert_problem(
            &server.get(&other).await,
            StatusCode::NOT_FOUND,
            "not-found",
        );
        server.stop().await;
    }

    /// STR-06.
    pub async fn content_types() {
        let server = TestServer::start().await;
        let created = server
            .upload(&catalog::media_names().metainfo().bytes, None)
            .await;
        let path = created.json()["links"]["self"].as_str().unwrap().to_owned();
        let expected = [
            ("video.mkv", "video/x-matroska", "inline"),
            ("video.mp4", "video/mp4", "inline"),
            ("audio.mp3", "audio/mpeg", "inline"),
            ("subtitles.vtt", "text/vtt", "inline"),
            ("image.png", "image/png", "inline"),
            ("page.html", "application/octet-stream", "attachment"),
            ("image.svg", "application/octet-stream", "attachment"),
            ("program.exe", "application/octet-stream", "attachment"),
        ];
        for (file, (name, media_type, disposition)) in
            files_of(&server, &path).await.iter().zip(expected)
        {
            assert_eq!(file["media_type"], media_type, "{name}");
            let content = file["links"]["content"].as_str().unwrap();
            let response = server
                .send(with_header(
                    Method::GET,
                    content,
                    header::RANGE,
                    "bytes=0-0",
                ))
                .await;
            assert_eq!(
                response.header(header::CONTENT_TYPE),
                Some(media_type),
                "{name}"
            );
            assert_eq!(
                response.header(header::CONTENT_DISPOSITION),
                Some(format!("{disposition}; filename=\"{name}\"").as_str())
            );
            assert_eq!(
                response.header(header::X_CONTENT_TYPE_OPTIONS),
                Some("nosniff")
            );
            assert_eq!(
                response.header(header::CONTENT_SECURITY_POLICY),
                Some("sandbox")
            );
            let refused = server
                .send(with_header(
                    Method::GET,
                    content,
                    header::ACCEPT,
                    "application/json",
                ))
                .await;
            assert_problem(&refused, StatusCode::NOT_ACCEPTABLE, "not-acceptable");
        }
        server.stop().await;
    }

    /// LIF-09 and the shutdown hook.
    pub async fn engine_unavailable() {
        let server = TestServer::start().await;
        let created = server
            .upload(&catalog::single_small().metainfo().bytes, None)
            .await;
        let path = created.json()["links"]["self"].as_str().unwrap().to_owned();
        let content = files_of(&server, &path).await[0]["links"]["content"]
            .as_str()
            .unwrap()
            .to_owned();
        let tag = server
            .get(&format!("{path}/configuration"))
            .await
            .header(header::ETAG)
            .unwrap()
            .to_owned();

        server.engine.set_readiness(Readiness::Unavailable {
            retry_after: Some(Duration::from_secs(5)),
        });
        let ready = server.get("/readyz").await;
        assert_problem(
            &ready,
            StatusCode::SERVICE_UNAVAILABLE,
            "engine-unavailable",
        );
        assert_eq!(ready.header(header::RETRY_AFTER), Some("5"));
        let refused = [
            server.create_magnet(&unknown_magnet(3)).await,
            server.send(empty(Method::DELETE, &path)).await,
            server
                .send(merge_patch(
                    &format!("{path}/configuration"),
                    &tag,
                    &json!({ "desired_state": "paused" }),
                ))
                .await,
            server.get(&content).await,
            server.get(&format!("{path}/statistics")).await,
        ];
        for exchange in refused {
            assert_problem(
                &exchange,
                StatusCode::SERVICE_UNAVAILABLE,
                "engine-unavailable",
            );
            assert_eq!(exchange.header(header::RETRY_AFTER), Some("5"));
        }
        for served in [
            "/api/v1/torrents",
            path.as_str(),
            "/api/v1/cache/statistics",
            "/livez",
        ] {
            assert_eq!(server.get(served).await.status, StatusCode::OK, "{served}");
        }
        assert_eq!(
            server.send(empty(Method::HEAD, &content)).await.status,
            StatusCode::OK
        );

        server.engine.set_readiness(Readiness::Ready);
        server.app.begin_shutdown();
        let ready = server.get("/readyz").await;
        assert_problem(
            &ready,
            StatusCode::SERVICE_UNAVAILABLE,
            "engine-unavailable",
        );
        assert_eq!(ready.header(header::RETRY_AFTER), None);
        let create = server.create_magnet(&unknown_magnet(4)).await;
        assert_problem(
            &create,
            StatusCode::SERVICE_UNAVAILABLE,
            "engine-unavailable",
        );
        assert_eq!(server.get(&path).await.status, StatusCode::OK);
        server.stop().await;
    }

    /// LIF-08, LIF-11, and `not-found` for every torrent resource.
    pub async fn deletion() {
        let server = TestServer::start().await;
        let spec = catalog::single_small();
        let created = server.upload(&spec.metainfo().bytes, None).await;
        let first_id = created.json()["id"].as_str().unwrap().to_owned();
        let path = created.json()["links"]["self"].as_str().unwrap().to_owned();
        let file = files_of(&server, &path).await[0]["links"]["self"]
            .as_str()
            .unwrap()
            .to_owned();
        assert_eq!(
            server.send(empty(Method::DELETE, &path)).await.status,
            StatusCode::NO_CONTENT
        );
        let gone = [
            server.get(&path).await,
            server.send(empty(Method::DELETE, &path)).await,
            server.get(&format!("{path}/configuration")).await,
            server
                .send(merge_patch(
                    &format!("{path}/configuration"),
                    "\"cfg-1\"",
                    &json!({ "desired_state": "paused" }),
                ))
                .await,
            server.get(&format!("{path}/files")).await,
            server.get(&file).await,
            server.get(&format!("{file}/content")).await,
            server.get(&format!("{path}/statistics")).await,
        ];
        for exchange in gone {
            assert_problem(&exchange, StatusCode::NOT_FOUND, "not-found");
        }
        let again = server.upload(&spec.metainfo().bytes, None).await;
        assert_eq!(again.status, StatusCode::CREATED);
        assert_ne!(again.json()["id"], first_id.as_str(), "LIF-08: a new ID");
        server.stop().await;
    }

    /// Torrent and cache statistics.
    pub async fn statistics() {
        let server = TestServer::start().await;
        let created = server
            .upload(&catalog::single_small().metainfo().bytes, None)
            .await;
        let id = server.id_of(&created);
        let path = created.json()["links"]["self"].as_str().unwrap().to_owned();
        server
            .engine
            .set_torrent_statistics(
                &id,
                TorrentStatistics {
                    downloaded_bytes: 4096,
                    download_rate_bytes_per_second: 1024,
                    peer_count: 3,
                    completion_ratio: Ratio::new(0.25),
                    ..TorrentStatistics::default()
                },
            )
            .unwrap();
        let statistics = server.get(&format!("{path}/statistics")).await;
        assert_eq!(statistics.header(header::CACHE_CONTROL), Some("no-cache"));
        let body = statistics.json();
        assert_eq!(body["downloaded_bytes"], 4096);
        assert_eq!(body["completion_ratio"], 0.25);
        assert_eq!(body["uploaded_bytes"], 0);
        server.engine.set_cache_statistics(CacheStatistics {
            capacity_bytes: 1 << 28,
            usage_bytes: 1 << 20,
            pinned_bytes: 1 << 16,
            eviction_count: 2,
        });
        let cache = server.get("/api/v1/cache/statistics").await.json();
        assert_eq!(cache["usage_bytes"], 1 << 20);
        assert_eq!(cache["eviction_count"], 2);
        server.stop().await;
    }

    /// OBS-01 to OBS-05: the system resource, the statistics, the metrics,
    /// and request IDs.
    pub async fn observability() {
        let logs = LogCapture::start();
        let server = TestServer::start().await;
        let (_, content) = uploaded_content(&server, &catalog::single_small()).await;
        server
            .upload(&catalog::multi_boundary().metainfo().bytes, Some("paused"))
            .await;
        let streamed = server.get(&content).await;
        assert_eq!(streamed.status, StatusCode::OK);
        let mut totals = EngineStatistics {
            active_reader_count: 1,
            idle_reader_count: 2,
            peer_count: 7,
            downloaded_bytes: 1 << 30,
            download_rate_bytes_per_second: 1 << 20,
            peer_connections: PeerConnections {
                tcp: ConnectionOutcomes {
                    succeeded: 5,
                    failed: 3,
                },
                ..PeerConnections::default()
            },
            ..EngineStatistics::default()
        };
        totals.piece_waits.waiting_reader_count = 1;
        server.engine.set_engine_statistics(totals);

        // OBS-01: the system resource describes the run.
        let system = server.get("/api/v1/system").await;
        assert_eq!(system.header(header::CACHE_CONTROL), Some("no-cache"));
        let body = system.json();
        assert_eq!(body["version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(body["os"], std::env::consts::OS);
        assert!(body["run_id"].as_str().unwrap().starts_with("run_"));
        if cfg!(target_os = "linux") {
            assert!(body["resident_memory_bytes"].as_u64().unwrap() > 0);
        }
        assert_eq!(body["links"]["metrics"], "/metrics");

        // OBS-02: the statistics count torrents by state and the bytes
        // streamed.
        let statistics = server.get("/api/v1/statistics").await.json();
        assert_eq!(statistics["torrent_count"], 2);
        assert_eq!(
            statistics["torrent_counts_by_observed_state"],
            json!({
                "retrieving_metadata": 0,
                "downloading": 1,
                "paused": 1,
                "failed": 0,
            })
        );
        assert_eq!(
            statistics["streamed_bytes"],
            catalog::single_small().total_size_bytes(),
            "the whole file streamed"
        );
        assert_eq!(
            (
                &statistics["active_reader_count"],
                &statistics["idle_reader_count"],
                &statistics["waiting_reader_count"],
                &statistics["downloaded_bytes"],
            ),
            (&json!(1), &json!(2), &json!(1), &json!(1 << 30))
        );

        // OBS-03: the metrics are well-formed and count what was served.
        let metrics = server.get("/metrics").await;
        assert_eq!(
            metrics.header(header::CONTENT_TYPE),
            Some("text/plain; version=0.0.4; charset=utf-8")
        );
        let text = String::from_utf8(metrics.body.clone()).unwrap();
        assert_exposition(&text);
        for line in [
            format!("torrplay_build_info{{version=\"{}\"}} 1", env!("CARGO_PKG_VERSION")),
            "torrplay_torrents{observed_state=\"paused\"} 1".to_owned(),
            "torrplay_readers{state=\"idle\"} 2".to_owned(),
            "torrplay_waiting_readers 1".to_owned(),
            "torrplay_peer_connections_total{transport=\"tcp\",outcome=\"failed\"} 3".to_owned(),
            "torrplay_downloaded_bytes_total 1073741824".to_owned(),
            "torrplay_piece_wait_seconds_count{outcome=\"ready\"} 0".to_owned(),
            "torrplay_http_requests_total{method=\"GET\",route=\"/api/v1/system\",status=\"200\"} 1"
                .to_owned(),
            "torrplay_http_response_head_seconds_count{method=\"GET\",route=\"/api/v1/statistics\"} 1"
                .to_owned(),
        ] {
            assert!(text.lines().any(|candidate| candidate == line), "{line} in\n{text}");
        }
        assert!(text.contains("\nprocess_start_time_seconds "), "{text}");
        assert!(!text.contains("trr_"), "no torrent IDs as labels");

        // OBS-04: a usable request ID is kept, another replaced, and a
        // problem and the request's log records name it.
        let kept = server
            .send(with_header(
                Method::GET,
                "/api/v1/system",
                header::HeaderName::from_static("x-request-id"),
                "proxy-7f3a.1",
            ))
            .await;
        assert_eq!(kept.header("x-request-id"), Some("proxy-7f3a.1"));
        let replaced = server
            .send(with_header(
                Method::GET,
                "/api/v1/system",
                header::HeaderName::from_static("x-request-id"),
                "two words",
            ))
            .await;
        assert!(
            replaced
                .header("x-request-id")
                .is_some_and(|id| id.starts_with("req_"))
        );
        let missing = server
            .get("/api/v1/torrents/trr_aaaaaaaaaaaaaaaaaaaaaaaaaa")
            .await;
        let request_id = missing.header("x-request-id").unwrap().to_owned();
        assert_eq!(missing.json()["request_id"], request_id.as_str());
        assert!(
            logs.text()
                .lines()
                .any(|line| line.contains(&request_id) && line.contains("status=404")),
            "{}",
            logs.text()
        );

        // OBS-05: without the engine, the statistics are unavailable and the
        // metrics keep the process; an engine fault fails both.
        server
            .engine
            .set_readiness(Readiness::Unavailable { retry_after: None });
        assert_problem(
            &server.get("/api/v1/statistics").await,
            StatusCode::SERVICE_UNAVAILABLE,
            "engine-unavailable",
        );
        let unavailable = String::from_utf8(server.get("/metrics").await.body).unwrap();
        assert_exposition(&unavailable);
        assert!(unavailable.contains("process_start_time_seconds"));
        assert!(!unavailable.contains("torrplay_readers"), "{unavailable}");
        server.engine.set_readiness(Readiness::Ready);
        server.engine.inject_fault(Some("the engine failed"));
        for path in ["/api/v1/statistics", "/metrics"] {
            assert_problem(
                &server.get(path).await,
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal-error",
            );
        }
        server.engine.inject_fault(None);
        server.stop().await;
    }

    /// Failures: content of a failed or paused torrent is a conflict.
    pub async fn failures() {
        let server = TestServer::start().await;
        let created = server
            .upload(&catalog::single_small().metainfo().bytes, Some("paused"))
            .await;
        let id = server.id_of(&created);
        let path = created.json()["links"]["self"].as_str().unwrap().to_owned();
        let content = files_of(&server, &path).await[0]["links"]["content"]
            .as_str()
            .unwrap()
            .to_owned();
        assert_problem(
            &server.get(&content).await,
            StatusCode::CONFLICT,
            "lifecycle-conflict",
        );
        server
            .engine
            .inject_failure(&id, "A piece could not be written to storage.")
            .unwrap();
        let torrent = server.get(&path).await.json();
        assert_eq!(torrent["observed_state"], "failed");
        assert_eq!(
            torrent["failure"]["detail"],
            "A piece could not be written to storage."
        );
        let failed = server
            .get("/api/v1/torrents?observed_state=failed")
            .await
            .json();
        assert_eq!(failed["items"].as_array().unwrap().len(), 1);
        server.stop().await;
    }

    /// API-21: a private tracker's passkey never appears in a response,
    /// across every operation and its errors. The server does not log.
    pub async fn passkey_redaction() {
        let logs = LogCapture::start();
        let server = TestServer::start().await;
        let secret = "s3cr3tpasskey";
        let uri = format!(
            "{}&tr=http%3A%2F%2Ftracker.test%2F{secret}%2Fannounce",
            magnet(&catalog::single_small())
        );
        let created = server.create_magnet(&uri).await;
        let path = created.json()["links"]["self"].as_str().unwrap().to_owned();
        let configuration = format!("{path}/configuration");
        let tag = server
            .get(&configuration)
            .await
            .header(header::ETAG)
            .unwrap()
            .to_owned();
        let file = format!("{path}/files/fil_aaaaaaaaaaaaaaaaaaaaaaaaaa");
        let mut exchanges = vec![
            created,
            server.create_magnet(&uri).await,
            server.create_magnet(&format!("{uri}&xt=urn:btih:zz")).await,
            server.get("/api/v1").await,
            server.get("/api/v1/torrents").await,
            server.get(&path).await,
            server.get(&configuration).await,
            server
                .send(merge_patch(
                    &configuration,
                    "\"cfg-0\"",
                    &json!({ "desired_state": "paused" }),
                ))
                .await,
            server
                .send(merge_patch(
                    &configuration,
                    &tag,
                    &json!({ "desired_state": "paused" }),
                ))
                .await,
            server.get(&format!("{path}/files")).await,
            server.get(&file).await,
            server.get(&format!("{file}/content")).await,
            server
                .send(empty(Method::HEAD, &format!("{file}/content")))
                .await,
            server.get(&format!("{path}/statistics")).await,
            server.get("/api/v1/cache/statistics").await,
            server.send(empty(Method::DELETE, &path)).await,
        ];
        exchanges.push(server.get(&path).await);
        // An engine failure that quotes the tracker URL is logged redacted.
        server.engine.inject_fault(Some(&format!(
            "announce to http://tracker.test/{secret}/announce failed"
        )));
        exchanges.push(server.get(&format!("{path}/statistics")).await);
        exchanges.push(server.get("/api/v1/cache/statistics").await);
        server.engine.inject_fault(None);
        for exchange in exchanges {
            let text = String::from_utf8_lossy(&exchange.body);
            assert!(!text.contains(secret), "{text}");
            for value in exchange.headers.values() {
                assert!(!value.to_str().unwrap_or("").contains(secret));
            }
        }
        server.stop().await;
        let logged = logs.text();
        assert!(logged.contains("the torrent engine failed"), "{logged}");
        assert!(logged.contains("http://tracker.test/…"), "{logged}");
        assert!(
            logged.contains("route=\"/api/v1/torrents/{torrent_id}\""),
            "{logged}"
        );
        assert!(!logged.contains(secret), "{logged}");
    }

    /// Unexpected engine failures are `internal-error`, and nothing they
    /// interrupt is left half applied.
    pub async fn engine_faults() {
        let server = TestServer::start().await;
        let created = server
            .upload(&catalog::single_small().metainfo().bytes, None)
            .await;
        let path = created.json()["links"]["self"].as_str().unwrap().to_owned();
        let file = files_of(&server, &path).await[0]["links"]["self"]
            .as_str()
            .unwrap()
            .to_owned();
        let configuration = format!("{path}/configuration");
        let tag = server
            .get(&configuration)
            .await
            .header(header::ETAG)
            .unwrap()
            .to_owned();

        server.engine.inject_fault(Some("the engine failed"));
        let failed = [
            server.create_magnet(&unknown_magnet(6)).await,
            server.get("/api/v1/torrents").await,
            server.get(&path).await,
            server
                .send(merge_patch(
                    &configuration,
                    &tag,
                    &json!({ "desired_state": "paused" }),
                ))
                .await,
            server.get(&format!("{path}/files")).await,
            server.get(&file).await,
            server.get(&format!("{file}/content")).await,
            server
                .send(empty(Method::HEAD, &format!("{file}/content")))
                .await,
            server.get(&format!("{path}/statistics")).await,
            server.get("/api/v1/cache/statistics").await,
        ];
        for exchange in failed {
            assert_eq!(exchange.status, StatusCode::INTERNAL_SERVER_ERROR);
            if !exchange.body.is_empty() {
                assert_eq!(exchange.problem(), "internal-error");
                assert!(!String::from_utf8_lossy(&exchange.body).contains("the engine failed"));
            }
        }

        server.engine.inject_fault(None);
        let kept = server.get(&configuration).await;
        assert_eq!(
            kept.header(header::ETAG),
            Some(tag.as_str()),
            "the failed PATCH stored nothing"
        );
        assert_eq!(kept.json()["desired_state"], "started");
        let listed = server.get("/api/v1/torrents").await.json();
        assert_eq!(
            listed["items"].as_array().unwrap().len(),
            1,
            "nothing was created"
        );

        // A deletion is stored before the engine removes the torrent, so a
        // fault in the engine does not undo it.
        server.engine.inject_fault(Some("the engine failed"));
        assert_eq!(
            server.send(empty(Method::DELETE, &path)).await.status,
            StatusCode::NO_CONTENT
        );
        server.engine.inject_fault(None);
        assert_problem(&server.get(&path).await, StatusCode::NOT_FOUND, "not-found");
        server.stop().await;
    }

    /// API-07: oversized bodies are refused before they are fully sent.
    pub async fn bodies_refused_while_streaming() {
        let server = TestServer::start().await;
        let chunk = |data: Vec<u8>| {
            let mut chunk = format!("{:x}\r\n", data.len()).into_bytes();
            chunk.extend_from_slice(&data);
            chunk.extend_from_slice(b"\r\n");
            chunk
        };
        let chunked = ("Transfer-Encoding", "chunked");
        let torrents = "/api/v1/torrents";
        // Chunked bodies that cross their limit and never end.
        let json = server
            .send_before_body_ends(
                Method::POST,
                torrents,
                &[("Content-Type", "application/json"), chunked],
                chunk(vec![b' '; 20 * 1024]),
            )
            .await;
        assert_problem(&json, StatusCode::PAYLOAD_TOO_LARGE, "content-too-large");
        let form_type = format!("multipart/form-data; boundary={BOUNDARY}");
        let mut form = format!(
            "--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"torrent_file\"\r\n\r\n"
        )
        .into_bytes();
        form.resize(form.len() + 11 * 1024 * 1024, b'd');
        let upload = server
            .send_before_body_ends(
                Method::POST,
                torrents,
                &[("Content-Type", &form_type), chunked],
                chunk(form),
            )
            .await;
        assert_problem(&upload, StatusCode::PAYLOAD_TOO_LARGE, "content-too-large");
        // A declared length over the limit is refused before any body.
        let declared = server
            .send_before_body_ends(
                Method::POST,
                torrents,
                &[
                    ("Content-Type", "application/json"),
                    ("Content-Length", "20000"),
                ],
                Vec::new(),
            )
            .await;
        assert_problem(
            &declared,
            StatusCode::PAYLOAD_TOO_LARGE,
            "content-too-large",
        );
        server.stop().await;
    }

    /// A body that stops arriving is `request-timeout`, and the server
    /// closes the connection.
    pub async fn slow_bodies_time_out() {
        let server = TestServer::start_with_body_timeout(Duration::from_millis(200)).await;
        let created = server.create_magnet(&unknown_magnet(5)).await;
        let configuration = format!(
            "{}/configuration",
            created.json()["links"]["self"].as_str().unwrap()
        );
        let tag = server
            .get(&configuration)
            .await
            .header(header::ETAG)
            .unwrap()
            .to_owned();
        let settings_tag = server
            .get("/api/v1/settings")
            .await
            .header(header::ETAG)
            .unwrap()
            .to_owned();
        let profile = default_profile(&server).await;
        let profile_tag = server
            .get(&profile)
            .await
            .header(header::ETAG)
            .unwrap()
            .to_owned();
        let small = catalog::single_small();
        let uploaded = server.upload(&small.metainfo().bytes, None).await.json();
        let uploaded_path = uploaded["links"]["self"].as_str().unwrap().to_owned();
        let file_id = files_of(&server, &uploaded_path).await[0]["id"]
            .as_str()
            .unwrap()
            .to_owned();
        let progress = format!(
            "{profile}/torrents/{}/progress/{file_id}",
            uploaded["id"].as_str().unwrap()
        );
        let partial = br#"{"desired_state":"#.to_vec();
        let length = (partial.len() + 10).to_string();
        let requests = [
            (
                Method::POST,
                "/api/v1/torrents".to_owned(),
                "application/json",
                None,
            ),
            (
                Method::PATCH,
                configuration.clone(),
                "application/merge-patch+json",
                Some(tag.as_str()),
            ),
            (
                Method::PATCH,
                "/api/v1/settings".to_owned(),
                "application/merge-patch+json",
                Some(settings_tag.as_str()),
            ),
            (
                Method::POST,
                format!(
                    "{}/files/fil_aaaaaaaaaaaaaaaaaaaaaaaaaa/preloads",
                    created.json()["links"]["self"].as_str().unwrap()
                ),
                "application/json",
                None,
            ),
            (
                Method::PUT,
                format!(
                    "{}/poster",
                    created.json()["links"]["self"].as_str().unwrap()
                ),
                "image/png",
                None,
            ),
            (
                Method::POST,
                "/api/v1/profiles".to_owned(),
                "application/json",
                None,
            ),
            (
                Method::PATCH,
                profile.clone(),
                "application/merge-patch+json",
                Some(profile_tag.as_str()),
            ),
            (Method::PUT, format!("{profile}/avatar"), "image/png", None),
            (Method::PUT, progress.clone(), "application/json", None),
            (
                Method::PUT,
                format!("{profile}/title-progress/tmdb-movie-603"),
                "application/json",
                None,
            ),
        ];
        for (method, path, media_type, if_match) in requests {
            let mut headers = vec![
                ("Content-Type", media_type),
                ("Content-Length", length.as_str()),
            ];
            if let Some(tag) = if_match {
                headers.push(("If-Match", tag));
            }
            let timed_out = server
                .send_before_body_ends(method, &path, &headers, partial.clone())
                .await;
            assert_problem(&timed_out, StatusCode::REQUEST_TIMEOUT, "request-timeout");
            assert_eq!(timed_out.header(header::CONNECTION), Some("close"));
        }
        let kept = server.get(&configuration).await;
        assert_eq!(kept.header(header::ETAG), Some(tag.as_str()));
        server.stop().await;
    }

    /// LIF-11 and LIF-13: a restarted server keeps its torrents' IDs,
    /// creation times, order, configuration ETags, and recorded metadata;
    /// content URLs keep working, and deletion lasts.
    pub async fn restart() {
        let server = TestServer::start().await;
        let small = catalog::single_small();
        let multi = catalog::multi_boundary();
        let many = catalog::many_files();
        let media = catalog::media_names();
        let upload = server.upload(&small.metainfo().bytes, None).await.json();
        let paused = server
            .upload(&multi.metainfo().bytes, Some("paused"))
            .await
            .json();
        server.engine.set_metadata_retrievable(
            torrplay_core::id::InfoHashV1::from_bytes(many.metainfo().info_hash),
            true,
        );
        let magnet_path = server.create_magnet(&magnet(&many)).await.json()["links"]["self"]
            .as_str()
            .unwrap()
            .to_owned();
        assert!(
            !server.get(&magnet_path).await.json()["name"].is_null(),
            "the server records the retrieved metadata"
        );
        let deleted = server.upload(&media.metainfo().bytes, None).await.json();
        let deleted_path = deleted["links"]["self"].as_str().unwrap().to_owned();
        assert_eq!(
            server
                .send(empty(Method::DELETE, &deleted_path))
                .await
                .status,
            StatusCode::NO_CONTENT
        );
        let paused_configuration = format!(
            "{}/configuration",
            paused["links"]["self"].as_str().unwrap()
        );
        let etag = server
            .get(&paused_configuration)
            .await
            .header(header::ETAG)
            .unwrap()
            .to_owned();
        let upload_path = upload["links"]["self"].as_str().unwrap().to_owned();
        let content = format!(
            "{}/content",
            files_of(&server, &upload_path).await[0]["links"]["self"]
                .as_str()
                .unwrap()
        );
        let before = server.get("/api/v1/torrents").await.json()["items"].clone();
        assert_eq!(before.as_array().unwrap().len(), 3);

        // The new engine cannot retrieve the magnet torrent's metadata.
        let server = server.restart().await;
        assert_eq!(
            server.get("/api/v1/torrents").await.json()["items"],
            before,
            "the same torrents, in the same order, with their metadata"
        );
        assert_eq!(
            server.get(&paused_configuration).await.header(header::ETAG),
            Some(etag.as_str())
        );
        let streamed = server.get(&content).await;
        assert_eq!(streamed.status, StatusCode::OK);
        let size = small.total_size_bytes();
        assert_eq!(streamed.body, expected_bytes(&small, 0, 0, size));
        assert_problem(
            &server.get(&deleted_path).await,
            StatusCode::NOT_FOUND,
            "not-found",
        );
        let again = server.upload(&media.metainfo().bytes, None).await;
        assert_eq!(again.status, StatusCode::CREATED, "its info hash is free");
        server.stop().await;
    }

    /// LIF-14: a failed database write is an internal error and changes
    /// nothing, and the change succeeds once writes work again.
    pub async fn store_failures() {
        let server = TestServer::start().await;
        let small = catalog::single_small();
        let created = server.upload(&small.metainfo().bytes, None).await.json();
        let path = created["links"]["self"].as_str().unwrap().to_owned();
        let configuration = format!("{path}/configuration");
        let etag = server
            .get(&configuration)
            .await
            .header(header::ETAG)
            .unwrap()
            .to_owned();
        let listed = || async { server.get("/api/v1/torrents").await.json()["items"].clone() };
        let before = listed().await;

        server.app.store().fail_writes(true);
        let multi = catalog::multi_boundary();
        assert_problem(
            &server.upload(&multi.metainfo().bytes, None).await,
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal-error",
        );
        assert_problem(
            &server
                .send(merge_patch(
                    &configuration,
                    &etag,
                    &json!({ "desired_state": "paused" }),
                ))
                .await,
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal-error",
        );
        assert_problem(
            &server.send(empty(Method::DELETE, &path)).await,
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal-error",
        );
        assert_eq!(listed().await, before, "nothing changed");
        assert_eq!(
            server.get(&configuration).await.header(header::ETAG),
            Some(etag.as_str())
        );

        server.app.store().fail_writes(false);
        let paused = server
            .send(merge_patch(
                &configuration,
                &etag,
                &json!({ "desired_state": "paused" }),
            ))
            .await;
        assert_eq!(paused.status, StatusCode::OK);
        let server = server.restart().await;
        assert_eq!(
            server.get(&configuration).await.json()["desired_state"],
            "paused",
            "the change that succeeded was stored"
        );
        assert_eq!(listed_len(&server).await, 1, "the failed creation was not");
        server.stop().await;
    }

    /// LIF-15: until the stored torrents are restored, the server is not
    /// ready and torrent operations are unavailable.
    pub async fn restoring() {
        let server = TestServer::start().await;
        let small = catalog::single_small();
        let path = server.upload(&small.metainfo().bytes, None).await.json()["links"]["self"]
            .as_str()
            .unwrap()
            .to_owned();
        let file = files_of(&server, &path).await[0]["links"]["self"]
            .as_str()
            .unwrap()
            .to_owned();
        let profile = default_profile(&server).await;
        let server = server
            .restart_with(torrplay_core::fake::FakeEngine::with_catalog(), false)
            .await;
        assert_eq!(
            server.get("/readyz").await.status,
            StatusCode::SERVICE_UNAVAILABLE
        );
        for exchange in [
            server.get("/api/v1/torrents").await,
            server.get(&path).await,
            server.get(&format!("{path}/configuration")).await,
            server.get(&format!("{path}/files")).await,
            server.get(&file).await,
        ] {
            assert_problem(
                &exchange,
                StatusCode::SERVICE_UNAVAILABLE,
                "engine-unavailable",
            );
        }
        let head = server
            .send(empty(Method::HEAD, &format!("{file}/content")))
            .await;
        assert_eq!(head.status, StatusCode::SERVICE_UNAVAILABLE);
        let poster = format!("{path}/poster");
        for exchange in [
            server.get(&poster).await,
            server.send(put_poster(&poster, "image/png", PNG)).await,
            server.send(empty(Method::DELETE, &poster)).await,
        ] {
            assert_problem(
                &exchange,
                StatusCode::SERVICE_UNAVAILABLE,
                "engine-unavailable",
            );
        }
        let head = server.send(empty(Method::HEAD, &poster)).await;
        assert_eq!(head.status, StatusCode::SERVICE_UNAVAILABLE);
        let torrent = path.rsplit('/').next().unwrap();
        let progress = format!("{profile}/torrents/{torrent}/progress");
        let file_id = file.rsplit('/').next().unwrap();
        let entry = format!("{profile}/up-next/upn_aaaaaaaaaaaaaaaaaaaaaaaaaa");
        for exchange in [
            server.get("/api/v1/profiles").await,
            server
                .send(json_request(
                    Method::POST,
                    "/api/v1/profiles",
                    &json!({ "name": "Kim" }),
                ))
                .await,
            server.get(&profile).await,
            server
                .send(merge_patch(
                    &profile,
                    "\"prf-x\"",
                    &json!({ "name": "Kim" }),
                ))
                .await,
            server.send(empty(Method::DELETE, &profile)).await,
            server.get(&format!("{profile}/avatar")).await,
            server
                .send(put_poster(&format!("{profile}/avatar"), "image/png", PNG))
                .await,
            server
                .send(empty(Method::DELETE, &format!("{profile}/avatar")))
                .await,
            server.get(&progress).await,
            server.get(&format!("{progress}/{file_id}")).await,
            server
                .send(put_progress(
                    &format!("{progress}/{file_id}"),
                    1.0,
                    10.0,
                    false,
                ))
                .await,
            server
                .send(empty(Method::DELETE, &format!("{progress}/{file_id}")))
                .await,
            server.get(&format!("{profile}/up-next")).await,
            server.get(&entry).await,
            server.send(empty(Method::DELETE, &entry)).await,
        ] {
            assert_problem(
                &exchange,
                StatusCode::SERVICE_UNAVAILABLE,
                "engine-unavailable",
            );
        }
        let head = server
            .send(empty(Method::HEAD, &format!("{profile}/avatar")))
            .await;
        assert_eq!(head.status, StatusCode::SERVICE_UNAVAILABLE);
        server.finish_restore().await;
        assert_eq!(server.get("/readyz").await.status, StatusCode::OK);
        assert_eq!(server.get(&path).await.status, StatusCode::OK);
        server.stop().await;
    }

    /// LIF-16: a stored torrent the engine refuses at startup is restored as
    /// failed, refuses content and configuration changes, and can be
    /// deleted.
    pub async fn refused_on_restore() {
        let server = TestServer::start().await;
        let small = catalog::single_small();
        let created = server.upload(&small.metainfo().bytes, None).await.json();
        let path = created["links"]["self"].as_str().unwrap().to_owned();
        let content = format!(
            "{}/content",
            files_of(&server, &path).await[0]["links"]["self"]
                .as_str()
                .unwrap()
        );
        // An engine without the fixture rejects its torrent file.
        let server = server
            .restart_with(torrplay_core::fake::FakeEngine::new(), true)
            .await;
        let torrent = server.get(&path).await.json();
        assert_eq!(torrent["observed_state"], "failed");
        assert!(torrent["failure"]["detail"].is_string());
        assert_problem(
            &server.get(&content).await,
            StatusCode::CONFLICT,
            "lifecycle-conflict",
        );
        let configuration = format!("{path}/configuration");
        let etag = server
            .get(&configuration)
            .await
            .header(header::ETAG)
            .unwrap()
            .to_owned();
        assert_problem(
            &server
                .send(merge_patch(
                    &configuration,
                    &etag,
                    &json!({ "desired_state": "paused" }),
                ))
                .await,
            StatusCode::CONFLICT,
            "lifecycle-conflict",
        );
        let retitled = server
            .send(merge_patch(
                &configuration,
                &etag,
                &json!({ "title": "Kept" }),
            ))
            .await;
        assert_eq!(
            retitled.status,
            StatusCode::OK,
            "library members need no engine"
        );
        assert_eq!(
            server.send(empty(Method::DELETE, &path)).await.status,
            StatusCode::NO_CONTENT
        );
        assert_eq!(listed_len(&server).await, 0);
        server.stop().await;
    }

    /// TMP-01 to TMP-07: a temporary torrent plays, stays out of the
    /// listing, refuses what only persisted torrents keep, expires once
    /// unwatched, makes room for another, and becomes persisted.
    pub async fn temporary_torrents() {
        let server = TestServer::start().await;
        let torrents = "/api/v1/torrents";
        let etag = |exchange: &Exchange| exchange.header(header::ETAG).unwrap().to_owned();
        let retrievable = |server: &TestServer, spec: &FixtureSpec| {
            server.engine.set_metadata_retrievable(
                torrplay_core::id::InfoHashV1::from_bytes(spec.metainfo().info_hash),
                true,
            );
        };

        // TMP-03: what a temporary torrent cannot be created with.
        let small = catalog::single_small();
        let refused = server
            .send_unchecked(json_request(
                Method::POST,
                torrents,
                &json!({
                    "magnet_uri": magnet(&small),
                    "retention": "temporary",
                    "storage_mode": "disk",
                    "title": "Kept",
                }),
            ))
            .await;
        assert_problem(
            &refused,
            StatusCode::UNPROCESSABLE_ENTITY,
            "validation-failed",
        );
        let mut pointers = errors(&refused, "pointer");
        pointers.sort();
        assert_eq!(pointers, ["/storage_mode", "/title"]);

        // TMP-01: it plays like any torrent.
        let created = create_temporary(&server, &small).await;
        assert_eq!(created.json()["retention"], "temporary");
        let path = created.json()["links"]["self"].as_str().unwrap().to_owned();
        retrievable(&server, &small);
        assert_eq!(server.get(&path).await.json()["name"], "single-small.bin");
        let configuration = server.get(&format!("{path}/configuration")).await;
        assert_eq!(configuration.json()["retention"], "temporary");
        let file_id = files_of(&server, &path).await[0]["id"]
            .as_str()
            .unwrap()
            .to_owned();
        let content = format!("{path}/files/{file_id}/content");
        assert_eq!(server.get(&content).await.status, StatusCode::OK);

        // TMP-02: listed only on request.
        let persisted = server.create_magnet(&magnet(&catalog::media_names())).await;
        let persisted_path = persisted.json()["links"]["self"]
            .as_str()
            .unwrap()
            .to_owned();
        for (query, total) in [("", 1), ("?retention=temporary", 1), ("?retention=any", 2)] {
            let page = server.get(&format!("{torrents}{query}")).await.json();
            assert_eq!(page["total"], total, "{query}");
            assert_eq!(page["facets"]["total"], total, "{query}");
        }
        assert_eq!(
            server.get(torrents).await.json()["items"][0]["retention"],
            "persisted"
        );

        // TMP-03: and what it cannot take later.
        let config_path = format!("{path}/configuration");
        let tag = etag(&configuration);
        for patch in [
            json!({ "title": "Kept" }),
            json!({ "storage_mode": "disk" }),
        ] {
            assert_problem(
                &server.send(merge_patch(&config_path, &tag, &patch)).await,
                StatusCode::CONFLICT,
                "lifecycle-conflict",
            );
        }
        assert_problem(
            &server
                .send(put_poster(&format!("{path}/poster"), "image/png", PNG))
                .await,
            StatusCode::CONFLICT,
            "lifecycle-conflict",
        );
        let persisted_configuration = format!("{persisted_path}/configuration");
        let persisted_tag = etag(&server.get(&persisted_configuration).await);
        let demoted = server
            .send(merge_patch(
                &persisted_configuration,
                &persisted_tag,
                &json!({ "retention": "temporary" }),
            ))
            .await;
        assert_problem(
            &demoted,
            StatusCode::UNPROCESSABLE_ENTITY,
            "validation-failed",
        );
        assert_eq!(errors(&demoted, "pointer"), ["/retention"]);

        // TMP-04: idle time counts from the last stream.
        server.advance_clock(Duration::from_secs(500));
        assert_eq!(server.get(&content).await.status, StatusCode::OK);
        server.advance_clock(Duration::from_secs(200));
        server.app.expire_temporaries().await;
        assert_eq!(server.get(&path).await.status, StatusCode::OK);
        server.advance_clock(Duration::from_secs(450));
        server.app.expire_temporaries().await;
        assert_problem(&server.get(&path).await, StatusCode::NOT_FOUND, "not-found");
        assert_eq!(server.get(&persisted_path).await.status, StatusCode::OK);

        // A stream in progress keeps it however long it lasts.
        let streamed = large("temporary-stream", 51);
        server.engine.add_fixture(&streamed);
        let watched = create_temporary(&server, &streamed).await;
        let watched_path = watched.json()["links"]["self"].as_str().unwrap().to_owned();
        retrievable(&server, &streamed);
        let watched_file = files_of(&server, &watched_path).await[0]["id"]
            .as_str()
            .unwrap()
            .to_owned();
        let stream = server
            .open_stream(&format!("{watched_path}/files/{watched_file}/content"))
            .await;
        server.advance_clock(Duration::from_secs(10_000));
        server.app.expire_temporaries().await;
        assert_eq!(server.get(&watched_path).await.status, StatusCode::OK);

        // TMP-05: at the limit, the one idle longest makes room, unless all
        // are watched.
        let settings = server.get("/api/v1/settings").await;
        let limited = server
            .send(merge_patch(
                "/api/v1/settings",
                &etag(&settings),
                &json!({ "temporary_torrents_max": 2 }),
            ))
            .await;
        assert_eq!(limited.status, StatusCode::OK);
        let idle = create_temporary(&server, &catalog::multi_boundary()).await;
        let idle_path = idle.json()["links"]["self"].as_str().unwrap().to_owned();
        server.advance_clock(Duration::from_secs(1));
        let newer = create_temporary(&server, &catalog::many_files()).await;
        assert_eq!(newer.status, StatusCode::CREATED);
        assert_problem(
            &server.get(&idle_path).await,
            StatusCode::NOT_FOUND,
            "not-found",
        );
        let newer_path = newer.json()["links"]["self"].as_str().unwrap().to_owned();
        let other = large("temporary-other", 52);
        server.engine.add_fixture(&other);
        let other_file = {
            retrievable(&server, &other);
            let other_created = create_temporary(&server, &other).await;
            assert_eq!(other_created.status, StatusCode::CREATED, "newer made room");
            let other_path = other_created.json()["links"]["self"]
                .as_str()
                .unwrap()
                .to_owned();
            let file = files_of(&server, &other_path).await[0]["id"]
                .as_str()
                .unwrap()
                .to_owned();
            format!("{other_path}/files/{file}/content")
        };
        assert_problem(
            &server.get(&newer_path).await,
            StatusCode::NOT_FOUND,
            "not-found",
        );
        let second_stream = server.open_stream(&other_file).await;
        let full = create_temporary(&server, &catalog::single_small()).await;
        assert_problem(&full, StatusCode::CONFLICT, "too-many-temporary-torrents");
        drop((stream, second_stream));

        // TMP-06: persisting keeps it, with library members set at once.
        let watched_configuration = format!("{watched_path}/configuration");
        let watched_tag = etag(&server.get(&watched_configuration).await);
        let kept = server
            .send(merge_patch(
                &watched_configuration,
                &watched_tag,
                &json!({ "retention": "persisted", "title": "Kept" }),
            ))
            .await;
        assert_eq!(kept.status, StatusCode::OK);
        assert_eq!(kept.json()["retention"], "persisted");
        assert_eq!(kept.json()["title"], "Kept");

        // TMP-07: a restart forgets the temporary torrent, not the kept one.
        let forgotten_path = other_file.split("/files/").next().unwrap().to_owned();
        let server = server.restart().await;
        assert_problem(
            &server.get(&forgotten_path).await,
            StatusCode::NOT_FOUND,
            "not-found",
        );
        let restored = server.get(&watched_path).await.json();
        assert_eq!(restored["retention"], "persisted");
        assert_eq!(restored["title"], "Kept");
        server.stop().await;
    }

    /// SET-10, SET-11, and SET-12: the DLNA name, DHT, and log level
    /// settings validate, reach the engine and the application's tasks, and
    /// yield to command-line overrides for one run.
    pub async fn named_dht_and_log_settings() {
        let server = TestServer::start().await;
        let path = "/api/v1/settings";
        let tag = |exchange: &Exchange| exchange.header(header::ETAG).unwrap().to_owned();
        let first = tag(&server.get(path).await);
        assert!(server.engine.dht_enabled());

        let invalid = server
            .send_unchecked(merge_patch(
                path,
                &first,
                &json!({ "dlna_name": 7, "dht_enabled": "no", "log_level": "verbose" }),
            ))
            .await;
        assert_problem(
            &invalid,
            StatusCode::UNPROCESSABLE_ENTITY,
            "validation-failed",
        );
        let mut pointers = errors(&invalid, "pointer");
        pointers.sort();
        assert_eq!(pointers, ["/dht_enabled", "/dlna_name", "/log_level"]);
        for name in ["", "Living\nroom", &"x".repeat(65)] {
            let invalid = server
                .send_unchecked(merge_patch(path, &first, &json!({ "dlna_name": name })))
                .await;
            assert_problem(
                &invalid,
                StatusCode::UNPROCESSABLE_ENTITY,
                "validation-failed",
            );
            assert_eq!(errors(&invalid, "pointer"), ["/dlna_name"], "{name:?}");
        }

        let mut levels = server.app.log_level();
        let mut media_server = server.app.media_server_setting();
        let changed = server
            .send(merge_patch(
                path,
                &first,
                &json!({
                    "dlna_name": "Кино: living room",
                    "dht_enabled": false,
                    "log_level": "debug",
                }),
            ))
            .await;
        assert_eq!(changed.status, StatusCode::OK);
        let body = changed.json();
        assert_eq!(body["dlna_name"], "Кино: living room");
        assert_eq!(body["dht_enabled"], false);
        assert_eq!(body["log_level"], "debug");
        assert_eq!(body["overrides"], json!({}));
        assert!(
            !server.engine.dht_enabled(),
            "turning the DHT off applies at once"
        );
        assert!(levels.has_changed().unwrap());
        assert_eq!(*levels.borrow_and_update(), LogLevel::Debug);
        assert!(media_server.has_changed().unwrap());
        assert_eq!(media_server.borrow_and_update().name, "Кино: living room");

        let reset = server
            .send(merge_patch(
                path,
                &tag(&changed),
                &json!({ "dht_enabled": null }),
            ))
            .await;
        assert_eq!(reset.json()["dht_enabled"], true);
        assert!(server.engine.dht_enabled(), "the default turns it on again");
        let stored = tag(&reset);

        // SET-12: a run with overrides reports them and keeps them in
        // effect while PATCH changes the stored values.
        let server = server
            .restart_overriding(Overrides {
                dht_enabled: Some(false),
                log_level: Some(LogOverride::Level(LogLevel::Trace)),
            })
            .await;
        let read = server.get(path).await;
        assert_eq!(
            read.json()["overrides"],
            json!({ "dht_enabled": false, "log_level": "trace" })
        );
        assert_eq!(read.json()["dht_enabled"], true, "the stored value");
        assert_eq!(read.json()["dlna_name"], "Кино: living room");
        assert_ne!(tag(&read), stored, "the overrides are part of the ETag");
        assert!(!server.engine.dht_enabled(), "--no-dht keeps the DHT off");
        let patched = server
            .send(merge_patch(
                path,
                &tag(&read),
                &json!({ "dht_enabled": true, "log_level": "warn" }),
            ))
            .await;
        assert_eq!(patched.status, StatusCode::OK);
        assert_eq!(patched.json()["log_level"], "warn");
        assert_eq!(
            patched.json()["overrides"],
            json!({ "dht_enabled": false, "log_level": "trace" })
        );
        assert!(!server.engine.dht_enabled());

        let server = server.restart().await;
        let read = server.get(path).await;
        assert_eq!(read.json()["overrides"], json!({}));
        assert_eq!(read.json()["log_level"], "warn");
        assert!(
            server.engine.dht_enabled(),
            "the stored value applies again"
        );
        assert_eq!(*server.app.log_level().borrow(), LogLevel::Warn);
        server.stop().await;
    }

    /// SET-01, SET-02, SET-04, and SET-05: settings start at their
    /// defaults, change with validated merge patches, reach the engine, and
    /// survive a restart; a failed write changes nothing.
    pub async fn settings() {
        let server = TestServer::start().await;
        let path = "/api/v1/settings";
        let read = server.get(path).await;
        assert_eq!(read.status, StatusCode::OK);
        assert_eq!(
            read.json(),
            json!({
                "piece_wait_seconds": 60,
                "idle_timeout_seconds": 30,
                "idle_readers_per_torrent": 4,
                "idle_readers_total": 16,
                "drain_period_seconds": 10,
                "peer_limit": 50,
                "cache_capacity_bytes": 268_435_456,
                "temporary_idle_seconds": 600,
                "temporary_torrents_max": 16,
                "temporary_progress_seconds": 2592000,
                "background_downloads": true,
                "dlna_enabled": false,
                "dlna_name": "TorrPlay",
                "dht_enabled": true,
                "log_level": "info",
                "overrides": {},
                "links": { "self": path },
            })
        );
        assert_eq!(
            server.get("/api/v1").await.json()["links"]["settings"],
            path
        );
        let tag = read.header(header::ETAG).unwrap().to_owned();
        assert_eq!(
            server
                .send(with_header(Method::GET, path, header::IF_NONE_MATCH, &tag))
                .await
                .status,
            StatusCode::NOT_MODIFIED
        );

        // Refused patches change nothing.
        assert_problem(
            &server
                .send_unchecked(patch(
                    path,
                    None,
                    br#"{"peer_limit":60}"#,
                    "application/merge-patch+json",
                ))
                .await,
            StatusCode::PRECONDITION_REQUIRED,
            "precondition-required",
        );
        assert_problem(
            &server
                .send_unchecked(patch(
                    path,
                    Some(&tag),
                    br#"{"peer_limit":60}"#,
                    "application/json",
                ))
                .await,
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "unsupported-media-type",
        );
        assert_problem(
            &server
                .send_unchecked(patch(
                    path,
                    Some(&tag),
                    b"{",
                    "application/merge-patch+json",
                ))
                .await,
            StatusCode::BAD_REQUEST,
            "malformed-request",
        );
        assert_problem(
            &server
                .send_unchecked(patch(
                    path,
                    Some(&tag),
                    &vec![b' '; 20_000],
                    "application/merge-patch+json",
                ))
                .await,
            StatusCode::PAYLOAD_TOO_LARGE,
            "content-too-large",
        );
        let invalid = server
            .send_unchecked(merge_patch(
                path,
                &tag,
                &json!({
                    "piece_wait_seconds": 0,
                    "peer_limit": "many",
                    "readahead_bytes": 1,
                    "idle_readers_total": 2,
                    "background_downloads": 1,
                }),
            ))
            .await;
        assert_problem(
            &invalid,
            StatusCode::UNPROCESSABLE_ENTITY,
            "validation-failed",
        );
        let mut pointers = errors(&invalid, "pointer");
        pointers.sort();
        assert_eq!(
            pointers,
            ["/background_downloads", "/peer_limit", "/readahead_bytes"],
            "member types are checked before bounds"
        );
        let invalid = server
            .send_unchecked(merge_patch(
                path,
                &tag,
                &json!({ "piece_wait_seconds": 0, "idle_readers_total": 2 }),
            ))
            .await;
        assert_problem(
            &invalid,
            StatusCode::UNPROCESSABLE_ENTITY,
            "validation-failed",
        );
        assert_eq!(errors(&invalid, "pointer"), ["/piece_wait_seconds"]);
        let related = server
            .send(merge_patch(path, &tag, &json!({ "idle_readers_total": 2 })))
            .await;
        assert_eq!(
            errors(&related, "pointer"),
            ["/idle_readers_per_torrent"],
            "the default per-torrent limit, 4, exceeds the new total"
        );
        assert_problem(
            &server
                .send(merge_patch(path, "\"set-9\"", &json!({ "peer_limit": 60 })))
                .await,
            StatusCode::PRECONDITION_FAILED,
            "precondition-failed",
        );
        assert_eq!(
            server.get(path).await.header(header::ETAG),
            Some(tag.as_str())
        );
        assert_eq!(server.engine.settings(), Settings::DEFAULT);

        server
            .engine
            .set_readiness(Readiness::Unavailable { retry_after: None });
        assert_problem(
            &server
                .send(merge_patch(path, &tag, &json!({ "peer_limit": 60 })))
                .await,
            StatusCode::SERVICE_UNAVAILABLE,
            "engine-unavailable",
        );
        assert_eq!(
            server.get(path).await.status,
            StatusCode::OK,
            "reads are served"
        );
        server.engine.set_readiness(Readiness::Ready);

        server.app.store().fail_writes(true);
        assert_problem(
            &server
                .send(merge_patch(path, &tag, &json!({ "peer_limit": 60 })))
                .await,
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal-error",
        );
        server.app.store().fail_writes(false);
        assert_eq!(server.get(path).await.json()["peer_limit"], 50);
        assert_eq!(
            server.engine.settings(),
            Settings::DEFAULT,
            "nothing applied"
        );

        // Explicit zero and false, and a value set again keeps the tag.
        let changed = server
            .send(merge_patch(
                path,
                &tag,
                &json!({
                    "idle_timeout_seconds": 0,
                    "peer_limit": 60,
                    "background_downloads": false,
                }),
            ))
            .await;
        assert_eq!(changed.status, StatusCode::OK);
        assert_eq!(changed.json()["idle_timeout_seconds"], 0);
        assert_eq!(changed.json()["background_downloads"], false);
        assert!(!server.engine.settings().background_downloads);
        let changed_tag = changed.header(header::ETAG).unwrap().to_owned();
        assert_ne!(changed_tag, tag);
        assert_eq!(server.engine.settings().idle_timeout, Duration::ZERO);
        assert_eq!(server.engine.settings().peer_limit, 60);
        let again = server
            .send(merge_patch(
                path,
                &changed_tag,
                &json!({ "peer_limit": 60 }),
            ))
            .await;
        assert_eq!(again.header(header::ETAG), Some(changed_tag.as_str()));

        let server = server.restart().await;
        let restored = server.get(path).await;
        assert_eq!(restored.header(header::ETAG), Some(changed_tag.as_str()));
        assert_eq!(restored.json()["idle_timeout_seconds"], 0, "zero survives");
        assert_eq!(restored.json()["peer_limit"], 60);
        assert_eq!(
            restored.json()["background_downloads"],
            false,
            "false survives"
        );
        assert_eq!(
            server.engine.settings().peer_limit,
            60,
            "applied at startup"
        );
        assert!(!server.engine.settings().background_downloads);
        let reset = server
            .send(merge_patch(
                path,
                &changed_tag,
                &json!({ "peer_limit": null, "background_downloads": null }),
            ))
            .await;
        assert_eq!(
            reset.json()["peer_limit"],
            50,
            "null returns to the default"
        );
        assert_eq!(reset.json()["background_downloads"], true);
        assert!(server.engine.settings().background_downloads);
        assert_eq!(reset.json()["idle_timeout_seconds"], 0);
        server.stop().await;
    }

    /// SET-05 and SET-09 for the cache capacity: one below a torrent's
    /// piece length is refused, and a failed write leaves the engine's
    /// capacity as it was, whether the change grows or shrinks it.
    pub async fn cache_capacity_settings() {
        const MIB: u64 = 1 << 20;
        let server = TestServer::start().await;
        server.engine.set_longest_piece(16 * MIB);
        let path = "/api/v1/settings";
        let tag = server
            .get(path)
            .await
            .header(header::ETAG)
            .unwrap()
            .to_owned();
        let capacity = |bytes: u64| json!({ "cache_capacity_bytes": bytes });

        let refused = server
            .send(merge_patch(path, &tag, &capacity(8 * MIB)))
            .await;
        assert_problem(
            &refused,
            StatusCode::UNPROCESSABLE_ENTITY,
            "validation-failed",
        );
        assert_eq!(errors(&refused, "pointer"), ["/cache_capacity_bytes"]);
        let out_of_bounds = server
            .send_unchecked(merge_patch(path, &tag, &capacity((64 << 30) + 1)))
            .await;
        assert_eq!(errors(&out_of_bounds, "pointer"), ["/cache_capacity_bytes"]);
        assert_eq!(server.engine.settings(), Settings::DEFAULT);

        server.app.store().fail_writes(true);
        for bytes in [512 * MIB, 32 * MIB] {
            assert_problem(
                &server.send(merge_patch(path, &tag, &capacity(bytes))).await,
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal-error",
            );
            assert_eq!(
                server.engine.settings(),
                Settings::DEFAULT,
                "the engine keeps the capacity a write of {bytes} bytes failed to change"
            );
        }
        server.app.store().fail_writes(false);
        assert_eq!(
            server.get(path).await.header(header::ETAG),
            Some(tag.as_str())
        );

        let shrunk = server
            .send(merge_patch(path, &tag, &capacity(16 * MIB)))
            .await;
        assert_eq!(shrunk.status, StatusCode::OK);
        assert_eq!(shrunk.json()["cache_capacity_bytes"], 16 * MIB);
        assert_eq!(server.engine.settings().cache_capacity_bytes, 16 * MIB);
        let tag = shrunk.header(header::ETAG).unwrap().to_owned();
        let grown = server
            .send(merge_patch(path, &tag, &capacity(64 << 30)))
            .await;
        assert_eq!(grown.json()["cache_capacity_bytes"], 64_u64 << 30);
        assert_eq!(server.engine.settings().cache_capacity_bytes, 64 << 30);
        server.stop().await;
    }

    /// SET-03: stored settings that leave a setting unset, or hold a value
    /// the current bounds refuse, run with the defaults for it.
    pub async fn stored_settings() {
        let server = TestServer::start().await;
        let mut explicit = Explicit {
            piece_wait_seconds: Some(5),
            ..Explicit::default()
        };
        // An earlier release's value beyond the current maximum.
        explicit.peer_limit = Some(9_000);
        server
            .app
            .store()
            .set_settings(StoredSettings {
                explicit,
                version: 3,
            })
            .await
            .unwrap();
        let server = server.restart().await;
        let settings = server.get("/api/v1/settings").await.json();
        assert_eq!(settings["piece_wait_seconds"], 5);
        assert_eq!(
            settings["peer_limit"], 50,
            "out of bounds takes the default"
        );
        assert_eq!(
            settings["idle_readers_total"], 16,
            "unset takes the default"
        );
        assert_eq!(server.engine.settings().piece_wait, Duration::from_secs(5));
        server.stop().await;
    }

    /// PRL-03, PRL-07, PRL-08, and the preload operations' responses:
    /// creating, listing, reading, and canceling preloads, their limits,
    /// how ended ones stay listed, and the conflicts of a torrent that is
    /// paused or lacks metadata.
    pub async fn preloads() {
        let server = TestServer::start().await;
        let small = catalog::single_small();
        let created = server.upload(&small.metainfo().bytes, None).await;
        let torrent = server.id_of(&created);
        let file = &files_of(&server, created.json()["links"]["self"].as_str().unwrap()).await[0];
        let preloads = file["links"]["preloads"].as_str().unwrap().to_owned();
        assert_eq!(
            preloads,
            format!("{}/preloads", file["links"]["self"].as_str().unwrap())
        );

        let listed = server.get(&preloads).await;
        assert_eq!(listed.status, StatusCode::OK);
        assert_eq!(listed.header(header::CACHE_CONTROL), Some("no-cache"));
        assert_eq!(listed.json()["items"], json!([]));

        let preload = server
            .send(json_request(Method::POST, &preloads, &json!({})))
            .await;
        assert_eq!(preload.status, StatusCode::CREATED);
        let body = preload.json();
        let location = body["links"]["self"].as_str().unwrap().to_owned();
        assert_eq!(preload.header(header::LOCATION), Some(location.as_str()));
        assert!(body["id"].as_str().unwrap().starts_with("prl_"));
        assert_eq!(body["state"], "downloading");
        assert_eq!(body["offset_bytes"], Value::Null);
        assert_eq!(
            body["ranges"],
            json!([{ "start_bytes": 0, "length_bytes": small.files()[0].size_bytes }]),
            "a file that fits is covered whole"
        );
        assert_eq!(body["links"]["file"], file["links"]["self"]);
        let read = server.get(&location).await;
        assert_eq!(read.status, StatusCode::OK);
        assert_eq!(read.header(header::CACHE_CONTROL), Some("no-cache"));
        assert_eq!(read.json()["id"], body["id"]);
        let expires = |body: &Value| {
            let at = |member: &str| {
                chrono::DateTime::parse_from_rfc3339(body[member].as_str().unwrap()).unwrap()
            };
            (at("expires_at") - at("created_at")).num_seconds()
        };
        assert_eq!(expires(&body), 120, "the default time to live");

        // Refused requests.
        let invalid = server
            .send_unchecked(json_request(
                Method::POST,
                &preloads,
                &json!({ "offset_bytes": -1, "ttl_seconds": 5, "extra": true }),
            ))
            .await;
        assert_problem(
            &invalid,
            StatusCode::UNPROCESSABLE_ENTITY,
            "validation-failed",
        );
        let mut pointers = errors(&invalid, "pointer");
        pointers.sort();
        assert_eq!(pointers, ["/extra", "/offset_bytes", "/ttl_seconds"]);
        let past_end = server
            .send(json_request(
                Method::POST,
                &preloads,
                &json!({ "offset_bytes": small.files()[0].size_bytes }),
            ))
            .await;
        assert_eq!(errors(&past_end, "pointer"), ["/offset_bytes"]);
        assert_problem(
            &server
                .send_unchecked(
                    request(Method::POST, &preloads)
                        .header(header::CONTENT_TYPE, "application/json")
                        .body(b"{".to_vec())
                        .unwrap(),
                )
                .await,
            StatusCode::BAD_REQUEST,
            "malformed-request",
        );
        let plain = server
            .send_unchecked(
                request(Method::POST, &preloads)
                    .header(header::CONTENT_TYPE, "text/plain")
                    .body(b"{}".to_vec())
                    .unwrap(),
            )
            .await;
        assert_problem(
            &plain,
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "unsupported-media-type",
        );
        assert_eq!(plain.header(header::ACCEPT), Some("application/json"));
        assert_problem(
            &server
                .send_unchecked(
                    request(Method::POST, &preloads)
                        .header(header::CONTENT_TYPE, "application/json")
                        .body(vec![b' '; 20_000])
                        .unwrap(),
                )
                .await,
            StatusCode::PAYLOAD_TOO_LARGE,
            "content-too-large",
        );

        // At most 4 active preloads of a file; an ended one frees a place
        // and stays listed.
        let mut ids = vec![body["id"].as_str().unwrap().to_owned()];
        for ttl in [10, 600, 3600] {
            let created = server
                .send(json_request(
                    Method::POST,
                    &preloads,
                    &json!({ "ttl_seconds": ttl, "offset_bytes": 0 }),
                ))
                .await;
            assert_eq!(created.status, StatusCode::CREATED);
            assert_eq!(expires(&created.json()), ttl);
            ids.push(created.json()["id"].as_str().unwrap().to_owned());
        }
        let fifth = server
            .send(json_request(Method::POST, &preloads, &json!({})))
            .await;
        assert_problem(&fifth, StatusCode::CONFLICT, "too-many-preloads");
        server
            .engine
            .set_preload_state(&torrent, &ids[1], PreloadState::Expired, None);
        let expired = server.get(&format!("{preloads}/{}", ids[1])).await.json();
        assert_eq!(expired["state"], "expired");
        assert!(expired["ended_at"].is_string());
        assert_eq!(
            server
                .send(json_request(Method::POST, &preloads, &json!({})))
                .await
                .status,
            StatusCode::CREATED,
            "the expired preload no longer counts"
        );
        let listed = server.get(&preloads).await.json();
        assert_eq!(listed["items"].as_array().unwrap().len(), 5);
        assert_eq!(listed["items"][1]["id"], ids[1].as_str(), "creation order");

        // Canceling removes a preload at once.
        let canceled = format!("{preloads}/{}", ids[0]);
        assert_eq!(
            server.send(empty(Method::DELETE, &canceled)).await.status,
            StatusCode::NO_CONTENT
        );
        assert_problem(
            &server.get(&canceled).await,
            StatusCode::NOT_FOUND,
            "not-found",
        );
        assert_problem(
            &server.send(empty(Method::DELETE, &canceled)).await,
            StatusCode::NOT_FOUND,
            "not-found",
        );
        let unknown_file = preloads.replace(
            file["id"].as_str().unwrap(),
            "fil_aaaaaaaaaaaaaaaaaaaaaaaaaa",
        );
        assert_problem(
            &server.get(&unknown_file).await,
            StatusCode::NOT_FOUND,
            "not-found",
        );
        assert_problem(
            &server
                .send(json_request(Method::POST, &unknown_file, &json!({})))
                .await,
            StatusCode::NOT_FOUND,
            "not-found",
        );

        // Pausing fails the torrent's preloads; a paused torrent takes no
        // new one.
        let configuration = format!(
            "{}/configuration",
            created.json()["links"]["self"].as_str().unwrap()
        );
        let tag = server
            .get(&configuration)
            .await
            .header(header::ETAG)
            .unwrap()
            .to_owned();
        server
            .send(merge_patch(
                &configuration,
                &tag,
                &json!({ "desired_state": "paused" }),
            ))
            .await;
        let failed = server.get(&format!("{preloads}/{}", ids[2])).await.json();
        assert_eq!(failed["state"], "failed");
        assert_eq!(failed["failure"]["detail"], "The torrent was paused.");
        assert_problem(
            &server
                .send(json_request(Method::POST, &preloads, &json!({})))
                .await,
            StatusCode::CONFLICT,
            "lifecycle-conflict",
        );

        // Without metadata, a file's preloads are a lifecycle conflict.
        let magnet = server.create_magnet(&unknown_magnet(7)).await;
        let unlisted = format!(
            "{}/files/fil_aaaaaaaaaaaaaaaaaaaaaaaaaa/preloads",
            magnet.json()["links"]["self"].as_str().unwrap()
        );
        for exchange in [
            server.get(&unlisted).await,
            server
                .send(json_request(Method::POST, &unlisted, &json!({})))
                .await,
            server
                .get(&format!("{unlisted}/prl_aaaaaaaaaaaaaaaaaaaaaaaaaa"))
                .await,
            server
                .send(empty(
                    Method::DELETE,
                    &format!("{unlisted}/prl_aaaaaaaaaaaaaaaaaaaaaaaaaa"),
                ))
                .await,
        ] {
            assert_problem(&exchange, StatusCode::CONFLICT, "lifecycle-conflict");
        }

        // Engine faults and unavailability.
        let one = format!("{preloads}/{}", ids[2]);
        server.engine.inject_fault(Some("the engine failed"));
        for exchange in [
            server.get(&preloads).await,
            server
                .send(json_request(Method::POST, &preloads, &json!({})))
                .await,
            server.get(&one).await,
            server.send(empty(Method::DELETE, &one)).await,
        ] {
            assert_problem(
                &exchange,
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal-error",
            );
        }
        server.engine.inject_fault(None);
        server
            .engine
            .set_readiness(Readiness::Unavailable { retry_after: None });
        for exchange in [
            server.get(&preloads).await,
            server
                .send(json_request(Method::POST, &preloads, &json!({})))
                .await,
            server.get(&one).await,
            server.send(empty(Method::DELETE, &one)).await,
        ] {
            assert_problem(
                &exchange,
                StatusCode::SERVICE_UNAVAILABLE,
                "engine-unavailable",
            );
        }
        server.engine.set_readiness(Readiness::Ready);
        server.stop().await;
    }

    /// The device profile's progress of a file, or None.
    async fn device_progress(server: &TestServer, torrent: &str, file: &str) -> Option<Value> {
        let profiles = server.get("/api/v1/profiles").await.json();
        let device = profiles["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| item["kind"] == "device")
            .unwrap()["links"]["self"]
            .as_str()
            .unwrap()
            .to_owned();
        let torrent = torrent.rsplit('/').next().unwrap();
        let read = server
            .get(&format!("{device}/torrents/{torrent}/progress/{file}"))
            .await;
        (read.status == StatusCode::OK).then(|| read.json())
    }

    /// Waits until the viewing at `path` reports no open stream, as the
    /// server notices a closed connection, and returns it.
    async fn ended_viewing(server: &TestServer, path: &str) -> Value {
        for _ in 0..200 {
            let viewing = server.get(path).await.json();
            if viewing["streaming"] == false {
                return viewing;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("the viewing's stream did not end");
    }

    /// The film's files: the film and its notes.
    async fn film_files(server: &TestServer, path: &str) -> (Value, Value) {
        let files = files_of(server, path).await;
        let named = |name: &str| {
            files
                .iter()
                .find(|file| file["path"].as_array().unwrap().last().unwrap() == name)
                .unwrap()
                .clone()
        };
        (named("Film.mkv"), named("notes.txt"))
    }

    /// VWG-01 to VWG-08 and the viewing operations' responses: creating
    /// viewings, the estimate their content links' streams feed, reading,
    /// expiry, deletion, and an unread viewing's hold on a temporary
    /// torrent.
    pub async fn viewings() {
        let server = TestServer::start_with_seek_indexes(film_indexes()).await;
        let spec = film();
        server.engine.add_fixture(&spec);
        let created = server.upload(&spec.metainfo().bytes, None).await;
        let torrent = created.json()["links"]["self"].as_str().unwrap().to_owned();
        let (film, notes) = film_files(&server, &torrent).await;
        let film_path = film["links"]["self"].as_str().unwrap().to_owned();
        let viewings = film["links"]["viewings"].as_str().unwrap().to_owned();
        assert_eq!(viewings, format!("{film_path}/viewings"));

        // VWG-01: a new viewing has no estimate yet.
        let made = server.send(empty(Method::POST, &viewings)).await;
        assert_eq!(made.status, StatusCode::CREATED);
        let viewing = made.json();
        let path = viewing["links"]["self"].as_str().unwrap().to_owned();
        assert_eq!(made.header(header::LOCATION), Some(path.as_str()));
        let id = viewing["id"].as_str().unwrap();
        assert!(id.starts_with("vwg_"), "{id}");
        assert_eq!(viewing["position_seconds"], Value::Null);
        assert_eq!(viewing["last_active_at"], Value::Null);
        assert_eq!(viewing["streaming"], false);
        let content = viewing["links"]["content"].as_str().unwrap().to_owned();
        assert_eq!(content, format!("{film_path}/content?viewing_id={id}"));
        assert_problem(
            &server
                .send(empty(
                    Method::POST,
                    &format!("{torrent}/files/fil_aaaaaaaaaaaaaaaaaaaaaaaaaa/viewings"),
                ))
                .await,
            StatusCode::NOT_FOUND,
            "not-found",
        );
        let retrieving = server.create_magnet(&unknown_magnet(9)).await.json()["links"]["self"]
            .as_str()
            .unwrap()
            .to_owned();
        let without_metadata =
            format!("{retrieving}/files/fil_aaaaaaaaaaaaaaaaaaaaaaaaaa/viewings");
        for exchange in [
            server.send(empty(Method::POST, &without_metadata)).await,
            server.get(&format!("{without_metadata}/{id}")).await,
            server
                .send(empty(Method::DELETE, &format!("{without_metadata}/{id}")))
                .await,
        ] {
            assert_problem(&exchange, StatusCode::CONFLICT, "lifecycle-conflict");
        }

        // VWG-02: the stream's estimate, which saves nothing.
        let stream = server.open_range(&content, 0, 4 << 20).await;
        server.advance_clock(Duration::from_secs(10));
        server.app.infer_progress().await;
        let playing = server.get(&path).await;
        assert_eq!(playing.status, StatusCode::OK);
        let playing = playing.json();
        assert_eq!(playing["streaming"], true);
        let seconds = |viewing: &Value| viewing["position_seconds"].as_f64().unwrap();
        assert!((10.0..11.0).contains(&seconds(&playing)), "{playing}");
        assert_eq!(playing["duration_seconds"], 100.0);
        assert!(playing["last_active_at"].is_string());
        drop(stream);
        let ended = ended_viewing(&server, &path).await;
        assert!((10.0..11.0).contains(&seconds(&ended)), "{ended}");
        let film_id = film["id"].as_str().unwrap();
        assert_eq!(device_progress(&server, &torrent, film_id).await, None);

        // VWG-03: a file that does not play has no estimate.
        let notes_viewings = notes["links"]["viewings"].as_str().unwrap();
        let notes_viewing = server
            .send(empty(Method::POST, notes_viewings))
            .await
            .json();
        let notes_path = notes_viewing["links"]["self"].as_str().unwrap().to_owned();
        let notes_content = notes_viewing["links"]["content"]
            .as_str()
            .unwrap()
            .to_owned();
        assert_eq!(server.get(&notes_content).await.status, StatusCode::OK);
        server.app.infer_progress().await;
        let read = server.get(&notes_path).await.json();
        assert_eq!(read["position_seconds"], Value::Null);
        assert_eq!(read["duration_seconds"], Value::Null);
        assert_eq!(
            read["last_active_at"],
            Value::Null,
            "nothing streamed into it"
        );

        // VWG-04: a viewing_id must name a viewing of the file; HEAD ignores
        // it.
        let notes_id = notes_viewing["id"].as_str().unwrap();
        for unknown in [
            format!("{film_path}/content?viewing_id=vwg_aaaaaaaaaaaaaaaaaaaaaaaaaa"),
            format!("{film_path}/content?viewing_id={notes_id}"),
        ] {
            assert_problem(
                &server.get(&unknown).await,
                StatusCode::NOT_FOUND,
                "not-found",
            );
        }
        let head = server
            .send(empty(
                Method::HEAD,
                &format!("{film_path}/content?viewing_id=vwg_aaaaaaaaaaaaaaaaaaaaaaaaaa"),
            ))
            .await;
        assert_eq!(head.status, StatusCode::OK);
        assert_problem(
            &server.get(&format!("{viewings}/{notes_id}")).await,
            StatusCode::NOT_FOUND,
            "not-found",
        );
        assert_eq!(
            server.send_raw(empty(Method::GET, &viewings)).await.status,
            StatusCode::METHOD_NOT_ALLOWED,
            "viewings are not listed"
        );

        // VWG-06: deletion.
        let deleted = server.send(empty(Method::DELETE, &notes_path)).await;
        assert_eq!(deleted.status, StatusCode::NO_CONTENT);
        for exchange in [
            server.get(&notes_path).await,
            server.get(&notes_content).await,
            server.send(empty(Method::DELETE, &notes_path)).await,
        ] {
            assert_problem(&exchange, StatusCode::NOT_FOUND, "not-found");
        }

        // Engine faults and unavailability.
        server.engine.inject_fault(Some("the engine failed"));
        for exchange in [
            server.send(empty(Method::POST, &viewings)).await,
            server.get(&path).await,
            server.send(empty(Method::DELETE, &path)).await,
        ] {
            assert_problem(
                &exchange,
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal-error",
            );
        }
        server.engine.inject_fault(None);
        server
            .engine
            .set_readiness(Readiness::Unavailable { retry_after: None });
        for exchange in [
            server.send(empty(Method::POST, &viewings)).await,
            server.get(&path).await,
            server.send(empty(Method::DELETE, &path)).await,
        ] {
            assert_problem(
                &exchange,
                StatusCode::SERVICE_UNAVAILABLE,
                "engine-unavailable",
            );
        }
        server.engine.set_readiness(Readiness::Ready);

        // VWG-05: a day without activity expires a viewing.
        server.advance_clock(Duration::from_secs(24 * 3600));
        for exchange in [server.get(&path).await, server.get(&content).await] {
            assert_problem(&exchange, StatusCode::NOT_FOUND, "not-found");
        }

        // VWG-08: deleting the torrent removes its viewings.
        let kept = server.send(empty(Method::POST, &viewings)).await.json();
        let kept = kept["links"]["self"].as_str().unwrap().to_owned();
        assert_eq!(
            server.send(empty(Method::DELETE, &torrent)).await.status,
            StatusCode::NO_CONTENT
        );
        assert_problem(&server.get(&kept).await, StatusCode::NOT_FOUND, "not-found");

        // VWG-07: an unread viewing keeps its temporary torrent past the idle
        // time until it is read.
        let temporary = create_temporary(&server, &spec).await;
        assert_eq!(temporary.status, StatusCode::CREATED);
        let temporary = temporary.json()["links"]["self"]
            .as_str()
            .unwrap()
            .to_owned();
        server.engine.set_metadata_retrievable(
            torrplay_core::id::InfoHashV1::from_bytes(spec.metainfo().info_hash),
            true,
        );
        let (temporary_film, _) = film_files(&server, &temporary).await;
        let held = server
            .send(empty(
                Method::POST,
                temporary_film["links"]["viewings"].as_str().unwrap(),
            ))
            .await
            .json();
        let held_path = held["links"]["self"].as_str().unwrap().to_owned();
        let played = server
            .send(
                request(Method::GET, held["links"]["content"].as_str().unwrap())
                    .header(header::RANGE, "bytes=0-1048575")
                    .body(Vec::new())
                    .unwrap(),
            )
            .await;
        assert_eq!(played.status, StatusCode::PARTIAL_CONTENT);
        // The server drops the finished body just after the client has it.
        tokio::time::sleep(Duration::from_millis(100)).await;
        server.advance_clock(Duration::from_secs(700));
        server.app.expire_temporaries().await;
        assert_eq!(
            server.get(&temporary).await.status,
            StatusCode::OK,
            "an unread viewing holds it past the idle time"
        );
        for _ in 0..200 {
            let read = server.get(&held_path).await;
            if read.status == StatusCode::OK && read.json()["streaming"] == false {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        server.app.expire_temporaries().await;
        assert_problem(
            &server.get(&temporary).await,
            StatusCode::NOT_FOUND,
            "not-found",
        );
        server.stop().await;
    }

    /// LIB-01 to LIB-05: library members are set at creation and by merge
    /// patches, normalized, validated, cleared with null, and the kind is
    /// inferred from file names unless configured.
    pub async fn library() {
        let server = TestServer::start().await;
        let created = server
            .send(json_request(
                Method::POST,
                "/api/v1/torrents",
                &json!({
                    "magnet_uri": unknown_magnet(30),
                    "title": " Example   Film ",
                    "kind": "movie",
                    "year": 2026,
                    "description": "One\r\nTwo",
                    "categories": ["Movies", "Kids"],
                    "genres": ["Comedy"],
                }),
            ))
            .await;
        assert_eq!(created.status, StatusCode::CREATED);
        let torrent = created.json();
        let path = torrent["links"]["self"].as_str().unwrap().to_owned();
        assert_eq!(torrent["title"], "Example Film");
        assert_eq!(torrent["kind"], "movie", "configured before metadata");
        assert_eq!(torrent["year"], 2026);
        assert_eq!(torrent["description"], "One\nTwo");
        assert_eq!(torrent["categories"], json!(["Movies", "Kids"]));
        assert_eq!(torrent["genres"], json!(["Comedy"]));
        assert_eq!(torrent["poster"], Value::Null);
        assert_eq!(torrent["links"]["poster"], format!("{path}/poster"));
        let configuration = format!("{path}/configuration");
        let read = server.get(&configuration).await;
        assert_eq!(read.json()["title"], "Example Film");
        assert_eq!(read.json()["categories"], json!(["Movies", "Kids"]));
        let mut etag = read.header(header::ETAG).unwrap().to_owned();

        // A form takes one part per category and genre.
        let small = catalog::single_small();
        let file = |name, value: &str| (name, None, value.as_bytes().to_vec());
        let torrent_file = (
            "torrent_file",
            Some("application/x-bittorrent"),
            small.metainfo().bytes,
        );
        let uploaded = server
            .send(form(&[
                torrent_file.clone(),
                file("title", "Small"),
                file("kind", "other"),
                file("year", "1999"),
                file("description", "Tiny."),
                file("categories", "Docs"),
                file("categories", "Kids"),
                file("genres", "Drama"),
            ]))
            .await;
        assert_eq!(uploaded.status, StatusCode::CREATED);
        let small_torrent = uploaded.json();
        assert_eq!(small_torrent["categories"], json!(["Docs", "Kids"]));
        assert_eq!(small_torrent["genres"], json!(["Drama"]));
        assert_eq!(small_torrent["year"], 1999);
        let refused = server
            .send_unchecked(form(&[
                torrent_file,
                file("year", "19x9"),
                file("title", "a"),
                file("title", "b"),
            ]))
            .await;
        assert_problem(
            &refused,
            StatusCode::UNPROCESSABLE_ENTITY,
            "validation-failed",
        );
        assert_eq!(errors(&refused, "pointer"), ["/title", "/year"]);
        let refused = server
            .send_unchecked(json_request(
                Method::POST,
                "/api/v1/torrents",
                &json!({ "magnet_uri": unknown_magnet(31), "kind": "film" }),
            ))
            .await;
        assert_eq!(errors(&refused, "pointer"), ["/kind"]);

        // LIB-02: text is normalized, and the same values keep the ETag.
        let normalized = json!({
            "title": "Cafe\u{301}  Noir",
            "categories": [" TV/Anime ", "Kids"],
            "genres": ["Drama"],
            "description": "Line\r\nTwo  ",
        });
        let changed = server
            .send(merge_patch(&configuration, &etag, &normalized))
            .await;
        assert_eq!(changed.status, StatusCode::OK);
        let body = changed.json();
        assert_eq!(body["title"], "Café Noir");
        assert_eq!(body["categories"], json!(["TV/Anime", "Kids"]));
        assert_eq!(body["description"], "Line\nTwo");
        let new_etag = changed.header(header::ETAG).unwrap().to_owned();
        assert_ne!(new_etag, etag);
        etag = new_etag;
        let again = server
            .send(merge_patch(
                &configuration,
                &etag,
                &json!({ "title": "Café Noir", "categories": ["TV/Anime", "Kids"] }),
            ))
            .await;
        assert_eq!(again.header(header::ETAG), Some(etag.as_str()));
        assert_eq!(server.get(&path).await.json()["title"], "Café Noir");

        // LIB-03: every invalid member is named and nothing changes.
        let many: Vec<String> = (0..17).map(|n| format!("Genre {n}")).collect();
        let invalid = server
            .send_unchecked(merge_patch(
                &configuration,
                &etag,
                &json!({
                    "title": "   ",
                    "categories": ["A", "A "],
                    "genres": many,
                    "year": 1799,
                    "description": "a\u{0}b",
                    "desired_state": "paused",
                }),
            ))
            .await;
        assert_problem(
            &invalid,
            StatusCode::UNPROCESSABLE_ENTITY,
            "validation-failed",
        );
        assert_eq!(
            errors(&invalid, "pointer"),
            [
                "/categories/1",
                "/description",
                "/genres",
                "/title",
                "/year"
            ]
        );
        let long = server
            .send_unchecked(merge_patch(
                &configuration,
                &etag,
                &json!({ "categories": ["x".repeat(129)], "title": "t".repeat(1025) }),
            ))
            .await;
        assert_eq!(errors(&long, "pointer"), ["/categories/0", "/title"]);
        let unchanged = server.get(&configuration).await;
        assert_eq!(unchanged.header(header::ETAG), Some(etag.as_str()));
        assert_eq!(unchanged.json()["desired_state"], "started");

        // LIB-04: null returns each member to its default.
        let cleared = server
            .send(merge_patch(
                &configuration,
                &etag,
                &json!({
                    "title": null,
                    "kind": null,
                    "year": null,
                    "description": null,
                    "categories": null,
                    "genres": null,
                }),
            ))
            .await;
        assert_eq!(cleared.status, StatusCode::OK);
        let body = cleared.json();
        for member in ["title", "kind", "year", "description"] {
            assert_eq!(body[member], Value::Null, "{member}");
        }
        assert_eq!(body["categories"], json!([]));
        assert_eq!(body["genres"], json!([]));
        assert_ne!(cleared.header(header::ETAG), Some(etag.as_str()));
        assert_eq!(
            server.get(&path).await.json()["kind"],
            Value::Null,
            "no kind is known before metadata"
        );

        // LIB-05: a kind follows the video files' names unless configured.
        let media = catalog::media_names();
        let movie = server.upload(&media.metainfo().bytes, None).await.json();
        assert_eq!(movie["kind"], "movie");
        let many_files = catalog::many_files();
        let other = server
            .upload(&many_files.metainfo().bytes, None)
            .await
            .json();
        assert_eq!(other["kind"], "other");
        let show = episodes();
        server.engine.add_fixture(&show);
        let series = server.upload(&show.metainfo().bytes, None).await.json();
        assert_eq!(series["kind"], "series");
        let series_path = series["links"]["self"].as_str().unwrap().to_owned();
        let series_configuration = format!("{series_path}/configuration");
        let tag = server
            .get(&series_configuration)
            .await
            .header(header::ETAG)
            .unwrap()
            .to_owned();
        let configured = server
            .send(merge_patch(
                &series_configuration,
                &tag,
                &json!({ "kind": "movie" }),
            ))
            .await;
        assert_eq!(configured.json()["kind"], "movie");
        assert_eq!(server.get(&series_path).await.json()["kind"], "movie");
        let tag = configured.header(header::ETAG).unwrap().to_owned();
        server
            .send(merge_patch(
                &series_configuration,
                &tag,
                &json!({ "kind": null }),
            ))
            .await;
        assert_eq!(server.get(&series_path).await.json()["kind"], "series");

        // Library members and posters need no engine.
        server
            .engine
            .set_readiness(Readiness::Unavailable { retry_after: None });
        let tag = server
            .get(&series_configuration)
            .await
            .header(header::ETAG)
            .unwrap()
            .to_owned();
        let retitled = server
            .send(merge_patch(
                &series_configuration,
                &tag,
                &json!({ "title": "Show" }),
            ))
            .await;
        assert_eq!(retitled.status, StatusCode::OK);
        let poster = format!("{series_path}/poster");
        assert_eq!(
            server
                .send(put_poster(&poster, "image/png", PNG))
                .await
                .status,
            StatusCode::CREATED
        );
        let tag = retitled.header(header::ETAG).unwrap().to_owned();
        assert_problem(
            &server
                .send(merge_patch(
                    &series_configuration,
                    &tag,
                    &json!({ "title": "Show", "desired_state": "paused" }),
                ))
                .await,
            StatusCode::SERVICE_UNAVAILABLE,
            "engine-unavailable",
        );
        server.engine.set_readiness(Readiness::Ready);
        server.stop().await;
    }

    /// LIB-06: the torrents collection filters by categories, genres, kind,
    /// and info hash.
    pub async fn library_filters() {
        let server = TestServer::start().await;
        let create = |body: Value| {
            let server = &server;
            async move {
                let created = server
                    .send(json_request(Method::POST, "/api/v1/torrents", &body))
                    .await;
                assert_eq!(created.status, StatusCode::CREATED);
                created.json()["id"].as_str().unwrap().to_owned()
            }
        };
        let plain = create(json!({ "magnet_uri": unknown_magnet(32) })).await;
        let small = catalog::single_small();
        let docs = server
            .send(form(&[
                (
                    "torrent_file",
                    Some("application/x-bittorrent"),
                    small.metainfo().bytes,
                ),
                ("categories", None, b"Docs".to_vec()),
                ("categories", None, b"Kids".to_vec()),
                ("genres", None, b"Drama".to_vec()),
            ]))
            .await
            .json()["id"]
            .as_str()
            .unwrap()
            .to_owned();
        let media = catalog::media_names();
        let show = episodes();
        server.engine.add_fixture(&show);
        for spec in [&media, &show] {
            server.engine.set_metadata_retrievable(
                torrplay_core::id::InfoHashV1::from_bytes(spec.metainfo().info_hash),
                true,
            );
        }
        let movie = create(json!({
            "magnet_uri": magnet(&media),
            "categories": ["Movies"],
            "genres": ["Comedy", "Drama"],
        }))
        .await;
        let series = create(json!({
            "magnet_uri": magnet(&show),
            "categories": ["Kids"],
        }))
        .await;
        let movie_path = format!("/api/v1/torrents/{movie}");
        torrent_until(&server, &movie_path, "its kind", |torrent| {
            torrent["kind"] == "movie"
        })
        .await;
        let series_path = format!("/api/v1/torrents/{series}");
        torrent_until(&server, &series_path, "its kind", |torrent| {
            torrent["kind"] == "series"
        })
        .await;

        let movie_hash = hex(&media.metainfo().info_hash).to_uppercase();
        let cases: [(String, Vec<&String>); 13] = [
            ("category=Kids".into(), vec![&series, &docs]),
            ("category=%20Kids%20".into(), vec![&series, &docs]),
            (
                "category=Kids&category=Movies".into(),
                vec![&series, &movie, &docs],
            ),
            ("category=kids".into(), vec![]),
            ("uncategorized=true".into(), vec![&plain]),
            (
                "uncategorized=true&category=Movies".into(),
                vec![&movie, &plain],
            ),
            ("genre=Drama".into(), vec![&movie, &docs]),
            ("genre=Drama&category=Kids".into(), vec![&docs]),
            ("kind=series".into(), vec![&series]),
            ("kind=movie&kind=series".into(), vec![&series, &movie]),
            ("kind=other".into(), vec![&docs]),
            (format!("info_hash={movie_hash}"), vec![&movie]),
            ("category=Kids&page_size=1".into(), vec![&series, &docs]),
        ];
        for (query, expected) in cases {
            assert_eq!(
                listed_ids(&server, &format!("/api/v1/torrents?{query}")).await,
                expected.into_iter().cloned().collect::<Vec<_>>(),
                "{query}"
            );
        }
        let first = server
            .get("/api/v1/torrents?category=Kids&page_size=1")
            .await
            .json();
        let next = first["links"]["next"].as_str().unwrap();
        assert!(next.contains("category=Kids"), "{next}");
        let cursor = next.split("cursor=").nth(1).unwrap();
        let mismatched = server
            .get(&format!(
                "/api/v1/torrents?genre=Drama&page_size=1&cursor={cursor}"
            ))
            .await;
        assert_problem(&mismatched, StatusCode::BAD_REQUEST, "malformed-request");
        assert_eq!(errors(&mismatched, "parameter"), ["cursor"]);
        for (query, parameter) in [
            ("category=%20", "category"),
            ("genre=", "genre"),
            ("uncategorized=false", "uncategorized"),
            ("uncategorized=true&uncategorized=true", "uncategorized"),
            ("kind=film", "kind"),
            ("info_hash=xyz", "info_hash"),
        ] {
            let refused = server
                .send_unchecked(empty(Method::GET, &format!("/api/v1/torrents?{query}")))
                .await;
            assert_problem(&refused, StatusCode::BAD_REQUEST, "malformed-request");
            assert_eq!(errors(&refused, "parameter"), [parameter], "{query}");
        }
        let long = "x".repeat(129);
        let refused = server
            .send_unchecked(empty(
                Method::GET,
                &format!("/api/v1/torrents?category={long}"),
            ))
            .await;
        assert_eq!(errors(&refused, "parameter"), ["category"]);
        server.stop().await;
    }

    /// LIB-07 and LIB-08: a poster is uploaded, served with validators,
    /// replaced, and deleted, and invalid uploads are refused.
    pub async fn posters() {
        let server = TestServer::start().await;
        let small = catalog::single_small();
        let torrent = server.upload(&small.metainfo().bytes, None).await.json();
        let path = torrent["links"]["self"].as_str().unwrap().to_owned();
        let poster = torrent["links"]["poster"].as_str().unwrap().to_owned();
        let configuration_tag = server
            .get(&format!("{path}/configuration"))
            .await
            .header(header::ETAG)
            .unwrap()
            .to_owned();
        let state_tag = || async {
            server
                .get(&path)
                .await
                .header(header::ETAG)
                .unwrap()
                .to_owned()
        };
        let before = state_tag().await;
        for method in [Method::GET, Method::HEAD, Method::DELETE] {
            let missing = server.send(empty(method.clone(), &poster)).await;
            assert_eq!(missing.status, StatusCode::NOT_FOUND, "{method}");
        }

        let created = server.send(put_poster(&poster, "image/png", PNG)).await;
        assert_eq!(created.status, StatusCode::CREATED);
        assert_eq!(created.header(header::LOCATION), Some(poster.as_str()));
        let png_tag = created.header(header::ETAG).unwrap().to_owned();
        let torrent = server.get(&path).await.json();
        assert_eq!(torrent["poster"]["media_type"], "image/png");
        assert_eq!(torrent["poster"]["size_bytes"], PNG.len());
        assert_ne!(
            state_tag().await,
            before,
            "the torrent's state ETag follows"
        );
        assert_eq!(
            server
                .get(&format!("{path}/configuration"))
                .await
                .header(header::ETAG),
            Some(configuration_tag.as_str()),
            "the configuration's does not"
        );

        let served = server.get(&poster).await;
        assert_eq!(served.status, StatusCode::OK);
        assert_eq!(served.body, PNG);
        assert_eq!(served.header(header::CONTENT_TYPE), Some("image/png"));
        assert_eq!(served.header(header::ETAG), Some(png_tag.as_str()));
        assert_eq!(served.header(header::CACHE_CONTROL), Some("no-cache"));
        assert_eq!(
            served.header(header::X_CONTENT_TYPE_OPTIONS),
            Some("nosniff")
        );
        assert_eq!(
            served.header(header::CONTENT_SECURITY_POLICY),
            Some("sandbox")
        );
        let ranged = server
            .send(with_header(
                Method::GET,
                &poster,
                header::RANGE,
                "bytes=0-3",
            ))
            .await;
        assert_eq!(ranged.status, StatusCode::OK, "a range is ignored");
        assert_eq!(ranged.body, PNG);
        for method in [Method::GET, Method::HEAD] {
            let cached = server
                .send(with_header(
                    method.clone(),
                    &poster,
                    header::IF_NONE_MATCH,
                    &png_tag,
                ))
                .await;
            assert_eq!(cached.status, StatusCode::NOT_MODIFIED, "{method}");
        }
        let head = server.send(empty(Method::HEAD, &poster)).await;
        assert_eq!(head.status, StatusCode::OK);
        assert!(head.body.is_empty());
        assert_eq!(
            head.header(header::CONTENT_LENGTH),
            Some(PNG.len().to_string().as_str())
        );

        let replaced = server.send(put_poster(&poster, "image/jpeg", JPEG)).await;
        assert_eq!(replaced.status, StatusCode::NO_CONTENT);
        let jpeg_tag = replaced.header(header::ETAG).unwrap().to_owned();
        assert_ne!(jpeg_tag, png_tag);
        let served = server.get(&poster).await;
        assert_eq!(served.body, JPEG);
        assert_eq!(served.header(header::CONTENT_TYPE), Some("image/jpeg"));
        assert_eq!(served.header(header::ETAG), Some(jpeg_tag.as_str()));

        // LIB-08.
        let unsupported = server
            .send_unchecked(put_poster(&poster, "text/plain", PNG))
            .await;
        assert_problem(
            &unsupported,
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "unsupported-media-type",
        );
        assert!(
            unsupported
                .header(header::ACCEPT)
                .unwrap()
                .contains("image/png")
        );
        let mismatched = server.send(put_poster(&poster, "image/png", JPEG)).await;
        assert_problem(
            &mismatched,
            StatusCode::UNPROCESSABLE_ENTITY,
            "validation-failed",
        );
        assert_eq!(errors(&mismatched, "pointer"), [""]);
        // Refused from its declared length, before any of the body.
        let declared = ((1 << 20) + 1).to_string();
        let large = server
            .send_before_body_ends(
                Method::PUT,
                &poster,
                &[
                    ("Content-Type", "image/png"),
                    ("Content-Length", declared.as_str()),
                ],
                Vec::new(),
            )
            .await;
        assert_problem(&large, StatusCode::PAYLOAD_TOO_LARGE, "content-too-large");
        assert_problem(
            &server
                .send(put_poster(
                    "/api/v1/torrents/trr_aaaaaaaaaaaaaaaaaaaaaaaaaa/poster",
                    "image/png",
                    PNG,
                ))
                .await,
            StatusCode::NOT_FOUND,
            "not-found",
        );
        assert_eq!(
            server.get(&poster).await.body,
            JPEG,
            "refused uploads change nothing"
        );

        let deleted = server.send(empty(Method::DELETE, &poster)).await;
        assert_eq!(deleted.status, StatusCode::NO_CONTENT);
        assert_problem(
            &server.get(&poster).await,
            StatusCode::NOT_FOUND,
            "not-found",
        );
        assert_eq!(server.get(&path).await.json()["poster"], Value::Null);
        server.stop().await;
    }

    /// LIB-09: library members and posters survive a restart, and a poster
    /// goes with its torrent.
    pub async fn library_restart() {
        let server = TestServer::start().await;
        let small = catalog::single_small();
        let torrent = server.upload(&small.metainfo().bytes, None).await.json();
        let path = torrent["links"]["self"].as_str().unwrap().to_owned();
        let configuration = format!("{path}/configuration");
        let poster = format!("{path}/poster");
        let tag = server
            .get(&configuration)
            .await
            .header(header::ETAG)
            .unwrap()
            .to_owned();
        let library = json!({
            "title": "Small",
            "kind": "other",
            "year": 2000,
            "description": "Tiny.",
            "categories": ["Z", "A"],
            "genres": ["Drama"],
        });
        let changed = server
            .send(merge_patch(&configuration, &tag, &library))
            .await;
        let tag = changed.header(header::ETAG).unwrap().to_owned();
        let uploaded = server.send(put_poster(&poster, "image/png", PNG)).await;
        let poster_tag = uploaded.header(header::ETAG).unwrap().to_owned();
        let before = server.get(&path).await;

        let server = server.restart().await;
        let after = server.get(&path).await;
        assert_eq!(after.json(), before.json());
        assert_eq!(after.header(header::ETAG), before.header(header::ETAG));
        assert_eq!(
            server.get(&configuration).await.header(header::ETAG),
            Some(tag.as_str())
        );
        let served = server.get(&poster).await;
        assert_eq!(served.body, PNG);
        assert_eq!(served.header(header::ETAG), Some(poster_tag.as_str()));

        assert_eq!(
            server.send(empty(Method::DELETE, &path)).await.status,
            StatusCode::NO_CONTENT
        );
        let again = server.upload(&small.metainfo().bytes, None).await.json();
        assert_eq!(again["poster"], Value::Null);
        assert_eq!(again["categories"], json!([]));
        let again_poster = again["links"]["poster"].as_str().unwrap();
        assert_problem(
            &server.get(again_poster).await,
            StatusCode::NOT_FOUND,
            "not-found",
        );
        server.stop().await;
    }

    /// LIB-10: failed writes change neither library members nor posters.
    pub async fn library_store_failures() {
        let server = TestServer::start().await;
        let small = catalog::single_small();
        let torrent = server.upload(&small.metainfo().bytes, None).await.json();
        let path = torrent["links"]["self"].as_str().unwrap().to_owned();
        let configuration = format!("{path}/configuration");
        let poster = format!("{path}/poster");
        let tag = server
            .get(&configuration)
            .await
            .header(header::ETAG)
            .unwrap()
            .to_owned();
        server.send(put_poster(&poster, "image/png", PNG)).await;
        let before = server.get(&path).await.json();

        server.app.store().fail_writes(true);
        for exchange in [
            server
                .send(merge_patch(
                    &configuration,
                    &tag,
                    &json!({ "title": "New" }),
                ))
                .await,
            server.send(put_poster(&poster, "image/jpeg", JPEG)).await,
            server.send(empty(Method::DELETE, &poster)).await,
        ] {
            assert_problem(
                &exchange,
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal-error",
            );
        }
        server.app.store().fail_writes(false);
        assert_eq!(server.get(&path).await.json(), before);
        assert_eq!(
            server.get(&configuration).await.header(header::ETAG),
            Some(tag.as_str())
        );
        assert_eq!(server.get(&poster).await.body, PNG);
        server.stop().await;
    }

    /// SRC-02 to SRC-04 and SRC-08: searches match titles and names, rank
    /// their matches, tolerate typos on request, and page with cursors.
    pub async fn search() {
        let server = TestServer::start().await;
        let title = |title: &str| json!({ "title": title });
        let spring = create_with(&server, 50, title("Spring.2019")).await;
        let springtime = create_with(&server, 51, title("Springtime Walk")).await;
        let dotted = create_with(&server, 52, title("Sintel.2010.1080p")).await;
        let inside = create_with(&server, 53, title("Xsintel Trailer")).await;
        let spaced = create_with(&server, 54, title("Sintel 2010")).await;
        let accented = create_with(&server, 55, title("Élan Vital")).await;
        let media = catalog::media_names();
        let named = server.upload(&media.metainfo().bytes, None).await.json()["id"]
            .as_str()
            .unwrap()
            .to_owned();
        let searched = |query: &str| {
            let server = &server;
            let path = format!("/api/v1/torrents?{query}");
            async move { listed_ids(server, &path).await }
        };
        let cases: [(&str, Vec<&String>); 10] = [
            ("search=spring", vec![&spring, &springtime]),
            ("search=sintel", vec![&spaced, &dotted, &inside]),
            ("search=SINTEL%202010", vec![&spaced, &dotted]),
            ("search=sintel.2010", vec![&spaced, &dotted]),
            ("search=sintel%202014", vec![]),
            ("search=elan", vec![&accented]),
            ("search=media", vec![&named]),
            ("search=sintl", vec![]),
            ("search=sintl&fuzzy=true", vec![&spaced, &dotted]),
            ("search=2011&fuzzy=true", vec![]),
        ];
        for (query, expected) in cases {
            assert_eq!(
                searched(query).await,
                expected.into_iter().cloned().collect::<Vec<_>>(),
                "{query}"
            );
        }
        let typo = create_with(&server, 56, title("Sintle Demo")).await;
        assert_eq!(
            searched("search=sintel&fuzzy=true").await,
            [&spaced, &dotted, &inside, &typo].map(Clone::clone),
            "typo matches rank below exact ones"
        );
        assert_eq!(
            searched("search=%20%20").await.len(),
            8,
            "whitespace alone is no search"
        );

        // SRC-04: pages of a search, and its cursor elsewhere.
        assert_eq!(
            searched("search=sintel&page_size=1").await,
            [&spaced, &dotted, &inside].map(Clone::clone)
        );
        let first = server
            .get("/api/v1/torrents?search=sintel&page_size=1")
            .await
            .json();
        assert_eq!(first["total"], 3);
        let next = first["links"]["next"].as_str().unwrap();
        assert!(next.contains("search=sintel"), "{next}");
        let cursor = next.split("cursor=").nth(1).unwrap();
        for query in [
            format!("search=spring&page_size=1&cursor={cursor}"),
            format!("search=sintel&fuzzy=true&page_size=1&cursor={cursor}"),
            format!("page_size=1&cursor={cursor}"),
        ] {
            let reused = server.get(&format!("/api/v1/torrents?{query}")).await;
            assert_problem(&reused, StatusCode::BAD_REQUEST, "malformed-request");
            assert_eq!(errors(&reused, "parameter"), ["cursor"], "{query}");
        }

        // SRC-08.
        let long = "a".repeat(1025);
        for (query, parameter) in [
            (format!("search={long}"), "search"),
            ("search=a&search=b".to_owned(), "search"),
            ("search=a&fuzzy=maybe".to_owned(), "fuzzy"),
        ] {
            let refused = server
                .send_unchecked(empty(Method::GET, &format!("/api/v1/torrents?{query}")))
                .await;
            assert_problem(&refused, StatusCode::BAD_REQUEST, "malformed-request");
            assert_eq!(errors(&refused, "parameter"), [parameter]);
        }
        server.stop().await;
    }

    /// SRC-05 to SRC-07: facets ignore the menu filters and the search, the
    /// listing version follows every change, and pages answer conditional
    /// requests.
    pub async fn facets() {
        let server = TestServer::start().await;
        let movie = create_with(
            &server,
            60,
            json!({ "kind": "movie", "categories": ["Movies", "Kids"], "genres": ["Comedy"] }),
        )
        .await;
        let series = create_with(
            &server,
            61,
            json!({ "kind": "series", "categories": ["Kids"], "genres": ["Drama"] }),
        )
        .await;
        let other = create_with(&server, 62, json!({ "kind": "other" })).await;
        create_with(
            &server,
            63,
            json!({ "kind": "other", "categories": ["Äpfel", "apple", "Zebra"] }),
        )
        .await;
        let page = |query: &str| {
            let server = &server;
            let path = format!("/api/v1/torrents?{query}");
            async move { server.get(&path).await.json() }
        };
        let all = page("").await;
        assert_eq!(all["total"], 4);
        assert_eq!(
            all["facets"],
            json!({
                "total": 4,
                "uncategorized": 1,
                "categories": [
                    { "name": "Kids", "count": 2 },
                    { "name": "Movies", "count": 1 },
                    { "name": "Zebra", "count": 1 },
                    { "name": "apple", "count": 1 },
                    { "name": "Äpfel", "count": 1 },
                ],
                "genres": [
                    { "name": "Comedy", "count": 1 },
                    { "name": "Drama", "count": 1 },
                ],
                "version": all["facets"]["version"],
            })
        );
        let version = all["facets"]["version"].as_str().unwrap().to_owned();
        let narrowed = page("category=Movies&genre=Drama&uncategorized=true&search=zzz").await;
        assert_eq!(narrowed["total"], 0);
        assert_eq!(
            narrowed["facets"], all["facets"],
            "menu filters and search leave facets"
        );
        let movies = page("kind=movie").await;
        assert_eq!(movies["facets"]["total"], 1);
        assert_eq!(
            movies["facets"]["categories"],
            json!([{ "name": "Kids", "count": 1 }, { "name": "Movies", "count": 1 }])
        );
        assert_ne!(movies["facets"]["version"], version.as_str());

        // SRC-07: conditional requests.
        let read = server.get("/api/v1/torrents").await;
        let etag = read.header(header::ETAG).unwrap().to_owned();
        let cached = server
            .send(with_header(
                Method::GET,
                "/api/v1/torrents",
                header::IF_NONE_MATCH,
                &etag,
            ))
            .await;
        assert_eq!(cached.status, StatusCode::NOT_MODIFIED);
        assert_eq!(cached.header(header::ETAG), Some(etag.as_str()));
        let other_query = server
            .send(with_header(
                Method::GET,
                "/api/v1/torrents?page_size=1",
                header::IF_NONE_MATCH,
                &etag,
            ))
            .await;
        assert_eq!(
            other_query.status,
            StatusCode::OK,
            "another query, another ETag"
        );

        // SRC-06: every change moves the version.
        let current = || async {
            server.get("/api/v1/torrents").await.json()["facets"]["version"]
                .as_str()
                .unwrap()
                .to_owned()
        };
        let mut seen = vec![version];
        let configuration = format!("/api/v1/torrents/{other}/configuration");
        let tag = server
            .get(&configuration)
            .await
            .header(header::ETAG)
            .unwrap()
            .to_owned();
        let retitled = server
            .send(merge_patch(
                &configuration,
                &tag,
                &json!({ "title": "New" }),
            ))
            .await;
        let tag = retitled.header(header::ETAG).unwrap().to_owned();
        seen.push(current().await);
        server
            .send(put_poster(
                &format!("/api/v1/torrents/{movie}/poster"),
                "image/png",
                PNG,
            ))
            .await;
        seen.push(current().await);
        server
            .send(merge_patch(
                &configuration,
                &tag,
                &json!({ "desired_state": "paused" }),
            ))
            .await;
        seen.push(current().await);
        server
            .send(empty(Method::DELETE, &format!("/api/v1/torrents/{series}")))
            .await;
        seen.push(current().await);
        create_with(&server, 64, json!({})).await;
        seen.push(current().await);
        let distinct: HashSet<&String> = seen.iter().collect();
        assert_eq!(distinct.len(), seen.len(), "{seen:?}");
        assert_eq!(current().await, *seen.last().unwrap(), "and only then");
        let changed = server
            .send(with_header(
                Method::GET,
                "/api/v1/torrents",
                header::IF_NONE_MATCH,
                &etag,
            ))
            .await;
        assert_eq!(changed.status, StatusCode::OK);
        assert_ne!(changed.header(header::ETAG), Some(etag.as_str()));
        server.stop().await;
    }

    /// PRF-01 to PRF-04: profiles are created, renamed under preconditions,
    /// limited, unique by name, and carry avatars, and the device profile
    /// keeps its name and stays.
    pub async fn profiles() {
        let server = TestServer::start().await;
        let listed = server.get("/api/v1/profiles").await;
        assert_eq!(listed.status, StatusCode::OK);
        let names: Vec<(String, String)> = listed.json()["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|item| {
                (
                    item["name"].as_str().unwrap().to_owned(),
                    item["kind"].as_str().unwrap().to_owned(),
                )
            })
            .collect();
        assert_eq!(
            names,
            [
                ("Default".to_owned(), "person".to_owned()),
                ("Devices".to_owned(), "device".to_owned())
            ],
            "the default and device profiles are created at startup"
        );

        // The device profile keeps its name and stays.
        let device = device_profile(&server).await;
        let device_etag = server
            .get(&device)
            .await
            .header(header::ETAG)
            .unwrap()
            .to_owned();
        let renamed_device = server
            .send(merge_patch(&device, &device_etag, &json!({ "name": "TV" })))
            .await;
        assert_problem(&renamed_device, StatusCode::CONFLICT, "lifecycle-conflict");
        let unchanged = server
            .send(merge_patch(
                &device,
                &device_etag,
                &json!({ "name": "Devices" }),
            ))
            .await;
        assert_eq!(
            unchanged.status,
            StatusCode::OK,
            "its own name is no change"
        );
        assert_problem(
            &server.send(empty(Method::DELETE, &device)).await,
            StatusCode::CONFLICT,
            "lifecycle-conflict",
        );
        assert_eq!(
            server.get("/api/v1").await.json()["links"]["profiles"],
            "/api/v1/profiles"
        );
        let created = server
            .send(json_request(
                Method::POST,
                "/api/v1/profiles",
                &json!({ "name": "  Alex  " }),
            ))
            .await;
        assert_eq!(created.status, StatusCode::CREATED);
        let path = created.header(header::LOCATION).unwrap().to_owned();
        assert_eq!(created.json()["name"], "Alex");
        let etag = created.header(header::ETAG).unwrap().to_owned();
        let read = server.get(&path).await;
        assert_eq!(read.header(header::ETAG), Some(etag.as_str()));
        let cached = server
            .send(with_header(
                Method::GET,
                &path,
                header::IF_NONE_MATCH,
                &etag,
            ))
            .await;
        assert_eq!(cached.status, StatusCode::NOT_MODIFIED);

        // Renaming needs the current ETag.
        let rename = json!({ "name": "Sam" });
        let body = serde_json::to_vec(&rename).unwrap();
        let missing = server
            .send_unchecked(patch(&path, None, &body, "application/merge-patch+json"))
            .await;
        assert_problem(
            &missing,
            StatusCode::PRECONDITION_REQUIRED,
            "precondition-required",
        );
        let stale = server
            .send(merge_patch(&path, "\"prf-stale\"", &rename))
            .await;
        assert_problem(
            &stale,
            StatusCode::PRECONDITION_FAILED,
            "precondition-failed",
        );
        let renamed = server.send(merge_patch(&path, &etag, &rename)).await;
        assert_eq!(renamed.status, StatusCode::OK);
        assert_eq!(renamed.json()["name"], "Sam");
        let etag = renamed.header(header::ETAG).unwrap().to_owned();
        let wrong_type = server
            .send_unchecked(patch(&path, Some(&etag), &body, "application/json"))
            .await;
        assert_problem(
            &wrong_type,
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "unsupported-media-type",
        );
        for (body, pointer) in [
            (json!({ "name": null }), "/name"),
            (json!({}), ""),
            (json!({ "id": "x" }), "/id"),
        ] {
            let refused = server
                .send_unchecked(merge_patch(&path, &etag, &body))
                .await;
            assert_problem(
                &refused,
                StatusCode::UNPROCESSABLE_ENTITY,
                "validation-failed",
            );
            assert_eq!(errors(&refused, "pointer"), [pointer]);
        }
        let not_json = server
            .send_unchecked(patch(
                &path,
                Some(&etag),
                b"{",
                "application/merge-patch+json",
            ))
            .await;
        assert_problem(&not_json, StatusCode::BAD_REQUEST, "malformed-request");

        // PRF-02.
        let duplicate = server
            .send(json_request(
                Method::POST,
                "/api/v1/profiles",
                &json!({ "name": "Default " }),
            ))
            .await;
        assert_problem(&duplicate, StatusCode::CONFLICT, "duplicate-profile");
        assert_eq!(
            duplicate.json()["profile"],
            server.get("/api/v1/profiles").await.json()["items"][0]["links"]["self"]
        );
        let taken = server
            .send(merge_patch(&path, &etag, &json!({ "name": "Default" })))
            .await;
        assert_problem(&taken, StatusCode::CONFLICT, "duplicate-profile");
        for body in [
            json!({ "name": "" }),
            json!({ "name": "x".repeat(65) }),
            json!({ "name": 3 }),
            json!({}),
        ] {
            let refused = server
                .send_unchecked(json_request(Method::POST, "/api/v1/profiles", &body))
                .await;
            assert_problem(
                &refused,
                StatusCode::UNPROCESSABLE_ENTITY,
                "validation-failed",
            );
        }
        let not_json = server
            .send_unchecked(
                request(Method::POST, "/api/v1/profiles")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(b"{".to_vec())
                    .unwrap(),
            )
            .await;
        assert_problem(&not_json, StatusCode::BAD_REQUEST, "malformed-request");
        let form = server
            .send_unchecked(
                request(Method::POST, "/api/v1/profiles")
                    .header(header::CONTENT_TYPE, "text/plain")
                    .body(b"Alex".to_vec())
                    .unwrap(),
            )
            .await;
        assert_problem(
            &form,
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "unsupported-media-type",
        );
        let large = json!({ "name": "x".repeat(20_000) });
        for exchange in [
            server
                .send_unchecked(json_request(Method::POST, "/api/v1/profiles", &large))
                .await,
            server
                .send_unchecked(merge_patch(&path, &etag, &large))
                .await,
        ] {
            assert_problem(
                &exchange,
                StatusCode::PAYLOAD_TOO_LARGE,
                "content-too-large",
            );
        }
        for n in 3..=32 {
            let created = server
                .send(json_request(
                    Method::POST,
                    "/api/v1/profiles",
                    &json!({ "name": format!("P{n}") }),
                ))
                .await;
            assert_eq!(created.status, StatusCode::CREATED, "profile {n}");
        }
        let too_many = server
            .send(json_request(
                Method::POST,
                "/api/v1/profiles",
                &json!({ "name": "P33" }),
            ))
            .await;
        assert_problem(&too_many, StatusCode::CONFLICT, "too-many-profiles");
        let first = server.get("/api/v1/profiles?page_size=1").await.json();
        assert!(first["links"]["next"].is_string());
        let bad_page = server
            .send_unchecked(empty(Method::GET, "/api/v1/profiles?page_size=0"))
            .await;
        assert_problem(&bad_page, StatusCode::BAD_REQUEST, "malformed-request");

        // PRF-03: an avatar under a poster's rules.
        let avatar = format!("{path}/avatar");
        for method in [Method::GET, Method::HEAD, Method::DELETE] {
            assert_eq!(
                server.send(empty(method.clone(), &avatar)).await.status,
                StatusCode::NOT_FOUND,
                "{method}"
            );
        }
        let uploaded = server.send(put_poster(&avatar, "image/png", PNG)).await;
        assert_eq!(uploaded.status, StatusCode::CREATED);
        assert_eq!(uploaded.header(header::LOCATION), Some(avatar.as_str()));
        let tag = uploaded.header(header::ETAG).unwrap().to_owned();
        let profile = server.get(&path).await;
        assert_eq!(profile.json()["avatar"]["media_type"], "image/png");
        assert_ne!(
            profile.header(header::ETAG),
            Some(etag.as_str()),
            "the avatar is part of the profile"
        );
        let served = server.get(&avatar).await;
        assert_eq!(served.body, PNG);
        assert_eq!(
            served.header(header::X_CONTENT_TYPE_OPTIONS),
            Some("nosniff")
        );
        assert_eq!(
            served.header(header::CONTENT_SECURITY_POLICY),
            Some("sandbox")
        );
        for method in [Method::GET, Method::HEAD] {
            let cached = server
                .send(with_header(
                    method.clone(),
                    &avatar,
                    header::IF_NONE_MATCH,
                    &tag,
                ))
                .await;
            assert_eq!(cached.status, StatusCode::NOT_MODIFIED, "{method}");
        }
        assert_eq!(
            server.send(empty(Method::HEAD, &avatar)).await.status,
            StatusCode::OK
        );
        assert_eq!(
            server
                .send(put_poster(&avatar, "image/jpeg", JPEG))
                .await
                .status,
            StatusCode::NO_CONTENT
        );
        assert_problem(
            &server
                .send_unchecked(put_poster(&avatar, "text/plain", PNG))
                .await,
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "unsupported-media-type",
        );
        assert_problem(
            &server.send(put_poster(&avatar, "image/png", JPEG)).await,
            StatusCode::UNPROCESSABLE_ENTITY,
            "validation-failed",
        );
        let declared = ((1 << 20) + 1).to_string();
        let large = server
            .send_before_body_ends(
                Method::PUT,
                &avatar,
                &[
                    ("Content-Type", "image/png"),
                    ("Content-Length", declared.as_str()),
                ],
                Vec::new(),
            )
            .await;
        assert_problem(&large, StatusCode::PAYLOAD_TOO_LARGE, "content-too-large");
        assert_eq!(
            server.send(empty(Method::DELETE, &avatar)).await.status,
            StatusCode::NO_CONTENT
        );
        assert_eq!(server.get(&path).await.json()["avatar"], Value::Null);

        // Deleting, down to the last profile.
        let unknown = "/api/v1/profiles/prf_aaaaaaaaaaaaaaaaaaaaaaaaaa";
        for exchange in [
            server.get(unknown).await,
            server.send(empty(Method::DELETE, unknown)).await,
            server.send(merge_patch(unknown, &etag, &rename)).await,
            server
                .send(put_poster(&format!("{unknown}/avatar"), "image/png", PNG))
                .await,
        ] {
            assert_problem(&exchange, StatusCode::NOT_FOUND, "not-found");
        }
        let everyone: Vec<String> = listed_ids_of(&server, "/api/v1/profiles").await;
        let people: Vec<&String> = everyone
            .iter()
            .filter(|id| !device.ends_with(id.as_str()))
            .collect();
        assert_eq!(
            people.len(),
            32,
            "the device profile does not count toward the limit"
        );
        for (index, id) in people.iter().enumerate() {
            let deleted = server
                .send(empty(Method::DELETE, &format!("/api/v1/profiles/{id}")))
                .await;
            if index + 1 == people.len() {
                assert_problem(&deleted, StatusCode::CONFLICT, "lifecycle-conflict");
            } else {
                assert_eq!(deleted.status, StatusCode::NO_CONTENT);
            }
        }
        assert_problem(&server.get(&path).await, StatusCode::NOT_FOUND, "not-found");
        server.stop().await;
    }

    /// PRG-01 to PRG-04: progress is saved per profile and file, marks files
    /// viewed, is validated, and goes with its torrent and profile.
    pub async fn progress() {
        let server = TestServer::start().await;
        let default = default_profile(&server).await;
        let other = server
            .send(json_request(
                Method::POST,
                "/api/v1/profiles",
                &json!({ "name": "Sam" }),
            ))
            .await
            .header(header::LOCATION)
            .unwrap()
            .to_owned();
        let media = catalog::media_names();
        let torrent = server.upload(&media.metainfo().bytes, None).await.json();
        let torrent_path = torrent["links"]["self"].as_str().unwrap().to_owned();
        let torrent_id = torrent["id"].as_str().unwrap().to_owned();
        let files = file_ids(&server, &torrent_path).await;
        let mkv = &files["video.mkv"];
        let mp4 = &files["video.mp4"];
        let collection = format!("{default}/torrents/{torrent_id}/progress");
        let item = format!("{collection}/{mkv}");

        // PRG-01.
        assert_problem(&server.get(&item).await, StatusCode::NOT_FOUND, "not-found");
        let device = device_profile(&server).await;
        let device_item = format!("{device}/torrents/{torrent_id}/progress/{mkv}");
        assert_problem(
            &server
                .send(put_progress(&device_item, 600.5, 5400.0, false))
                .await,
            StatusCode::CONFLICT,
            "lifecycle-conflict",
        );
        assert_problem(
            &server.send(empty(Method::DELETE, &device_item)).await,
            StatusCode::NOT_FOUND,
            "not-found",
        );
        let created = server.send(put_progress(&item, 600.5, 5400.0, false)).await;
        assert_eq!(created.status, StatusCode::CREATED);
        assert_eq!(created.header(header::LOCATION), Some(item.as_str()));
        assert_eq!(created.json()["position_seconds"], 600.5);
        assert_eq!(
            created.json()["links"]["content"],
            format!("{torrent_path}/files/{mkv}/content")
        );
        let replaced = server.send(put_progress(&item, 900.0, 5400.0, false)).await;
        assert_eq!(replaced.status, StatusCode::OK);
        assert_eq!(server.get(&item).await.json()["position_seconds"], 900.0);
        let magnet = server.create_magnet(&unknown_magnet(70)).await.json()["id"]
            .as_str()
            .unwrap()
            .to_owned();
        let early = format!("{default}/torrents/{magnet}/progress");
        for exchange in [
            server.get(&early).await,
            server.get(&format!("{early}/{mkv}")).await,
            server
                .send(put_progress(&format!("{early}/{mkv}"), 1.0, 10.0, false))
                .await,
            server
                .send(empty(Method::DELETE, &format!("{early}/{mkv}")))
                .await,
        ] {
            assert_problem(&exchange, StatusCode::CONFLICT, "lifecycle-conflict");
        }
        let unknown_file = format!("{collection}/fil_aaaaaaaaaaaaaaaaaaaaaaaaaa");
        for exchange in [
            server.get(&unknown_file).await,
            server
                .send(put_progress(&unknown_file, 1.0, 10.0, false))
                .await,
            server.send(empty(Method::DELETE, &unknown_file)).await,
            server
                .get(&format!(
                    "{default}/torrents/trr_aaaaaaaaaaaaaaaaaaaaaaaaaa/progress"
                ))
                .await,
            server
                .get("/api/v1/profiles/prf_aaaaaaaaaaaaaaaaaaaaaaaaaa/torrents/x/progress")
                .await,
        ] {
            assert_problem(&exchange, StatusCode::NOT_FOUND, "not-found");
        }

        // PRG-02.
        let viewed = server
            .send(put_progress(&item, 4860.0, 5400.0, false))
            .await
            .json();
        assert_eq!(viewed["viewed"], true, "90% marks it viewed");
        assert_eq!(viewed["position_seconds"], 0.0);
        assert!(viewed["viewed_at"].is_string());
        let rewatching = server
            .send(put_progress(&item, 300.0, 5400.0, true))
            .await
            .json();
        assert_eq!(
            rewatching["position_seconds"], 300.0,
            "a viewed file keeps its position"
        );
        assert_eq!(rewatching["viewed_at"], viewed["viewed_at"]);
        let unmarked = server
            .send(put_progress(&item, 300.0, 5400.0, false))
            .await
            .json();
        assert_eq!(
            (unmarked["viewed"].clone(), unmarked["viewed_at"].clone()),
            (json!(false), Value::Null)
        );
        let marked = server
            .send(put_progress(
                &format!("{collection}/{mp4}"),
                50.0,
                100.0,
                true,
            ))
            .await
            .json();
        assert_eq!(
            (marked["viewed"].clone(), marked["position_seconds"].clone()),
            (json!(true), json!(0.0))
        );

        // PRG-03.
        for (body, pointers) in [
            (
                json!({ "position_seconds": 11, "duration_seconds": 10, "viewed": false }),
                vec!["/position_seconds"],
            ),
            (
                json!({ "position_seconds": 0, "duration_seconds": 0, "viewed": false }),
                vec!["/duration_seconds"],
            ),
            (
                json!({ "position_seconds": 1, "duration_seconds": 10, "viewed": false, "x": 1 }),
                vec!["/x"],
            ),
        ] {
            let refused = server
                .send_unchecked(json_request(Method::PUT, &item, &body))
                .await;
            assert_problem(
                &refused,
                StatusCode::UNPROCESSABLE_ENTITY,
                "validation-failed",
            );
            assert_eq!(errors(&refused, "pointer"), pointers);
        }
        let not_json = server
            .send_unchecked(
                request(Method::PUT, &item)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(b"{".to_vec())
                    .unwrap(),
            )
            .await;
        assert_problem(&not_json, StatusCode::BAD_REQUEST, "malformed-request");
        let text = server
            .send_unchecked(
                request(Method::PUT, &item)
                    .header(header::CONTENT_TYPE, "text/plain")
                    .body(b"1".to_vec())
                    .unwrap(),
            )
            .await;
        assert_problem(
            &text,
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "unsupported-media-type",
        );
        let large = json!({ "position_seconds": 1, "duration_seconds": 2, "viewed": false, "pad": "x".repeat(20_000) });
        assert_problem(
            &server
                .send_unchecked(json_request(Method::PUT, &item, &large))
                .await,
            StatusCode::PAYLOAD_TOO_LARGE,
            "content-too-large",
        );

        // PRG-04: per profile, listed in metadata order, deleted with owners.
        let sams = format!("{other}/torrents/{torrent_id}/progress/{mkv}");
        server.send(put_progress(&sams, 42.0, 5400.0, false)).await;
        assert_eq!(server.get(&item).await.json()["position_seconds"], 300.0);
        assert_eq!(server.get(&sams).await.json()["position_seconds"], 42.0);
        let page = server.get(&collection).await.json();
        let listed: Vec<&str> = page["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|item| item["file_id"].as_str().unwrap())
            .collect();
        assert_eq!(listed, [mkv.as_str(), mp4.as_str()], "metadata order");
        let paged = server
            .get(&format!("{collection}?page_size=1"))
            .await
            .json();
        assert!(paged["links"]["next"].is_string());
        let bad_page = server
            .send_unchecked(empty(Method::GET, &format!("{collection}?page_size=0")))
            .await;
        assert_problem(&bad_page, StatusCode::BAD_REQUEST, "malformed-request");
        assert_eq!(
            server.send(empty(Method::DELETE, &item)).await.status,
            StatusCode::NO_CONTENT
        );
        assert_problem(&server.get(&item).await, StatusCode::NOT_FOUND, "not-found");
        assert_eq!(
            server.send(empty(Method::DELETE, &other)).await.status,
            StatusCode::NO_CONTENT
        );
        assert_eq!(
            server
                .send(empty(Method::DELETE, &torrent_path))
                .await
                .status,
            StatusCode::NO_CONTENT
        );
        let again = server.upload(&media.metainfo().bytes, None).await.json();
        let again_id = again["id"].as_str().unwrap();
        let page = server
            .get(&format!("{default}/torrents/{again_id}/progress"))
            .await
            .json();
        assert_eq!(
            page["items"],
            json!([]),
            "a new torrent starts without progress"
        );
        server.stop().await;
    }

    /// UPN-01 to UPN-05: up next resumes files, moves a series to its next
    /// episode, honors dismissals, and answers conditional requests.
    pub async fn up_next() {
        let server = TestServer::start().await;
        let profile = default_profile(&server).await;
        let up_next = format!("{profile}/up-next");
        let media = catalog::media_names();
        let movies = server.upload(&media.metainfo().bytes, None).await.json();
        let movies_id = movies["id"].as_str().unwrap().to_owned();
        let movie_files = file_ids(&server, movies["links"]["self"].as_str().unwrap()).await;
        let show = series();
        server.engine.add_fixture(&show);
        let shows = server.upload(&show.metainfo().bytes, None).await.json();
        assert_eq!(shows["kind"], "series");
        let shows_id = shows["id"].as_str().unwrap().to_owned();
        let episodes = file_ids(&server, shows["links"]["self"].as_str().unwrap()).await;
        let progress =
            |torrent: &str, file: &str| format!("{profile}/torrents/{torrent}/progress/{file}");
        let entries = || async {
            server.get(&up_next).await.json()["items"]
                .as_array()
                .unwrap()
                .clone()
        };
        assert_eq!(entries().await, Vec::<Value>::new());

        // UPN-01: files of a movie torrent, above 1% only.
        server
            .send(put_progress(
                &progress(&movies_id, &movie_files["video.mkv"]),
                0.5,
                100.0,
                false,
            ))
            .await;
        server
            .send(put_progress(
                &progress(&movies_id, &movie_files["video.mp4"]),
                30.0,
                100.0,
                false,
            ))
            .await;
        let listed = entries().await;
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0]["reason"], "resume");
        assert_eq!(listed[0]["file"]["id"], movie_files["video.mp4"].as_str());
        assert_eq!(listed[0]["progress"]["position_seconds"], 30.0);
        assert_eq!(listed[0]["torrent"]["id"], movies_id.as_str());

        // UPN-02: viewing an episode moves its series to the next one.
        server
            .send(put_progress(
                &progress(&shows_id, &episodes["Series.S01E01.mkv"]),
                95.0,
                100.0,
                false,
            ))
            .await;
        let listed = entries().await;
        assert_eq!(listed.len(), 2);
        assert_eq!(listed[0]["reason"], "next_episode", "most recent first");
        assert_eq!(
            listed[0]["file"]["id"],
            episodes["Series.S01E02.mkv"].as_str()
        );
        assert_eq!(listed[0]["progress"], Value::Null);
        let series_entry = listed[0]["id"].as_str().unwrap().to_owned();
        let entry = server.get(&format!("{up_next}/{series_entry}")).await;
        assert_eq!(entry.status, StatusCode::OK);
        assert_eq!(entry.json(), listed[0]);

        // UPN-03: one entry per series, for its latest episode.
        server
            .send(put_progress(
                &progress(&shows_id, &episodes["Series.S01E02.mkv"]),
                10.0,
                100.0,
                false,
            ))
            .await;
        server
            .send(put_progress(
                &progress(&shows_id, &episodes["Series.S01E03.mkv"]),
                20.0,
                100.0,
                false,
            ))
            .await;
        let listed = entries().await;
        assert_eq!(listed.len(), 2);
        assert_eq!(
            (listed[0]["reason"].clone(), listed[0]["id"].clone()),
            (json!("resume"), json!(series_entry))
        );
        assert_eq!(
            listed[0]["file"]["id"],
            episodes["Series.S01E03.mkv"].as_str()
        );
        server
            .send(put_progress(
                &progress(&shows_id, &episodes["Series.S01E03.mkv"]),
                99.0,
                100.0,
                false,
            ))
            .await;
        let listed = entries().await;
        assert_eq!(listed.len(), 1, "the last episode viewed leaves no entry");

        // UPN-05: conditional requests.
        let read = server.get(&up_next).await;
        let etag = read.header(header::ETAG).unwrap().to_owned();
        let cached = server
            .send(with_header(
                Method::GET,
                &up_next,
                header::IF_NONE_MATCH,
                &etag,
            ))
            .await;
        assert_eq!(cached.status, StatusCode::NOT_MODIFIED);

        // UPN-04: dismissal, until progress returns.
        let movie_entry = listed[0]["id"].as_str().unwrap().to_owned();
        let entry_path = format!("{up_next}/{movie_entry}");
        assert_eq!(
            server.send(empty(Method::DELETE, &entry_path)).await.status,
            StatusCode::NO_CONTENT
        );
        assert_eq!(entries().await, Vec::<Value>::new());
        assert_problem(
            &server.send(empty(Method::DELETE, &entry_path)).await,
            StatusCode::NOT_FOUND,
            "not-found",
        );
        assert_problem(
            &server.get(&entry_path).await,
            StatusCode::NOT_FOUND,
            "not-found",
        );
        let changed = server
            .send(with_header(
                Method::GET,
                &up_next,
                header::IF_NONE_MATCH,
                &etag,
            ))
            .await;
        assert_eq!(
            changed.status,
            StatusCode::OK,
            "a dismissal changes the page"
        );
        server
            .send(put_progress(
                &progress(&movies_id, &movie_files["video.mp4"]),
                40.0,
                100.0,
                false,
            ))
            .await;
        let listed = entries().await;
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0]["id"], movie_entry.as_str(), "the same ID");

        assert_problem(
            &server
                .get("/api/v1/profiles/prf_aaaaaaaaaaaaaaaaaaaaaaaaaa/up-next")
                .await,
            StatusCode::NOT_FOUND,
            "not-found",
        );
        let bad_page = server
            .send_unchecked(empty(Method::GET, &format!("{up_next}?page_size=0")))
            .await;
        assert_problem(&bad_page, StatusCode::BAD_REQUEST, "malformed-request");
        server.stop().await;
    }

    /// PRF-04: a person profile that has the device profile's name keeps
    /// it, and no default profile is added beside it.
    pub async fn device_profile_name_taken() {
        let data = tempfile::TempDir::new().unwrap();
        {
            let (store, _) = torrplay_store::Store::open(data.path()).unwrap();
            store
                .insert_profile(&torrplay_core::profiles::Profile {
                    id: torrplay_core::id::ProfileId::from_random_bytes(
                        [7; torrplay_core::id::TORRENT_ID_RANDOM_BYTES],
                    ),
                    name: "Devices".into(),
                    kind: torrplay_core::profiles::ProfileKind::Person,
                    created_at: std::time::SystemTime::UNIX_EPOCH,
                    avatar: None,
                })
                .await
                .unwrap();
        }
        let server = TestServer::start_in(data).await;
        let listed = server.get("/api/v1/profiles").await.json();
        let names: Vec<(&str, &str)> = listed["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|item| {
                (
                    item["name"].as_str().unwrap(),
                    item["kind"].as_str().unwrap(),
                )
            })
            .collect();
        assert_eq!(names, [("Devices", "person"), ("Devices 2", "device")]);
        server.stop().await;
    }

    /// PRG-05 and PRF-04: profiles, avatars, progress, and dismissals
    /// survive a restart, and the device profile keeps its ID.
    pub async fn profiles_restart() {
        let server = TestServer::start().await;
        let profile = default_profile(&server).await;
        server
            .send(put_poster(&format!("{profile}/avatar"), "image/png", PNG))
            .await;
        let media = catalog::media_names();
        let torrent = server.upload(&media.metainfo().bytes, None).await.json();
        let torrent_id = torrent["id"].as_str().unwrap().to_owned();
        let files = file_ids(&server, torrent["links"]["self"].as_str().unwrap()).await;
        let item = format!(
            "{profile}/torrents/{torrent_id}/progress/{}",
            files["video.mkv"]
        );
        server.send(put_progress(&item, 30.0, 100.0, false)).await;
        let other = format!(
            "{profile}/torrents/{torrent_id}/progress/{}",
            files["video.mp4"]
        );
        server.send(put_progress(&other, 30.0, 100.0, false)).await;
        let up_next = format!("{profile}/up-next");
        let dismissed = server.get(&up_next).await.json()["items"][0]["id"]
            .as_str()
            .unwrap()
            .to_owned();
        server
            .send(empty(Method::DELETE, &format!("{up_next}/{dismissed}")))
            .await;
        let profile_before = server.get(&profile).await;
        let progress_before = server.get(&item).await.json();
        let up_next_before = server.get(&up_next).await.json();
        let avatar_before = server.get(&format!("{profile}/avatar")).await;

        let device = device_profile(&server).await;

        let server = server.restart().await;
        assert_eq!(
            device_profile(&server).await,
            device,
            "the device profile keeps its ID"
        );
        let profile_after = server.get(&profile).await;
        assert_eq!(profile_after.json(), profile_before.json());
        assert_eq!(
            profile_after.header(header::ETAG),
            profile_before.header(header::ETAG)
        );
        assert_eq!(server.get(&item).await.json(), progress_before);
        assert_eq!(
            server.get(&up_next).await.json(),
            up_next_before,
            "the dismissal holds"
        );
        let avatar_after = server.get(&format!("{profile}/avatar")).await;
        assert_eq!(avatar_after.body, avatar_before.body);
        assert_eq!(
            avatar_after.header(header::ETAG),
            avatar_before.header(header::ETAG)
        );
        assert_eq!(
            listed_ids_of(&server, "/api/v1/profiles").await.len(),
            2,
            "no second default or device profile"
        );
        server.stop().await;
    }

    /// PRG-08 to PRG-11 and the title progress operations' responses:
    /// saving, reading, listing with a prefix across pages, key validation,
    /// the device refusal, failures, and the file last played outliving
    /// its torrent across a restart.
    pub async fn title_progress() {
        let server = TestServer::start().await;
        let profile = default_profile(&server).await;
        let titles = format!("{profile}/title-progress");
        let at = |key: &str| format!("{titles}/{key}");
        let media = catalog::media_names();
        let torrent = server.upload(&media.metainfo().bytes, None).await.json();
        let torrent_id = torrent["id"].as_str().unwrap().to_owned();
        let torrent_path = torrent["links"]["self"].as_str().unwrap().to_owned();
        let video = file_ids(&server, &torrent_path).await["video.mkv"].clone();

        // PRG-08: saving, reading, and listing.
        let first = server
            .send(put_progress(&at("tmdb-tv-1399-s1e1"), 100.0, 2760.0, false))
            .await;
        assert_eq!(first.status, StatusCode::CREATED);
        assert_eq!(
            first.header(header::LOCATION),
            Some(at("tmdb-tv-1399-s1e1").as_str())
        );
        let episode = server
            .send(json_request(
                Method::PUT,
                &at("tmdb-tv-1399-s1e2"),
                &json!({
                    "position_seconds": 812.0,
                    "duration_seconds": 2760.0,
                    "viewed": false,
                    "torrent_id": torrent_id,
                    "file_id": video,
                }),
            ))
            .await;
        assert_eq!(episode.status, StatusCode::CREATED);
        let episode = episode.json();
        assert_eq!(episode["torrent_id"], torrent_id.as_str());
        assert_eq!(
            episode["links"]["file"],
            format!("{torrent_path}/files/{video}").as_str()
        );
        server
            .send(put_progress(&at("tmdb-movie-603"), 10.0, 100.0, false))
            .await;
        let viewed = server
            .send(put_progress(
                &at("tmdb-tv-1399-s1e1"),
                2700.0,
                2760.0,
                false,
            ))
            .await;
        assert_eq!(viewed.status, StatusCode::OK);
        assert_eq!(viewed.json()["viewed"], true, "90% marks it viewed");
        let read = server.get(&at("tmdb-tv-1399-s1e2")).await;
        assert_eq!(read.status, StatusCode::OK);
        assert_eq!(read.json()["position_seconds"], 812.0);

        let mut keys = Vec::new();
        let mut page = format!("{titles}?title_key_prefix=tmdb-tv-1399-&page_size=1");
        loop {
            let listed = server.get(&page).await;
            assert_eq!(listed.status, StatusCode::OK);
            let listed = listed.json();
            for item in listed["items"].as_array().unwrap() {
                keys.push(item["title_key"].as_str().unwrap().to_owned());
            }
            match listed["links"]["next"].as_str() {
                Some(next) => page = next.to_owned(),
                None => break,
            }
        }
        assert_eq!(
            keys,
            ["tmdb-tv-1399-s1e1", "tmdb-tv-1399-s1e2"],
            "the episodes, most recently saved first"
        );
        let all = server.get(&titles).await.json();
        assert_eq!(all["items"].as_array().unwrap().len(), 3);

        // PRG-09: keys, parameters, and bodies the operations refuse.
        for key in ["tmdb-tv-1399", "tmdb-movie-0603", "imdb-603"] {
            for exchange in [
                server.send_unchecked(empty(Method::GET, &at(key))).await,
                server
                    .send_unchecked(put_progress(&at(key), 1.0, 10.0, false))
                    .await,
                server.send_unchecked(empty(Method::DELETE, &at(key))).await,
            ] {
                assert_problem(&exchange, StatusCode::BAD_REQUEST, "malformed-request");
            }
        }
        for query in ["title_key_prefix=", "color=red", "cursor=x"] {
            assert_problem(
                &server
                    .send_unchecked(empty(Method::GET, &format!("{titles}?{query}")))
                    .await,
                StatusCode::BAD_REQUEST,
                "malformed-request",
            );
        }
        for body in [
            json!({ "position_seconds": 1, "duration_seconds": 10, "viewed": false, "torrent_id": torrent_id }),
            json!({ "position_seconds": 1, "duration_seconds": 10, "viewed": false, "torrent_id": torrent_id, "file_id": "fil_aaaaaaaaaaaaaaaaaaaaaaaaaa" }),
            json!({ "position_seconds": 11, "duration_seconds": 10, "viewed": false }),
        ] {
            assert_problem(
                &server
                    .send_unchecked(json_request(Method::PUT, &at("tmdb-movie-603"), &body))
                    .await,
                StatusCode::UNPROCESSABLE_ENTITY,
                "validation-failed",
            );
        }
        assert_problem(
            &server
                .send_unchecked(
                    request(Method::PUT, &at("tmdb-movie-603"))
                        .header(header::CONTENT_TYPE, "text/plain")
                        .body(b"{}".to_vec())
                        .unwrap(),
                )
                .await,
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "unsupported-media-type",
        );
        assert_problem(
            &server
                .send_unchecked(
                    request(Method::PUT, &at("tmdb-movie-603"))
                        .header(header::CONTENT_TYPE, "application/json")
                        .body(vec![b' '; 16385])
                        .unwrap(),
                )
                .await,
            StatusCode::PAYLOAD_TOO_LARGE,
            "content-too-large",
        );
        let unknown = "/api/v1/profiles/prf_aaaaaaaaaaaaaaaaaaaaaaaaaa/title-progress";
        for exchange in [
            server.get(unknown).await,
            server.get(&format!("{unknown}/tmdb-movie-603")).await,
            server
                .send(put_progress(
                    &format!("{unknown}/tmdb-movie-603"),
                    1.0,
                    10.0,
                    false,
                ))
                .await,
            server
                .send(empty(Method::DELETE, &format!("{unknown}/tmdb-movie-603")))
                .await,
            server.get(&at("tmdb-movie-604")).await,
            server
                .send(empty(Method::DELETE, &at("tmdb-movie-604")))
                .await,
        ] {
            assert_problem(&exchange, StatusCode::NOT_FOUND, "not-found");
        }

        // PRG-10: the device profile's title progress is never saved.
        let device = device_profile(&server).await;
        let device_title = format!("{device}/title-progress/tmdb-movie-603");
        assert_problem(
            &server
                .send(put_progress(&device_title, 1.0, 10.0, false))
                .await,
            StatusCode::CONFLICT,
            "lifecycle-conflict",
        );
        assert_problem(
            &server.get(&device_title).await,
            StatusCode::NOT_FOUND,
            "not-found",
        );

        // Failed writes change nothing.
        server.app.store().fail_writes(true);
        for exchange in [
            server
                .send(put_progress(&at("tmdb-movie-603"), 50.0, 100.0, false))
                .await,
            server
                .send(empty(Method::DELETE, &at("tmdb-movie-603")))
                .await,
        ] {
            assert_problem(
                &exchange,
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal-error",
            );
        }
        server.app.store().fail_writes(false);
        assert_eq!(
            server.get(&at("tmdb-movie-603")).await.json()["position_seconds"],
            10.0
        );

        // Deleting clears it.
        assert_eq!(
            server
                .send(empty(Method::DELETE, &at("tmdb-movie-603")))
                .await
                .status,
            StatusCode::NO_CONTENT
        );
        assert_problem(
            &server.get(&at("tmdb-movie-603")).await,
            StatusCode::NOT_FOUND,
            "not-found",
        );

        // PRG-11: the file last played outlives its torrent and a restart.
        assert_eq!(
            server
                .send(empty(Method::DELETE, &torrent_path))
                .await
                .status,
            StatusCode::NO_CONTENT
        );
        let server = server.restart().await;
        let kept = server.get(&at("tmdb-tv-1399-s1e2")).await;
        assert_eq!(kept.status, StatusCode::OK);
        let kept = kept.json();
        assert_eq!(kept["torrent_id"], torrent_id.as_str());
        assert_eq!(kept["file_id"], video.as_str());
        assert_eq!(kept["position_seconds"], 812.0);
        assert!(
            kept["links"].get("file").is_none() && kept["links"].get("content").is_none(),
            "{kept}"
        );

        server.app.begin_shutdown();
        for exchange in [
            server
                .send(put_progress(&at("tmdb-movie-605"), 1.0, 10.0, false))
                .await,
            server
                .send(empty(Method::DELETE, &at("tmdb-tv-1399-s1e2")))
                .await,
        ] {
            assert_problem(
                &exchange,
                StatusCode::SERVICE_UNAVAILABLE,
                "engine-unavailable",
            );
        }
        server.stop().await;
    }

    /// Adds `spec` as a temporary torrent, its metadata retrievable, and
    /// returns its path, its ID, and the path of the profile's progress of
    /// its video.
    async fn add_temporary(
        server: &TestServer,
        spec: &FixtureSpec,
        profile: &str,
    ) -> (String, String, String) {
        server.engine.set_metadata_retrievable(
            torrplay_core::id::InfoHashV1::from_bytes(spec.metainfo().info_hash),
            true,
        );
        let created = create_temporary(server, spec).await;
        assert_eq!(created.status, StatusCode::CREATED);
        let created = created.json();
        let path = created["links"]["self"].as_str().unwrap().to_owned();
        let id = created["id"].as_str().unwrap().to_owned();
        let video = file_ids(server, &path).await["video.mkv"].clone();
        let item = format!("{profile}/torrents/{id}/progress/{video}");
        (path, id, item)
    }

    /// TMP-09 to TMP-12: a temporary torrent's progress is kept by its info
    /// hash, so it outlives the torrent until `temporary_progress_seconds`
    /// pass, and promotion makes it ordinary progress.
    pub async fn temporary_progress() {
        let server = TestServer::start().await;
        let profile = default_profile(&server).await;
        let spec = catalog::media_names();

        // TMP-09: saving, reading, listing, and clearing.
        let (path, id, item) = add_temporary(&server, &spec, &profile).await;
        let saved = server.send(put_progress(&item, 30.0, 100.0, false)).await;
        assert_eq!(saved.status, StatusCode::CREATED);
        assert_eq!(server.get(&item).await.json()["position_seconds"], 30.0);
        let listed = server
            .get(&format!("{profile}/torrents/{id}/progress"))
            .await
            .json();
        assert_eq!(listed["items"].as_array().unwrap().len(), 1);
        assert_eq!(
            server.send(empty(Method::DELETE, &item)).await.status,
            StatusCode::NO_CONTENT
        );
        assert_problem(&server.get(&item).await, StatusCode::NOT_FOUND, "not-found");
        let device = device_profile(&server).await;
        let video = item.rsplit('/').next().unwrap();
        assert_problem(
            &server
                .send(put_progress(
                    &format!("{device}/torrents/{id}/progress/{video}"),
                    30.0,
                    100.0,
                    false,
                ))
                .await,
            StatusCode::CONFLICT,
            "lifecycle-conflict",
        );
        let up_next = format!("{profile}/up-next");
        server.send(put_progress(&item, 40.0, 100.0, false)).await;
        assert!(
            server.get(&up_next).await.json()["items"]
                .as_array()
                .unwrap()
                .is_empty(),
            "up next leaves temporary torrents out"
        );

        // TMP-10: the progress outlives the torrent.
        assert_eq!(
            server.send(empty(Method::DELETE, &path)).await.status,
            StatusCode::NO_CONTENT
        );
        let (path, id, item) = add_temporary(&server, &spec, &profile).await;
        assert_eq!(
            server.get(&item).await.json()["position_seconds"],
            40.0,
            "found again by info hash"
        );

        // TMP-11: promotion makes it ordinary progress, which a restart keeps.
        let configuration = format!("{path}/configuration");
        let tag = server
            .get(&configuration)
            .await
            .header(header::ETAG)
            .unwrap()
            .to_owned();
        let promoted = server
            .send(merge_patch(
                &configuration,
                &tag,
                &json!({ "retention": "persisted" }),
            ))
            .await;
        assert_eq!(promoted.status, StatusCode::OK);
        assert_eq!(server.get(&item).await.json()["position_seconds"], 40.0);
        let server = server.restart().await;
        assert_eq!(server.get(&item).await.json()["position_seconds"], 40.0);
        let entries = server.get(&up_next).await.json();
        assert_eq!(
            entries["items"][0]["torrent"]["id"],
            id.as_str(),
            "{entries}"
        );

        // TMP-12: after the retention, the progress is gone.
        assert_eq!(
            server.send(empty(Method::DELETE, &path)).await.status,
            StatusCode::NO_CONTENT
        );
        let (path, _, item) = add_temporary(&server, &spec, &profile).await;
        server.send(put_progress(&item, 50.0, 100.0, false)).await;
        assert_eq!(
            server.send(empty(Method::DELETE, &path)).await.status,
            StatusCode::NO_CONTENT
        );
        server.advance_clock(Duration::from_secs(2_592_000 - 60));
        server.app.expire_temporaries().await;
        let (path, _, item) = add_temporary(&server, &spec, &profile).await;
        assert_eq!(
            server.get(&item).await.json()["position_seconds"],
            50.0,
            "kept until the retention passes"
        );
        assert_eq!(
            server.send(empty(Method::DELETE, &path)).await.status,
            StatusCode::NO_CONTENT
        );
        server.advance_clock(Duration::from_secs(61));
        server.app.expire_temporaries().await;
        let (path, _, item) = add_temporary(&server, &spec, &profile).await;
        assert_problem(&server.get(&item).await, StatusCode::NOT_FOUND, "not-found");
        assert_eq!(
            server.send(empty(Method::DELETE, &path)).await.status,
            StatusCode::NO_CONTENT
        );
        let server = server.restart().await;
        let (_, _, item) = add_temporary(&server, &spec, &profile).await;
        assert_problem(&server.get(&item).await, StatusCode::NOT_FOUND, "not-found");
        server.stop().await;
    }

    /// PRG-06: failed writes change no profile, progress, or dismissal.
    pub async fn profiles_store_failures() {
        let server = TestServer::start().await;
        let profile = default_profile(&server).await;
        let media = catalog::media_names();
        let torrent = server.upload(&media.metainfo().bytes, None).await.json();
        let torrent_id = torrent["id"].as_str().unwrap().to_owned();
        let files = file_ids(&server, torrent["links"]["self"].as_str().unwrap()).await;
        let item = format!(
            "{profile}/torrents/{torrent_id}/progress/{}",
            files["video.mkv"]
        );
        server.send(put_progress(&item, 30.0, 100.0, false)).await;
        let avatar = format!("{profile}/avatar");
        server.send(put_poster(&avatar, "image/png", PNG)).await;
        let extra = server
            .send(json_request(
                Method::POST,
                "/api/v1/profiles",
                &json!({ "name": "Sam" }),
            ))
            .await
            .header(header::LOCATION)
            .unwrap()
            .to_owned();
        let up_next = format!("{profile}/up-next");
        let entry = server.get(&up_next).await.json()["items"][0]["id"]
            .as_str()
            .unwrap()
            .to_owned();
        let etag = server
            .get(&profile)
            .await
            .header(header::ETAG)
            .unwrap()
            .to_owned();
        let before = (
            server.get(&profile).await.json(),
            server.get(&item).await.json(),
            server.get(&up_next).await.json(),
            listed_ids_of(&server, "/api/v1/profiles").await,
        );

        server.app.store().fail_writes(true);
        for exchange in [
            server
                .send(json_request(
                    Method::POST,
                    "/api/v1/profiles",
                    &json!({ "name": "Kim" }),
                ))
                .await,
            server
                .send(merge_patch(&profile, &etag, &json!({ "name": "Alex" })))
                .await,
            server.send(empty(Method::DELETE, &extra)).await,
            server.send(put_poster(&avatar, "image/jpeg", JPEG)).await,
            server.send(empty(Method::DELETE, &avatar)).await,
            server.send(put_progress(&item, 60.0, 100.0, false)).await,
            server.send(empty(Method::DELETE, &item)).await,
            server
                .send(empty(Method::DELETE, &format!("{up_next}/{entry}")))
                .await,
        ] {
            assert_problem(
                &exchange,
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal-error",
            );
        }
        server.app.store().fail_writes(false);
        let after = (
            server.get(&profile).await.json(),
            server.get(&item).await.json(),
            server.get(&up_next).await.json(),
            listed_ids_of(&server, "/api/v1/profiles").await,
        );
        assert_eq!(after, before);
        server.stop().await;
    }

    /// AUT-01: without a password requests need no token, and nothing
    /// issues one; invalid bodies are refused first.
    pub async fn authentication_off() {
        let server = TestServer::start().await;
        assert_problem(
            &server.get("/api/v1/password").await,
            StatusCode::NOT_FOUND,
            "not-found",
        );
        let listed = server
            .send(bearer_empty(Method::GET, "/api/v1/torrents", "tpa_unknown"))
            .await;
        assert_eq!(listed.status, StatusCode::OK, "credentials are ignored");
        let sign_in = json!({ "password": PASSWORD, "name": "Laptop" });
        assert_problem(
            &server
                .send(json_request(
                    Method::POST,
                    "/api/v1/access-tokens",
                    &sign_in,
                ))
                .await,
            StatusCode::CONFLICT,
            "lifecycle-conflict",
        );
        assert_problem(
            &server
                .send(json_request(
                    Method::POST,
                    "/api/v1/playback-tokens",
                    &json!({}),
                ))
                .await,
            StatusCode::CONFLICT,
            "lifecycle-conflict",
        );
        let tokens = server.get("/api/v1/access-tokens").await;
        assert_eq!(tokens.json()["items"], json!([]));
        assert_problem(
            &server
                .send_unchecked(empty(Method::GET, "/api/v1/access-tokens?page_size=0"))
                .await,
            StatusCode::BAD_REQUEST,
            "malformed-request",
        );
        let unknown = "/api/v1/access-tokens/atk_aaaaaaaaaaaaaaaaaaaaaaaaaa";
        assert_problem(
            &server.get(unknown).await,
            StatusCode::NOT_FOUND,
            "not-found",
        );
        assert_problem(
            &server.send(empty(Method::DELETE, unknown)).await,
            StatusCode::NOT_FOUND,
            "not-found",
        );
        assert_problem(
            &server.send(empty(Method::DELETE, "/api/v1/password")).await,
            StatusCode::NOT_FOUND,
            "not-found",
        );

        for path in [
            "/api/v1/password",
            "/api/v1/access-tokens",
            "/api/v1/playback-tokens",
        ] {
            let method = if path == "/api/v1/password" {
                Method::PUT
            } else {
                Method::POST
            };
            let malformed = request(method.clone(), path)
                .header(header::CONTENT_TYPE, "application/json")
                .body(b"{".to_vec())
                .unwrap();
            assert_problem(
                &server.send_unchecked(malformed).await,
                StatusCode::BAD_REQUEST,
                "malformed-request",
            );
            let text = request(method.clone(), path)
                .header(header::CONTENT_TYPE, "text/plain")
                .body(b"{}".to_vec())
                .unwrap();
            let refused = server.send_unchecked(text).await;
            assert_problem(
                &refused,
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                "unsupported-media-type",
            );
            assert_eq!(refused.header(header::ACCEPT), Some("application/json"));
        }
        let invalid = [
            (
                Method::PUT,
                "/api/v1/password",
                json!({ "password": "short" }),
                "/password",
            ),
            (
                Method::PUT,
                "/api/v1/password",
                json!({ "password": PASSWORD, "current_password": PASSWORD }),
                "/current_password",
            ),
            (
                Method::PUT,
                "/api/v1/password",
                json!({ "password": PASSWORD, "revoke_access_tokens": "yes" }),
                "/revoke_access_tokens",
            ),
            (Method::PUT, "/api/v1/password", json!({}), "/password"),
            (
                Method::POST,
                "/api/v1/access-tokens",
                json!({ "password": PASSWORD, "name": " " }),
                "/name",
            ),
            (
                Method::POST,
                "/api/v1/access-tokens",
                json!({ "password": PASSWORD, "name": "TV", "scope": "admin" }),
                "/scope",
            ),
            (
                Method::POST,
                "/api/v1/access-tokens",
                json!({ "password": PASSWORD, "name": "TV", "expires_in_seconds": 60 }),
                "/expires_in_seconds",
            ),
            (
                Method::POST,
                "/api/v1/access-tokens",
                json!({ "name": "TV" }),
                "/password",
            ),
            (
                Method::POST,
                "/api/v1/playback-tokens",
                json!({ "expires_in_seconds": 59 }),
                "/expires_in_seconds",
            ),
            (
                Method::POST,
                "/api/v1/playback-tokens",
                json!({ "issuer": "me" }),
                "/issuer",
            ),
        ];
        for (method, path, body, member) in invalid {
            let refused = server
                .send_unchecked(json_request(method, path, &body))
                .await;
            assert_problem(
                &refused,
                StatusCode::UNPROCESSABLE_ENTITY,
                "validation-failed",
            );
            assert_eq!(errors(&refused, "pointer"), [member], "{body}");
        }
        server.stop().await;
    }

    /// AUT-02 and AUT-04 to AUT-07: a password turns authentication on;
    /// tokens grant by scope and torrent, and revoking them takes effect at
    /// once.
    pub async fn authentication_on() {
        let logs = LogCapture::start();
        let server = TestServer::start().await;
        let (torrent, content) = uploaded_content(&server, &catalog::single_small()).await;
        let (other_torrent, other_content) =
            uploaded_content(&server, &catalog::multi_boundary()).await;
        let poster = format!("/api/v1/torrents/{torrent}/poster");
        server.send(put_poster(&poster, "image/png", PNG)).await;
        set_first_password(&server).await;

        // AUT-02: every operation but the public ones needs a token.
        assert_unauthenticated(&server.get("/api/v1/torrents").await, None);
        assert_unauthenticated(&server.get("/api/v1/password").await, None);
        assert_unauthenticated(
            &server
                .send(bearer_empty(Method::GET, "/api/v1/torrents", "tpa_unknown"))
                .await,
            Some("invalid_token"),
        );
        assert_unauthenticated(
            &server
                .send(with_header(
                    Method::GET,
                    "/api/v1/torrents",
                    header::AUTHORIZATION,
                    "Basic dXNlcjpwYXNz",
                ))
                .await,
            Some("invalid_request"),
        );
        for public in [
            "/api/v1",
            "/api/v1/openapi.yaml",
            "/api/v1/reference",
            "/api/v1/reference/scalar.js",
            "/livez",
            "/readyz",
        ] {
            let accept = if public.ends_with(".yaml") {
                "application/yaml"
            } else if public.ends_with(".js") {
                "*/*"
            } else if public.ends_with("reference") {
                "text/html"
            } else {
                "application/json"
            };
            let answered = server
                .send(with_header(Method::GET, public, header::ACCEPT, accept))
                .await;
            assert_eq!(answered.status, StatusCode::OK, "{public}");
        }
        let wrong = server
            .send(json_request(
                Method::POST,
                "/api/v1/access-tokens",
                &json!({ "password": "not the password", "name": "Laptop" }),
            ))
            .await;
        assert_problem(&wrong, StatusCode::FORBIDDEN, "invalid-credentials");
        let (laptop, api) = sign_in(&server, "  Laptop  ", "api").await;
        assert_eq!(
            server
                .send(bearer_empty(Method::GET, "/api/v1/torrents", &api))
                .await
                .status,
            StatusCode::OK
        );
        let password = server
            .send(bearer_empty(Method::GET, "/api/v1/password", &api))
            .await;
        assert_eq!(password.status, StatusCode::OK);
        assert_eq!(password.header(header::CACHE_CONTROL), Some("no-cache"));
        let token_path = format!("/api/v1/access-tokens/{laptop}");
        let token = server
            .send(bearer_empty(Method::GET, &token_path, &api))
            .await;
        assert_eq!(token.json()["name"], "Laptop", "the name is normalized");
        assert_eq!(token.json()["scope"], "api");
        assert!(
            token.json().get("secret").is_none(),
            "only creation shows it"
        );
        let listed = server
            .send(bearer_empty(Method::GET, "/api/v1/access-tokens", &api))
            .await;
        assert_eq!(listed.json()["items"][0]["id"], laptop.as_str());

        // AUT-04: a change needs the current password.
        let change = |body: Value| bearer_json(Method::PUT, "/api/v1/password", &api, &body);
        let missing = server
            .send_unchecked(change(json!({ "password": "a new passphrase" })))
            .await;
        assert_problem(
            &missing,
            StatusCode::UNPROCESSABLE_ENTITY,
            "validation-failed",
        );
        assert_eq!(errors(&missing, "pointer"), ["/current_password"]);
        assert_problem(
            &server
                .send(change(json!({
                    "password": "a new passphrase",
                    "current_password": "a wrong one",
                })))
                .await,
            StatusCode::FORBIDDEN,
            "invalid-credentials",
        );
        let changed = server
            .send(change(json!({
                "password": PASSWORD,
                "current_password": PASSWORD,
            })))
            .await;
        assert_eq!(changed.status, StatusCode::OK);
        assert_eq!(
            server
                .send(bearer_empty(Method::GET, "/api/v1/torrents", &api))
                .await
                .status,
            StatusCode::OK,
            "a change keeps tokens unless it revokes them"
        );

        // AUT-05: a playback access token reads content and posters only.
        let (_, television) = sign_in(&server, "Living room TV", "playback").await;
        for path in [content.clone(), format!("{content}?token={television}")] {
            let mut streamed = request(Method::GET, &path);
            if !path.contains("token=") {
                streamed = streamed.header(header::AUTHORIZATION, format!("Bearer {television}"));
            }
            let streamed = server.send(streamed.body(Vec::new()).unwrap()).await;
            assert_eq!(streamed.status, StatusCode::OK, "{path}");
        }
        assert_eq!(
            server
                .send(empty(Method::HEAD, &format!("{poster}?token={television}")))
                .await
                .status,
            StatusCode::OK
        );
        let scoped = server
            .send(bearer_empty(Method::GET, "/api/v1/torrents", &television))
            .await;
        assert_problem(&scoped, StatusCode::FORBIDDEN, "insufficient-scope");
        assert!(
            scoped
                .header(header::WWW_AUTHENTICATE)
                .unwrap()
                .contains(r#"error="insufficient_scope""#)
        );
        assert_problem(
            &server
                .send(bearer_json(
                    Method::POST,
                    "/api/v1/playback-tokens",
                    &television,
                    &json!({}),
                ))
                .await,
            StatusCode::FORBIDDEN,
            "insufficient-scope",
        );

        // AUT-12: a metrics access token reads the metrics, the system
        // resource, and statistics, as a bearer token only.
        let (_, scraper) = sign_in(&server, "Prometheus", "metrics").await;
        for path in [
            "/metrics",
            "/api/v1/system",
            "/api/v1/statistics",
            "/api/v1/cache/statistics",
        ] {
            let read = server.send(bearer_empty(Method::GET, path, &scraper)).await;
            assert_eq!(read.status, StatusCode::OK, "{path}");
        }
        for path in [
            "/api/v1/torrents".to_owned(),
            format!("/api/v1/torrents/{torrent}/statistics"),
            content.clone(),
        ] {
            assert_problem(
                &server
                    .send(bearer_empty(Method::GET, &path, &scraper))
                    .await,
                StatusCode::FORBIDDEN,
                "insufficient-scope",
            );
        }
        assert_unauthenticated(
            &server.get(&format!("{content}?token={scraper}")).await,
            Some("invalid_token"),
        );
        assert_unauthenticated(&server.get("/metrics").await, None);
        assert_problem(
            &server
                .send(bearer_empty(Method::GET, "/metrics", &television))
                .await,
            StatusCode::FORBIDDEN,
            "insufficient-scope",
        );

        // AUT-06: a playback token of one torrent.
        let one = playback_token(&server, &api, Some(&torrent)).await;
        assert_eq!(
            server
                .send(empty(Method::GET, &format!("{content}?token={one}")))
                .await
                .status,
            StatusCode::OK
        );
        assert_problem(
            &server
                .send(empty(Method::GET, &format!("{other_content}?token={one}")))
                .await,
            StatusCode::FORBIDDEN,
            "insufficient-scope",
        );
        let every = playback_token(&server, &api, None).await;
        assert_eq!(
            server
                .send(empty(
                    Method::GET,
                    &format!("{other_content}?token={every}")
                ))
                .await
                .status,
            StatusCode::OK
        );
        let unknown_torrent = server
            .send_unchecked(bearer_json(
                Method::POST,
                "/api/v1/playback-tokens",
                &api,
                &json!({ "torrent_id": "trr_aaaaaaaaaaaaaaaaaaaaaaaaaa" }),
            ))
            .await;
        assert_problem(
            &unknown_torrent,
            StatusCode::UNPROCESSABLE_ENTITY,
            "validation-failed",
        );
        assert_eq!(errors(&unknown_torrent, "pointer"), ["/torrent_id"]);
        assert_eq!(other_torrent.len(), torrent.len());

        // AUT-07: an api token never travels in a URL, and a playback token
        // must be intact and current.
        assert_unauthenticated(
            &server
                .send(empty(Method::GET, &format!("{content}?token={api}")))
                .await,
            Some("invalid_token"),
        );
        let mut tampered = one.clone().into_bytes();
        let last = tampered.len() - 1;
        tampered[last] = if tampered[last] == b'a' { b'b' } else { b'a' };
        let tampered = String::from_utf8(tampered).unwrap();
        assert_unauthenticated(
            &server
                .send(empty(Method::GET, &format!("{content}?token={tampered}")))
                .await,
            Some("invalid_token"),
        );
        server.advance_clock(Duration::from_secs(86_400));
        assert_unauthenticated(
            &server
                .send(empty(Method::GET, &format!("{content}?token={one}")))
                .await,
            Some("invalid_token"),
        );

        // Revoking the issuer revokes its playback tokens.
        let fresh = playback_token(&server, &api, None).await;
        let (spare, spare_secret) = sign_in(&server, "Phone", "api").await;
        let revoked = server
            .send(bearer_empty(Method::DELETE, &token_path, &spare_secret))
            .await;
        assert_eq!(revoked.status, StatusCode::NO_CONTENT);
        assert_unauthenticated(
            &server
                .send(empty(Method::GET, &format!("{content}?token={fresh}")))
                .await,
            Some("invalid_token"),
        );
        assert_unauthenticated(
            &server
                .send(bearer_empty(Method::GET, "/api/v1/torrents", &api))
                .await,
            Some("invalid_token"),
        );
        assert_problem(
            &server
                .send(bearer_empty(Method::GET, &token_path, &spare_secret))
                .await,
            StatusCode::NOT_FOUND,
            "not-found",
        );
        assert_problem(
            &server
                .send(bearer_empty(Method::DELETE, &token_path, &spare_secret))
                .await,
            StatusCode::NOT_FOUND,
            "not-found",
        );

        // A change that revokes every token signs every client out.
        let revoking = server
            .send(bearer_json(
                Method::PUT,
                "/api/v1/password",
                &spare_secret,
                &json!({
                    "password": PASSWORD,
                    "current_password": PASSWORD,
                    "revoke_access_tokens": true,
                }),
            ))
            .await;
        assert_eq!(revoking.status, StatusCode::OK);
        for secret in [&spare_secret, &television] {
            assert_unauthenticated(
                &server
                    .send(bearer_empty(Method::GET, &content, secret))
                    .await,
                Some("invalid_token"),
            );
        }
        let _ = spare;

        // Removing the password turns authentication off.
        let (_, last) = sign_in(&server, "Laptop", "api").await;
        let removed = server
            .send(bearer_empty(Method::DELETE, "/api/v1/password", &last))
            .await;
        assert_eq!(removed.status, StatusCode::NO_CONTENT);
        assert_eq!(server.get("/api/v1/torrents").await.status, StatusCode::OK);
        assert_problem(
            &server.get("/api/v1/password").await,
            StatusCode::NOT_FOUND,
            "not-found",
        );
        set_first_password(&server).await;
        assert_unauthenticated(
            &server
                .send(bearer_empty(Method::GET, "/api/v1/torrents", &last))
                .await,
            Some("invalid_token"),
        );

        let text = logs.text();
        for secret in [&api, &television, &one, &last] {
            assert!(!text.contains(secret.as_str()), "a credential was logged");
        }
        assert!(!text.contains(PASSWORD), "the password was logged");
        server.stop().await;
    }

    /// AUT-03: wrong passwords make every password check wait.
    pub async fn sign_in_limits() {
        let server = TestServer::start().await;
        set_first_password(&server).await;
        let (_, secret) = sign_in(&server, "Laptop", "api").await;
        let wrong = json!({ "password": "not the password", "name": "Laptop" });
        for _ in 0..5 {
            assert_problem(
                &server
                    .send(json_request(Method::POST, "/api/v1/access-tokens", &wrong))
                    .await,
                StatusCode::FORBIDDEN,
                "invalid-credentials",
            );
        }
        let waiting = server
            .send(json_request(
                Method::POST,
                "/api/v1/access-tokens",
                &json!({ "password": PASSWORD, "name": "Laptop" }),
            ))
            .await;
        assert_problem(&waiting, StatusCode::TOO_MANY_REQUESTS, "too-many-attempts");
        assert_eq!(waiting.header(header::RETRY_AFTER), Some("1"));
        let change = server
            .send(bearer_json(
                Method::PUT,
                "/api/v1/password",
                &secret,
                &json!({ "password": PASSWORD, "current_password": PASSWORD }),
            ))
            .await;
        assert_problem(&change, StatusCode::TOO_MANY_REQUESTS, "too-many-attempts");
        tokio::time::sleep(Duration::from_millis(1100)).await;
        sign_in(&server, "Laptop", "api").await;
        server.stop().await;
    }

    /// The application keeps at most 64 access tokens, and drops expired
    /// ones when it creates another.
    pub async fn access_token_limits() {
        let server = TestServer::start().await;
        set_first_password(&server).await;
        let now = std::time::SystemTime::now();
        for n in 0..64_u8 {
            let token = torrplay_core::credentials::AccessToken {
                id: torrplay_core::id::AccessTokenId::from_random_bytes([n; 16]),
                name: format!("Device {n}"),
                scope: torrplay_core::credentials::Scope::Api,
                created_at: now,
                // One expires within the day.
                expires_at: (n == 0).then(|| now + Duration::from_secs(3600)),
                last_used_at: None,
                digest: torrplay_core::credentials::new_secret().unwrap().1,
            };
            server
                .app
                .store()
                .insert_access_token(&token, &[])
                .await
                .unwrap();
        }
        let server = server.restart().await;
        let sign_in_body = json!({ "password": PASSWORD, "name": "One too many" });
        assert_problem(
            &server
                .send(json_request(
                    Method::POST,
                    "/api/v1/access-tokens",
                    &sign_in_body,
                ))
                .await,
            StatusCode::CONFLICT,
            "too-many-access-tokens",
        );
        server.advance_clock(Duration::from_secs(7200));
        let (_, secret) = sign_in(&server, "Replacing an expired one", "api").await;
        let listed = server
            .send(bearer_empty(
                Method::GET,
                "/api/v1/access-tokens?page_size=200",
                &secret,
            ))
            .await;
        assert_eq!(listed.json()["items"].as_array().unwrap().len(), 64);
        let first = server
            .send(bearer_empty(
                Method::GET,
                "/api/v1/access-tokens?page_size=1",
                &secret,
            ))
            .await;
        let next = first.json()["links"]["next"].as_str().unwrap().to_owned();
        assert_eq!(
            server
                .send(bearer_empty(Method::GET, &next, &secret))
                .await
                .json()["items"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        server.stop().await;
    }

    /// AUT-09: the password and tokens survive a restart.
    pub async fn authentication_restart() {
        let server = TestServer::start().await;
        set_first_password(&server).await;
        let (_, secret) = sign_in(&server, "Laptop", "api").await;
        let token = playback_token(&server, &secret, None).await;
        let server = server.restart().await;
        assert_unauthenticated(&server.get("/api/v1/torrents").await, None);
        assert_eq!(
            server
                .send(bearer_empty(Method::GET, "/api/v1/torrents", &secret))
                .await
                .status,
            StatusCode::OK
        );
        let (_, content) = {
            let removed = server
                .send(bearer_empty(Method::DELETE, "/api/v1/password", &secret))
                .await;
            assert_eq!(removed.status, StatusCode::NO_CONTENT);
            uploaded_content(&server, &catalog::single_small()).await
        };
        let server = server.restart().await;
        set_first_password(&server).await;
        assert_unauthenticated(
            &server
                .send(empty(Method::GET, &format!("{content}?token={token}")))
                .await,
            Some("invalid_token"),
        );
        server.stop().await;
    }

    /// AUT-10: failed writes change no credential.
    pub async fn authentication_store_failures() {
        let server = TestServer::start().await;
        server.app.store().fail_writes(true);
        assert_problem(
            &server
                .send(json_request(
                    Method::PUT,
                    "/api/v1/password",
                    &json!({ "password": PASSWORD }),
                ))
                .await,
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal-error",
        );
        server.app.store().fail_writes(false);
        assert_problem(
            &server.get("/api/v1/password").await,
            StatusCode::NOT_FOUND,
            "not-found",
        );
        set_first_password(&server).await;
        let (laptop, secret) = sign_in(&server, "Laptop", "api").await;
        server.app.store().fail_writes(true);
        for exchange in [
            server
                .send(json_request(
                    Method::POST,
                    "/api/v1/access-tokens",
                    &json!({ "password": PASSWORD, "name": "Phone" }),
                ))
                .await,
            server
                .send(bearer_empty(
                    Method::DELETE,
                    &format!("/api/v1/access-tokens/{laptop}"),
                    &secret,
                ))
                .await,
            server
                .send(bearer_empty(Method::DELETE, "/api/v1/password", &secret))
                .await,
        ] {
            assert_problem(
                &exchange,
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal-error",
            );
        }
        server.app.store().fail_writes(false);
        let listed = server
            .send(bearer_empty(Method::GET, "/api/v1/access-tokens", &secret))
            .await;
        assert_eq!(listed.json()["items"].as_array().unwrap().len(), 1);
        assert_eq!(listed.json()["items"][0]["id"], laptop.as_str());
        server.stop().await;
    }

    /// Credential changes are refused once shutdown begins, and playback
    /// tokens for a torrent wait for the stored torrents.
    pub async fn authentication_unavailable() {
        let server = TestServer::start().await;
        let (torrent, _) = uploaded_content(&server, &catalog::single_small()).await;
        set_first_password(&server).await;
        let (laptop, secret) = sign_in(&server, "Laptop", "api").await;
        let server = server
            .restart_with(torrplay_core::fake::FakeEngine::with_catalog(), false)
            .await;
        assert_problem(
            &server
                .send(bearer_json(
                    Method::POST,
                    "/api/v1/playback-tokens",
                    &secret,
                    &json!({ "torrent_id": torrent }),
                ))
                .await,
            StatusCode::SERVICE_UNAVAILABLE,
            "engine-unavailable",
        );
        server.finish_restore().await;
        server.app.begin_shutdown();
        for exchange in [
            server
                .send(json_request(
                    Method::POST,
                    "/api/v1/access-tokens",
                    &json!({ "password": PASSWORD, "name": "Phone" }),
                ))
                .await,
            server
                .send(bearer_json(
                    Method::PUT,
                    "/api/v1/password",
                    &secret,
                    &json!({ "password": PASSWORD, "current_password": PASSWORD }),
                ))
                .await,
            server
                .send(bearer_empty(
                    Method::DELETE,
                    &format!("/api/v1/access-tokens/{laptop}"),
                    &secret,
                ))
                .await,
            server
                .send(bearer_empty(Method::DELETE, "/api/v1/password", &secret))
                .await,
        ] {
            assert_problem(
                &exchange,
                StatusCode::SERVICE_UNAVAILABLE,
                "engine-unavailable",
            );
        }
        server.stop().await;
    }

    /// Bodies of credential operations are bounded in size and time.
    pub async fn authentication_bodies() {
        let server = TestServer::start_with_body_timeout(Duration::from_millis(200)).await;
        set_first_password(&server).await;
        let (_, secret) = sign_in(&server, "Laptop", "api").await;
        let authorization = format!("Bearer {secret}");
        let partial = br#"{"password":"#.to_vec();
        let length = (partial.len() + 10).to_string();
        for (method, path) in [
            (Method::PUT, "/api/v1/password"),
            (Method::POST, "/api/v1/access-tokens"),
            (Method::POST, "/api/v1/playback-tokens"),
        ] {
            let headers = [
                ("Content-Type", "application/json"),
                ("Authorization", authorization.as_str()),
            ];
            let mut slow = headers.to_vec();
            slow.push(("Content-Length", length.as_str()));
            let timed_out = server
                .send_before_body_ends(method.clone(), path, &slow, partial.clone())
                .await;
            assert_problem(&timed_out, StatusCode::REQUEST_TIMEOUT, "request-timeout");
            let mut large = headers.to_vec();
            large.push(("Content-Length", "20000"));
            let refused = server
                .send_before_body_ends(method, path, &large, Vec::new())
                .await;
            assert_problem(&refused, StatusCode::PAYLOAD_TOO_LARGE, "content-too-large");
        }
        server.stop().await;
    }

    /// AUT-08: requests name an allowed host, except for the health checks.
    pub async fn allowed_hosts() {
        let server = TestServer::start_allowing(&["torrplay.home.arpa"]).await;
        let port = server.get("/api/v1").await.status;
        assert_eq!(port, StatusCode::OK);
        for host in [
            "192.168.1.10:8080",
            "localhost:8080",
            "mypc.local",
            "TorrPlay.home.arpa.",
        ] {
            let answered = server
                .send(with_header(Method::GET, "/api/v1", header::HOST, host))
                .await;
            assert_eq!(answered.status, StatusCode::OK, "{host}");
        }
        let refused = server
            .send(with_header(
                Method::GET,
                "/api/v1/torrents",
                header::HOST,
                "evil.example:8080",
            ))
            .await;
        assert_problem(
            &refused,
            StatusCode::MISDIRECTED_REQUEST,
            "misdirected-request",
        );
        assert!(
            refused.json()["detail"]
                .as_str()
                .unwrap()
                .contains("--allowed-host")
        );
        let live = server
            .send(with_header(
                Method::GET,
                "/livez",
                header::HOST,
                "evil.example",
            ))
            .await;
        assert_eq!(live.status, StatusCode::OK, "health checks name no host");
        server.stop().await;
    }

    async fn listed_len(server: &TestServer) -> usize {
        server.get("/api/v1/torrents").await.json()["items"]
            .as_array()
            .unwrap()
            .len()
    }
}

/// Responses no test can produce with the fake engine, with reasons.
const EXEMPT: &[(&str, u16)] = &[
    // These answer from constants and the registry without the engine;
    // only a failure to serialize their bodies could make them 500.
    ("getEntryPoint", 500),
    ("getOpenApiDocument", 500),
    // The page is a constant, and the script a bundle a unit test
    // decompresses, so only a failed worker could make them 500.
    ("getApiReference", 500),
    ("getApiReferenceScript", 500),
    ("getTorrentConfiguration", 500),
    ("getSettings", 500),
    // The run and the process's resources need no engine or store.
    ("getSystem", 500),
    // A process that answers is live.
    ("getLiveness", 503),
    // A failed read of the store, which tests cannot inject, and a HEAD,
    // which answers from the registry without the store.
    ("getTorrentPoster", 500),
    ("headTorrentPoster", 500),
    // These answer from the application's own records, as the entry point
    // does, or read an avatar from the store, which tests cannot fail.
    ("listProfiles", 500),
    ("getProfile", 500),
    ("getProfileAvatar", 500),
    ("headProfileAvatar", 500),
    ("listTorrentProgress", 500),
    ("getFileProgress", 500),
    ("listTitleProgress", 500),
    ("getTitleProgress", 500),
    ("listUpNext", 500),
    ("getUpNextEntry", 500),
    // These answer from the credentials in memory, and issuing a playback
    // token stores nothing, so only a failed serialization or signature,
    // which a key the store gave cannot cause, could make them 500.
    ("getPassword", 500),
    ("listAccessTokens", 500),
    ("getAccessToken", 500),
    ("createPlaybackToken", 500),
];

macro_rules! scenarios {
    ($($name:ident),* $(,)?) => {
        $(
            #[tokio::test]
            async fn $name() {
                scenario::$name().await;
            }
        )*

        /// API-03: every documented response is produced and checked.
        #[tokio::test]
        async fn every_documented_response_is_exercised() {
            $( scenario::$name().await; )*
            let contract = torrplay_contract::contract().unwrap();
            let unexercised = contract.unexercised(EXEMPT);
            assert!(unexercised.is_empty(), "no scenario produces {unexercised:?}");
        }
    };
}

scenarios!(
    service_endpoints,
    temporary_torrents,
    named_dht_and_log_settings,
    api_reference,
    magnet_creation,
    upload_creation,
    duplicates,
    rejected_sources,
    invalid_creation_bodies,
    torrent_pages,
    configuration_changes,
    methods_and_media_types,
    file_pages,
    content_ranges,
    content_types,
    engine_unavailable,
    deletion,
    statistics,
    observability,
    failures,
    passkey_redaction,
    bodies_refused_while_streaming,
    slow_bodies_time_out,
    engine_faults,
    restart,
    store_failures,
    restoring,
    refused_on_restore,
    settings,
    stored_settings,
    disk_creation,
    storage_transitions,
    cache_capacity_settings,
    preloads,
    viewings,
    title_progress,
    temporary_progress,
    library,
    library_filters,
    posters,
    library_restart,
    library_store_failures,
    search,
    facets,
    profiles,
    progress,
    up_next,
    device_profile_name_taken,
    profiles_restart,
    profiles_store_failures,
    authentication_off,
    authentication_on,
    sign_in_limits,
    access_token_limits,
    authentication_restart,
    authentication_store_failures,
    authentication_unavailable,
    authentication_bodies,
    allowed_hosts,
);

/// Serves the application's router as if every client had `address`, and
/// returns the base URL and the serving task.
async fn serve_as(
    server: &TestServer,
    address: &str,
) -> (String, tokio::task::JoinHandle<std::io::Result<()>>) {
    use axum::extract::connect_info::MockConnectInfo;
    let client: std::net::SocketAddr = address.parse().unwrap();
    let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let app = torrplay_server::router(std::sync::Arc::clone(&server.app))
        .layer(MockConnectInfo(client))
        .into_make_service();
    (
        base,
        tokio::spawn(std::future::IntoFuture::into_future(axum::serve(
            listener, app,
        ))),
    )
}

/// AUT-10: without a password, only local networks may use the operations
/// that need a credential once one is set; the entry point and health
/// checks answer everyone, and a password lets any client sign in.
#[tokio::test]
async fn without_a_password_only_local_networks_use_the_application() {
    let server = TestServer::start().await;
    let client = reqwest::Client::new();
    let status = |base: &str, method: reqwest::Method, path: &str| {
        let request = client.request(method, format!("{base}{path}"));
        async move {
            let response = request.send().await.unwrap();
            let status = response.status().as_u16();
            let bytes = response.bytes().await.unwrap();
            let body: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
            (status, body["type"].as_str().map(str::to_owned))
        }
    };
    let forbidden = Some("tag:skunkie.github.io,2026:torrplay/problems/forbidden".to_owned());

    let (public, public_task) = serve_as(&server, "203.0.113.7:50000").await;
    for (method, path) in [
        (reqwest::Method::GET, "/api/v1/torrents"),
        (reqwest::Method::GET, "/api/v1/password"),
        (reqwest::Method::PUT, "/api/v1/password"),
        (reqwest::Method::GET, "/api/v1/settings"),
    ] {
        assert_eq!(
            status(&public, method.clone(), path).await,
            (403, forbidden.clone()),
            "{method} {path}"
        );
    }
    for path in ["/api/v1", "/api/v1/openapi.yaml", "/livez"] {
        assert_eq!(
            status(&public, reqwest::Method::GET, path).await.0,
            200,
            "{path} answers everyone"
        );
    }

    let (overlay, overlay_task) = serve_as(&server, "[::ffff:100.101.102.103]:50000").await;
    assert_eq!(
        status(&overlay, reqwest::Method::GET, "/api/v1/torrents")
            .await
            .0,
        200,
        "a Tailscale client is local"
    );

    // With a password, any client may sign in and needs its credential.
    set_first_password(&server).await;
    assert_eq!(
        status(&public, reqwest::Method::GET, "/api/v1/torrents")
            .await
            .0,
        401
    );
    let signed_in = client
        .post(format!("{public}/api/v1/access-tokens"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(json!({ "password": PASSWORD, "name": "Phone" }).to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(signed_in.status().as_u16(), 201);

    for task in [public_task, overlay_task] {
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
    }
    server.stop().await;
}
