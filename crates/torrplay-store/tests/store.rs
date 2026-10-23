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
use torrplay_core::id::{InfoHashV1, TORRENT_ID_RANDOM_BYTES, TorrentId};
use torrplay_core::lifecycle::{Configuration, DesiredState};
use torrplay_store::{FILE_NAME, Store, StoreError, StoredTorrent};

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
        configuration: Configuration::new(DesiredState::Started),
        magnet_uri: (!file).then(|| format!("magnet:?xt=urn:btih:{}", info_hash(n))),
        torrent_file: file.then(|| vec![(n % 251) as u8; 1000 + n as usize]),
    }
}

fn open(dir: &TempDir) -> (Store, Vec<StoredTorrent>) {
    Store::open(dir.path()).expect("the store opens")
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
            supported: 1
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
        let (store, stored) = Store::open(Path::new(&path)).unwrap();
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

        let (store, stored) = Store::open(dir.path()).expect("the store opens after a kill");
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
