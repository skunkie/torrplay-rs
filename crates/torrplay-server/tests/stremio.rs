// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! STM-01 to STM-08: the Stremio addon's resources over the fake engine, as
//! `docs/stremio.md` specifies them. They are outside the OpenAPI contract,
//! so these exchanges are checked here rather than against it.

// Scenarios and their helpers assert as tests do; clippy's test exemption
// covers only test functions.
#![allow(clippy::expect_used, clippy::unwrap_used)]

// Each test binary uses only part of the shared harness.
#[allow(dead_code)]
mod common;

use common::{Exchange, TestServer, film, json_request, request};
use http::{Method, StatusCode, header};
use serde_json::{Value, json};
use torrplay_fixtures::{FileSpec, FixtureSpec, Layout, catalog, hex};

const PASSWORD: &str = "correct horse battery staple";

/// A series whose episodes come out of order in its metadata.
fn series() -> FixtureSpec {
    FixtureSpec {
        id: "series".into(),
        torrent_name: "Show.S01".into(),
        seed: 42,
        piece_length: 16 * 1024,
        layout: Layout::MultiFile {
            files: ["Show.S01E02.mp4", "Show.S01E01.mp4", "notes.txt"]
                .into_iter()
                .map(|name| FileSpec {
                    path: vec![name.into()],
                    size_bytes: 20_000,
                })
                .collect(),
        },
    }
}

/// A movie with one file, to pause.
fn paused_movie() -> FixtureSpec {
    FixtureSpec {
        id: "paused".into(),
        torrent_name: "Paused".into(),
        seed: 43,
        piece_length: 16 * 1024,
        layout: Layout::SingleFile { size_bytes: 20_000 },
    }
}

/// Uploads `spec`, returning its ID and its files.
async fn upload(
    server: &TestServer,
    spec: &FixtureSpec,
    state: Option<&str>,
) -> (String, Vec<Value>) {
    server.engine.add_fixture(spec);
    let created = server.upload(&spec.metainfo().bytes, state).await;
    assert_eq!(created.status, StatusCode::CREATED, "{created:?}");
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

async fn get(server: &TestServer, path: &str) -> Exchange {
    server
        .send_raw(request(Method::GET, path).body(Vec::new()).unwrap())
        .await
}

/// The JSON of a `200` resource, which allows every origin.
async fn document(server: &TestServer, path: &str) -> Value {
    let read = get(server, path).await;
    assert_eq!(read.status, StatusCode::OK, "{path}: {read:?}");
    assert_eq!(read.header(header::CONTENT_TYPE), Some("application/json"));
    assert_eq!(read.header(header::ACCESS_CONTROL_ALLOW_ORIGIN), Some("*"));
    read.json()
}

fn ids(metas: &Value) -> Vec<String> {
    metas["metas"]
        .as_array()
        .unwrap()
        .iter()
        .map(|meta| meta["id"].as_str().unwrap().to_owned())
        .collect()
}

/// A library of a series, a movie, a paused movie, a temporary movie, and a
/// torrent without video files, newest last.
struct Library {
    server: TestServer,
    show: String,
    show_files: Vec<Value>,
    movie: String,
    movie_files: Vec<Value>,
}

async fn library() -> Library {
    let server = TestServer::start().await;
    let (show, show_files) = upload(&server, &series(), None).await;
    configure(&server, &show, json!({ "genres": ["Drama"] })).await;
    let (movie, movie_files) = upload(&server, &film(), None).await;
    configure(
        &server,
        &movie,
        json!({ "title": "Sintel", "genres": ["Fantasy", "Drama"] }),
    )
    .await;
    upload(&server, &paused_movie(), Some("paused")).await;
    upload(&server, &catalog::single_small(), None).await;
    let temporary = catalog::media_names();
    server.engine.add_fixture(&temporary);
    let created = server
        .send(json_request(
            Method::POST,
            "/api/v1/torrents",
            &json!({
                "magnet_uri": format!("magnet:?xt=urn:btih:{}", hex(&temporary.metainfo().info_hash)),
                "retention": "temporary",
            }),
        ))
        .await;
    assert_eq!(created.status, StatusCode::CREATED, "{created:?}");
    Library {
        server,
        show,
        show_files,
        movie,
        movie_files,
    }
}

/// STM-01 without a password, STM-02 to STM-06, and STM-08.
#[tokio::test]
async fn the_addon_presents_movies_and_series() {
    let Library {
        server,
        show,
        show_files,
        movie,
        movie_files,
    } = library().await;

    // STM-01: the manifest.
    let manifest = document(&server, "/stremio/manifest.json").await;
    assert_eq!(manifest["id"], "org.torrplay.stremio");
    assert_eq!(manifest["idPrefixes"], json!(["torrplay:"]));
    assert_eq!(manifest["types"], json!(["movie", "series"]));
    let catalogs = manifest["catalogs"].as_array().unwrap();
    assert_eq!(catalogs.len(), 2);
    assert_eq!(
        catalogs[0]["extra"][2],
        json!({ "name": "genre", "options": ["Drama", "Fantasy"] })
    );

    // STM-02: each catalog lists its kind's started, persisted torrents with
    // video files.
    let movies = document(&server, "/stremio/catalog/movie/torrplay-movies.json").await;
    assert_eq!(ids(&movies), [format!("torrplay:{movie}")], "{movies}");
    assert_eq!(movies["metas"][0]["name"], "Sintel");
    assert_eq!(movies["metas"][0]["genres"], json!(["Fantasy", "Drama"]));
    let shows = document(&server, "/stremio/catalog/series/torrplay-series.json").await;
    assert_eq!(ids(&shows), [format!("torrplay:{show}")]);
    assert_eq!(shows["metas"][0]["name"], "Show.S01");
    let unknown = document(&server, "/stremio/catalog/movie/elsewhere.json").await;
    assert!(ids(&unknown).is_empty());

    // STM-03: search, genre, and skip.
    for (extra, expected) in [
        ("search=sintle", vec![format!("torrplay:{movie}")]),
        ("search=nothing", Vec::new()),
        ("genre=Fantasy", vec![format!("torrplay:{movie}")]),
        ("genre=Drama&skip=1", Vec::new()),
    ] {
        let found = document(
            &server,
            &format!("/stremio/catalog/movie/torrplay-movies/{extra}.json"),
        )
        .await;
        assert_eq!(ids(&found), expected, "{extra}");
    }
    let refused = get(
        &server,
        "/stremio/catalog/movie/torrplay-movies/skip=-1.json",
    )
    .await;
    assert_eq!(refused.status, StatusCode::BAD_REQUEST);

    // STM-04: metadata.
    let meta = document(
        &server,
        &format!("/stremio/meta/series/torrplay:{show}.json"),
    )
    .await;
    let videos: Vec<(String, Value, Value)> = meta["meta"]["videos"]
        .as_array()
        .unwrap()
        .iter()
        .map(|video| {
            (
                video["id"].as_str().unwrap().to_owned(),
                video["season"].clone(),
                video["episode"].clone(),
            )
        })
        .collect();
    let episode = |name| format!("torrplay:{show}:{}", file_id(&show_files, name));
    assert_eq!(
        videos,
        [
            (episode("Show.S01E01.mp4"), json!(1), json!(1)),
            (episode("Show.S01E02.mp4"), json!(1), json!(2)),
        ],
        "episodes in order, notes left out"
    );
    let film_id = file_id(&movie_files, "Film.mkv");
    let meta = document(
        &server,
        &format!("/stremio/meta/movie/torrplay:{movie}.json"),
    )
    .await;
    assert_eq!(
        meta["meta"]["behaviorHints"]["defaultVideoId"],
        format!("torrplay:{movie}:{film_id}")
    );
    let missing = get(
        &server,
        "/stremio/meta/movie/torrplay:trr_aaaaaaaaaaaaaaaaaaaaaaaaaa.json",
    )
    .await;
    assert_eq!(missing.status, StatusCode::NOT_FOUND);

    // STM-05: streams.
    let streams = document(
        &server,
        &format!("/stremio/stream/movie/torrplay:{movie}:{film_id}.json"),
    )
    .await;
    let stream = &streams["streams"][0];
    let play = format!("/stremio/play/{movie}/{film_id}/Film.mkv");
    assert_eq!(stream["url"], format!("{}{play}", server_origin(&server)));
    assert_eq!(
        stream["behaviorHints"],
        json!({
            "filename": "Film.mkv",
            "bingeGroup": format!("torrplay-{movie}"),
            "notWebReady": true,
        })
    );
    let all = document(
        &server,
        &format!("/stremio/stream/series/torrplay:{show}.json"),
    )
    .await;
    assert_eq!(all["streams"].as_array().unwrap().len(), 2);
    assert_eq!(all["streams"][0]["behaviorHints"].get("notWebReady"), None);
    let none = document(&server, "/stremio/stream/movie/tt0133093.json").await;
    assert_eq!(none["streams"], json!([]));

    // STM-06: play URLs serve the content.
    let range = server
        .send_raw(
            request(Method::GET, &play)
                .header(header::RANGE, "bytes=100-199")
                .body(Vec::new())
                .unwrap(),
        )
        .await;
    assert_eq!(range.status, StatusCode::PARTIAL_CONTENT);
    assert_eq!(range.body.len(), 100);
    let head = server
        .send_raw(request(Method::HEAD, &play).body(Vec::new()).unwrap())
        .await;
    assert_eq!(head.status, StatusCode::OK);
    assert_eq!(head.header(header::CONTENT_LENGTH), Some("41943040"));
    let renamed = get(
        &server,
        &format!("/stremio/play/{movie}/{film_id}/Other.mkv"),
    )
    .await;
    assert_eq!(renamed.status, StatusCode::NOT_FOUND);

    // STM-08: read-only, and readable by any origin even when refused.
    let posted = server
        .send_raw(
            request(Method::POST, "/stremio/manifest.json")
                .body(Vec::new())
                .unwrap(),
        )
        .await;
    assert_eq!(posted.status, StatusCode::METHOD_NOT_ALLOWED);
    assert_eq!(
        posted.header(header::ACCESS_CONTROL_ALLOW_ORIGIN),
        Some("*")
    );
    server.stop().await;
}

/// The origin URLs carry: the address the test client named.
fn server_origin(server: &TestServer) -> String {
    server.origin().to_owned()
}

/// STM-01 and STM-07 with a password: a playback token in the path or the
/// query opens the addon and goes into every URL it hands out.
#[tokio::test]
async fn a_password_needs_a_playback_token() {
    let Library { server, movie, .. } = library().await;
    let set = server
        .send(json_request(
            Method::PUT,
            "/api/v1/password",
            &json!({ "password": PASSWORD }),
        ))
        .await;
    assert_eq!(set.status, StatusCode::CREATED);
    let token = |scope: &str| {
        let server = &server;
        let scope = scope.to_owned();
        async move {
            let created = server
                .send(json_request(
                    Method::POST,
                    "/api/v1/access-tokens",
                    &json!({ "password": PASSWORD, "name": "Stremio", "scope": scope }),
                ))
                .await;
            assert_eq!(created.status, StatusCode::CREATED, "{created:?}");
            created.json()["secret"].as_str().unwrap().to_owned()
        }
    };
    let playback = token("playback").await;
    let api = token("api").await;

    let refused = get(&server, "/stremio/manifest.json").await;
    assert_eq!(refused.status, StatusCode::UNAUTHORIZED);
    assert_eq!(
        refused.header(header::ACCESS_CONTROL_ALLOW_ORIGIN),
        Some("*")
    );
    document(&server, &format!("/stremio/{playback}/manifest.json")).await;
    document(&server, &format!("/stremio/manifest.json?token={playback}")).await;
    let movies = document(
        &server,
        &format!("/stremio/{playback}/catalog/movie/torrplay-movies.json"),
    )
    .await;
    assert_eq!(movies["metas"][0]["id"], format!("torrplay:{movie}"));
    let streams = document(
        &server,
        &format!("/stremio/{playback}/stream/movie/torrplay:{movie}.json"),
    )
    .await;
    let url = streams["streams"][0]["url"].as_str().unwrap().to_owned();
    assert!(
        url.contains(&format!("/stremio/{playback}/play/{movie}/")),
        "{url}"
    );
    let path = url.strip_prefix(&server_origin(&server)).unwrap();
    assert_eq!(
        server
            .send_raw(request(Method::HEAD, path).body(Vec::new()).unwrap())
            .await
            .status,
        StatusCode::OK
    );

    // STM-07: an api token in a URL, and a playback token for one torrent.
    assert_eq!(
        get(&server, &format!("/stremio/{api}/manifest.json"))
            .await
            .status,
        StatusCode::UNAUTHORIZED
    );
    let issued = server
        .send(
            request(Method::POST, "/api/v1/playback-tokens")
                .header(header::AUTHORIZATION, format!("Bearer {api}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(json!({ "torrent_id": movie }).to_string().into_bytes())
                .unwrap(),
        )
        .await;
    assert_eq!(issued.status, StatusCode::OK, "{issued:?}");
    let one = issued.json()["token"].as_str().unwrap().to_owned();
    let scoped = get(&server, &format!("/stremio/{one}/manifest.json")).await;
    assert_eq!(scoped.status, StatusCode::FORBIDDEN);
    assert_eq!(
        scoped.json()["type"],
        "tag:skunkie.github.io,2026:torrplay/problems/insufficient-scope"
    );
    server.stop().await;
}

/// STM-07 without a password: the addon answers only local networks.
#[tokio::test]
async fn without_a_password_the_addon_answers_only_local_networks() {
    use axum::extract::connect_info::MockConnectInfo;
    let server = TestServer::start().await;
    let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let public: std::net::SocketAddr = "203.0.113.7:50000".parse().unwrap();
    let app = torrplay_server::router(std::sync::Arc::clone(&server.app))
        .layer(MockConnectInfo(public))
        .into_make_service();
    let task = tokio::spawn(std::future::IntoFuture::into_future(axum::serve(
        listener, app,
    )));
    let refused = reqwest::get(format!("{base}/stremio/manifest.json"))
        .await
        .unwrap();
    assert_eq!(refused.status().as_u16(), 403);
    assert_eq!(
        refused.headers()["access-control-allow-origin"],
        "*",
        "Stremio's web client can read the refusal"
    );
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    server.stop().await;
}
