// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! Disk storage end to end, as ADR 9 decides: the application over HTTP,
//! the librqbit engine, and a local seeder on loopback.

// Helpers assert as tests do; clippy's test exemption covers only test
// functions.
#![allow(clippy::expect_used)]

mod common;

use std::path::PathBuf;

use common::{Running, expected, json};
use reqwest::{Client, StatusCode};
use torrplay::ShutdownTimings;
use torrplay_engine::EngineConfig;
use torrplay_engine::test_support::Seeder;
use torrplay_fixtures::catalog;

/// A creation form for a torrent file in disk storage.
fn disk_upload(torrent_file: &[u8]) -> (String, Vec<u8>) {
    const BOUNDARY: &str = "torrplay-boundary";
    let mut body = format!(
        "--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"storage_mode\"\r\n\r\ndisk\r\n\
         --{BOUNDARY}\r\nContent-Disposition: form-data; name=\"torrent_file\"; \
         filename=\"upload.torrent\"\r\nContent-Type: application/x-bittorrent\r\n\r\n"
    )
    .into_bytes();
    body.extend_from_slice(torrent_file);
    body.extend_from_slice(format!("\r\n--{BOUNDARY}--\r\n").as_bytes());
    (format!("multipart/form-data; boundary={BOUNDARY}"), body)
}

/// DSK-01 and DSK-06 end to end: a torrent created in disk storage streams,
/// keeps its files at their API paths, streams again after a restart with
/// no peer reachable, and deleting it deletes its directory and resume
/// bitfield.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_disk_torrent_streams_survives_a_restart_and_deletes_its_data() -> anyhow::Result<()> {
    let spec = catalog::multi_boundary();
    let seeder = Seeder::start(&spec, &spec.metainfo().bytes).await?;
    let downloads = tempfile::tempdir()?;
    let download_dir: PathBuf = downloads.path().to_path_buf();
    let in_downloads = |config: &mut EngineConfig| config.download_dir = download_dir.clone();
    let client = Client::new();
    let running =
        Running::start(vec![seeder.addr], ShutdownTimings::default(), in_downloads).await?;

    let (content_type, body) = disk_upload(&spec.metainfo().bytes);
    let created = client
        .post(running.url("/api/v1/torrents"))
        .header("content-type", content_type)
        .body(body)
        .send()
        .await?;
    assert_eq!(created.status(), StatusCode::CREATED);
    let torrent = json(created).await?;
    assert_eq!(torrent["storage_mode"], "disk");
    let id = torrent["id"].as_str().expect("an ID").to_owned();
    let path = torrent["links"]["self"]
        .as_str()
        .expect("a link")
        .to_owned();
    let files = json(
        client
            .get(running.url(&format!("{path}/files")))
            .send()
            .await?,
    )
    .await?;
    let content: Vec<String> = files["items"]
        .as_array()
        .expect("file items")
        .iter()
        .map(|file| {
            file["links"]["content"]
                .as_str()
                .expect("a link")
                .to_owned()
        })
        .collect();
    let stream_all = async |running: &Running| -> anyhow::Result<()> {
        for (index, link) in content.iter().enumerate() {
            let response = client.get(running.url(link)).send().await?;
            assert_eq!(response.status(), StatusCode::OK);
            let size = spec.files()[index].size_bytes;
            assert!(
                response.bytes().await? == expected(&spec, index, 0, size),
                "file {index}"
            );
        }
        Ok(())
    };
    stream_all(&running).await?;

    let directory = download_dir.join(&id);
    let mut first = directory.join(&spec.torrent_name);
    first.extend(&spec.files()[0].path);
    assert!(first.is_file(), "files sit at their API paths");
    assert!(directory.with_extension("have").is_file());

    seeder.stop().await;
    let running = running.restart(in_downloads).await?;
    stream_all(&running).await?;

    let deleted = client.delete(running.url(&path)).send().await?;
    assert_eq!(deleted.status(), StatusCode::NO_CONTENT);
    assert!(
        !directory.exists(),
        "deleting the torrent deletes its files"
    );
    assert!(!directory.with_extension("have").exists());
    running.stop().await?;
    Ok(())
}
