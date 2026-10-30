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

use common::{MIB, Running, eventually, expected, json, set_desired_state, upload};
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
         --{BOUNDARY}\r\nContent-Disposition: form-data; name=\"metainfo\"; \
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

/// Creates `spec` in disk storage and returns its ID, its path, and the
/// content path of each file.
async fn create_on_disk(
    running: &Running,
    client: &Client,
    spec: &torrplay_fixtures::FixtureSpec,
) -> anyhow::Result<(String, String, Vec<String>)> {
    let (content_type, body) = disk_upload(&spec.metainfo().bytes);
    let created = client
        .post(running.url("/api/v1/torrents"))
        .header("content-type", content_type)
        .body(body)
        .send()
        .await?;
    assert_eq!(created.status(), StatusCode::CREATED);
    let torrent = json(created).await?;
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
    let content = files["items"]
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
    Ok((id, path, content))
}

/// Streams file `index` of `spec` from `link` and checks it byte for byte.
async fn stream_file(
    running: &Running,
    client: &Client,
    spec: &torrplay_fixtures::FixtureSpec,
    link: &str,
    index: usize,
) -> anyhow::Result<()> {
    let response = client.get(running.url(link)).send().await?;
    let status = response.status();
    if status != StatusCode::OK {
        anyhow::bail!("{status}: {}", response.text().await?);
    }
    let size = spec.files()[index].size_bytes;
    assert!(
        response.bytes().await? == expected(spec, index, 0, size),
        "file {index}"
    );
    Ok(())
}

/// DSK-04 and the orphan sweep: a transition that a crash interrupted runs
/// again at startup, and entries of the download directory named like
/// torrent IDs that no disk torrent owns are deleted, other entries kept.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn startup_finishes_interrupted_transitions_and_sweeps_orphans() -> anyhow::Result<()> {
    let spec = catalog::single_small();
    let seeder = Seeder::start(&spec, &spec.metainfo().bytes).await?;
    let downloads = tempfile::tempdir()?;
    let download_dir: PathBuf = downloads.path().to_path_buf();
    let in_downloads = |config: &mut EngineConfig| config.download_dir = download_dir.clone();
    let client = Client::new();
    let running =
        Running::start(vec![seeder.addr], ShutdownTimings::default(), in_downloads).await?;
    let (id, path, content) = create_on_disk(&running, &client, &spec).await?;
    stream_file(&running, &client, &spec, &content[0], 0).await?;
    let directory = download_dir.join(&id);
    assert!(directory.is_dir());

    let orphan = torrplay_core::id::TorrentId::from_random_bytes([7; 16]).to_string();
    let running = running
        .restart_after(in_downloads, async |data: &std::path::Path| {
            // The configuration asks for memory storage, as when a crash
            // interrupted the transition to it.
            let (store, contents) = torrplay_store::Store::open(data)?;
            let stored = &contents.torrents[0];
            let mut configuration = stored.configuration.clone();
            configuration.set_storage_mode(torrplay_core::lifecycle::StorageMode::Memory);
            store.set_configuration(&stored.id, &configuration).await?;
            store.record_clean_shutdown().await?;
            store.close().await?;
            std::fs::create_dir_all(download_dir.join(&orphan).join("x"))?;
            std::fs::write(download_dir.join(format!("{orphan}.have")), b"x")?;
            std::fs::write(download_dir.join("notes.txt"), b"kept")?;
            Ok(())
        })
        .await?;

    eventually("the transition finishing", async || {
        let torrent = json(client.get(running.url(&path)).send().await?).await?;
        Ok(
            (torrent["storage_mode"] == "memory" && torrent["storage_transition"].is_null())
                .then_some(()),
        )
    })
    .await?;
    assert!(!directory.exists(), "memory storage keeps nothing on disk");
    assert!(!download_dir.join(&orphan).exists(), "an orphan is deleted");
    assert!(!download_dir.join(format!("{orphan}.have")).exists());
    assert!(
        download_dir.join("notes.txt").is_file(),
        "other entries stay"
    );
    stream_file(&running, &client, &spec, &content[0], 0).await?;

    running.stop().await?;
    seeder.stop().await;
    Ok(())
}

/// DSK-07: after an unclean shutdown, a disk torrent checks its data in
/// full instead of trusting its resume bitfield, so a piece the bitfield
/// claims but the file lost is downloaded again rather than served.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_unclean_shutdown_checks_disk_data_in_full() -> anyhow::Result<()> {
    // Enough pieces that sampling the bitfield would likely miss the one
    // damaged.
    let spec = catalog::large(16 * MIB);
    let seeder = Seeder::start(&spec, &spec.metainfo().bytes).await?;
    let downloads = tempfile::tempdir()?;
    let download_dir: PathBuf = downloads.path().to_path_buf();
    let in_downloads = |config: &mut EngineConfig| config.download_dir = download_dir.clone();
    let client = Client::new();
    let running =
        Running::start(vec![seeder.addr], ShutdownTimings::default(), in_downloads).await?;
    let (id, _path, content) = create_on_disk(&running, &client, &spec).await?;
    stream_file(&running, &client, &spec, &content[0], 0).await?;

    let file = download_dir.join(&id).join(&spec.torrent_name);
    let running = running
        .crash_and_restart(in_downloads, async || {
            // Data a power loss kept from reaching the disk, which the
            // bitfield already records.
            use std::os::unix::fs::FileExt as _;
            let damaged = std::fs::OpenOptions::new().write(true).open(&file)?;
            damaged.write_all_at(&vec![0; MIB as usize], 40 * MIB)?;
            Ok(())
        })
        .await?;
    stream_file(&running, &client, &spec, &content[0], 0).await?;

    running.stop().await?;
    seeder.stop().await;
    Ok(())
}

/// DSK-08: a disk torrent whose file was emptied from outside fails when a
/// read finds the data gone; starting it again checks its data in full and
/// downloads the file again.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_disk_torrent_fails_when_its_data_goes_and_recovers() -> anyhow::Result<()> {
    let spec = catalog::single_small();
    let seeder = Seeder::start(&spec, &spec.metainfo().bytes).await?;
    let downloads = tempfile::tempdir()?;
    let download_dir: PathBuf = downloads.path().to_path_buf();
    let in_downloads = |config: &mut EngineConfig| config.download_dir = download_dir.clone();
    let client = Client::new();
    let running =
        Running::start(vec![seeder.addr], ShutdownTimings::default(), in_downloads).await?;
    let (id, path, content) = create_on_disk(&running, &client, &spec).await?;
    stream_file(&running, &client, &spec, &content[0], 0).await?;

    std::fs::OpenOptions::new()
        .write(true)
        .open(download_dir.join(&id).join(&spec.torrent_name))?
        .set_len(0)?;
    // The stream ends where the data is gone, failing the request if the
    // response had not begun.
    if let Ok(response) = client.get(running.url(&content[0])).send().await {
        assert!(
            response.bytes().await.is_err(),
            "the stream ends where the data is gone"
        );
    }
    eventually("the torrent failing", async || {
        let torrent = json(client.get(running.url(&path)).send().await?).await?;
        Ok((torrent["observed_state"] == "failed").then_some(()))
    })
    .await?;

    set_desired_state(&running, &client, &path, "paused").await?;
    set_desired_state(&running, &client, &path, "started").await?;
    stream_file(&running, &client, &spec, &content[0], 0).await?;

    running.stop().await?;
    seeder.stop().await;
    Ok(())
}
