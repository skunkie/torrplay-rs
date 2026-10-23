// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! Disk storage through librqbit: a torrent downloads into its directory at
//! the paths ADR 9 maps, records its pieces in its resume bitfield, and
//! starts again from the bitfield without peers or a full check.

mod support;

use std::path::Path;
use std::sync::Arc;

use anyhow::Context;
use librqbit::{AddTorrent, AddTorrentOptions, ManagedTorrent, Session};
use librqbit_core::hash_id::Id20;
use support::{Seeder, bounded, new_session};
use torrplay_engine::disk::{DiskStorageFactory, bitfield};
use torrplay_fixtures::{FixtureSpec, catalog, generator};

/// Adds `spec` to `session` in disk storage below `directory`, with `bits`
/// as its resume bitfield and initial pieces.
async fn add(
    session: &Arc<Session>,
    spec: &FixtureSpec,
    directory: &Path,
    bits: Vec<u8>,
    peers: Vec<std::net::SocketAddr>,
) -> anyhow::Result<Arc<ManagedTorrent>> {
    let factory = DiskStorageFactory::new();
    factory.expect(
        Id20::new(spec.metainfo().info_hash),
        directory.to_path_buf(),
        bits.clone(),
    )?;
    let handle = session
        .add_torrent(
            AddTorrent::from_bytes(spec.metainfo().bytes),
            Some(AddTorrentOptions {
                storage_factory: Some(factory.into_boxed()),
                initial_peers: Some(peers),
                initial_pieces: Some(bits),
                ..Default::default()
            }),
        )
        .await?
        .into_handle()
        .context("the torrent")?;
    handle.wait_until_initialized().await?;
    Ok(handle)
}

/// DSK-01's engine part: downloading into the torrent's directory, then
/// starting again from the resume bitfield.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_disk_torrent_starts_again_from_its_resume_bitfield() -> anyhow::Result<()> {
    let spec = catalog::multi_boundary();
    let seeder = Seeder::start(&spec, &spec.metainfo().bytes).await?;
    let downloads = tempfile::tempdir()?;
    let directory = downloads.path().join("trr_7ndq4m2kzb6hxw3rjpt5yc4ula");
    let pieces = u32::try_from(spec.metainfo().piece_count)?;
    let none = vec![0; bitfield::bitfield_bytes(pieces)];

    let session_dir = tempfile::tempdir()?;
    let session = new_session(session_dir.path()).await?;
    let handle = add(&session, &spec, &directory, none, vec![seeder.addr]).await?;
    bounded("downloading the torrent", handle.wait_until_completed()).await?;
    session.stop().await;
    seeder.stop().await;

    for (index, file) in spec.files().iter().enumerate() {
        let mut path = directory.join(&spec.torrent_name);
        path.extend(&file.path);
        let bytes = std::fs::read(&path).with_context(|| format!("{}", path.display()))?;
        let mut expected = vec![0; usize::try_from(file.size_bytes)?];
        generator::fill(spec.file_seed(index), 0, &mut expected);
        assert!(bytes == expected, "file {index} at its API path");
    }
    let bits =
        bitfield::load(&bitfield::path_for(&directory), pieces)?.context("a resume bitfield")?;
    let recorded: u32 = bits.iter().map(|byte| byte.count_ones()).sum();
    assert_eq!(recorded, pieces, "every verified piece is recorded");

    // No peer is reachable now, so only the bitfield can complete it.
    let session_dir = tempfile::tempdir()?;
    let session = new_session(session_dir.path()).await?;
    let handle = add(&session, &spec, &directory, bits, Vec::new()).await?;
    let stats = handle.stats();
    assert_eq!(
        stats.progress_bytes,
        spec.total_size_bytes(),
        "the torrent starts complete from its bitfield"
    );
    session.stop().await;
    Ok(())
}
