// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! Backup and restore, BAK-01 to BAK-12 but those that need the real
//! engine or a killed process.

use std::collections::BTreeMap;
use std::io::{Cursor, Read, Write};
use std::sync::Arc;

use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

use torrplay_core::id::TorrentId;

use super::*;

const BACKUP: &str = "/api/v1/backup";

/// The entries of an archive, by name.
fn entries(archive: &[u8]) -> BTreeMap<String, Vec<u8>> {
    let mut archive = ZipArchive::new(Cursor::new(archive)).expect("a zip archive");
    (0..archive.len())
        .filter_map(|index| {
            let mut file = archive.by_index(index).unwrap();
            if file.is_dir() {
                return None;
            }
            let mut bytes = Vec::new();
            file.read_to_end(&mut bytes).unwrap();
            Some((file.name().unwrap().into_owned(), bytes))
        })
        .collect()
}

/// An archive of `entries`, with directory entries when `directories`.
fn archive_of(entries: &BTreeMap<String, Vec<u8>>, directories: bool) -> Vec<u8> {
    let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
    let options = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Deflated)
        .compression_level(Some(9));
    if directories {
        for directory in ["metainfo/", "posters/", "avatars/"] {
            zip.add_directory(directory, options).unwrap();
        }
    }
    for (name, bytes) in entries {
        zip.start_file(name.as_str(), options).unwrap();
        zip.write_all(bytes).unwrap();
    }
    zip.finish().unwrap().into_inner()
}

/// `entries` with `backup.json` changed by `change`.
fn with_document(
    entries: &BTreeMap<String, Vec<u8>>,
    change: impl FnOnce(&mut Value),
) -> BTreeMap<String, Vec<u8>> {
    let mut entries = entries.clone();
    let mut document: Value = serde_json::from_slice(&entries["backup.json"]).unwrap();
    change(&mut document);
    entries.insert("backup.json".into(), serde_json::to_vec(&document).unwrap());
    entries
}

fn put_backup(archive: Vec<u8>) -> http::Request<Vec<u8>> {
    request(Method::PUT, BACKUP)
        .header(header::CONTENT_TYPE, "application/zip")
        .body(archive)
        .unwrap()
}

/// Downloads a backup and returns its entries.
async fn download(server: &TestServer) -> BTreeMap<String, Vec<u8>> {
    let backup = server.get(BACKUP).await;
    assert_eq!(backup.status, StatusCode::OK, "{backup:?}");
    entries(&backup.body)
}

/// Waits until the torrents a restore adds are back.
async fn restored(server: &TestServer) {
    for _ in 0..200 {
        if server.get("/readyz").await.status == StatusCode::OK {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("the restore never ended");
}

/// The IDs of the torrents listed, persisted ones only, in listing order.
async fn listed(server: &TestServer) -> Vec<String> {
    server.get("/api/v1/torrents").await.json()["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|torrent| torrent["id"].as_str().unwrap().to_owned())
        .collect()
}

fn temporary_files(server: &TestServer) -> Vec<String> {
    std::fs::read_dir(server.data_dir())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .filter(|name| name.starts_with("backup-"))
        .collect()
}

/// A library to back up: an uploaded torrent with a poster and progress, a
/// magnet torrent without metadata, a temporary torrent, a profile with an
/// avatar and title progress, and an explicit setting. Returns the uploaded
/// torrent's path and its first file's content path.
async fn library(server: &TestServer) -> (String, String) {
    let small = catalog::single_small();
    let uploaded = server.upload(&small.metainfo().bytes, None).await.json();
    let path = uploaded["links"]["self"].as_str().unwrap().to_owned();
    let poster = server
        .send(put_poster(&format!("{path}/poster"), "image/png", PNG))
        .await;
    assert_eq!(poster.status, StatusCode::CREATED);
    server.create_magnet(&unknown_magnet(7)).await;
    let temporary = server
        .send(json_request(
            Method::POST,
            "/api/v1/torrents",
            &json!({ "magnet_uri": unknown_magnet(8), "retention": "temporary" }),
        ))
        .await;
    assert_eq!(temporary.status, StatusCode::CREATED);
    let file = &files_of(server, &path).await[0];
    let content = file["links"]["content"].as_str().unwrap().to_owned();
    let profile = default_profile(server).await;
    let file_id = file["id"].as_str().unwrap();
    let torrent_id = uploaded["id"].as_str().unwrap();
    let saved = server
        .send(put_progress(
            &format!("{profile}/torrents/{torrent_id}/progress/{file_id}"),
            30.0,
            600.0,
            false,
        ))
        .await;
    assert_eq!(saved.status, StatusCode::CREATED);
    let sam = server
        .send(json_request(
            Method::POST,
            "/api/v1/profiles",
            &json!({ "name": "Sam" }),
        ))
        .await
        .json();
    let sam = sam["links"]["self"].as_str().unwrap().to_owned();
    let avatar = server
        .send(put_poster(&format!("{sam}/avatar"), "image/png", PNG))
        .await;
    assert_eq!(avatar.status, StatusCode::CREATED);
    let title = server
        .send(put_progress(
            &format!("{sam}/title-progress/tmdb-tv-1399-s1e2"),
            812.0,
            2760.0,
            false,
        ))
        .await;
    assert_eq!(title.status, StatusCode::CREATED);
    let settings = server.get("/api/v1/settings").await;
    let tag = settings.header(header::ETAG).unwrap().to_owned();
    let changed = server
        .send(merge_patch(
            "/api/v1/settings",
            &tag,
            &json!({ "peer_limit": 80 }),
        ))
        .await;
    assert_eq!(changed.status, StatusCode::OK);
    (path, content)
}

/// BAK-01: a download holds every record of the library, its torrent
/// files, posters, and avatars, and nothing secret or temporary.
pub async fn backups_hold_the_records() {
    let server = TestServer::start().await;
    let (path, _) = library(&server).await;
    let id = path.rsplit('/').next().unwrap();
    let backup = server.get(BACKUP).await;
    assert_eq!(backup.status, StatusCode::OK);
    assert_eq!(backup.header(header::CONTENT_TYPE), Some("application/zip"));
    assert_eq!(
        backup.header(header::CONTENT_LENGTH),
        Some(backup.body.len().to_string().as_str())
    );
    assert_eq!(backup.header(header::CACHE_CONTROL), Some("no-store"));
    let disposition = backup.header(header::CONTENT_DISPOSITION).unwrap();
    assert!(
        disposition.starts_with("attachment; filename=\"torrplay-backup-"),
        "{disposition}"
    );
    let entries = entries(&backup.body);
    let document: torrplay_api::Backup =
        serde_json::from_slice(&entries["backup.json"]).expect("backup.json is a Backup");
    assert_eq!(document.format_version, 1);
    assert_eq!(
        document.torrents.len(),
        2,
        "the temporary torrent is left out"
    );
    assert_eq!(document.settings.peer_limit, Some(80));
    assert_eq!(document.profiles.len(), 3, "Default, Devices, and Sam");
    let sam = document
        .profiles
        .iter()
        .find(|profile| profile.name == "Sam")
        .unwrap();
    assert_eq!(sam.title_progress.len(), 1);
    assert_eq!(
        entries[&format!("metainfo/{id}.torrent")],
        catalog::single_small().metainfo().bytes
    );
    assert_eq!(entries[&format!("posters/{id}")], PNG);
    assert_eq!(entries[&format!("avatars/{}", sam.id)], PNG);
    let text = String::from_utf8_lossy(&entries["backup.json"]).into_owned();
    assert!(
        !text.contains("argon2") && !text.contains("tpa_"),
        "no credentials"
    );
    assert!(
        temporary_files(&server).is_empty(),
        "no temporary file stays"
    );

    // Every GET answers HEAD, which the contract leaves implicit here.
    let head = server.send_raw(empty(Method::HEAD, BACKUP)).await;
    assert_eq!(head.status, StatusCode::OK);
    assert!(head.body.is_empty());
    assert_eq!(head.header(header::CONTENT_LENGTH), None);
    server.stop().await;
}

/// BAK-02 and BAK-12: a backup restored on another installation gives the
/// same records under the same IDs, whichever zip tool wrote it again.
pub async fn backups_restore_elsewhere() {
    let source = TestServer::start().await;
    let (path, content) = library(&source).await;
    let order = listed(&source).await;
    let configuration = source.get(&format!("{path}/configuration")).await.json();
    let profiles = source.get("/api/v1/profiles").await.json()["items"].clone();
    let backup = download(&source).await;
    source.stop().await;

    for rewritten in [false, true] {
        let target = TestServer::start().await;
        let archive = if rewritten {
            archive_of(&backup, true)
        } else {
            archive_of(&backup, false)
        };
        let done = target.send(put_backup(archive)).await;
        assert_eq!(done.status, StatusCode::NO_CONTENT, "{done:?}");
        restored(&target).await;
        assert_eq!(
            listed(&target).await,
            order,
            "the persisted torrents, in their order"
        );
        let restored_configuration = target.get(&format!("{path}/configuration")).await.json();
        assert_eq!(restored_configuration, configuration);
        let poster = target.get(&format!("{path}/poster")).await;
        assert_eq!(poster.body, PNG);
        let restored_profiles = target.get("/api/v1/profiles").await.json()["items"].clone();
        assert_eq!(restored_profiles, profiles);
        let settings = target.get("/api/v1/settings").await.json();
        assert_eq!(settings["peer_limit"], 80);
        let streamed = target.get(&content).await;
        assert_eq!(streamed.status, StatusCode::OK, "a saved URL streams");
        assert!(temporary_files(&target).is_empty());
        target.stop().await;
    }
}

/// BAK-03: a restore deletes the torrents the backup lacks, a torrent
/// added again under another ID among them, and ends preloads and
/// temporary torrents, keeping only the backup's disk data.
pub async fn restores_replace_what_the_backup_lacks() {
    let server = TestServer::start().await;
    let small = catalog::single_small();
    let kept = server
        .send(form(&[
            (
                "metainfo",
                Some("application/x-bittorrent"),
                small.metainfo().bytes.clone(),
            ),
            ("storage_mode", None, b"disk".to_vec()),
        ]))
        .await;
    assert_eq!(kept.status, StatusCode::CREATED, "{kept:?}");
    let kept = kept.json()["links"]["self"].as_str().unwrap().to_owned();
    let backup = download(&server).await;

    // After the backup: the torrent deleted and added again, another one
    // in disk storage with a preload, and a temporary torrent.
    assert_eq!(
        server.send(empty(Method::DELETE, &kept)).await.status,
        StatusCode::NO_CONTENT
    );
    let again = server.upload(&small.metainfo().bytes, None).await.json();
    let again = again["links"]["self"].as_str().unwrap().to_owned();
    let other = catalog::multi_boundary();
    let lacking = server
        .send(form(&[
            (
                "metainfo",
                Some("application/x-bittorrent"),
                other.metainfo().bytes.clone(),
            ),
            ("storage_mode", None, b"disk".to_vec()),
        ]))
        .await
        .json();
    let lacking = lacking["links"]["self"].as_str().unwrap().to_owned();
    let file = files_of(&server, &lacking).await[0]["links"]["self"]
        .as_str()
        .unwrap()
        .to_owned();
    let preload = server
        .send(json_request(
            Method::POST,
            &format!("{file}/preloads"),
            &json!({}),
        ))
        .await;
    assert_eq!(preload.status, StatusCode::CREATED);
    let preload = preload.header(header::LOCATION).unwrap().to_owned();
    let temporary = server
        .send(json_request(
            Method::POST,
            "/api/v1/torrents",
            &json!({ "magnet_uri": unknown_magnet(9), "retention": "temporary" }),
        ))
        .await;
    let temporary = temporary.header(header::LOCATION).unwrap().to_owned();

    let done = server.send(put_backup(archive_of(&backup, false))).await;
    assert_eq!(done.status, StatusCode::NO_CONTENT, "{done:?}");
    restored(&server).await;
    for gone in [&again, &lacking, &preload, &temporary] {
        assert_eq!(
            server.get(gone).await.status,
            StatusCode::NOT_FOUND,
            "{gone}"
        );
    }
    assert_eq!(
        server.get(&kept).await.status,
        StatusCode::OK,
        "its old ID is back"
    );
    let kept_id = kept.rsplit('/').next().unwrap();
    assert_eq!(
        server.engine.kept_disk_data(),
        Some(HashSet::from([TorrentId::from_stored(kept_id).unwrap()])),
        "only the backup's disk torrent keeps its data"
    );
    server.stop().await;
}

/// BAK-04: a restore keeps the password and access tokens, and conditional
/// requests from before it fail.
pub async fn restores_keep_credentials_and_change_etags() {
    let server = TestServer::start().await;
    let created = server.create_magnet(&unknown_magnet(3)).await.json();
    let configuration = format!(
        "{}/configuration",
        created["links"]["self"].as_str().unwrap()
    );
    set_first_password(&server).await;
    let (_, secret) = sign_in(&server, "Laptop", "api").await;
    let tag_of = |exchange: &Exchange| exchange.header(header::ETAG).unwrap().to_owned();
    let configuration_tag = tag_of(
        &server
            .send(bearer_empty(Method::GET, &configuration, &secret))
            .await,
    );
    let settings_tag = tag_of(
        &server
            .send(bearer_empty(Method::GET, "/api/v1/settings", &secret))
            .await,
    );
    let backup = server
        .send(bearer_empty(Method::GET, BACKUP, &secret))
        .await;
    assert_eq!(backup.status, StatusCode::OK);
    let done = server
        .send(
            bearer(Method::PUT, BACKUP, &secret)
                .header(header::CONTENT_TYPE, "application/zip")
                .body(backup.body.clone())
                .unwrap(),
        )
        .await;
    assert_eq!(done.status, StatusCode::NO_CONTENT, "{done:?}");
    for _ in 0..200 {
        if server.get("/readyz").await.status == StatusCode::OK {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let listing = server
        .send(bearer_empty(Method::GET, "/api/v1/torrents", &secret))
        .await;
    assert_eq!(listing.status, StatusCode::OK, "the token still works");
    for (path, tag, body) in [
        (
            configuration.as_str(),
            configuration_tag,
            json!({ "title": "After" }),
        ),
        (
            "/api/v1/settings",
            settings_tag,
            json!({ "peer_limit": 60 }),
        ),
    ] {
        let mut stale = merge_patch(path, &tag, &body);
        stale.headers_mut().insert(
            header::AUTHORIZATION,
            format!("Bearer {secret}").parse().unwrap(),
        );
        assert_eq!(
            server.send(stale).await.status,
            StatusCode::PRECONDITION_FAILED,
            "{path}"
        );
    }
    server.stop().await;
}

/// BAK-05: an archive that breaks a rule is refused whole, naming the
/// member at fault where there is one, and changes nothing.
pub async fn invalid_backups_change_nothing() {
    let server = TestServer::start().await;
    let (path, _) = library(&server).await;
    let id = path.rsplit('/').next().unwrap().to_owned();
    let backup = download(&server).await;
    let before = listed(&server).await;
    let sam = {
        let document: Value = serde_json::from_slice(&backup["backup.json"]).unwrap();
        document["profiles"]
            .as_array()
            .unwrap()
            .iter()
            .position(|profile| profile["name"] == "Sam")
            .unwrap()
    };
    let mut without_document = backup.clone();
    without_document.remove("backup.json");
    let mut without_poster = backup.clone();
    without_poster.remove(&format!("posters/{id}"));
    let mut with_extra = backup.clone();
    with_extra.insert("notes.txt".into(), b"extra".to_vec());
    let mut oversized = backup.clone();
    oversized.insert(format!("posters/{id}"), vec![0; (1 << 20) + 1]);
    let mut other_metainfo = backup.clone();
    other_metainfo.insert(
        format!("metainfo/{id}.torrent"),
        catalog::multi_boundary().metainfo().bytes.clone(),
    );
    let at = |pointer: &str| Some(pointer.to_owned());
    let cases: Vec<(&str, Vec<u8>, Option<String>)> = vec![
        ("not a zip", b"not a zip".to_vec(), None),
        ("no backup.json", archive_of(&without_document, false), None),
        (
            "a later format",
            archive_of(
                &with_document(&backup, |doc| doc["format_version"] = json!(2)),
                false,
            ),
            at("/format_version"),
        ),
        (
            "an unknown member",
            archive_of(
                &with_document(&backup, |doc| doc["extra"] = json!(1)),
                false,
            ),
            at("/extra"),
        ),
        (
            "a malformed profile ID",
            archive_of(
                &with_document(&backup, |doc| doc["profiles"][0]["id"] = json!("prf_x")),
                false,
            ),
            at("/profiles/0/id"),
        ),
        (
            "a repeated torrent",
            archive_of(
                &with_document(&backup, |doc| {
                    let first = doc["torrents"][0].clone();
                    doc["torrents"].as_array_mut().unwrap().push(first);
                }),
                false,
            ),
            at("/torrents/2/id"),
        ),
        (
            "a torrent file of another info hash",
            archive_of(&other_metainfo, false),
            at("/torrents/0"),
        ),
        (
            "a missing poster",
            archive_of(&without_poster, false),
            at("/torrents/0/poster"),
        ),
        (
            "an avatar of another size",
            archive_of(
                &with_document(&backup, |doc| {
                    doc["profiles"][sam]["avatar"]["size_bytes"] = json!(PNG.len() + 1);
                }),
                false,
            ),
            Some(format!("/profiles/{sam}/avatar/size_bytes")),
        ),
        (
            "an entry that inflates past its limit",
            archive_of(&oversized, false),
            at("/torrents/0/poster"),
        ),
        (
            "progress of a torrent the backup lacks",
            archive_of(
                &with_document(&backup, |doc| {
                    doc["profiles"][0]["progress"][0]["torrent_id"] =
                        json!("trr_aaaaaaaaaaaaaaaaaaaaaaaaaa");
                }),
                false,
            ),
            at("/profiles/0/progress/0/torrent_id"),
        ),
        (
            "two device profiles",
            archive_of(
                &with_document(&backup, |doc| {
                    doc["profiles"][sam]["kind"] = json!("device");
                }),
                false,
            ),
            at("/profiles"),
        ),
        ("an extra entry", archive_of(&with_extra, false), None),
    ];
    for (case, archive, pointer) in cases {
        let refused = server.send(put_backup(archive)).await;
        assert_problem(
            &refused,
            StatusCode::UNPROCESSABLE_ENTITY,
            "validation-failed",
        );
        let pointers: Vec<String> = refused.json()["errors"]
            .as_array()
            .map(|errors| {
                errors
                    .iter()
                    .filter_map(|error| error["pointer"].as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        match pointer {
            Some(pointer) => assert!(pointers.contains(&pointer), "{case}: {pointers:?}"),
            None => assert!(pointers.is_empty(), "{case}: {pointers:?}"),
        }
    }
    assert_eq!(listed(&server).await, before, "nothing changed");
    assert!(temporary_files(&server).is_empty());
    server.stop().await;
}

/// BAK-06: a body over the limit, of another media type, that stops
/// arriving, or without room is refused; so is a download without room.
pub async fn restore_bounds() {
    let server = TestServer::start_with_body_timeout(Duration::from_millis(200)).await;
    let too_large = server
        .send_before_body_ends(
            Method::PUT,
            BACKUP,
            &[
                ("Content-Type", "application/zip"),
                ("Content-Length", "17179869185"),
            ],
            Vec::new(),
        )
        .await;
    assert_problem(
        &too_large,
        StatusCode::PAYLOAD_TOO_LARGE,
        "content-too-large",
    );
    let json = server
        .send_unchecked(json_request(Method::PUT, BACKUP, &json!({})))
        .await;
    assert_problem(
        &json,
        StatusCode::UNSUPPORTED_MEDIA_TYPE,
        "unsupported-media-type",
    );
    let stalled = server
        .send_before_body_ends(
            Method::PUT,
            BACKUP,
            &[
                ("Content-Type", "application/zip"),
                ("Transfer-Encoding", "chunked"),
            ],
            b"4\r\nPK\x03\x04\r\n".to_vec(),
        )
        .await;
    assert_problem(&stalled, StatusCode::REQUEST_TIMEOUT, "request-timeout");
    server.app.set_data_free_bytes(Some(Arc::new(|| 0)));
    let no_room = server.send(put_backup(b"PK".to_vec())).await;
    assert_problem(
        &no_room,
        StatusCode::INSUFFICIENT_STORAGE,
        "insufficient-storage",
    );
    let no_room = server.get(BACKUP).await;
    assert_problem(
        &no_room,
        StatusCode::INSUFFICIENT_STORAGE,
        "insufficient-storage",
    );
    server.app.set_data_free_bytes(None);
    assert!(temporary_files(&server).is_empty());
    server.stop().await;
}

/// BAK-07 and BAK-08: a download needs no engine, a restore does, and a
/// download whose read fails, or a restore whose write fails, changes
/// nothing and leaves no temporary file.
pub async fn restores_need_the_engine_and_a_write() {
    let server = TestServer::start().await;
    library(&server).await;
    let backup = download(&server).await;
    let before = listed(&server).await;
    server
        .engine
        .set_readiness(Readiness::Unavailable { retry_after: None });
    assert_eq!(server.get(BACKUP).await.status, StatusCode::OK);
    let refused = server.send(put_backup(archive_of(&backup, false))).await;
    assert_problem(
        &refused,
        StatusCode::SERVICE_UNAVAILABLE,
        "engine-unavailable",
    );
    server.engine.set_readiness(Readiness::Ready);

    server.app.store().fail_reads(true);
    let failed = server.get(BACKUP).await;
    assert_problem(&failed, StatusCode::INTERNAL_SERVER_ERROR, "internal-error");
    server.app.store().fail_reads(false);
    assert!(temporary_files(&server).is_empty());

    server.app.store().fail_writes(true);
    let failed = server.send(put_backup(archive_of(&backup, false))).await;
    assert_problem(&failed, StatusCode::INTERNAL_SERVER_ERROR, "internal-error");
    server.app.store().fail_writes(false);
    assert_eq!(listed(&server).await, before, "nothing changed");

    server.app.begin_shutdown();
    assert_eq!(server.get(BACKUP).await.status, StatusCode::OK);
    let refused = server.send(put_backup(archive_of(&backup, false))).await;
    assert_problem(
        &refused,
        StatusCode::SERVICE_UNAVAILABLE,
        "engine-unavailable",
    );
    server.stop().await;
}

/// BAK-11: restored settings apply at once, and command-line overrides
/// stay in effect.
pub async fn restored_settings_apply_at_once() {
    let server = TestServer::start().await;
    let backup = download(&server).await;
    let server = server
        .restart_overriding(Overrides {
            log_level: Some(LogOverride::Level(LogLevel::Debug)),
            ..Overrides::default()
        })
        .await;
    let changed = with_document(&backup, |doc| {
        doc["settings"] = json!({
            "dlna_enabled": true,
            "cache_capacity_bytes": 134_217_728,
            "log_level": "warn",
        });
    });
    let done = server.send(put_backup(archive_of(&changed, false))).await;
    assert_eq!(done.status, StatusCode::NO_CONTENT, "{done:?}");
    restored(&server).await;
    assert!(server.app.media_server_setting().borrow().enabled);
    assert_eq!(server.engine.settings().cache_capacity_bytes, 134_217_728);
    let settings = server.get("/api/v1/settings").await.json();
    assert_eq!(settings["dlna_enabled"], true);
    assert_eq!(settings["log_level"], "warn");
    assert_eq!(settings["overrides"]["log_level"], "debug");
    server.stop().await;
}
