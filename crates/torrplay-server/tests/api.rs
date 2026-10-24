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

mod common;

use std::collections::HashSet;
use std::time::Duration;

use common::{
    BOUNDARY, Exchange, LogCapture, TestServer, form, form_request, json_request, request,
};
use http::{Method, StatusCode, header};
use serde_json::{Value, json};
use torrplay_core::engine::{CacheStatistics, Ratio, TorrentStatistics};
use torrplay_core::id::FileId;
use torrplay_core::lifecycle::{Readiness, StorageMode};
use torrplay_core::preload::PreloadState;
use torrplay_core::settings::{Explicit, Settings};
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
                "background_downloads": true,
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
    ("getTorrentConfiguration", 500),
    ("getSettings", 500),
    // A process that answers is live.
    ("getLiveness", 503),
    // A failed read of the store, which tests cannot inject, and a HEAD,
    // which answers from the registry without the store.
    ("getTorrentPoster", 500),
    ("headTorrentPoster", 500),
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
    library,
    library_filters,
    posters,
    library_restart,
    library_store_failures,
);
