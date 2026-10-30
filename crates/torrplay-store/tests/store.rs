// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! The store against real database files.

// Cases and their helpers assert as tests do; clippy's test exemption
// covers only test functions.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, SystemTime};

use tempfile::TempDir;
use torrplay_core::credentials::{AccessToken, PasswordHash, Scope, SigningKey, new_secret};
use torrplay_core::id::{
    AccessTokenId, FileId, InfoHashV1, ProfileId, TORRENT_ID_RANDOM_BYTES, TorrentId, UpNextEntryId,
};
use torrplay_core::image::{ImageInfo, ImageType};
use torrplay_core::library::{Kind, Library};
use torrplay_core::lifecycle::{Configuration, DesiredState, StorageMode};
use torrplay_core::profiles::{Profile, ProfileKind, Progress, TitleKey, TitleProgress};
use torrplay_core::settings::{Explicit, LogLevel};
use torrplay_store::{
    FILE_NAME, Store, StoreError, StoredDismissal, StoredProgress, StoredSettings,
    StoredTemporaryProgress, StoredTitleProgress, StoredTorrent,
};

/// Turns a database back into version 16, before IMDb IDs.
const UNDO_VERSION_17: &str = "ALTER TABLE torrents DROP COLUMN imdb_id;";
/// Turns a database back into version 15, before the metrics scope, keeping
/// the access tokens.
const UNDO_VERSION_16: &str = "CREATE TABLE access_tokens_v15 (
        id TEXT PRIMARY KEY,
        secret_digest BLOB NOT NULL CHECK (length(secret_digest) = 32),
        name TEXT NOT NULL,
        scope TEXT NOT NULL CHECK (scope IN ('api', 'playback')),
        created_at_ns INTEGER NOT NULL CHECK (created_at_ns >= 0),
        expires_at_ns INTEGER CHECK (expires_at_ns >= 0),
        last_used_at_ns INTEGER CHECK (last_used_at_ns >= 0),
        CONSTRAINT access_tokens_secret_digest_key UNIQUE (secret_digest)
    ) STRICT;
    INSERT INTO access_tokens_v15 SELECT * FROM access_tokens;
    DROP TABLE access_tokens;
    ALTER TABLE access_tokens_v15 RENAME TO access_tokens;";

/// Turns a database back into version 14, before temporary torrents'
/// progress.
const UNDO_VERSION_15: &str = "DROP TABLE temporary_progress;
    ALTER TABLE settings DROP COLUMN temporary_progress_seconds;";

/// Turns a database back into version 13, before title progress.
const UNDO_VERSION_14: &str = "DROP TABLE title_progress;";

/// Turns a database back into version 12, before profile kinds.
const UNDO_VERSION_13: &str = "DROP INDEX profiles_kind_device_key;
    ALTER TABLE profiles DROP COLUMN kind;";

/// Turns a database back into version 11, before the temporary torrents'
/// settings.
const UNDO_VERSION_12: &str = "ALTER TABLE settings DROP COLUMN temporary_torrents_max;
    ALTER TABLE settings DROP COLUMN temporary_idle_seconds;";

/// Turns a database back into version 10, before the name, DHT, and log
/// level settings.
const UNDO_VERSION_11: &str = "ALTER TABLE settings DROP COLUMN log_level;
    ALTER TABLE settings DROP COLUMN dht_enabled;
    ALTER TABLE settings DROP COLUMN dlna_name;";

/// Turns a database back into version 9, before the media server.
const UNDO_VERSION_10: &str = "DROP TABLE media_server;
    ALTER TABLE settings DROP COLUMN dlna_enabled;";

/// Turns a database back into version 8, before credentials.
const UNDO_VERSION_9: &str = "DROP TABLE access_tokens;
    DROP TABLE credentials;";

/// Turns a database back into version 7, before profiles.
const UNDO_VERSION_8: &str = "DROP TABLE up_next_dismissals;
    DROP TABLE progress;
    DROP TABLE profile_avatars;
    DROP TABLE profiles;";

/// Turns a database back into version 6, before the library.
const UNDO_VERSION_7: &str = "DROP TABLE torrent_posters;
    DROP TABLE torrent_genres;
    DROP TABLE torrent_categories;
    ALTER TABLE torrents DROP COLUMN description;
    ALTER TABLE torrents DROP COLUMN year;
    ALTER TABLE torrents DROP COLUMN kind;
    ALTER TABLE torrents DROP COLUMN title;";

fn id(n: u32) -> TorrentId {
    let mut bytes = [0; TORRENT_ID_RANDOM_BYTES];
    bytes[..4].copy_from_slice(&n.to_be_bytes());
    TorrentId::from_random_bytes(bytes)
}

fn info_hash(n: u32) -> InfoHashV1 {
    let mut bytes = [0; 20];
    bytes[..4].copy_from_slice(&n.to_be_bytes());
    InfoHashV1::from_bytes(bytes)
}

/// A torrent created `nanos` after the epoch: from a torrent file if `n` is
/// even, from a magnet URI otherwise.
fn torrent(n: u32, nanos: u64) -> StoredTorrent {
    let file = n.is_multiple_of(2);
    StoredTorrent {
        id: id(n),
        info_hash: info_hash(n),
        created_at: SystemTime::UNIX_EPOCH + Duration::from_nanos(nanos),
        // Every third asks for disk storage that is not in effect yet, as
        // after an interrupted transition, so both modes must round-trip.
        configuration: Configuration::new(DesiredState::Started).with_storage_mode(
            if n.is_multiple_of(3) {
                StorageMode::Disk
            } else {
                StorageMode::Memory
            },
        ),
        storage_mode: StorageMode::Memory,
        magnet_uri: (!file).then(|| format!("magnet:?xt=urn:btih:{}", info_hash(n))),
        torrent_file: file.then(|| vec![(n % 251) as u8; 1000 + n as usize]),
        poster: None,
    }
}

/// Opens the store in `dir` and returns it with its torrents.
fn open(dir: &TempDir) -> (Store, Vec<StoredTorrent>) {
    let (store, contents) = Store::open(dir.path()).expect("the store opens");
    (store, contents.torrents)
}

#[tokio::test]
async fn torrents_survive_a_reopen_in_creation_order() {
    let dir = TempDir::new().unwrap();
    // Out of insertion order, two sharing a creation time to the nanosecond,
    // and one with sub-millisecond precision.
    let torrents = [
        torrent(1, 3_000_000_123),
        torrent(2, 1_000_000_000),
        torrent(3, 2_000_000_000),
        torrent(4, 2_000_000_000),
        torrent(5, 1_700_000_000_000_000_001),
    ];
    {
        let (store, stored) = open(&dir);
        assert!(stored.is_empty(), "a new database is empty");
        for torrent in &torrents {
            store.insert(torrent.clone()).await.unwrap();
        }
        store.close().await.unwrap();
    }
    let (_store, stored) = open(&dir);
    let mut expected = torrents.to_vec();
    expected.sort_by(|a, b| (a.created_at, &a.id).cmp(&(b.created_at, &b.id)));
    assert_eq!(stored, expected);
}

#[tokio::test]
async fn configurations_and_retrieved_torrent_files_are_stored() {
    let dir = TempDir::new().unwrap();
    let magnet = torrent(1, 10);
    let upload = torrent(2, 20);
    let mut paused = magnet.configuration.clone();
    paused.set_desired_state(DesiredState::Paused);
    {
        let (store, _) = open(&dir);
        store.insert(magnet.clone()).await.unwrap();
        store.insert(upload.clone()).await.unwrap();
        store.set_configuration(&magnet.id, &paused).await.unwrap();
        store
            .set_torrent_file(&magnet.id, b"retrieved".to_vec())
            .await
            .unwrap();
        store
            .set_torrent_file(&magnet.id, b"retrieved again".to_vec())
            .await
            .unwrap();
        store
            .set_torrent_file(&upload.id, b"another".to_vec())
            .await
            .unwrap();
        assert!(matches!(
            store.set_configuration(&id(9), &paused).await,
            Err(StoreError::UnknownTorrent)
        ));
        assert!(matches!(
            store.set_torrent_file(&id(9), b"x".to_vec()).await,
            Err(StoreError::UnknownTorrent)
        ));
    }
    let (_store, stored) = open(&dir);
    assert_eq!(stored[0].configuration, paused);
    assert_eq!(
        stored[0].configuration.etag(),
        paused.etag(),
        "the entity tag survives"
    );
    assert_eq!(
        stored[0].torrent_file.as_deref(),
        Some(&b"retrieved"[..]),
        "the first torrent file is kept"
    );
    assert_eq!(
        stored[0].magnet_uri, magnet.magnet_uri,
        "and the magnet URI"
    );
    assert_eq!(stored[1], upload, "an upload keeps its original bytes");
}

#[tokio::test]
async fn deletion_is_durable_and_frees_the_info_hash() {
    let dir = TempDir::new().unwrap();
    {
        let (store, _) = open(&dir);
        store.insert(torrent(2, 10)).await.unwrap();
        store.insert(torrent(3, 20)).await.unwrap();
        store.delete(&id(2)).await.unwrap();
        store.delete(&id(2)).await.unwrap();
        let mut again = torrent(2, 30);
        again.id = id(7);
        store.insert(again).await.unwrap();
    }
    let (store, stored) = open(&dir);
    drop(store);
    let ids: Vec<&TorrentId> = stored.iter().map(|torrent| &torrent.id).collect();
    assert_eq!(ids, [&id(3), &id(7)]);
    let orphans: i64 = rusqlite_count(&dir, "SELECT count(*) FROM torrent_files");
    assert_eq!(orphans, 1, "the deleted torrent's file went with it");
}

/// Reads a count from a closed store's database file.
fn rusqlite_count(dir: &TempDir, sql: &str) -> i64 {
    // The open store holds the file's lock; this runs after it was dropped.
    let connection = rusqlite::Connection::open(dir.path().join(FILE_NAME)).unwrap();
    connection.query_row(sql, [], |row| row.get(0)).unwrap()
}

#[tokio::test]
async fn a_second_store_cannot_open_the_same_database() {
    let dir = TempDir::new().unwrap();
    let (store, _) = open(&dir);
    assert!(matches!(
        Store::open(dir.path()),
        Err(StoreError::InUse { .. })
    ));
    store.close().await.unwrap();
    assert!(matches!(
        store.insert(torrent(1, 1)).await,
        Err(StoreError::Closed)
    ));
    let (_reopened, _) = open(&dir);
}

#[tokio::test]
async fn a_newer_schema_is_refused() {
    let dir = TempDir::new().unwrap();
    drop(open(&dir));
    rusqlite::Connection::open(dir.path().join(FILE_NAME))
        .unwrap()
        .pragma_update(None, "user_version", 99)
        .unwrap();
    assert!(matches!(
        Store::open(dir.path()),
        Err(StoreError::TooNew {
            found: 99,
            supported: 17
        })
    ));
}

#[tokio::test]
async fn an_inconsistent_row_fails_the_open() {
    let dir = TempDir::new().unwrap();
    drop(open(&dir));
    rusqlite::Connection::open(dir.path().join(FILE_NAME))
        .unwrap()
        .execute(
            "INSERT INTO torrents (id, info_hash, created_at_ns, desired_state,
                                   configuration_version, magnet_uri)
             VALUES ('trr_short', zeroblob(20), 1, 'started', 1, 'magnet:?')",
            [],
        )
        .unwrap();
    assert!(matches!(
        Store::open(dir.path()),
        Err(StoreError::Corrupt(detail)) if detail.contains("malformed ID")
    ));
}

#[tokio::test]
async fn a_failed_write_stores_nothing() {
    let dir = TempDir::new().unwrap();
    {
        let (store, _) = open(&dir);
        store.insert(torrent(1, 1)).await.unwrap();
        store.fail_writes(true);
        assert!(matches!(
            store.insert(torrent(2, 2)).await,
            Err(StoreError::Injected)
        ));
        assert!(matches!(
            store.delete(&id(1)).await,
            Err(StoreError::Injected)
        ));
        store.fail_writes(false);
        store.insert(torrent(3, 3)).await.unwrap();
    }
    let (_store, stored) = open(&dir);
    let ids: Vec<&TorrentId> = stored.iter().map(|torrent| &torrent.id).collect();
    assert_eq!(ids, [&id(1), &id(3)]);
}

#[tokio::test]
async fn settings_keep_explicit_values_and_their_version() {
    let dir = TempDir::new().unwrap();
    {
        let (store, contents) = Store::open(dir.path()).unwrap();
        assert_eq!(
            contents.settings,
            StoredSettings::default(),
            "a new database has every setting unset"
        );
        let stored = StoredSettings {
            explicit: Explicit {
                idle_timeout_seconds: Some(0),
                peer_limit: Some(80),
                background_downloads: Some(false),
                ..Explicit::default()
            },
            version: 4,
        };
        store.set_settings(stored.clone()).await.unwrap();
        store
            .set_settings(StoredSettings {
                version: 5,
                ..stored
            })
            .await
            .unwrap();
    }
    let (_store, contents) = Store::open(dir.path()).unwrap();
    assert_eq!(contents.settings.version, 5);
    assert_eq!(
        contents.settings.explicit.idle_timeout_seconds,
        Some(0),
        "zero stays set"
    );
    assert_eq!(contents.settings.explicit.peer_limit, Some(80));
    assert_eq!(
        contents.settings.explicit.background_downloads,
        Some(false),
        "false stays set"
    );
    assert_eq!(
        contents.settings.explicit.piece_wait_seconds, None,
        "unset stays unset"
    );
}

#[tokio::test]
async fn a_version_1_database_is_copied_then_migrated() {
    let dir = TempDir::new().unwrap();
    {
        let (store, _) = open(&dir);
        store.insert(torrent(2, 10)).await.unwrap();
    }
    // A database as version 1 left it.
    {
        let connection = rusqlite::Connection::open(dir.path().join(FILE_NAME)).unwrap();
        connection
            .execute_batch(&format!(
                "{UNDO_VERSION_17} {UNDO_VERSION_16} {UNDO_VERSION_15} {UNDO_VERSION_14} {UNDO_VERSION_13} {UNDO_VERSION_12} {UNDO_VERSION_11} {UNDO_VERSION_10} {UNDO_VERSION_9} {UNDO_VERSION_8}
                 {UNDO_VERSION_7}
                 DROP TABLE settings;
                 ALTER TABLE torrents DROP COLUMN desired_storage_mode;
                 DROP TABLE runs;
                 PRAGMA user_version = 1;",
            ))
            .unwrap();
    }
    let (store, contents) = Store::open(dir.path()).unwrap();
    assert_eq!(contents.torrents, [torrent(2, 10)]);
    assert_eq!(contents.settings, StoredSettings::default());
    store
        .set_settings(StoredSettings {
            version: 2,
            ..StoredSettings::default()
        })
        .await
        .unwrap();
    drop(store);

    let backup = dir.path().join(format!("{FILE_NAME}.v1.bak"));
    let copy = rusqlite::Connection::open(&backup).unwrap();
    let version: u32 = copy
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    assert_eq!(version, 1, "the copy is the database before the migration");
    let torrents: i64 = copy
        .query_row("SELECT count(*) FROM torrents", [], |row| row.get(0))
        .unwrap();
    assert_eq!(torrents, 1);
    let modified = std::fs::metadata(&backup).unwrap().modified().unwrap();
    drop(copy);

    // A migration that did not finish keeps the first copy.
    {
        let connection = rusqlite::Connection::open(dir.path().join(FILE_NAME)).unwrap();
        connection
            .execute_batch(&format!(
                "{UNDO_VERSION_17} {UNDO_VERSION_16} {UNDO_VERSION_15} {UNDO_VERSION_14} {UNDO_VERSION_13} {UNDO_VERSION_12} {UNDO_VERSION_11} {UNDO_VERSION_10} {UNDO_VERSION_9} {UNDO_VERSION_8}
                 {UNDO_VERSION_7}
                 DROP TABLE settings;
                 ALTER TABLE torrents DROP COLUMN desired_storage_mode;
                 DROP TABLE runs;
                 PRAGMA user_version = 1;",
            ))
            .unwrap();
    }
    drop(Store::open(dir.path()).unwrap());
    assert_eq!(
        std::fs::metadata(&backup).unwrap().modified().unwrap(),
        modified,
        "an existing copy is not replaced"
    );
}

#[tokio::test]
async fn settings_from_version_2_keep_their_values_and_lack_the_cache_capacity() {
    let dir = TempDir::new().unwrap();
    {
        let (store, _) = open(&dir);
        store
            .set_settings(StoredSettings {
                explicit: Explicit {
                    piece_wait_seconds: Some(90),
                    ..Explicit::default()
                },
                version: 2,
            })
            .await
            .unwrap();
    }
    // A database as version 2 left it.
    {
        let connection = rusqlite::Connection::open(dir.path().join(FILE_NAME)).unwrap();
        connection
            .execute_batch(&format!(
                "{UNDO_VERSION_17} {UNDO_VERSION_16} {UNDO_VERSION_15} {UNDO_VERSION_14} {UNDO_VERSION_13} {UNDO_VERSION_12} {UNDO_VERSION_11} {UNDO_VERSION_10} {UNDO_VERSION_9} {UNDO_VERSION_8}
                 {UNDO_VERSION_7}
                 ALTER TABLE settings DROP COLUMN cache_capacity_bytes;
                 ALTER TABLE settings DROP COLUMN background_downloads;
                 ALTER TABLE torrents DROP COLUMN desired_storage_mode;
                 DROP TABLE runs;
                 PRAGMA user_version = 2;",
            ))
            .unwrap();
    }
    let (store, contents) = Store::open(dir.path()).unwrap();
    assert_eq!(
        contents.settings.explicit,
        Explicit {
            piece_wait_seconds: Some(90),
            ..Explicit::default()
        },
        "the cache capacity is unset, so it takes the current default"
    );
    let stored = StoredSettings {
        explicit: Explicit {
            cache_capacity_bytes: Some(64 << 30),
            ..contents.settings.explicit
        },
        version: 3,
    };
    store.set_settings(stored.clone()).await.unwrap();
    drop(store);
    let (_store, contents) = Store::open(dir.path()).unwrap();
    assert_eq!(contents.settings, stored);
    assert!(dir.path().join(format!("{FILE_NAME}.v2.bak")).exists());
}

#[tokio::test]
async fn settings_from_version_5_lack_the_background_downloads_switch() {
    let dir = TempDir::new().unwrap();
    {
        let (store, _) = open(&dir);
        store
            .set_settings(StoredSettings {
                explicit: Explicit {
                    peer_limit: Some(20),
                    ..Explicit::default()
                },
                version: 2,
            })
            .await
            .unwrap();
    }
    // A database as version 5 left it.
    {
        let connection = rusqlite::Connection::open(dir.path().join(FILE_NAME)).unwrap();
        connection
            .execute_batch(&format!(
                "{UNDO_VERSION_17} {UNDO_VERSION_16} {UNDO_VERSION_15} {UNDO_VERSION_14} {UNDO_VERSION_13} {UNDO_VERSION_12} {UNDO_VERSION_11} {UNDO_VERSION_10} {UNDO_VERSION_9} {UNDO_VERSION_8}
                 {UNDO_VERSION_7}
                 ALTER TABLE settings DROP COLUMN background_downloads;
                 PRAGMA user_version = 5;",
            ))
            .unwrap();
    }
    let (_store, contents) = Store::open(dir.path()).unwrap();
    assert_eq!(
        contents.settings.explicit,
        Explicit {
            peer_limit: Some(20),
            ..Explicit::default()
        },
        "the switch is unset, so it takes the current default"
    );
    assert!(dir.path().join(format!("{FILE_NAME}.v5.bak")).exists());
}

#[tokio::test]
async fn torrents_from_version_3_use_memory_storage() {
    let dir = TempDir::new().unwrap();
    {
        let (store, _) = open(&dir);
        store.insert(torrent(1, 10)).await.unwrap();
    }
    // A database as version 3 left it.
    {
        let connection = rusqlite::Connection::open(dir.path().join(FILE_NAME)).unwrap();
        connection
            .execute_batch(&format!(
                "{UNDO_VERSION_17} {UNDO_VERSION_16} {UNDO_VERSION_15} {UNDO_VERSION_14} {UNDO_VERSION_13} {UNDO_VERSION_12} {UNDO_VERSION_11} {UNDO_VERSION_10} {UNDO_VERSION_9} {UNDO_VERSION_8}
                 {UNDO_VERSION_7}
                 ALTER TABLE settings DROP COLUMN background_downloads;
                 ALTER TABLE torrents DROP COLUMN desired_storage_mode;
                 DROP TABLE runs;
                 PRAGMA user_version = 3;",
            ))
            .unwrap();
    }
    let (_store, stored) = open(&dir);
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].configuration.storage_mode(), StorageMode::Memory);
    assert_eq!(stored[0].storage_mode, StorageMode::Memory);
    assert!(dir.path().join(format!("{FILE_NAME}.v3.bak")).exists());
}

#[tokio::test]
async fn a_configured_storage_mode_is_stored_apart_from_the_one_in_effect() {
    let dir = TempDir::new().unwrap();
    {
        let (store, _) = open(&dir);
        let mut disk = torrent(2, 20);
        disk.configuration = disk.configuration.with_storage_mode(StorageMode::Disk);
        disk.storage_mode = StorageMode::Disk;
        store.insert(disk).await.unwrap();
    }
    let (store, stored) = open(&dir);
    assert_eq!(stored[0].configuration.storage_mode(), StorageMode::Disk);
    assert_eq!(stored[0].storage_mode, StorageMode::Disk);
    // A completed transition records the mode in effect alone.
    store
        .set_storage_mode(&stored[0].id, StorageMode::Memory)
        .await
        .unwrap();
    assert!(matches!(
        store.set_storage_mode(&id(99), StorageMode::Memory).await,
        Err(StoreError::UnknownTorrent)
    ));
    drop(store);
    let (_store, stored) = open(&dir);
    assert_eq!(stored[0].storage_mode, StorageMode::Memory);
    assert_eq!(stored[0].configuration.storage_mode(), StorageMode::Disk);
}

#[tokio::test]
async fn a_run_counts_as_clean_only_once_it_records_its_shutdown() {
    let dir = TempDir::new().unwrap();
    let (store, contents) = Store::open(dir.path()).unwrap();
    assert!(contents.clean_shutdown, "a new database has no unclean run");
    drop(store);
    let (store, contents) = Store::open(dir.path()).unwrap();
    assert!(
        !contents.clean_shutdown,
        "the run before ended without recording its shutdown"
    );
    store.record_clean_shutdown().await.unwrap();
    store.close().await.unwrap();
    let (_store, contents) = Store::open(dir.path()).unwrap();
    assert!(contents.clean_shutdown);
}

const CHILD: &str = "TORRPLAY_STORE_CHILD";

/// The writing child of the recovery test: inserts torrents with 64 KiB
/// torrent files, reporting each, until killed.
#[test]
#[ignore = "run by the recovery test as its writing child"]
fn child() {
    let Ok(path) = std::env::var(CHILD) else {
        return;
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    runtime.block_on(async {
        let (store, contents) = Store::open(Path::new(&path)).unwrap();
        let stored = contents.torrents;
        let mut out = std::io::stdout();
        for n in (stored.len() as u32 * 2..).step_by(2) {
            let mut torrent = torrent(n, u64::from(n));
            torrent.torrent_file = Some(vec![(n % 251) as u8; 64 * 1024]);
            store.insert(torrent).await.unwrap();
            writeln!(out, "inserted {}", n / 2 + 1).unwrap();
            out.flush().unwrap();
        }
    });
}

fn spawn(dir: &TempDir) -> Child {
    Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "child", "--ignored", "--nocapture"])
        .env(CHILD, dir.path())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap()
}

/// Reads the child's output until it reports an insert, and returns how many
/// it reported in all.
fn wait_for_insert(child: &mut Child) -> u64 {
    let mut reader = BufReader::new(child.stdout.as_mut().unwrap());
    let mut line = String::new();
    loop {
        line.clear();
        assert!(reader.read_line(&mut line).unwrap() > 0, "the child ended");
        if let Some(count) = line.trim().strip_prefix("inserted ") {
            return count.parse().unwrap();
        }
    }
}

#[test]
fn inserts_killed_at_any_moment_leave_whole_torrents() {
    let dir = TempDir::new().unwrap();
    let mut stored_before = 0;
    for (round, delay_ms) in [0u64, 1, 3, 7, 11, 17, 23, 31].into_iter().enumerate() {
        let mut child = spawn(&dir);
        let reported = wait_for_insert(&mut child);
        std::thread::sleep(Duration::from_millis(delay_ms));
        child.kill().unwrap();
        child.wait().unwrap();

        let (store, contents) = Store::open(dir.path()).expect("the store opens after a kill");
        let stored = contents.torrents;
        drop(store);
        for torrent in &stored {
            let n = u32::from_be_bytes(torrent.info_hash.as_bytes()[..4].try_into().unwrap());
            let mut expected = self::torrent(n, u64::from(n));
            expected.torrent_file = Some(vec![(n % 251) as u8; 64 * 1024]);
            assert_eq!(torrent, &expected, "round {round}: whole torrents only");
        }
        let count = stored.len() as u64;
        assert!(count >= reported, "round {round}: reported inserts survive");
        assert!(
            count >= stored_before,
            "round {round}: nothing stored is lost"
        );
        stored_before = count;
    }
}

fn library() -> Library {
    Library {
        title: Some("Example Film".into()),
        kind: Some(Kind::Movie),
        year: Some(2026),
        imdb_id: Some("tt0133093".into()),
        description: Some("Line one.\n\tLine two.".into()),
        categories: vec!["Movies".into(), "Kids".into()],
        genres: vec!["Comedy".into(), "Animation".into()],
    }
}

#[tokio::test]
async fn library_members_survive_a_reopen_in_order() {
    let dir = TempDir::new().unwrap();
    let mut created = torrent(1, 10);
    created.configuration = created.configuration.clone().with_library(library());
    let plain = torrent(2, 20);
    let mut changed = plain.configuration.clone();
    changed.set_library(Library {
        categories: vec!["Z".into(), "A".into(), "M".into()],
        ..Library::default()
    });
    let mut cleared = created.configuration.clone();
    cleared.set_library(Library::default());
    {
        let (store, _) = open(&dir);
        store.insert(created.clone()).await.unwrap();
        store.insert(plain.clone()).await.unwrap();
        store.set_configuration(&plain.id, &changed).await.unwrap();
    }
    {
        let (store, stored) = open(&dir);
        assert_eq!(stored[0], created, "set at creation");
        assert_eq!(stored[1].configuration, changed, "replaced, in order");
        store
            .set_configuration(&created.id, &cleared)
            .await
            .unwrap();
    }
    let (_store, stored) = open(&dir);
    assert_eq!(stored[0].configuration.library(), &Library::default());
    assert_eq!(stored[0].configuration.etag(), cleared.etag());
}

#[tokio::test]
async fn posters_are_stored_replaced_and_deleted_with_their_torrent() {
    let dir = TempDir::new().unwrap();
    let at = SystemTime::UNIX_EPOCH + Duration::from_nanos(1_700_000_000_000_000_007);
    let png = b"\x89PNG\r\n\x1a\nfirst".to_vec();
    let jpeg = vec![0xff, 0xd8, 0xff, 0xe0, 1, 2, 3];
    let first = ImageInfo::of(ImageType::Png, &png, at);
    let second = ImageInfo::of(ImageType::Jpeg, &jpeg, at + Duration::from_secs(1));
    {
        let (store, _) = open(&dir);
        store.insert(torrent(1, 10)).await.unwrap();
        store.insert(torrent(2, 20)).await.unwrap();
        assert_eq!(store.poster(&id(1)).await.unwrap(), None);
        store.set_poster(&id(1), &first, png.clone()).await.unwrap();
        store.set_poster(&id(2), &first, png.clone()).await.unwrap();
        assert_eq!(
            store.poster(&id(1)).await.unwrap(),
            Some((first.clone(), png.clone()))
        );
        store
            .set_poster(&id(1), &second, jpeg.clone())
            .await
            .unwrap();
        assert!(matches!(
            store.set_poster(&id(9), &first, png.clone()).await,
            Err(StoreError::UnknownTorrent)
        ));
        assert!(matches!(
            store.delete_poster(&id(9)).await,
            Err(StoreError::UnknownTorrent)
        ));
    }
    {
        let (store, stored) = open(&dir);
        assert_eq!(stored[0].poster.as_ref(), Some(&second), "the replacement");
        assert_eq!(stored[1].poster.as_ref(), Some(&first));
        assert_eq!(
            store.poster(&id(1)).await.unwrap(),
            Some((second.clone(), jpeg))
        );
        store.delete_poster(&id(1)).await.unwrap();
        store.delete_poster(&id(1)).await.unwrap();
        store.delete(&id(2)).await.unwrap();
    }
    let (store, stored) = open(&dir);
    assert_eq!(stored[0].poster, None);
    assert_eq!(store.poster(&id(1)).await.unwrap(), None);
    drop(store);
    assert_eq!(
        rusqlite_count(&dir, "SELECT count(*) FROM torrent_posters"),
        0,
        "the deleted torrent's poster went with it"
    );
}

#[tokio::test]
async fn torrents_from_version_6_have_empty_library_members() {
    let dir = TempDir::new().unwrap();
    let stored_torrent = torrent(1, 10);
    {
        let (store, _) = open(&dir);
        store.insert(stored_torrent.clone()).await.unwrap();
    }
    // A database as version 6 left it.
    {
        let connection = rusqlite::Connection::open(dir.path().join(FILE_NAME)).unwrap();
        connection
            .execute_batch(&format!(
                "{UNDO_VERSION_17} {UNDO_VERSION_16} {UNDO_VERSION_15} {UNDO_VERSION_14} {UNDO_VERSION_13} {UNDO_VERSION_12} {UNDO_VERSION_11} {UNDO_VERSION_10} {UNDO_VERSION_9} {UNDO_VERSION_8}
                 {UNDO_VERSION_7} PRAGMA user_version = 6;"
            ))
            .unwrap();
    }
    let (_store, stored) = open(&dir);
    assert_eq!(stored, [stored_torrent]);
    assert_eq!(stored[0].configuration.library(), &Library::default());
    assert!(dir.path().join(format!("{FILE_NAME}.v6.bak")).exists());
}

#[tokio::test]
async fn profiles_from_version_12_are_people_and_one_device_profile_is_kept() {
    let dir = TempDir::new().unwrap();
    let alex = profile(1, "Alex");
    {
        let (store, _) = open(&dir);
        store.insert_profile(&alex).await.unwrap();
    }
    // A database as version 12 left it.
    {
        let connection = rusqlite::Connection::open(dir.path().join(FILE_NAME)).unwrap();
        connection
            .execute_batch(&format!(
                "{UNDO_VERSION_17} {UNDO_VERSION_16} {UNDO_VERSION_15} {UNDO_VERSION_14} {UNDO_VERSION_13} PRAGMA user_version = 12;"
            ))
            .unwrap();
    }
    let devices = Profile {
        kind: ProfileKind::Device,
        ..profile(2, "Devices")
    };
    {
        let (store, contents) = Store::open(dir.path()).unwrap();
        assert_eq!(
            contents.profiles,
            std::slice::from_ref(&alex),
            "an existing profile is a person's"
        );
        store.insert_profile(&devices).await.unwrap();
        assert!(
            store
                .insert_profile(&Profile {
                    kind: ProfileKind::Device,
                    ..profile(3, "More devices")
                })
                .await
                .is_err(),
            "a second device profile is refused"
        );
    }
    let (_store, contents) = Store::open(dir.path()).unwrap();
    let mut profiles = contents.profiles;
    profiles.sort_by(|a, b| a.id.cmp(&b.id));
    assert_eq!(profiles, [alex, devices]);
}

/// PRG-11: title progress from version 13 starts empty, keeps the file
/// that played though no torrent is stored, and survives reopening; a save
/// that makes room deletes the title it replaces in the same write.
#[tokio::test]
async fn title_progress_is_stored_apart_from_torrents() {
    let dir = TempDir::new().unwrap();
    let alex = profile(1, "Alex");
    {
        let (store, _) = open(&dir);
        store.insert_profile(&alex).await.unwrap();
    }
    {
        let connection = rusqlite::Connection::open(dir.path().join(FILE_NAME)).unwrap();
        connection
            .execute_batch(&format!(
                "{UNDO_VERSION_17} {UNDO_VERSION_16} {UNDO_VERSION_15} {UNDO_VERSION_14} PRAGMA user_version = 13;"
            ))
            .unwrap();
    }
    let at = |secs| SystemTime::UNIX_EPOCH + Duration::from_secs(secs);
    let saved = |key: &str, secs, played| StoredTitleProgress {
        profile: alex.id.clone(),
        key: TitleKey::parse(key).unwrap(),
        progress: TitleProgress {
            progress: Progress {
                position_ms: 812_000,
                duration_ms: 2_760_000,
                viewed_at: None,
                updated_at: at(secs),
            },
            played,
        },
    };
    let played = Some((
        TorrentId::from_random_bytes([7; TORRENT_ID_RANDOM_BYTES]),
        FileId::derive(&["Episode.mkv"]),
    ));
    let episode = saved("tmdb-tv-1399-s1e2", 20, played);
    let movie = saved("tmdb-movie-603", 30, None);
    {
        let (store, contents) = Store::open(dir.path()).unwrap();
        assert!(contents.titles.is_empty());
        let first = saved("tmdb-tv-1399-s1e1", 10, None);
        store.set_title_progress(&first, None).await.unwrap();
        store.set_title_progress(&episode, None).await.unwrap();
        store
            .set_title_progress(&movie, Some(&first.key))
            .await
            .unwrap();
        assert!(matches!(
            store
                .set_title_progress(
                    &StoredTitleProgress {
                        profile: profile(9, "Nobody").id,
                        ..movie.clone()
                    },
                    None
                )
                .await,
            Err(StoreError::UnknownProfile)
        ));
    }
    let (store, contents) = Store::open(dir.path()).unwrap();
    let mut titles = contents.titles;
    titles.sort_by(|a, b| a.key.cmp(&b.key));
    assert_eq!(titles, [movie.clone(), episode.clone()]);
    store
        .delete_title_progress(&alex.id, &episode.key)
        .await
        .unwrap();
    drop(store);
    let (_store, contents) = Store::open(dir.path()).unwrap();
    assert_eq!(contents.titles, [movie]);
}

/// TMP-11 and TMP-12: temporary torrents' progress survives reopening,
/// becomes the progress of the torrent a promotion stores in the same
/// write, and expires by when it was last saved.
#[tokio::test]
async fn temporary_progress_is_kept_by_info_hash() {
    let dir = TempDir::new().unwrap();
    let alex = profile(1, "Alex");
    let at = |secs| SystemTime::UNIX_EPOCH + Duration::from_secs(secs);
    let promoted = torrent(1, 1);
    let kept = |info_hash, file: &str, secs| StoredTemporaryProgress {
        profile: alex.id.clone(),
        info_hash,
        file: FileId::derive(&[file]),
        progress: Progress {
            position_ms: 30_000,
            duration_ms: 100_000,
            viewed_at: None,
            updated_at: at(secs),
        },
    };
    let film = kept(promoted.info_hash, "film.mkv", 20);
    let old = kept(InfoHashV1::from_bytes([9; 20]), "old.mkv", 10);
    let recent = kept(InfoHashV1::from_bytes([9; 20]), "recent.mkv", 30);
    {
        let (store, _) = open(&dir);
        store.insert_profile(&alex).await.unwrap();
        for progress in [&film, &old, &recent] {
            store.set_temporary_progress(progress).await.unwrap();
        }
    }
    let (store, contents) = Store::open(dir.path()).unwrap();
    let mut temporary = contents.temporary_progress;
    temporary.sort_by(|a, b| a.file.cmp(&b.file));
    let mut expected = vec![film.clone(), old.clone(), recent.clone()];
    expected.sort_by(|a, b| a.file.cmp(&b.file));
    assert_eq!(temporary, expected);

    store
        .promote(promoted.clone(), promoted.info_hash)
        .await
        .unwrap();
    store.expire_temporary_progress(at(15)).await.unwrap();
    drop(store);
    let (_store, contents) = Store::open(dir.path()).unwrap();
    assert_eq!(
        contents.temporary_progress,
        [recent],
        "promoted and expired rows are gone"
    );
    assert_eq!(
        contents.progress,
        [StoredProgress {
            profile: alex.id.clone(),
            torrent: promoted.id.clone(),
            file: film.file.clone(),
            progress: film.progress,
        }]
    );
    assert_eq!(contents.torrents.len(), 1);
}

fn profile(n: u8, name: &str) -> Profile {
    Profile {
        id: ProfileId::from_random_bytes([n; TORRENT_ID_RANDOM_BYTES]),
        name: name.into(),
        kind: ProfileKind::Person,
        created_at: SystemTime::UNIX_EPOCH + Duration::from_nanos(1_000 + u64::from(n)),
        avatar: None,
    }
}

#[tokio::test]
async fn profiles_progress_and_dismissals_survive_and_go_with_their_owners() {
    let dir = TempDir::new().unwrap();
    let at = |secs| SystemTime::UNIX_EPOCH + Duration::from_secs(secs);
    let alex = profile(1, "Alex");
    let sam = profile(2, "Sam");
    let file = FileId::derive(&["Film.mkv"]);
    let progress = |profile: &Profile, torrent: u32, position_ms| StoredProgress {
        profile: profile.id.clone(),
        torrent: id(torrent),
        file: file.clone(),
        progress: Progress {
            position_ms,
            duration_ms: 10_000,
            viewed_at: (position_ms == 0).then(|| at(4)),
            updated_at: at(5),
        },
    };
    let entry = UpNextEntryId::of_file(&id(1), &file);
    let dismissal = StoredDismissal {
        profile: alex.id.clone(),
        entry: entry.clone(),
        torrent: id(1),
        at: at(6),
    };
    let png = b"\x89PNG\r\n\x1a\navatar".to_vec();
    let avatar = ImageInfo::of(ImageType::Png, &png, at(7));
    {
        let (store, _) = open(&dir);
        store.insert(torrent(1, 10)).await.unwrap();
        store.insert(torrent(2, 20)).await.unwrap();
        store.insert_profile(&alex).await.unwrap();
        store.insert_profile(&sam).await.unwrap();
        assert!(
            store.insert_profile(&profile(3, "Alex")).await.is_err(),
            "names are unique"
        );
        store.rename_profile(&sam.id, "Sammy").await.unwrap();
        store
            .set_avatar(&alex.id, &avatar, png.clone())
            .await
            .unwrap();
        store
            .set_progress(&progress(&alex, 1, 2_500))
            .await
            .unwrap();
        store
            .set_progress(&progress(&alex, 1, 3_000))
            .await
            .unwrap();
        store.set_progress(&progress(&sam, 1, 0)).await.unwrap();
        store.set_progress(&progress(&alex, 2, 100)).await.unwrap();
        store.set_dismissal(&dismissal).await.unwrap();
        assert!(matches!(
            store
                .set_progress(&progress(&profile(9, "Nobody"), 1, 1))
                .await,
            Err(StoreError::UnknownProfile)
        ));
        assert!(matches!(
            store.set_progress(&progress(&alex, 9, 1)).await,
            Err(StoreError::UnknownTorrent)
        ));
        assert!(matches!(
            store
                .set_avatar(&profile(9, "Nobody").id, &avatar, png.clone())
                .await,
            Err(StoreError::UnknownProfile)
        ));
    }
    {
        let (store, contents) = Store::open(dir.path()).unwrap();
        let mut profiles = contents.profiles.clone();
        profiles.sort_by(|a, b| a.id.cmp(&b.id));
        assert_eq!(
            profiles,
            [
                Profile {
                    avatar: Some(avatar.clone()),
                    ..alex.clone()
                },
                Profile {
                    name: "Sammy".into(),
                    ..sam.clone()
                }
            ]
        );
        assert_eq!(contents.progress.len(), 3);
        assert!(
            contents.progress.contains(&progress(&alex, 1, 3_000)),
            "replaced"
        );
        assert!(contents.progress.contains(&progress(&sam, 1, 0)));
        assert_eq!(contents.dismissals, [dismissal]);
        assert_eq!(store.avatar(&alex.id).await.unwrap(), Some((avatar, png)));
        store
            .delete_progress(&alex.id, &id(2), &file)
            .await
            .unwrap();
        store.delete(&id(1)).await.unwrap();
        store.delete_profile(&sam.id).await.unwrap();
    }
    let (_store, contents) = Store::open(dir.path()).unwrap();
    assert_eq!(contents.profiles.len(), 1);
    assert!(
        contents.progress.is_empty(),
        "each deletion took its progress"
    );
    assert!(
        contents.dismissals.is_empty(),
        "and the torrent its dismissals"
    );
}

#[tokio::test]
async fn a_version_7_database_gains_empty_profiles() {
    let dir = TempDir::new().unwrap();
    {
        let (store, _) = open(&dir);
        store.insert(torrent(1, 10)).await.unwrap();
    }
    // A database as version 7 left it.
    {
        let connection = rusqlite::Connection::open(dir.path().join(FILE_NAME)).unwrap();
        connection
            .execute_batch(&format!(
                "{UNDO_VERSION_17} {UNDO_VERSION_16} {UNDO_VERSION_15} {UNDO_VERSION_14} {UNDO_VERSION_13} {UNDO_VERSION_12} {UNDO_VERSION_11} {UNDO_VERSION_10} {UNDO_VERSION_9} {UNDO_VERSION_8} PRAGMA user_version = 7;"
            ))
            .unwrap();
    }
    let (_store, contents) = Store::open(dir.path()).unwrap();
    assert_eq!(contents.torrents.len(), 1);
    assert!(contents.profiles.is_empty());
    assert!(dir.path().join(format!("{FILE_NAME}.v7.bak")).exists());
}

fn access_token(n: u8, expires_at: Option<SystemTime>) -> AccessToken {
    AccessToken {
        id: AccessTokenId::from_random_bytes([n; TORRENT_ID_RANDOM_BYTES]),
        name: format!("Device {n}"),
        scope: if n.is_multiple_of(2) {
            Scope::Api
        } else {
            Scope::Playback
        },
        created_at: SystemTime::UNIX_EPOCH + Duration::from_nanos(u64::from(n)),
        expires_at,
        last_used_at: None,
        digest: new_secret().unwrap().1,
    }
}

#[tokio::test]
async fn credentials_survive_a_reopen_and_keep_their_signing_key() {
    let dir = TempDir::new().unwrap();
    let (key, hash, laptop) = {
        let (store, contents) = Store::open(dir.path()).unwrap();
        assert_eq!(
            contents.credentials.password, None,
            "authentication starts off"
        );
        assert!(contents.credentials.tokens.is_empty());
        let hash = PasswordHash::new("correct horse").unwrap();
        let at = SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000);
        store.set_password(&hash, at, false).await.unwrap();
        let laptop = access_token(2, None);
        store.insert_access_token(&laptop, &[]).await.unwrap();
        let used = at + Duration::from_secs(60);
        store.touch_access_token(&laptop.id, used).await.unwrap();
        let laptop = AccessToken {
            last_used_at: Some(used),
            ..laptop
        };
        (contents.credentials.key, (hash, at), laptop)
    };
    let (_store, contents) = Store::open(dir.path()).unwrap();
    assert_eq!(contents.credentials.key, key, "the key is drawn once");
    assert_eq!(contents.credentials.password, Some(hash));
    assert!(
        contents
            .credentials
            .password
            .unwrap()
            .0
            .verify("correct horse"),
        "the hash verifies after a reopen"
    );
    assert_eq!(contents.credentials.tokens, [laptop]);
}

#[tokio::test]
async fn a_version_15_database_keeps_its_tokens_and_takes_metrics_tokens() {
    let dir = TempDir::new().unwrap();
    let (laptop, television) = (access_token(2, None), access_token(3, None));
    {
        let (store, _) = Store::open(dir.path()).unwrap();
        store.insert_access_token(&laptop, &[]).await.unwrap();
        store.insert_access_token(&television, &[]).await.unwrap();
    }
    {
        let connection = rusqlite::Connection::open(dir.path().join(FILE_NAME)).unwrap();
        connection
            .execute_batch(&format!("{UNDO_VERSION_17} {UNDO_VERSION_16} PRAGMA user_version = 15;"))
            .unwrap();
        let refused = connection.execute(
            "UPDATE access_tokens SET scope = 'metrics' WHERE id = ?1",
            [laptop.id.as_str()],
        );
        assert!(refused.is_err(), "version 15 has no metrics scope");
    }
    let scraper = AccessToken {
        scope: Scope::Metrics,
        ..access_token(4, None)
    };
    {
        let (store, contents) = Store::open(dir.path()).unwrap();
        assert_eq!(
            contents.credentials.tokens,
            [laptop.clone(), television.clone()]
        );
        store.insert_access_token(&scraper, &[]).await.unwrap();
    }
    let (_store, contents) = Store::open(dir.path()).unwrap();
    assert_eq!(contents.credentials.tokens, [laptop, television, scraper]);
}

#[tokio::test]
async fn expired_access_tokens_are_deleted_on_open_and_on_insert() {
    let dir = TempDir::new().unwrap();
    let past = SystemTime::UNIX_EPOCH + Duration::from_secs(1);
    let future = SystemTime::now() + Duration::from_secs(3600);
    {
        let (store, _) = open(&dir);
        store
            .insert_access_token(&access_token(1, Some(past)), &[])
            .await
            .unwrap();
        store
            .insert_access_token(&access_token(2, Some(past)), &[])
            .await
            .unwrap();
        let renewed = access_token(3, Some(future));
        store
            .insert_access_token(&renewed, &[access_token(2, None).id])
            .await
            .unwrap();
    }
    assert_eq!(
        rusqlite_count(&dir, "SELECT count(*) FROM access_tokens"),
        2,
        "the insert deleted the expired token it was given"
    );
    let (store, contents) = Store::open(dir.path()).unwrap();
    assert_eq!(
        contents.credentials.tokens,
        [access_token(3, Some(future))].map(|token| AccessToken {
            digest: contents.credentials.tokens[0].digest,
            ..token
        })
    );
    drop(store);
    assert_eq!(
        rusqlite_count(&dir, "SELECT count(*) FROM access_tokens"),
        1
    );
}

#[tokio::test]
async fn revoking_and_removing_the_password_delete_every_access_token() {
    let dir = TempDir::new().unwrap();
    let hash = PasswordHash::new("correct horse").unwrap();
    let at = SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000);
    let new_key = SigningKey::new().unwrap();
    {
        let (store, _) = open(&dir);
        store.set_password(&hash, at, false).await.unwrap();
        store
            .insert_access_token(&access_token(2, None), &[])
            .await
            .unwrap();
        store.set_password(&hash, at, true).await.unwrap();
    }
    let (store, contents) = Store::open(dir.path()).unwrap();
    assert!(
        contents.credentials.tokens.is_empty(),
        "the change revoked them"
    );
    assert!(contents.credentials.password.is_some());
    drop(store);
    {
        let (store, _) = open(&dir);
        store
            .insert_access_token(&access_token(4, None), &[])
            .await
            .unwrap();
        store
            .delete_access_token(&access_token(4, None).id)
            .await
            .unwrap();
        store
            .insert_access_token(&access_token(6, None), &[])
            .await
            .unwrap();
        store.remove_password(&new_key).await.unwrap();
    }
    let (_store, contents) = Store::open(dir.path()).unwrap();
    assert_eq!(contents.credentials.password, None);
    assert!(contents.credentials.tokens.is_empty());
    assert_eq!(contents.credentials.key, new_key);
}

#[tokio::test]
async fn a_version_8_database_gains_credentials_without_a_password() {
    let dir = TempDir::new().unwrap();
    {
        let (store, _) = open(&dir);
        store.insert(torrent(1, 10)).await.unwrap();
    }
    {
        let connection = rusqlite::Connection::open(dir.path().join(FILE_NAME)).unwrap();
        connection
            .execute_batch(&format!(
                "{UNDO_VERSION_17} {UNDO_VERSION_16} {UNDO_VERSION_15} {UNDO_VERSION_14} {UNDO_VERSION_13} {UNDO_VERSION_12} {UNDO_VERSION_11} {UNDO_VERSION_10} {UNDO_VERSION_9} PRAGMA user_version = 8;"
            ))
            .unwrap();
    }
    let (_store, contents) = Store::open(dir.path()).unwrap();
    assert_eq!(contents.torrents.len(), 1);
    assert_eq!(contents.credentials.password, None);
    assert!(dir.path().join(format!("{FILE_NAME}.v8.bak")).exists());
}

#[tokio::test]
async fn the_media_server_keeps_its_uuid_and_switch() {
    let dir = TempDir::new().unwrap();
    let udn = {
        let (store, contents) = Store::open(dir.path()).unwrap();
        assert!(!contents.settings.explicit.effective().dlna_enabled);
        store
            .set_settings(StoredSettings {
                explicit: Explicit {
                    dlna_enabled: Some(true),
                    ..Explicit::default()
                },
                version: 2,
            })
            .await
            .unwrap();
        contents.media_server_udn
    };
    let (_store, contents) = Store::open(dir.path()).unwrap();
    assert_eq!(contents.media_server_udn, udn, "the UUID is drawn once");
    assert_eq!(contents.settings.explicit.dlna_enabled, Some(true));
}

#[tokio::test]
async fn a_version_9_database_gains_the_media_server_switch_unset() {
    let dir = TempDir::new().unwrap();
    drop(open(&dir));
    {
        let connection = rusqlite::Connection::open(dir.path().join(FILE_NAME)).unwrap();
        connection
            .execute_batch(&format!(
                "{UNDO_VERSION_17} {UNDO_VERSION_16} {UNDO_VERSION_15} {UNDO_VERSION_14} {UNDO_VERSION_13} {UNDO_VERSION_12} {UNDO_VERSION_11} {UNDO_VERSION_10} PRAGMA user_version = 9;"
            ))
            .unwrap();
    }
    let (_store, contents) = Store::open(dir.path()).unwrap();
    assert_eq!(contents.settings.explicit.dlna_enabled, None);
    assert_eq!(contents.media_server_udn.len(), 36);
}

#[tokio::test]
async fn the_name_dht_and_log_settings_survive_a_restart() {
    let dir = TempDir::new().unwrap();
    let explicit = Explicit {
        temporary_idle_seconds: Some(60),
        temporary_torrents_max: Some(1),
        dlna_name: Some("Кино: living room".into()),
        dht_enabled: Some(false),
        log_level: Some(LogLevel::Trace),
        ..Explicit::default()
    };
    {
        let (store, contents) = Store::open(dir.path()).unwrap();
        assert_eq!(contents.settings.explicit, Explicit::default());
        store
            .set_settings(StoredSettings {
                explicit: explicit.clone(),
                version: 2,
            })
            .await
            .unwrap();
    }
    let (store, contents) = Store::open(dir.path()).unwrap();
    assert_eq!(
        contents.settings.explicit, explicit,
        "explicit false is kept"
    );
    store
        .set_settings(StoredSettings {
            explicit: Explicit::default(),
            version: 3,
        })
        .await
        .unwrap();
    drop(store);
    let (_store, contents) = Store::open(dir.path()).unwrap();
    assert_eq!(
        contents.settings.explicit,
        Explicit::default(),
        "unset again"
    );
}

#[tokio::test]
async fn a_version_10_database_gains_the_new_settings_unset() {
    let dir = TempDir::new().unwrap();
    {
        let (store, _) = Store::open(dir.path()).unwrap();
        store
            .set_settings(StoredSettings {
                explicit: Explicit {
                    dlna_enabled: Some(true),
                    peer_limit: Some(7),
                    ..Explicit::default()
                },
                version: 4,
            })
            .await
            .unwrap();
    }
    {
        let connection = rusqlite::Connection::open(dir.path().join(FILE_NAME)).unwrap();
        connection
            .execute_batch(&format!(
                "{UNDO_VERSION_17} {UNDO_VERSION_16} {UNDO_VERSION_15} {UNDO_VERSION_14} {UNDO_VERSION_13} {UNDO_VERSION_12} {UNDO_VERSION_11} PRAGMA user_version = 10;"
            ))
            .unwrap();
    }
    let (_store, contents) = Store::open(dir.path()).unwrap();
    let explicit = &contents.settings.explicit;
    assert_eq!(explicit.dlna_enabled, Some(true));
    assert_eq!(explicit.peer_limit, Some(7));
    assert_eq!(
        (
            explicit.dlna_name.as_deref(),
            explicit.dht_enabled,
            explicit.log_level
        ),
        (None, None, None)
    );
    assert_eq!(contents.settings.version, 4);
}
