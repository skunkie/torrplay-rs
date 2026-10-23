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

use common::{MIB, Running, eventually, expected, json, upload};
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

/// Changes the storage mode of the torrent at `path` with a merge patch.
async fn patch_storage_mode(
    running: &Running,
    client: &Client,
    path: &str,
    mode: &str,
) -> anyhow::Result<()> {
    let configuration = running.url(&format!("{path}/configuration"));
    let current = client.get(&configuration).send().await?;
    let etag = current.headers()["etag"].clone();
    let patched = client
        .patch(&configuration)
        .header("content-type", "application/merge-patch+json")
        .header("if-match", etag)
        .body(format!(r#"{{"storage_mode":"{mode}"}}"#))
        .send()
        .await?;
    assert_eq!(patched.status(), StatusCode::OK);
    Ok(())
}

/// DSK-02 end to end: an HTTP stream continues byte for byte while its
/// torrent moves from memory to disk storage and back, and the move back
/// deletes the torrent's directory.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_stream_continues_while_its_torrent_changes_storage() -> anyhow::Result<()> {
    let spec = catalog::large(4 * MIB);
    let seeder = Seeder::start(&spec, &spec.metainfo().bytes).await?;
    let downloads = tempfile::tempdir()?;
    let download_dir: PathBuf = downloads.path().to_path_buf();
    let tune = |config: &mut EngineConfig| {
        *config = config.clone().with_cache_capacity(8 * MIB);
        config.download_dir = download_dir.clone();
    };
    let client = Client::new();
    let running = Running::start(vec![seeder.addr], ShutdownTimings::default(), tune).await?;
    let created = upload(&running, &client, &spec).await?;
    let size = spec.total_size_bytes();

    let mut response = client.get(running.url(&created.content[0])).send().await?;
    assert_eq!(response.status(), StatusCode::OK);
    let mut received = Vec::new();
    let directory = download_dir.join(&created.id);
    for (mode, from) in [("disk", size / 3), ("memory", 2 * size / 3)] {
        while (received.len() as u64) < from {
            let chunk = response.chunk().await?.expect("more of the body");
            received.extend_from_slice(&chunk);
        }
        patch_storage_mode(&running, &client, &created.path, mode).await?;
        eventually(&format!("{mode} storage"), async || {
            let torrent = json(client.get(running.url(&created.path)).send().await?).await?;
            Ok(
                (torrent["storage_mode"] == mode && torrent["storage_transition"].is_null())
                    .then_some(()),
            )
        })
        .await?;
        assert_eq!(directory.exists(), mode == "disk");
    }
    while let Some(chunk) = response.chunk().await? {
        received.extend_from_slice(&chunk);
    }
    assert!(received == expected(&spec, 0, 0, size), "the whole file");

    running.stop().await?;
    seeder.stop().await;
    Ok(())
}
