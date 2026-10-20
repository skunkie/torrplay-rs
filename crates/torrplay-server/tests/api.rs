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

use common::{BOUNDARY, Exchange, TestServer, form, form_request, json_request, request};
use http::{Method, StatusCode, header};
use serde_json::{Value, json};
use torrplay_core::engine::{CacheStatistics, Ratio, TorrentStatistics};
use torrplay_core::id::FileId;
use torrplay_core::lifecycle::Readiness;
use torrplay_fixtures::{FixtureSpec, catalog, generator, hex};

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
        for exchange in exchanges {
            let text = String::from_utf8_lossy(&exchange.body);
            assert!(!text.contains(secret), "{text}");
            for value in exchange.headers.values() {
                assert!(!value.to_str().unwrap_or("").contains(secret));
            }
        }
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
        let head = |media_type: &str, framing: &str| {
            format!(
                "POST /api/v1/torrents HTTP/1.1\r\nHost: test\r\n\
Content-Type: {media_type}\r\n{framing}\r\n"
            )
        };
        // Chunked bodies that cross their limit and never end.
        let json_head = head("application/json", "Transfer-Encoding: chunked\r\n");
        let status = server
            .status_line_before_body_ends(&json_head, chunk(vec![b' '; 20 * 1024]))
            .await;
        assert_eq!(status, "HTTP/1.1 413 Payload Too Large");
        let form_head = head(
            &format!("multipart/form-data; boundary={BOUNDARY}"),
            "Transfer-Encoding: chunked\r\n",
        );
        let mut form = format!(
            "--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"torrent_file\"\r\n\r\n"
        )
        .into_bytes();
        form.resize(form.len() + 11 * 1024 * 1024, b'd');
        let status = server
            .status_line_before_body_ends(&form_head, chunk(form))
            .await;
        assert_eq!(status, "HTTP/1.1 413 Payload Too Large");
        // A declared length over the limit is refused before any body.
        let declared = head("application/json", "Content-Length: 20000\r\n");
        let status = server
            .status_line_before_body_ends(&declared, Vec::new())
            .await;
        assert_eq!(status, "HTTP/1.1 413 Payload Too Large");
        server.stop().await;
    }

    /// LIF-11: a restarted server has no torrents, and new IDs differ.
    pub async fn restart() {
        let metainfo = catalog::single_small().metainfo().bytes;
        let first = TestServer::start_with_random_ids().await;
        let before = first.upload(&metainfo, None).await.json()["id"].clone();
        first.stop().await;
        let second = TestServer::start_with_random_ids().await;
        let listed = second.get("/api/v1/torrents").await.json();
        assert!(listed["items"].as_array().unwrap().is_empty());
        let after = second.upload(&metainfo, None).await.json()["id"].clone();
        assert_ne!(after, before, "IDs from before the restart do not reappear");
        second.stop().await;
    }
}

/// Responses no test can produce with the fake engine, with reasons.
const EXEMPT: &[(&str, u16)] = &[
    // The fake engine has no unexpected failures to inject.
    ("createTorrent", 500),
    ("deleteTorrent", 500),
    ("getCacheStatistics", 500),
    ("getEntryPoint", 500),
    ("getOpenApiDocument", 500),
    ("getTorrent", 500),
    ("getTorrentConfiguration", 500),
    ("getTorrentFile", 500),
    ("getTorrentFileContent", 500),
    ("getTorrentStatistics", 500),
    ("headTorrentFileContent", 500),
    ("listTorrentFiles", 500),
    ("listTorrents", 500),
    ("updateTorrentConfiguration", 500),
    // A process that answers is live.
    ("getLiveness", 503),
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
    restart,
);
