// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! Measures the ADR 8 spike's workload on each enabled backend:
//!
//! ```bash
//! cargo run --release -p torrplay-store --example measure -- <DIR>
//! ```
//!
//! Each backend gets a fresh database in `<DIR>`: 1,000 torrents with 64
//! KiB of metainfo, each its own durable commit, then 10,000 magnet torrents
//! in batches of 1,000, then reads of pages, single torrents, and settings.

// A measurement tool: any failure ends it.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::path::Path;
use std::time::{Duration, Instant};

use serde_json::json;
use torrplay_store::{Cursor, Invalid, Opener, Source, StorageMode, Torrent};

fn torrent(n: u32, metainfo: bool) -> Torrent {
    let mut info_hash = [0; 20];
    info_hash[..4].copy_from_slice(&n.to_be_bytes());
    Torrent {
        id: format!("trr_{n:026}"),
        info_hash,
        created_ms: 1_800_000_000_000 + u64::from(n),
        source: if metainfo {
            Source::Metainfo(vec![(n % 251) as u8; 64 * 1024])
        } else {
            Source::Magnet(format!("magnet:?xt=urn:btih:{n:040}"))
        },
        configuration: json!({"desired_state": "started", "storage_mode": "memory"}).to_string(),
        version: 1,
        storage_mode: StorageMode::Memory,
    }
}

fn percentiles(mut samples: Vec<Duration>) -> String {
    samples.sort();
    let at = |q: f64| samples[((samples.len() - 1) as f64 * q) as usize];
    format!(
        "median {:.2?}, p90 {:.2?}, max {:.2?}",
        at(0.5),
        at(0.9),
        at(1.0)
    )
}

fn timed<T>(f: impl FnOnce() -> T) -> (T, Duration) {
    let started = Instant::now();
    let value = f();
    (value, started.elapsed())
}

fn size(dir: &Path) -> u64 {
    std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .flatten()
                .filter_map(|entry| entry.metadata().ok())
                .map(|metadata| metadata.len())
                .sum()
        })
        .unwrap_or(0)
}

fn measure(name: &str, open: Opener, dir: &Path) {
    let dir = dir.join(name);
    std::fs::create_dir_all(&dir).expect("a directory");
    let path = dir.join("store.db");
    let (store, opened) = timed(|| open(&path).expect("the store opens"));
    println!("{name}: create {opened:.2?}");

    let commits: Vec<Duration> = (0..1000)
        .map(|n| timed(|| store.insert(&torrent(n, true)).expect("an insert")).1)
        .collect();
    println!(
        "{name}: 1,000 inserts with 64 KiB metainfo: {}",
        percentiles(commits)
    );

    let (_, batches) = timed(|| {
        for batch in 0..10u32 {
            let torrents: Vec<Torrent> = (0..1000)
                .map(|n| torrent(10_000 + batch * 1000 + n, false))
                .collect();
            store.insert_batch(&torrents, &mut || {}).expect("a batch");
        }
    });
    println!("{name}: 10,000 magnet inserts in 10 batches: {batches:.2?}");
    println!("{name}: files {:.1} MiB", size(&dir) as f64 / 1048576.0);

    let snapshot = store.snapshot().expect("a snapshot");
    let mut pages = Vec::new();
    let mut after: Option<Cursor> = None;
    loop {
        let (page, took) = timed(|| snapshot.list(after.as_ref(), 50).expect("a page"));
        let Some(last) = page.last() else { break };
        after = Some(Cursor::from(last));
        pages.push(took);
    }
    println!(
        "{name}: {} pages of 50: {}",
        pages.len(),
        percentiles(pages)
    );
    let gets: Vec<Duration> = (0..1000)
        .map(|n| timed(|| snapshot.get(&torrent(n, true).id).expect("a get")).1)
        .collect();
    println!("{name}: get with 64 KiB metainfo: {}", percentiles(gets));
    drop(snapshot);
    let snapshots: Vec<Duration> = (0..1000)
        .map(|_| timed(|| drop(store.snapshot().expect("a snapshot"))).1)
        .collect();
    println!(
        "{name}: snapshot open and close: {}",
        percentiles(snapshots)
    );
    let accept = |_: &serde_json::Map<String, serde_json::Value>| Ok::<(), Invalid>(());
    let patches: Vec<Duration> = (0..200)
        .map(|n| {
            timed(|| {
                store
                    .patch_settings(&json!({"peer_limit": n}), &accept)
                    .expect("a patch")
                    .expect("valid")
            })
            .1
        })
        .collect();
    println!("{name}: settings patch: {}", percentiles(patches));
    drop(store);
    let (_, reopened) = timed(|| drop(open(&path).expect("the store reopens")));
    println!("{name}: reopen {reopened:.2?}");
}

fn main() {
    let dir = std::env::args().nth(1).expect("usage: measure <DIR>");
    let dir = Path::new(&dir);
    #[cfg(feature = "sqlite")]
    measure("sqlite", torrplay_store::sqlite::SqliteStore::opener, dir);
    #[cfg(feature = "redb")]
    measure("redb", torrplay_store::redb::RedbStore::opener, dir);
}
