// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! The ADR 8 spike's cases, run against every backend.

// Cases and their helpers assert as tests do; clippy's test exemption
// covers only test functions.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::path::Path;

use serde_json::{Map, Value, json};
use tempfile::TempDir;
use torrplay_store::{
    Cursor, Inserted, Invalid, Opener, Snapshot, Source, StorageMode, Store, StoreError, Torrent,
    Updated, effective_settings,
};

/// Creates a schema version 1 database with these torrents' IDs, info
/// hashes, and creation times.
type CreateV1 = fn(&Path, &[(String, [u8; 20], u64)]) -> Result<(), StoreError>;

fn torrent(n: u32, created_ms: u64) -> Torrent {
    let mut info_hash = [0; 20];
    info_hash[..4].copy_from_slice(&n.to_be_bytes());
    Torrent {
        id: format!("trr_{n:026}"),
        info_hash,
        created_ms,
        source: if n.is_multiple_of(2) {
            Source::Metainfo(vec![n as u8; 1000 + n as usize])
        } else {
            Source::Magnet(format!("magnet:?xt=urn:btih:{n:040}"))
        },
        configuration: json!({"desired_state": "started"}).to_string(),
        version: 1,
        storage_mode: StorageMode::Memory,
    }
}

fn store(open: Opener, dir: &TempDir) -> Box<dyn Store> {
    open(&dir.path().join("store.db")).expect("the store opens")
}

/// Lists every torrent page by page.
fn traverse(snapshot: &dyn Snapshot, page: usize) -> Vec<String> {
    let mut ids = Vec::new();
    let mut after: Option<Cursor> = None;
    loop {
        let items = snapshot.list(after.as_ref(), page).expect("a page");
        let Some(last) = items.last() else {
            return ids;
        };
        after = Some(Cursor::from(last));
        ids.extend(items.into_iter().map(|item| item.id));
    }
}

fn object(value: Value) -> Map<String, Value> {
    match value {
        Value::Object(map) => map,
        _ => Map::new(),
    }
}

fn accept(_: &Map<String, Value>) -> Result<(), Invalid> {
    Ok(())
}

fn pages_list_in_creation_order_then_by_id(open: Opener) {
    let dir = TempDir::new().unwrap();
    let store = store(open, &dir);
    // Creation times out of insertion order, several shared.
    let times = [50, 10, 30, 10, 40, 10, 20, 30, 50, 0, 20, 40];
    for (n, &time) in times.iter().enumerate() {
        assert_eq!(
            store.insert(&torrent(n as u32, time)).unwrap(),
            Inserted::Created
        );
    }
    let mut expected: Vec<(u64, String)> = times
        .iter()
        .enumerate()
        .map(|(n, &time)| (time, torrent(n as u32, time).id))
        .collect();
    expected.sort();
    let expected: Vec<String> = expected.into_iter().map(|(_, id)| id).collect();
    for page in [1, 5, 12, 50] {
        assert_eq!(
            traverse(&*store.snapshot().unwrap(), page),
            expected,
            "pages of {page}"
        );
    }
}

fn a_traversal_neither_repeats_nor_skips_lasting_torrents(open: Opener) {
    let dir = TempDir::new().unwrap();
    let store = store(open, &dir);
    for n in 0..20 {
        store.insert(&torrent(n, u64::from(n) * 10)).unwrap();
    }
    let mut seen = Vec::new();
    let mut after: Option<Cursor> = None;
    let mut extra = 100;
    loop {
        // Each page comes from a fresh snapshot, as each request does, and
        // torrents come and go between pages.
        let items = store.snapshot().unwrap().list(after.as_ref(), 3).unwrap();
        let Some(last) = items.last() else { break };
        after = Some(Cursor::from(last));
        seen.extend(items.into_iter().map(|item| item.id));
        store
            .insert(&torrent(extra, u64::from(extra % 7) * 25))
            .unwrap();
        store.delete(&torrent(extra - 1, 0).id).unwrap();
        extra += 1;
    }
    let lasting: Vec<String> = (0..20).map(|n| torrent(n, 0).id).collect();
    for id in &lasting {
        assert_eq!(seen.iter().filter(|seen| *seen == id).count(), 1, "{id}");
    }
}

fn a_duplicate_info_hash_reports_the_existing_torrent(open: Opener) {
    let dir = TempDir::new().unwrap();
    let store = store(open, &dir);
    let first = torrent(1, 10);
    store.insert(&first).unwrap();
    let mut second = torrent(2, 20);
    second.info_hash = first.info_hash;
    assert_eq!(
        store.insert(&second).unwrap(),
        Inserted::Duplicate {
            existing: first.id.clone()
        }
    );
    let snapshot = store.snapshot().unwrap();
    assert_eq!(snapshot.count().unwrap(), 1);
    assert_eq!(
        snapshot.get(&second.id).unwrap(),
        None,
        "nothing was stored"
    );
}

fn sources_round_trip_and_deletion_removes_everything(open: Opener) {
    let dir = TempDir::new().unwrap();
    let store = store(open, &dir);
    let mut large = torrent(2, 10);
    large.source = Source::Metainfo((0..2 * 1024 * 1024).map(|i| (i % 251) as u8).collect());
    let magnet = torrent(3, 20);
    store.insert(&large).unwrap();
    store.insert(&magnet).unwrap();
    {
        let snapshot = store.snapshot().unwrap();
        assert_eq!(snapshot.get(&large.id).unwrap().as_ref(), Some(&large));
        assert_eq!(snapshot.get(&magnet.id).unwrap().as_ref(), Some(&magnet));
    }
    assert!(store.delete(&large.id).unwrap());
    assert!(!store.delete(&large.id).unwrap(), "already deleted");
    let snapshot = store.snapshot().unwrap();
    assert_eq!(snapshot.get(&large.id).unwrap(), None);
    assert_eq!(traverse(&*snapshot, 10), std::slice::from_ref(&magnet.id));
    drop(snapshot);
    assert_eq!(
        store.insert(&large).unwrap(),
        Inserted::Created,
        "a deleted torrent's info hash is free again"
    );
}

fn a_stale_version_conflicts(open: Opener) {
    let dir = TempDir::new().unwrap();
    let store = store(open, &dir);
    let torrent = torrent(1, 10);
    store.insert(&torrent).unwrap();
    let paused = json!({"desired_state": "paused"}).to_string();
    assert_eq!(
        store.update_configuration(&torrent.id, 1, &paused).unwrap(),
        Updated::Replaced(2)
    );
    assert_eq!(
        store.update_configuration(&torrent.id, 1, "{}").unwrap(),
        Updated::Conflict { current: 2 }
    );
    assert_eq!(
        store.update_configuration("trr_missing", 1, "{}").unwrap(),
        Updated::NotFound
    );
    let stored = store.snapshot().unwrap().get(&torrent.id).unwrap().unwrap();
    assert_eq!((stored.version, stored.configuration), (2, paused));
}

fn a_snapshot_sees_one_state_while_writes_commit(open: Opener) {
    let dir = TempDir::new().unwrap();
    let store = store(open, &dir);
    for n in 0..3 {
        store.insert(&torrent(n, u64::from(n))).unwrap();
    }
    let before = store.snapshot().unwrap();
    store.insert(&torrent(10, 5)).unwrap();
    store.delete(&torrent(0, 0).id).unwrap();
    store
        .update_configuration(&torrent(1, 1).id, 1, "{}")
        .unwrap();
    store
        .patch_settings(&json!({"dht": false}), &accept)
        .unwrap()
        .unwrap();

    assert_eq!(before.count().unwrap(), 3);
    assert_eq!(traverse(&*before, 2).len(), 3);
    assert!(before.get(&torrent(0, 0).id).unwrap().is_some());
    assert_eq!(before.get(&torrent(1, 1).id).unwrap().unwrap().version, 1);
    assert!(before.settings().unwrap().is_empty());

    let after = store.snapshot().unwrap();
    assert_eq!(after.count().unwrap(), 3);
    assert!(after.get(&torrent(0, 0).id).unwrap().is_none());
    assert_eq!(after.get(&torrent(1, 1).id).unwrap().unwrap().version, 2);
    assert_eq!(after.settings().unwrap(), object(json!({"dht": false})));
}

fn settings_keep_explicit_values_and_take_new_defaults(open: Opener) {
    let dir = TempDir::new().unwrap();
    let defaults_v1 = object(json!({"cache_capacity": 256, "dht": true}));
    {
        let store = store(open, &dir);
        assert_eq!(
            effective_settings(&defaults_v1, &store.snapshot().unwrap().settings().unwrap()),
            defaults_v1,
            "an empty store has the defaults"
        );
        store
            .patch_settings(&json!({"dht": false, "cache_capacity": 0}), &accept)
            .unwrap()
            .unwrap();
        let refuse = |settings: &Map<String, Value>| match settings.get("cache_capacity") {
            Some(Value::Number(n)) if n.as_i64() == Some(-1) => {
                Err(Invalid("cache_capacity must not be negative".into()))
            }
            _ => Ok(()),
        };
        assert!(
            store
                .patch_settings(&json!({"dht": true, "cache_capacity": -1}), &refuse)
                .unwrap()
                .is_err()
        );
    }
    // Restarted with a version that adds a setting.
    let store = store(open, &dir);
    let explicit = store.snapshot().unwrap().settings().unwrap();
    assert_eq!(
        explicit,
        object(json!({"dht": false, "cache_capacity": 0})),
        "explicit false and zero survive, and the refused update changed nothing"
    );
    let defaults_v2 = object(json!({"cache_capacity": 256, "dht": true, "peer_limit": 50}));
    assert_eq!(
        effective_settings(&defaults_v2, &explicit),
        object(json!({"cache_capacity": 0, "dht": false, "peer_limit": 50}))
    );
    store
        .patch_settings(&json!({"cache_capacity": null}), &accept)
        .unwrap()
        .unwrap();
    assert_eq!(
        effective_settings(&defaults_v2, &store.snapshot().unwrap().settings().unwrap()),
        object(json!({"cache_capacity": 256, "dht": false, "peer_limit": 50})),
        "a removed value returns to its default"
    );
}

fn torrents_survive_a_restart(open: Opener) {
    let dir = TempDir::new().unwrap();
    let torrents: Vec<Torrent> = (0..10).map(|n| torrent(n, u64::from(n))).collect();
    {
        let store = store(open, &dir);
        for torrent in &torrents {
            store.insert(torrent).unwrap();
        }
    }
    let store = store(open, &dir);
    let snapshot = store.snapshot().unwrap();
    for torrent in &torrents {
        assert_eq!(snapshot.get(&torrent.id).unwrap().as_ref(), Some(torrent));
    }
}

fn a_version_1_database_migrates(open: Opener, create_v1: CreateV1) {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("store.db");
    let old: Vec<(String, [u8; 20], u64)> = (0..3)
        .map(|n| {
            let t = torrent(n, u64::from(n));
            (t.id, t.info_hash, t.created_ms)
        })
        .collect();
    create_v1(&path, &old).unwrap();
    let store = open(&path).unwrap();
    let snapshot = store.snapshot().unwrap();
    assert_eq!(traverse(&*snapshot, 2).len(), 3);
    for (id, ..) in &old {
        assert_eq!(
            snapshot.get(id).unwrap().unwrap().storage_mode,
            StorageMode::Memory,
            "torrents from before the storage mode are in memory"
        );
    }
    drop(snapshot);
    let mut disk = torrent(7, 7);
    disk.storage_mode = StorageMode::Disk;
    store.insert(&disk).unwrap();
    assert_eq!(
        store
            .snapshot()
            .unwrap()
            .get(&disk.id)
            .unwrap()
            .unwrap()
            .storage_mode,
        StorageMode::Disk
    );
}

macro_rules! cases {
    ($backend:ident, $open:expr, $create_v1:expr) => {
        mod $backend {
            #[test]
            fn pages_list_in_creation_order_then_by_id() {
                super::pages_list_in_creation_order_then_by_id($open);
            }
            #[test]
            fn a_traversal_neither_repeats_nor_skips_lasting_torrents() {
                super::a_traversal_neither_repeats_nor_skips_lasting_torrents($open);
            }
            #[test]
            fn a_duplicate_info_hash_reports_the_existing_torrent() {
                super::a_duplicate_info_hash_reports_the_existing_torrent($open);
            }
            #[test]
            fn sources_round_trip_and_deletion_removes_everything() {
                super::sources_round_trip_and_deletion_removes_everything($open);
            }
            #[test]
            fn a_stale_version_conflicts() {
                super::a_stale_version_conflicts($open);
            }
            #[test]
            fn a_snapshot_sees_one_state_while_writes_commit() {
                super::a_snapshot_sees_one_state_while_writes_commit($open);
            }
            #[test]
            fn settings_keep_explicit_values_and_take_new_defaults() {
                super::settings_keep_explicit_values_and_take_new_defaults($open);
            }
            #[test]
            fn torrents_survive_a_restart() {
                super::torrents_survive_a_restart($open);
            }
            #[test]
            fn a_version_1_database_migrates() {
                super::a_version_1_database_migrates($open, $create_v1);
            }
        }
    };
}

#[cfg(feature = "sqlite")]
cases!(
    sqlite,
    torrplay_store::sqlite::SqliteStore::opener,
    torrplay_store::sqlite::SqliteStore::create_v1
);
#[cfg(feature = "redb")]
cases!(
    redb,
    torrplay_store::redb::RedbStore::opener,
    torrplay_store::redb::RedbStore::create_v1
);

#[cfg(feature = "sqlite")]
#[test]
fn sqlite_refuses_a_newer_schema() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("store.db");
    drop(torrplay_store::sqlite::SqliteStore::open(&path).unwrap());
    rusqlite::Connection::open(&path)
        .unwrap()
        .pragma_update(None, "user_version", 99)
        .unwrap();
    assert!(matches!(
        torrplay_store::sqlite::SqliteStore::open(&path),
        Err(StoreError::TooNew { found: 99 })
    ));
}

#[cfg(feature = "redb")]
#[test]
fn redb_refuses_a_newer_schema() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("store.db");
    drop(torrplay_store::redb::RedbStore::open(&path).unwrap());
    let database = ::redb::Database::create(&path).unwrap();
    let transaction = database.begin_write().unwrap();
    transaction
        .open_table(::redb::TableDefinition::<&str, u32>::new("meta"))
        .unwrap()
        .insert("schema_version", 99)
        .unwrap();
    transaction.commit().unwrap();
    drop(database);
    assert!(matches!(
        torrplay_store::redb::RedbStore::open(&path),
        Err(StoreError::TooNew { found: 99 })
    ));
}
