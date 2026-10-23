// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! A write killed mid-transaction, then recovered, for every backend.
//!
//! The test binary runs itself again as the writing child, whose test
//! `child` does nothing unless `STORE_CHILD` names its task. A killed process
//! loses what it had not committed; the operating system still writes out
//! what it had, so these cases check transaction atomicity and recovery on
//! reopening, not power loss.

// Cases and their helpers assert as tests do; clippy's test exemption
// covers only test functions.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use serde_json::json;
use tempfile::TempDir;
use torrplay_store::{Cursor, Opener, Source, StorageMode, Store, Torrent};

const CHILD: &str = "STORE_CHILD";
const BATCH: u32 = 10;

fn torrent(n: u32) -> Torrent {
    let mut info_hash = [0; 20];
    info_hash[..4].copy_from_slice(&n.to_be_bytes());
    Torrent {
        id: format!("trr_{n:026}"),
        info_hash,
        created_ms: u64::from(n),
        source: Source::Metainfo(vec![(n % 251) as u8; 64 * 1024]),
        configuration: json!({"desired_state": "started"}).to_string(),
        version: 1,
        storage_mode: StorageMode::Memory,
    }
}

fn opener(backend: &str) -> Opener {
    match backend {
        #[cfg(feature = "sqlite")]
        "sqlite" => torrplay_store::sqlite::SqliteStore::opener,
        #[cfg(feature = "redb")]
        "redb" => torrplay_store::redb::RedbStore::opener,
        other => panic!("unknown backend {other}"),
    }
}

/// Starts this binary's `child` test with a task.
fn spawn(backend: &str, task: &str, path: &Path) -> Child {
    Command::new(std::env::current_exe().expect("the test binary"))
        .args(["--exact", "child", "--ignored", "--nocapture"])
        .env(CHILD, format!("{backend}|{task}|{}", path.display()))
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("the child starts")
}

/// Reads the child's output until a line starts with `marker`.
fn wait_for(child: &mut Child, marker: &str) -> String {
    let stdout = child.stdout.as_mut().expect("piped output");
    let mut reader = BufReader::new(stdout);
    let mut line = String::new();
    loop {
        line.clear();
        assert!(
            reader.read_line(&mut line).expect("the child's output") > 0,
            "the child ended before {marker}"
        );
        if line.starts_with(marker) {
            return line;
        }
    }
}

fn kill(mut child: Child) {
    child.kill().expect("the child is killed");
    let status = child.wait().expect("the child is reaped");
    assert!(!status.success(), "the child was killed, not finished");
}

/// The child: `commit-then-hang` commits five torrents, then starts a batch
/// and hangs right before committing it; `commit-forever` commits batches
/// until killed, reporting each.
#[test]
#[ignore = "run by the recovery tests as their writing child"]
fn child() {
    let Ok(task) = std::env::var(CHILD) else {
        return;
    };
    let mut parts = task.splitn(3, '|');
    let (Some(backend), Some(task), Some(path)) = (parts.next(), parts.next(), parts.next()) else {
        panic!("malformed task {task}");
    };
    let store = opener(backend)(&PathBuf::from(path)).expect("the child opens the store");
    let mut out = std::io::stdout();
    match task {
        "commit-then-hang" => {
            for n in 0..5 {
                store.insert(&torrent(n)).unwrap();
            }
            let batch: Vec<Torrent> = (100..200).map(torrent).collect();
            store
                .insert_batch(&batch, &mut || {
                    writeln!(out, "ready").unwrap();
                    out.flush().unwrap();
                    std::thread::sleep(Duration::from_secs(600));
                })
                .unwrap();
        }
        "commit-forever" => {
            let start = store.snapshot().unwrap().count().unwrap() as u32;
            // Until killed.
            for first in (start..).step_by(BATCH as usize) {
                let torrents: Vec<Torrent> = (first..first + BATCH).map(torrent).collect();
                store.insert_batch(&torrents, &mut || {}).unwrap();
                writeln!(out, "committed {}", first + BATCH).unwrap();
                out.flush().unwrap();
            }
        }
        other => panic!("unknown task {other}"),
    }
}

/// Every listed torrent can be read, and the listing and the count agree.
fn check_consistent(store: &dyn Store) -> u64 {
    let snapshot = store.snapshot().unwrap();
    let count = snapshot.count().unwrap();
    let mut listed = 0;
    let mut after: Option<Cursor> = None;
    loop {
        let page = snapshot.list(after.as_ref(), 50).unwrap();
        let Some(last) = page.last() else { break };
        after = Some(Cursor::from(last));
        for item in &page {
            let stored = snapshot.get(&item.id).unwrap().expect("a listed torrent");
            let n: u32 = item.id[4..].parse().unwrap();
            assert_eq!(stored, torrent(n), "{} is whole", item.id);
        }
        listed += page.len() as u64;
    }
    assert_eq!(listed, count, "the listing and the count agree");
    count
}

fn a_write_killed_before_commit_leaves_the_previous_state(backend: &str) {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("store.db");
    let mut child = spawn(backend, "commit-then-hang", &path);
    wait_for(&mut child, "ready");
    kill(child);

    let store = opener(backend)(&path).expect("the store opens after the kill");
    assert_eq!(check_consistent(&*store), 5, "only the committed torrents");
    assert!(
        store
            .snapshot()
            .unwrap()
            .get(&torrent(100).id)
            .unwrap()
            .is_none()
    );
    assert_eq!(
        store.insert(&torrent(100)).unwrap(),
        torrplay_store::Inserted::Created,
        "the interrupted batch left nothing behind"
    );
}

fn writes_killed_at_any_moment_leave_whole_batches(backend: &str) {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("store.db");
    let mut committed = 0;
    // Kills land at different points of a commit: right after a report,
    // and some milliseconds into the following batches.
    for (round, delay_ms) in [0u64, 1, 3, 7, 11, 17, 23, 31, 41, 53]
        .into_iter()
        .enumerate()
    {
        let mut child = spawn(backend, "commit-forever", &path);
        let line = wait_for(&mut child, "committed");
        let reported: u64 = line.trim()["committed ".len()..].parse().unwrap();
        std::thread::sleep(Duration::from_millis(delay_ms));
        kill(child);

        let store = opener(backend)(&path).expect("the store opens after the kill");
        let count = check_consistent(&*store);
        assert_eq!(
            count % u64::from(BATCH),
            0,
            "round {round}: whole batches only"
        );
        assert!(count >= reported, "round {round}: reported commits survive");
        assert!(
            count >= committed,
            "round {round}: nothing committed is lost"
        );
        committed = count;
    }
    assert!(committed > 0);
}

macro_rules! recovery {
    ($backend:ident) => {
        mod $backend {
            #[test]
            fn a_write_killed_before_commit_leaves_the_previous_state() {
                super::a_write_killed_before_commit_leaves_the_previous_state(stringify!($backend));
            }
            #[test]
            fn writes_killed_at_any_moment_leave_whole_batches() {
                super::writes_killed_at_any_moment_leave_whole_batches(stringify!($backend));
            }
        }
    };
}

#[cfg(feature = "sqlite")]
recovery!(sqlite);
#[cfg(feature = "redb")]
recovery!(redb);
