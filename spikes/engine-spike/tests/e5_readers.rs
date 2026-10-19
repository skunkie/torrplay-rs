// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! E5: readers that hold no bounded engine resource while they wait (patch
//! P3). librqbit's blocking semaphore has eight permits by default, and
//! piece writes need one; a stream that holds one while it waits for a
//! missing piece takes it from the writes that would deliver the piece.

use std::io::SeekFrom;
use std::sync::Arc;

use anyhow::Context;
use engine_spike::{Leecher, Seeder, bounded};
use librqbit::{AddTorrent, AddTorrentOptions, ManagedTorrent};
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use torrplay_engine::cache::PieceCache;
use torrplay_engine::storage::{CachedTorrent, MemoryStorageFactory};
use torrplay_fixtures::{FixtureSpec, catalog, generator};

const MIB: u64 = 1024 * 1024;

/// More readers than the blocking semaphore's eight permits.
const READERS: u64 = 12;

/// Adds `spec` in memory storage, paused until attached and initialized,
/// as E3 found the adapter must.
async fn add_paused_in_memory(
    leecher: &Leecher,
    seeder: &Seeder,
    spec: &FixtureSpec,
    factory: &MemoryStorageFactory,
) -> anyhow::Result<(Arc<ManagedTorrent>, CachedTorrent)> {
    let options = AddTorrentOptions {
        storage_factory: Some(factory.clone().into_boxed()),
        disable_upload: true,
        paused: true,
        ..leecher.add_options(&[seeder.addr])?
    };
    let handle = bounded(
        "adding a torrent",
        leecher
            .session
            .add_torrent(AddTorrent::from_bytes(spec.metainfo().bytes), Some(options)),
    )
    .await?
    .into_handle()
    .context("the torrent was not added")?;
    let cached = factory.attach(&handle)?;
    bounded("the initial check", handle.wait_until_initialized()).await?;
    Ok((handle, cached))
}

/// RDR-04: twelve readers open streams on pieces that are all missing, and
/// only then does the torrent start. Every reader must get its bytes, so
/// piece writes continued while the readers waited.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn twelve_waiting_readers_do_not_block_piece_writes() -> anyhow::Result<()> {
    let spec = catalog::single_small();
    let piece_length = u64::from(spec.piece_length);
    let seeder = Seeder::start(&spec, &spec.metainfo().bytes).await?;
    let leecher = Leecher::start().await?;
    let factory = MemoryStorageFactory::new(PieceCache::new(4 * MIB));
    let (handle, _cached) = add_paused_in_memory(&leecher, &seeder, &spec, &factory).await?;

    // Each reader opens its stream and seeks before the torrent starts, so
    // all twelve are waiting at once.
    let (opened_tx, mut opened_rx) = tokio::sync::mpsc::channel(READERS as usize);
    let (start_tx, _) = tokio::sync::broadcast::channel::<()>(1);
    let readers: Vec<_> = (0..READERS)
        .map(|reader| {
            let handle = handle.clone();
            let opened = opened_tx.clone();
            let mut start = start_tx.subscribe();
            let offset = reader * 5 * piece_length + 1000;
            let seed = spec.file_seed(0);
            tokio::spawn(async move {
                let mut stream = bounded("opening a stream", handle.clone().stream(0)).await?;
                bounded("seeking", async {
                    Ok(stream.seek(SeekFrom::Start(offset)).await?)
                })
                .await?;
                opened.send(()).await?;
                start.recv().await?;
                let mut bytes = vec![0; piece_length as usize];
                bounded("reading a missing piece", async {
                    Ok(stream.read_exact(&mut bytes).await?)
                })
                .await?;
                anyhow::ensure!(
                    generator::first_mismatch(seed, offset, &bytes).is_none(),
                    "reader {reader} read wrong bytes"
                );
                anyhow::Ok(())
            })
        })
        .collect();
    drop(opened_tx);
    bounded("every reader opening its stream", async {
        for _ in 0..READERS {
            opened_rx.recv().await.context("a reader ended early")?;
        }
        Ok(())
    })
    .await?;

    let _ = start_tx.send(());
    bounded("starting the torrent", leecher.session.unpause(&handle)).await?;
    for reader in readers {
        reader.await??;
    }

    leecher.session.stop().await;
    seeder.session.stop().await;
    Ok(())
}
