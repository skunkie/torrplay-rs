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
use torrplay_fixtures::{FileSpec, FixtureSpec, Layout, catalog, hex};

const CONTENT_DIRECTORY: &str = "urn:schemas-upnp-org:service:ContentDirectory:1";

/// Sets `dlna_enabled`.
async fn enable(server: &TestServer, on: bool) {
    let patched = change_settings(server, json!({ "dlna_enabled": on })).await;
    assert_eq!(patched.json()["dlna_enabled"], on);
}

/// Applies a merge patch to the settings.
async fn change_settings(server: &TestServer, patch: serde_json::Value) -> Exchange {
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
                .body(serde_json::to_vec(&patch).unwrap())
                .unwrap(),
        )
        .await;
    assert_eq!(patched.status, StatusCode::OK, "{patched:?}");
    patched
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
    let icon = |path: &'static str| {
        let server = &server;
        async move {
            server
                .send_raw(request(Method::GET, path).body(Vec::new()).unwrap())
                .await
        }
    };
    assert_eq!(
        icon("/dlna/icons/icon-48.png").await.status,
        StatusCode::NOT_FOUND,
        "icons are hidden while the media server is off"
    );
    enable(&server, true).await;
    let found = description().await;
    assert_eq!(found.status, StatusCode::OK);
    assert_eq!(
        found.header(header::CONTENT_TYPE),
        Some(r#"text/xml; charset="utf-8""#)
    );
    let body = text(&found);
    assert!(
        body.contains("<friendlyName>TorrPlay</friendlyName>"),
        "{body}"
    );
    // SET-10: the description carries a new name at once.
    change_settings(&server, json!({ "dlna_name": "Den & kitchen" })).await;
    let renamed = text(&description().await);
    assert!(
        renamed.contains("<friendlyName>Den &amp; kitchen</friendlyName>"),
        "{renamed}"
    );
    let udn = between(&body, "<UDN>", "</UDN>").to_owned();
    assert!(udn.starts_with("uuid:") && udn.len() == 41, "{udn}");

    // DLNA-15: the listed icons, PNG before JPEG and small before large.
    let listed: Vec<(String, u32, String)> = body
        .split("<icon>")
        .skip(1)
        .map(|icon| {
            (
                between(icon, "<mimetype>", "</mimetype>").to_owned(),
                between(icon, "<width>", "</width>").parse().unwrap(),
                between(icon, "<url>", "</url>").to_owned(),
            )
        })
        .collect();
    let expected = [
        ("image/png", 48, "/dlna/icons/icon-48.png"),
        ("image/png", 120, "/dlna/icons/icon-120.png"),
        ("image/jpeg", 48, "/dlna/icons/icon-48.jpg"),
        ("image/jpeg", 120, "/dlna/icons/icon-120.jpg"),
    ];
    assert_eq!(
        listed,
        expected.map(|(media, size, url)| (media.to_owned(), size, url.to_owned()))
    );
    for (media_type, size, path) in expected {
        let served = icon(path).await;
        assert_eq!(served.status, StatusCode::OK, "{path}");
        assert_eq!(served.header(header::CONTENT_TYPE), Some(media_type));
        assert_eq!(image_size(&served.body), Some((size, size)), "{path}");
    }
    assert_eq!(
        icon("/dlna/icons/icon-64.png").await.status,
        StatusCode::NOT_FOUND
    );
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

/// TMP-08: a temporary torrent is never shown to televisions, and its media
/// URL answers as an unknown one does.
#[tokio::test]
async fn temporary_torrents_stay_off_the_media_server() {
    let server = TestServer::start().await;
    let spec = catalog::media_names();
    let created = server
        .send(json_request(
            Method::POST,
            "/api/v1/torrents",
            &json!({
                "magnet_uri": format!("magnet:?xt=urn:btih:{}", hex(&spec.metainfo().info_hash)),
                "retention": "temporary",
            }),
        ))
        .await;
    assert_eq!(created.status, StatusCode::CREATED);
    let id = created.json()["id"].as_str().unwrap().to_owned();
    server.engine.set_metadata_retrievable(
        torrplay_core::id::InfoHashV1::from_bytes(spec.metainfo().info_hash),
        true,
    );
    let files = server
        .get(&format!("/api/v1/torrents/{id}/files?page_size=200"))
        .await
        .json()["items"]
        .as_array()
        .unwrap()
        .clone();
    enable(&server, true).await;

    let (all, body) = browse(&server, "all", "BrowseDirectChildren").await;
    assert!(!all.contains(&id), "{all}");
    assert!(body.contains("<TotalMatches>0</TotalMatches>"), "{body}");
    let video = file_id(&files, "video.mkv");
    let media = server
        .send_raw(
            request(Method::GET, &format!("/dlna/media/{id}/{video}/video.mkv"))
                .body(Vec::new())
                .unwrap(),
        )
        .await;
    assert_eq!(media.status, StatusCode::NOT_FOUND);
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

/// Searches `container` with `criteria`, sorted by `sort`.
async fn search(server: &TestServer, container: &str, criteria: &str, sort: &str) -> Exchange {
    let criteria = criteria
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('"', "&quot;");
    call(
        server,
        "Search",
        &[
            ("ContainerID", container),
            ("SearchCriteria", &criteria),
            ("Filter", "*"),
            ("StartingIndex", "0"),
            ("RequestedCount", "0"),
            ("SortCriteria", sort),
        ],
    )
    .await
}

fn didl_of(exchange: &Exchange) -> String {
    assert_eq!(exchange.status, StatusCode::OK, "{}", text(exchange));
    between(&text(exchange), "<Result>", "</Result>")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&amp;", "&")
}

/// DLNA-10 to DLNA-12: searches match titles through the shared matcher,
/// find each object once, rank and sort, and refuse what they cannot read.
#[tokio::test]
async fn searches_find_rank_and_sort() {
    let server = TestServer::start().await;
    let (media, _) = upload(&server, &catalog::media_names()).await;
    let show_spec = series();
    server.engine.add_fixture(&show_spec);
    let (show, _) = upload(&server, &show_spec).await;
    configure(
        &server,
        &media,
        json!({ "title": "Sintel extras", "genres": ["Animation"] }),
    )
    .await;
    configure(
        &server,
        &show,
        json!({ "title": "Sintel", "categories": ["TV"] }),
    )
    .await;
    enable(&server, true).await;

    let titled = didl_of(&search(&server, "0", r#"dc:title contains "sintel""#, "").await);
    let whole = titled.find(&format!("t:{show}")).unwrap();
    let partial = titled.find(&format!("t:{media}")).unwrap();
    assert!(whole < partial, "the closer title first: {titled}");
    assert_eq!(
        titled.matches(&format!(r#"id="t:{show}""#)).count(),
        1,
        "each once"
    );
    let typo = search(&server, "0", r#"dc:title contains "sintle""#, "").await;
    assert!(
        text(&typo).contains("<TotalMatches>0</TotalMatches>"),
        "no typo tolerance"
    );

    let videos = search(
        &server,
        "0",
        r#"(upnp:class derivedfrom "object.item.videoItem") and (dc:title contains "S01E01")"#,
        "",
    )
    .await;
    assert!(
        text(&videos).contains("<TotalMatches>1</TotalMatches>"),
        "{}",
        text(&videos)
    );
    let animated = didl_of(&search(&server, "0", r#"upnp:genre = "animation""#, "").await);
    assert!(animated.contains(&format!("t:{media}")) && !animated.contains(&format!("t:{show}")));
    let in_category = search(&server, "category:TV", "*", "").await;
    assert!(
        text(&in_category).contains("<TotalMatches>3</TotalMatches>"),
        "the series and its two episodes"
    );
    let without = didl_of(
        &search(
            &server,
            &format!("t:{media}"),
            r#"dc:title doesNotContain "video""#,
            "",
        )
        .await,
    );
    assert!(without.contains("audio.mp3") && !without.contains("video.mkv"));

    let sorted = didl_of(&search(&server, "all", "upnp:genre exists false", "+dc:title").await);
    assert!(sorted.contains(&format!("t:{show}")));
    let browsed = call(
        &server,
        "Browse",
        &[
            ("ObjectID", "all"),
            ("BrowseFlag", "BrowseDirectChildren"),
            ("StartingIndex", "0"),
            ("RequestedCount", "0"),
            ("SortCriteria", "+dc:title"),
        ],
    )
    .await;
    let browsed = didl_of(&browsed);
    assert!(
        browsed.find(&format!("t:{show}")).unwrap() < browsed.find(&format!("t:{media}")).unwrap(),
        "Sintel before Sintel extras"
    );
    assert!(
        browsed.contains("<dc:date>"),
        "torrents carry their creation date"
    );

    assert_eq!(
        fault_code(&search(&server, "0", r#"dc:creator = "x""#, "").await),
        "708"
    );
    assert_eq!(
        fault_code(&search(&server, "0", "dc:title contains", "").await),
        "708"
    );
    assert_eq!(
        fault_code(&search(&server, "0", "*", "+upnp:class").await),
        "709"
    );
    let capabilities = call(&server, "GetSearchCapabilities", &[]).await;
    assert!(text(&capabilities).contains("dc:title,dc:date,upnp:class,upnp:genre,@id,@parentID"));
    server.stop().await;
}

fn method(name: &str) -> Method {
    Method::from_bytes(name.as_bytes()).unwrap()
}

/// Accepts one `NOTIFY` on `listener` and answers it, or `None` if none
/// arrives in time.
async fn next_event(listener: &tokio::net::TcpListener) -> Option<String> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let accepted =
        tokio::time::timeout(std::time::Duration::from_millis(500), listener.accept()).await;
    let (mut stream, _) = accepted.ok()?.ok()?;
    let mut received = vec![0; 8192];
    let length = stream.read(&mut received).await.ok()?;
    stream
        .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
        .await
        .ok()?;
    Some(String::from_utf8_lossy(&received[..length]).into_owned())
}

/// DLNA-13 and DLNA-14: subscriptions receive events, renew, and end; bad
/// subscriptions are refused.
#[tokio::test]
async fn subscribers_receive_events() {
    let server = TestServer::start().await;
    let (media, _) = upload(&server, &catalog::media_names()).await;
    enable(&server, true).await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let callback = format!(
        "<http://127.0.0.1:{}/events>",
        listener.local_addr().unwrap().port()
    );
    let events = "/dlna/content-directory/events";
    let subscribe = |headers: &[(&str, &str)]| {
        let mut builder = request(method("SUBSCRIBE"), events);
        for (name, value) in headers {
            builder = builder.header(*name, *value);
        }
        builder.body(Vec::new()).unwrap()
    };

    let granted = server
        .send_raw(subscribe(&[
            ("CALLBACK", &callback),
            ("NT", "upnp:event"),
            ("TIMEOUT", "Second-300"),
        ]))
        .await;
    assert_eq!(granted.status, StatusCode::OK);
    assert_eq!(granted.header("timeout"), Some("Second-300"));
    let sid = granted.header("sid").unwrap().to_owned();
    assert!(sid.starts_with("uuid:"));

    server.app.notify_media_server_subscribers().await;
    let initial = next_event(&listener).await.expect("the initial event");
    assert!(initial.contains(&format!("SID: {sid}\r\n")) && initial.contains("SEQ: 0\r\n"));
    let first: u32 = between(&initial, "<SystemUpdateID>", "</SystemUpdateID>")
        .parse()
        .unwrap();

    server.app.notify_media_server_subscribers().await;
    assert_eq!(next_event(&listener).await, None, "nothing changed");
    configure(&server, &media, json!({ "title": "Renamed" })).await;
    server.app.notify_media_server_subscribers().await;
    let changed = next_event(&listener)
        .await
        .expect("an event for the change");
    assert!(changed.contains("SEQ: 1\r\n"));
    assert!(changed.contains(&format!("<SystemUpdateID>{}</SystemUpdateID>", first + 1)));

    let renewed = server
        .send_raw(subscribe(&[("SID", &sid), ("TIMEOUT", "Second-600")]))
        .await;
    assert_eq!(renewed.status, StatusCode::OK);
    assert_eq!(renewed.header("timeout"), Some("Second-600"));
    let ended = server
        .send_raw(
            request(method("UNSUBSCRIBE"), events)
                .header("SID", &sid)
                .body(Vec::new())
                .unwrap(),
        )
        .await;
    assert_eq!(ended.status, StatusCode::OK);
    configure(&server, &media, json!({ "title": "Renamed again" })).await;
    server.app.notify_media_server_subscribers().await;
    assert_eq!(
        next_event(&listener).await,
        None,
        "no events after unsubscribing"
    );

    let elsewhere = server
        .send_raw(subscribe(&[
            ("CALLBACK", "<http://192.168.1.99/events>"),
            ("NT", "upnp:event"),
        ]))
        .await;
    assert_eq!(
        elsewhere.status,
        StatusCode::PRECONDITION_FAILED,
        "a callback elsewhere"
    );
    let no_nt = server.send_raw(subscribe(&[("CALLBACK", &callback)])).await;
    assert_eq!(no_nt.status, StatusCode::PRECONDITION_FAILED);
    let both = server
        .send_raw(subscribe(&[
            ("SID", &sid),
            ("CALLBACK", &callback),
            ("NT", "upnp:event"),
        ]))
        .await;
    assert_eq!(both.status, StatusCode::BAD_REQUEST);
    let unknown = server.send_raw(subscribe(&[("SID", "uuid:unknown")])).await;
    assert_eq!(unknown.status, StatusCode::PRECONDITION_FAILED);
    let other = server
        .send_raw(request(Method::GET, events).body(Vec::new()).unwrap())
        .await;
    assert_eq!(other.status, StatusCode::METHOD_NOT_ALLOWED);
    assert_eq!(other.header(header::ALLOW), Some("SUBSCRIBE, UNSUBSCRIBE"));

    let cm = server
        .send_raw(
            request(method("SUBSCRIBE"), "/dlna/connection-manager/events")
                .header("CALLBACK", &callback)
                .header("NT", "upnp:event")
                .body(Vec::new())
                .unwrap(),
        )
        .await;
    assert_eq!(cm.status, StatusCode::OK);
    enable(&server, false).await;
    enable(&server, true).await;
    server.app.notify_media_server_subscribers().await;
    assert_eq!(
        next_event(&listener).await,
        None,
        "turning off ended every subscription"
    );
    server.stop().await;
}

/// The width and height a PNG's header or a JPEG's start-of-frame segment
/// declares.
fn image_size(bytes: &[u8]) -> Option<(u32, u32)> {
    let be16 = |at: usize| u32::from(u16::from_be_bytes([bytes[at], bytes[at + 1]]));
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        let be32 = |at: usize| u32::from_be_bytes(bytes[at..at + 4].try_into().unwrap());
        return Some((be32(16), be32(20)));
    }
    if !bytes.starts_with(&[0xFF, 0xD8]) {
        return None;
    }
    let mut at = 2;
    while at + 9 < bytes.len() {
        let marker = bytes[at + 1];
        let length = be16(at + 2) as usize;
        if matches!(marker, 0xC0..=0xC3) {
            return Some((be16(at + 7), be16(at + 5)));
        }
        at += 2 + length;
    }
    None
}
