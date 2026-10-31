// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! Backup and restore through the real engine.

#![allow(clippy::expect_used)]

mod common;

use std::io::{Cursor, Read, Write};

use std::path::PathBuf;

use common::{MIB, Running, eventually, json, upload};
use reqwest::{Client, StatusCode};
use serde_json::{Value, json};
use torrplay::ShutdownTimings;
use torrplay_engine::EngineConfig;
use torrplay_fixtures::{FixtureSpec, catalog};
use zip::write::SimpleFileOptions;
use zip::{ZipArchive, ZipWriter};

/// `archive` with `backup.json` changed by `change`.
fn with_document(archive: &[u8], change: impl FnOnce(&mut Value)) -> anyhow::Result<Vec<u8>> {
    let mut change = Some(change);
    let mut source = ZipArchive::new(Cursor::new(archive))?;
    let mut target = ZipWriter::new(Cursor::new(Vec::new()));
    for index in 0..source.len() {
        let mut file = source.by_index(index)?;
        let name = file.name()?.into_owned();
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        if name == "backup.json"
            && let Some(change) = change.take()
        {
            let mut document: Value = serde_json::from_slice(&bytes)?;
            change(&mut document);
            bytes = serde_json::to_vec(&document)?;
        }
        target.start_file(name, SimpleFileOptions::default())?;
        target.write_all(&bytes)?;
    }
    Ok(target.finish()?.into_inner())
}

/// BAK-09: a restored torrent the engine refuses, here for pieces longer
/// than the restored cache capacity, is restored as failed.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_restored_torrent_the_engine_refuses_fails() -> anyhow::Result<()> {
    let oversized = catalog::oversized_piece();
    let running = Running::start(Vec::new(), ShutdownTimings::default(), |config| {
        *config = config.clone().with_cache_capacity(64 * MIB);
    })
    .await?;
    let client = Client::new();
    let created = upload(&running, &client, &oversized).await?;
    let backup = client
        .get(running.url("/api/v1/backup"))
        .send()
        .await?
        .bytes()
        .await?;
    let smaller = with_document(&backup, |document| {
        document["settings"] = json!({ "cache_capacity_bytes": 8 * MIB });
    })?;
    let restored = client
        .put(running.url("/api/v1/backup"))
        .header("content-type", "application/zip")
        .body(smaller)
        .send()
        .await?;
    assert_eq!(restored.status(), StatusCode::NO_CONTENT);
    let torrent = eventually("the restored torrent", async || {
        let response = client.get(running.url(&created.path)).send().await?;
        if response.status() != StatusCode::OK {
            return Ok(None);
        }
        let torrent = json(response).await?;
        Ok((torrent["observed_state"] == "failed").then_some(torrent))
    })
    .await?;
    let detail = torrent["failure"]["detail"].as_str().unwrap_or_default();
    assert!(!detail.is_empty(), "the failure names its cause");
    running.stop().await?;
    Ok(())
}

/// Creates `spec`'s torrent in disk storage and returns its ID.
async fn on_disk(running: &Running, client: &Client, spec: &FixtureSpec) -> anyhow::Result<String> {
    const BOUNDARY: &str = "torrplay-boundary";
    let mut body = format!(
        "--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"storage_mode\"\r\n\r\ndisk\r\n\
         --{BOUNDARY}\r\nContent-Disposition: form-data; name=\"metainfo\"; \
         filename=\"upload.torrent\"\r\nContent-Type: application/x-bittorrent\r\n\r\n"
    )
    .into_bytes();
    body.extend_from_slice(&spec.metainfo().bytes);
    body.extend_from_slice(format!("\r\n--{BOUNDARY}--\r\n").as_bytes());
    let created = client
        .post(running.url("/api/v1/torrents"))
        .header(
            "content-type",
            format!("multipart/form-data; boundary={BOUNDARY}"),
        )
        .body(body)
        .send()
        .await?;
    assert_eq!(created.status(), StatusCode::CREATED);
    Ok(json(created).await?["id"]
        .as_str()
        .expect("an ID")
        .to_owned())
}

/// BAK-03 through the engine: a restore deletes the directory of a disk
/// torrent the backup lacks and keeps the backup's own.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_restore_keeps_only_the_backups_disk_data() -> anyhow::Result<()> {
    let downloads = tempfile::tempdir()?;
    let download_dir: PathBuf = downloads.path().to_path_buf();
    let in_downloads = |config: &mut EngineConfig| config.download_dir = download_dir.clone();
    let running = Running::start(Vec::new(), ShutdownTimings::default(), in_downloads).await?;
    let client = Client::new();
    let kept = on_disk(&running, &client, &catalog::single_small()).await?;
    let directory = |id: &str| download_dir.join(id);
    eventually("the kept torrent's directory", async || {
        Ok(directory(&kept).is_dir().then_some(()))
    })
    .await?;
    let backup = client
        .get(running.url("/api/v1/backup"))
        .send()
        .await?
        .bytes()
        .await?;
    let lacking = on_disk(&running, &client, &catalog::multi_boundary()).await?;
    eventually("the lacking torrent's directory", async || {
        Ok(directory(&lacking).is_dir().then_some(()))
    })
    .await?;
    let restored = client
        .put(running.url("/api/v1/backup"))
        .header("content-type", "application/zip")
        .body(backup)
        .send()
        .await?;
    assert_eq!(restored.status(), StatusCode::NO_CONTENT);
    running.ready(&client).await?;
    assert!(
        !directory(&lacking).exists(),
        "the lacking torrent's data is deleted"
    );
    assert!(
        directory(&kept).is_dir(),
        "the backup's torrent keeps its data"
    );
    running.stop().await?;
    Ok(())
}
