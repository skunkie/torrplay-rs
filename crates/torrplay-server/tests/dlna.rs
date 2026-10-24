// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! The DLNA media server's resources over the fake engine, as
//! `docs/dlna.md` specifies them. They are outside the OpenAPI contract, so
//! these exchanges are checked here rather than against it.

// Scenarios and their helpers assert as tests do; clippy's test exemption
// covers only test functions.
#![allow(clippy::expect_used, clippy::unwrap_used)]

// Each test binary uses only part of the shared harness.
#[allow(dead_code)]
mod common;

use common::{Exchange, TestServer, form_request, json_request, request};
use http::{Method, StatusCode, header};
use serde_json::{Value, json};
use torrplay_fixtures::{FileSpec, FixtureSpec, Layout, catalog};

const CONTENT_DIRECTORY: &str = "urn:schemas-upnp-org:service:ContentDirectory:1";

/// Sets `dlna_enabled`.
async fn enable(server: &TestServer, on: bool) {
    let etag = server
        .get("/api/v1/settings")
        .await
        .header(header::ETAG)
        .unwrap()
        .to_owned();
    let patched = server
        .send(
            request(Method::PATCH, "/api/v1/settings")
                .header(header::CONTENT_TYPE, "application/merge-patch+json")
                .header(header::IF_MATCH, etag)
                .body(serde_json::to_vec(&json!({ "dlna_enabled": on })).unwrap())
                .unwrap(),
        )
        .await;
    assert_eq!(patched.status, StatusCode::OK, "{patched:?}");
    assert_eq!(patched.json()["dlna_enabled"], on);
}

/// Calls a ContentDirectory action with `arguments`.
async fn call(server: &TestServer, action: &str, arguments: &[(&str, &str)]) -> Exchange {
    let arguments: String = arguments
        .iter()
        .map(|(name, value)| format!("<{name}>{value}</{name}>"))
        .collect();
    let body = format!(
        r#"<?xml version="1.0"?><s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/"><s:Body><u:{action} xmlns:u="{CONTENT_DIRECTORY}">{arguments}</u:{action}></s:Body></s:Envelope>"#
    );
    server
        .send_raw(
            request(Method::POST, "/dlna/content-directory/control")
                .header(header::CONTENT_TYPE, r#"text/xml; charset="utf-8""#)
                .header("SOAPACTION", format!(r#""{CONTENT_DIRECTORY}#{action}""#))
                .body(body.into_bytes())
                .unwrap(),
        )
        .await
}

/// Browses `id`, returning the response's unescaped DIDL-Lite and counts.
async fn browse(server: &TestServer, id: &str, flag: &str) -> (String, String) {
    let browsed = call(
        server,
        "Browse",
        &[
            ("ObjectID", id),
            ("BrowseFlag", flag),
            ("Filter", "*"),
            ("StartingIndex", "0"),
            ("RequestedCount", "0"),
            ("SortCriteria", ""),
        ],
    )
    .await;
    assert_eq!(browsed.status, StatusCode::OK, "{}", text(&browsed));
    let body = text(&browsed);
    let didl = between(&body, "<Result>", "</Result>")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&");
    (didl, body)
}

fn text(exchange: &Exchange) -> String {
    String::from_utf8_lossy(&exchange.body).into_owned()
}

fn between<'a>(text: &'a str, start: &str, end: &str) -> &'a str {
    let from = text.find(start).map(|at| at + start.len()).unwrap_or(0);
    let to = text[from..].find(end).map_or(text.len(), |at| from + at);
    &text[from..to]
}

fn fault_code(exchange: &Exchange) -> String {
    assert_eq!(exchange.status, StatusCode::INTERNAL_SERVER_ERROR);
    between(&text(exchange), "<errorCode>", "</errorCode>").to_owned()
}

/// Uploads `spec` and returns the torrent's ID and its files by name.
async fn upload(server: &TestServer, spec: &FixtureSpec) -> (String, Vec<Value>) {
    let created = server.upload(&spec.metainfo().bytes, None).await;
    assert_eq!(created.status, StatusCode::CREATED);
    let id = created.json()["id"].as_str().unwrap().to_owned();
    let files = server
        .get(&format!("/api/v1/torrents/{id}/files?page_size=200"))
        .await
        .json()["items"]
        .as_array()
        .unwrap()
        .clone();
    (id, files)
}

fn file_id(files: &[Value], name: &str) -> String {
    files
        .iter()
        .find(|file| file["path"].as_array().unwrap().last().unwrap() == name)
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned()
}

/// A series whose episodes come out of order in its metadata.
fn series() -> FixtureSpec {
    FixtureSpec {
        id: "series".into(),
        torrent_name: "Series".into(),
        seed: 41,
        piece_length: 16 * 1024,
        layout: Layout::MultiFile {
            files: ["Series.S01E02.mkv", "Series.S01E01.mkv", "notes.txt"]
                .into_iter()
                .map(|name| FileSpec {
                    path: vec![name.into()],
                    size_bytes: 20_000,
                })
                .collect(),
        },
    }
}

async fn configure(server: &TestServer, id: &str, patch: Value) {
    let path = format!("/api/v1/torrents/{id}/configuration");
    let etag = server
        .get(&path)
        .await
        .header(header::ETAG)
        .unwrap()
        .to_owned();
    let patched = server
        .send(
            request(Method::PATCH, &path)
                .header(header::CONTENT_TYPE, "application/merge-patch+json")
                .header(header::IF_MATCH, etag)
                .body(serde_json::to_vec(&patch).unwrap())
                .unwrap(),
        )
        .await;
    assert_eq!(patched.status, StatusCode::OK, "{patched:?}");
}

/// DLNA-01: the media server answers only while it is on, with its stored
/// identity, and a password does not hide it.
#[tokio::test]
async fn the_media_server_answers_only_while_on() {
    let server = TestServer::start().await;
    let description = || async {
        server
            .send_raw(
                request(Method::GET, "/dlna/description.xml")
                    .body(Vec::new())
                    .unwrap(),
            )
            .await
    };
    assert_eq!(description().await.status, StatusCode::NOT_FOUND);
    enable(&server, true).await;
    let found = description().await;
    assert_eq!(found.status, StatusCode::OK);
    assert_eq!(
        found.header(header::CONTENT_TYPE),
        Some(r#"text/xml; charset="utf-8""#)
    );
    let body = text(&found);
    assert!(
        body.contains("<friendlyName>TorrPlay test</friendlyName>"),
        "{body}"
    );
    let udn = between(&body, "<UDN>", "</UDN>").to_owned();
    assert!(udn.starts_with("uuid:") && udn.len() == 41, "{udn}");
    for path in [
        "/dlna/content-directory.xml",
        "/dlna/connection-manager.xml",
    ] {
        let scpd = server
            .send_raw(request(Method::GET, path).body(Vec::new()).unwrap())
            .await;
        assert_eq!(scpd.status, StatusCode::OK, "{path}");
    }

    // A password protects the native API, not the media server.
    let set = server
        .send(json_request(
            Method::PUT,
            "/api/v1/password",
            &json!({ "password": "correct horse battery staple" }),
        ))
        .await;
    assert_eq!(set.status, StatusCode::CREATED);
    assert_eq!(description().await.status, StatusCode::OK);
    assert_eq!(
        server.get("/api/v1/torrents").await.status,
        StatusCode::UNAUTHORIZED
    );

    let server = server.restart().await;
    let again = server
        .send_raw(
            request(Method::GET, "/dlna/description.xml")
                .body(Vec::new())
                .unwrap(),
        )
        .await;
    assert!(text(&again).contains(&udn), "the UUID survives a restart");
    server.stop().await;
}

/// DLNA-03 to DLNA-05: browsing the root, a container, a torrent, and a
/// file; categories round-trip; series are in episode order and paused
/// torrents are hidden.
#[tokio::test]
async fn the_content_tree_browses() {
    let server = TestServer::start().await;
    let (media, media_files) = upload(&server, &catalog::media_names()).await;
    let show_spec = series();
    server.engine.add_fixture(&show_spec);
    let (show, show_files) = upload(&server, &show_spec).await;
    let paused = server
        .send(form_request(
            &catalog::single_small().metainfo().bytes,
            Some("paused"),
        ))
        .await;
    assert_eq!(paused.status, StatusCode::CREATED);
    let paused = paused.json()["id"].as_str().unwrap().to_owned();
    configure(
        &server,
        &media,
        json!({ "title": "Media & More", "categories": ["Films/1970s: Classics"] }),
    )
    .await;
    enable(&server, true).await;

    let (root, body) = browse(&server, "0", "BrowseDirectChildren").await;
    assert!(
        body.contains("<NumberReturned>6</NumberReturned><TotalMatches>6</TotalMatches>"),
        "{body}"
    );
    assert!(
        root.contains(r#"<container id="all" parentID="0" restricted="1" childCount="2""#),
        "{root}"
    );
    assert!(
        root.contains(r#"<container id="series" parentID="0" restricted="1" childCount="1""#),
        "{root}"
    );

    let (all, _) = browse(&server, "all", "BrowseDirectChildren").await;
    let newest = all.find(&format!("t:{show}")).unwrap();
    let older = all.find(&format!("t:{media}")).unwrap();
    assert!(newest < older, "newest first: {all}");
    assert!(!all.contains(&paused), "a paused torrent is hidden");
    assert!(
        all.contains("<dc:title>Media &amp; More</dc:title>"),
        "{all}"
    );

    let (categories, _) = browse(&server, "categories", "BrowseDirectChildren").await;
    let category = "category:Films%2F1970s%3A%20Classics";
    assert!(categories.contains(category), "{categories}");
    let (listed, _) = browse(&server, category, "BrowseDirectChildren").await;
    assert!(listed.contains(&format!("t:{media}")));
    assert_eq!(
        fault_code(
            &call(
                &server,
                "Browse",
                &[
                    ("ObjectID", "category:Films/1970s: Classics"),
                    ("BrowseFlag", "BrowseDirectChildren"),
                    ("StartingIndex", "0"),
                    ("RequestedCount", "0"),
                ],
            )
            .await
        ),
        "701",
        "a non-canonical encoding names nothing"
    );

    let (files, _) = browse(&server, &format!("t:{media}"), "BrowseDirectChildren").await;
    let video = file_id(&media_files, "video.mkv");
    for playable in ["video.mkv", "video.mp4", "audio.mp3", "image.png"] {
        assert!(
            files.contains(&format!("<dc:title>{playable}</dc:title>")),
            "{playable}"
        );
    }
    for hidden in ["subtitles.vtt", "page.html", "program.exe"] {
        assert!(!files.contains(hidden), "{hidden} is not playable");
    }
    let resource = format!("/dlna/media/{media}/{video}/video.mkv</res>");
    assert!(files.contains(&resource), "{files}");
    assert!(
        files.contains("http-get:*:video/x-matroska:DLNA.ORG_OP=01"),
        "{files}"
    );

    let (item, _) = browse(&server, &format!("f:{media}:{video}"), "BrowseMetadata").await;
    assert!(item.contains(&format!(
        r#"<item id="f:{media}:{video}" parentID="t:{media}""#
    )));

    let (episodes, _) = browse(&server, &format!("t:{show}"), "BrowseDirectChildren").await;
    let first = episodes
        .find(&file_id(&show_files, "Series.S01E01.mkv"))
        .unwrap();
    let second = episodes
        .find(&file_id(&show_files, "Series.S01E02.mkv"))
        .unwrap();
    assert!(first < second, "episodes in episode order: {episodes}");
    server.stop().await;
}

/// DLNA-06: media carries DLNA's headers and keeps the native streaming
/// behavior.
#[tokio::test]
async fn media_streams_with_dlna_headers() {
    let server = TestServer::start().await;
    let (media, files) = upload(&server, &catalog::media_names()).await;
    let video = file_id(&files, "video.mkv");
    enable(&server, true).await;
    let path = format!("/dlna/media/{media}/{video}/video.mkv");
    let get = |headers: &[(&str, &str)]| {
        let mut builder = request(Method::GET, &path);
        for (name, value) in headers {
            builder = builder.header(*name, *value);
        }
        builder.body(Vec::new()).unwrap()
    };

    let features = server
        .send_raw(get(&[("getcontentFeatures.dlna.org", "1")]))
        .await;
    assert_eq!(features.status, StatusCode::OK);
    assert_eq!(
        features.header("contentFeatures.dlna.org"),
        Some("DLNA.ORG_OP=01;DLNA.ORG_CI=0;DLNA.ORG_FLAGS=01700000000000000000000000000000")
    );
    assert_eq!(features.header("transferMode.dlna.org"), Some("Streaming"));
    assert_eq!(
        features.header(header::CONTENT_TYPE),
        Some("video/x-matroska")
    );
    assert_eq!(features.body.len(), 300_000);

    let plain = server.send_raw(get(&[])).await;
    assert_eq!(plain.header("contentFeatures.dlna.org"), None);
    assert_eq!(plain.header("transferMode.dlna.org"), None);

    let interactive = server
        .send_raw(get(&[("transferMode.dlna.org", "Interactive")]))
        .await;
    assert_eq!(
        interactive.header("transferMode.dlna.org"),
        Some("Interactive")
    );
    let refused = server
        .send_raw(get(&[("transferMode.dlna.org", "Push")]))
        .await;
    assert_eq!(refused.status, StatusCode::NOT_ACCEPTABLE);

    let range = server.send_raw(get(&[("Range", "bytes=100-199")])).await;
    assert_eq!(range.status, StatusCode::PARTIAL_CONTENT);
    assert_eq!(range.body.len(), 100);

    let head = server
        .send_raw(
            request(Method::HEAD, &path)
                .header("getcontentFeatures.dlna.org", "1")
                .body(Vec::new())
                .unwrap(),
        )
        .await;
    assert_eq!(head.status, StatusCode::OK);
    assert_eq!(head.header("transferMode.dlna.org"), Some("Streaming"));
    assert!(head.body.is_empty());

    let wrong = server
        .send_raw(
            request(
                Method::GET,
                &format!("/dlna/media/{media}/{video}/video.mp4"),
            )
            .body(Vec::new())
            .unwrap(),
        )
        .await;
    assert_eq!(
        wrong.status,
        StatusCode::NOT_FOUND,
        "the name must be the file's"
    );

    let poster = format!("/api/v1/torrents/{media}/poster");
    let uploaded = server
        .send(
            request(Method::PUT, &poster)
                .header(header::CONTENT_TYPE, "image/png")
                .body(b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR\0\0\0\x01\0\0\0\x01".to_vec())
                .unwrap(),
        )
        .await;
    assert_eq!(uploaded.status, StatusCode::CREATED);
    let art = server
        .send_raw(
            request(Method::GET, &format!("/dlna/posters/{media}"))
                .body(Vec::new())
                .unwrap(),
        )
        .await;
    assert_eq!(art.status, StatusCode::OK);
    assert_eq!(art.header(header::CONTENT_TYPE), Some("image/png"));
    server.stop().await;
}

/// DLNA-07: faults for unknown actions, missing arguments, and a library
/// still being restored.
#[tokio::test]
async fn actions_fail_with_upnp_errors() {
    let server = TestServer::start().await;
    upload(&server, &catalog::media_names()).await;
    enable(&server, true).await;
    assert_eq!(
        fault_code(&call(&server, "DestroyObject", &[]).await),
        "401"
    );
    assert_eq!(fault_code(&call(&server, "Browse", &[]).await), "402");
    let unannounced = server
        .send_raw(
            request(Method::POST, "/dlna/content-directory/control")
                .body(b"<s:Envelope/>".to_vec())
                .unwrap(),
        )
        .await;
    assert_eq!(fault_code(&unannounced), "401", "no SOAPACTION");

    let server = server
        .restart_with(torrplay_core::fake::FakeEngine::with_catalog(), false)
        .await;
    let restoring = call(
        &server,
        "Browse",
        &[
            ("ObjectID", "0"),
            ("BrowseFlag", "BrowseMetadata"),
            ("StartingIndex", "0"),
            ("RequestedCount", "0"),
        ],
    )
    .await;
    assert_eq!(fault_code(&restoring), "720");
    server.finish_restore().await;
    let capabilities = call(&server, "GetSortCapabilities", &[]).await;
    assert_eq!(capabilities.status, StatusCode::OK);
    let connection = server
        .send_raw(
            request(Method::POST, "/dlna/connection-manager/control")
                .header(
                    "SOAPACTION",
                    r#""urn:schemas-upnp-org:service:ConnectionManager:1#GetProtocolInfo""#,
                )
                .body(
                    br#"<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/"><s:Body><u:GetProtocolInfo xmlns:u="urn:schemas-upnp-org:service:ConnectionManager:1"/></s:Body></s:Envelope>"#
                        .to_vec(),
                )
                .unwrap(),
        )
        .await;
    assert_eq!(connection.status, StatusCode::OK, "{}", text(&connection));
    assert!(text(&connection).contains("video/x-matroska"));
    server.stop().await;
}

/// DLNA-08: the system update ID grows when the content tree changes.
#[tokio::test]
async fn the_update_id_follows_the_content_tree() {
    let server = TestServer::start().await;
    let (media, _) = upload(&server, &catalog::media_names()).await;
    enable(&server, true).await;
    let update_id = || async {
        let answered = call(&server, "GetSystemUpdateID", &[]).await;
        between(&text(&answered), "<Id>", "</Id>")
            .parse::<u32>()
            .unwrap()
    };
    let first = update_id().await;
    assert_eq!(update_id().await, first, "an unchanged tree keeps its ID");
    configure(&server, &media, json!({ "title": "Renamed" })).await;
    assert_eq!(update_id().await, first + 1);
    server.stop().await;
}
