// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! Backup and restore through the real engine, and `torrplay backup` on a
//! stopped application.

#![allow(clippy::expect_used)]

mod common;

use std::io::{Cursor, Read, Write};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use common::{MIB, Running, eventually, json, upload};
use reqwest::{Client, StatusCode};
use serde_json::{Value, json};
use torrplay::{BackupCounts, ShutdownTimings, create_backup, restore_backup};
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

/// `archive`'s `backup.json`, without the time it was written.
fn document(archive: &[u8]) -> anyhow::Result<Value> {
    let mut archive = ZipArchive::new(Cursor::new(archive))?;
    let mut document: Value = serde_json::from_reader(archive.by_name("backup.json")?)?;
    document
        .as_object_mut()
        .expect("an object")
        .remove("created_at");
    Ok(document)
}

/// The IDs of the torrents the application lists.
async fn listed(running: &Running, client: &Client) -> anyhow::Result<Vec<Value>> {
    let listing = json(client.get(running.url("/api/v1/torrents")).send().await?).await?;
    Ok(listing["items"]
        .as_array()
        .expect("items")
        .iter()
        .map(|torrent| torrent["id"].clone())
        .collect())
}

/// BAK-13: `torrplay backup create` on a stopped application writes the
/// archive the API would, and `torrplay backup restore` restores it, after
/// refusing an invalid one; the next start adds the backup's torrents and
/// deletes the disk data of the others.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_stopped_application_is_backed_up_and_restored() -> anyhow::Result<()> {
    let downloads = tempfile::tempdir()?;
    let download_dir: PathBuf = downloads.path().to_path_buf();
    let in_downloads = |config: &mut EngineConfig| config.download_dir = download_dir.clone();
    let running = Running::start(Vec::new(), ShutdownTimings::default(), in_downloads).await?;
    let client = Client::new();
    let kept = upload(&running, &client, &catalog::single_small()).await?;
    let through_api = client
        .get(running.url("/api/v1/backup"))
        .send()
        .await?
        .bytes()
        .await?;
    let archives = tempfile::tempdir()?;
    let archive = archives.path().join("backup.zip");
    let running = running
        .restart_after(in_downloads, async |data: &Path| {
            let (path, counts) = create_backup(data, Some(&archive), SystemTime::now()).await?;
            assert_eq!(path, archive);
            assert_eq!(
                counts,
                BackupCounts {
                    torrents: 1,
                    profiles: 2
                },
                "the torrent, the device profile, and the default person"
            );
            let existing = create_backup(data, Some(&archive), SystemTime::now()).await;
            assert!(existing.is_err(), "an existing file is never replaced");
            assert_eq!(
                document(&std::fs::read(&archive)?)?,
                document(&through_api)?,
                "the same archive as the API's"
            );
            Ok(())
        })
        .await?;
    let lacking = on_disk(&running, &client, &catalog::multi_boundary()).await?;
    let directory = |id: &str| download_dir.join(id);
    eventually("the lacking torrent's directory", async || {
        Ok(directory(&lacking).is_dir().then_some(()))
    })
    .await?;
    let before = listed(&running, &client).await?;
    let invalid = archives.path().join("invalid.zip");
    std::fs::write(
        &invalid,
        with_document(&std::fs::read(&archive)?, |document| {
            document["torrents"][0]["info_hash"]["value"] = json!("0".repeat(40));
        })?,
    )?;
    let running = running
        .restart_after(in_downloads, async |data: &Path| {
            let refused = restore_backup(data, &invalid).await;
            let message = format!("{:#}", refused.expect_err("another info hash"));
            assert!(
                message.contains("/torrents/0"),
                "the message points at the torrent: {message}"
            );
            Ok(())
        })
        .await?;
    assert_eq!(
        listed(&running, &client).await?,
        before,
        "a refused restore changes nothing"
    );
    let running = running
        .restart_after(in_downloads, async |data: &Path| {
            let counts = restore_backup(data, &archive).await?;
            assert_eq!(counts.torrents, 1);
            Ok(())
        })
        .await?;
    assert_eq!(listed(&running, &client).await?, [json!(kept.id)]);
    assert!(
        !directory(&lacking).exists(),
        "the next start deletes the lacking torrent's data"
    );
    running.stop().await?;
    Ok(())
}

/// BAK-14: the backup commands refuse a running application, which holds
/// the database's lock, and a data directory without a database.
#[tokio::test]
async fn backup_commands_refuse_a_running_application() -> anyhow::Result<()> {
    let data = tempfile::tempdir()?;
    let archives = tempfile::tempdir()?;
    let archive = archives.path().join("backup.zip");
    let empty = create_backup(&data.path().join("none"), Some(&archive), SystemTime::now()).await;
    assert!(empty.is_err(), "no database to back up");
    assert!(!data.path().join("none").exists(), "no database is created");
    let (running, _) = torrplay_store::Store::open(data.path())?;
    let created = create_backup(data.path(), Some(&archive), SystemTime::now()).await;
    let message = format!("{:#}", created.expect_err("the store is locked"));
    assert!(message.contains("opening the store"), "{message}");
    assert!(!archive.exists(), "no archive is left behind");
    running.close().await?;
    create_backup(data.path(), Some(&archive), SystemTime::now()).await?;
    let (running, _) = torrplay_store::Store::open(data.path())?;
    let restored = restore_backup(data.path(), &archive).await;
    let message = format!("{:#}", restored.expect_err("the store is locked"));
    assert!(message.contains("opening the store"), "{message}");
    running.close().await?;
    Ok(())
}
