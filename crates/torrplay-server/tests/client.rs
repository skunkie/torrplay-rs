// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! WEB-01 to WEB-03: the web client's files, served at the paths no route
//! of the application takes, as ADR 11 decides.

#![allow(clippy::expect_used, clippy::unwrap_used)]

#[allow(dead_code)]
mod common;

use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::Arc;

use common::{Exchange, TestServer, json_request, request};
use http::{Method, StatusCode, header};
use serde_json::json;
use sha2::{Digest, Sha256};
use torrplay_server::{ClientFile, ClientFiles};

const INDEX: &str = "<!doctype html><title>TorrPlay</title>";
const SCRIPT: &str = "console.log('client')";

/// A bundle of an `index.html`, a script Vite named by its content, and an
/// icon it did not.
fn bundle() -> ClientFiles {
    let files: HashMap<&'static str, &'static [u8]> = HashMap::from([
        ("index.html", INDEX.as_bytes()),
        ("assets/index-BQsKp6sj.js", SCRIPT.as_bytes()),
        ("favicon.svg", b"<svg/>".as_slice()),
    ]);
    Arc::new(move |path: &str| {
        files.get(path).map(|bytes| ClientFile {
            bytes: Cow::Borrowed(bytes),
            sha256: Sha256::digest(bytes).into(),
        })
    })
}

async fn get(server: &TestServer, path: &str) -> Exchange {
    send(server, Method::GET, path, None).await
}

async fn send(server: &TestServer, method: Method, path: &str, tag: Option<&str>) -> Exchange {
    let mut builder = request(method, path);
    if let Some(tag) = tag {
        builder = builder.header(header::IF_NONE_MATCH, tag);
    }
    // The client's paths are not the API's, so the contract has nothing to
    // check them against.
    server.send_raw(builder.body(Vec::new()).unwrap()).await
}

/// WEB-01: the bundle's files are served with their media types and
/// strong ETags, revalidated with `304`; files named by their content are
/// cached for a year, and the others revalidated.
#[tokio::test]
async fn files_are_served_with_their_types_and_tags() {
    let server = TestServer::start_with_client(bundle()).await;
    let script = get(&server, "/assets/index-BQsKp6sj.js").await;
    assert_eq!(script.status, StatusCode::OK);
    assert_eq!(script.body, SCRIPT.as_bytes());
    assert_eq!(
        script.header(header::CONTENT_TYPE),
        Some("text/javascript; charset=utf-8")
    );
    assert_eq!(
        script.header(header::CACHE_CONTROL),
        Some("public, max-age=31536000, immutable")
    );
    assert_eq!(
        script.header(header::X_CONTENT_TYPE_OPTIONS),
        Some("nosniff")
    );
    let tag = script.header(header::ETAG).expect("an ETag").to_owned();
    assert!(tag.starts_with("\"client-"), "{tag}");
    let unchanged = send(
        &server,
        Method::GET,
        "/assets/index-BQsKp6sj.js",
        Some(&tag),
    )
    .await;
    assert_eq!(unchanged.status, StatusCode::NOT_MODIFIED);
    assert!(unchanged.body.is_empty());
    assert_eq!(unchanged.header(header::ETAG), Some(tag.as_str()));
    let head = send(&server, Method::HEAD, "/assets/index-BQsKp6sj.js", None).await;
    assert_eq!(head.status, StatusCode::OK);
    assert!(head.body.is_empty());
    assert_eq!(
        head.header(header::CONTENT_LENGTH),
        Some(SCRIPT.len().to_string().as_str())
    );
    let icon = get(&server, "/favicon.svg").await;
    assert_eq!(icon.header(header::CONTENT_TYPE), Some("image/svg+xml"));
    assert_eq!(icon.header(header::CACHE_CONTROL), Some("no-cache"));
    server.stop().await;
}

/// WEB-02: every page path is answered with `index.html`, with the
/// client's content security policy; a missing file, a path the
/// application's interfaces own, and other methods are `not-found`.
#[tokio::test]
async fn page_paths_get_the_client_and_others_are_not_found() {
    let server = TestServer::start_with_client(bundle()).await;
    for path in [
        "/",
        "/library",
        "/titles/trr_7ndq4m2kzb6hxw3rjpt5yc4ula",
        "/settings/",
    ] {
        let page = get(&server, path).await;
        assert_eq!(page.status, StatusCode::OK, "{path}");
        assert_eq!(page.body, INDEX.as_bytes(), "{path}");
        assert_eq!(
            page.header(header::CONTENT_TYPE),
            Some("text/html; charset=utf-8"),
            "{path}"
        );
        assert_eq!(
            page.header(header::CACHE_CONTROL),
            Some("no-cache"),
            "{path}"
        );
        let policy = page
            .header(header::CONTENT_SECURITY_POLICY)
            .expect("a policy");
        assert!(policy.contains("frame-ancestors 'none'"), "{policy}");
    }
    for path in [
        "/assets/missing.js",
        "/api/v1/missing",
        "/api",
        "/dlna/missing",
        "/stremio/missing",
        "/.well-known/missing",
        "/livez/missing",
    ] {
        let missing = get(&server, path).await;
        assert_eq!(missing.status, StatusCode::NOT_FOUND, "{path}");
        assert_eq!(missing.problem(), "not-found", "{path}");
    }
    let posted = send(&server, Method::POST, "/library", None).await;
    assert_eq!(posted.status, StatusCode::NOT_FOUND);
    server.stop().await;
}

/// WEB-03: the client loads without a credential while a password is set,
/// since it signs in itself; without a bundle, page paths are `not-found`.
#[tokio::test]
async fn the_client_needs_no_credential_and_may_be_absent() {
    let server = TestServer::start_with_client(bundle()).await;
    let set = server
        .send(json_request(
            Method::PUT,
            "/api/v1/password",
            &json!({ "password": "correct horse battery staple" }),
        ))
        .await;
    assert_eq!(set.status, StatusCode::CREATED);
    assert_eq!(get(&server, "/").await.status, StatusCode::OK);
    assert_eq!(
        get(&server, "/assets/index-BQsKp6sj.js").await.status,
        StatusCode::OK
    );
    assert_eq!(
        server.get("/api/v1/torrents").await.status,
        StatusCode::UNAUTHORIZED,
        "the API still needs one"
    );
    server.stop().await;

    let without = TestServer::start().await;
    let page = get(&without, "/library").await;
    assert_eq!(page.status, StatusCode::NOT_FOUND);
    assert_eq!(page.problem(), "not-found");
    without.stop().await;
}
