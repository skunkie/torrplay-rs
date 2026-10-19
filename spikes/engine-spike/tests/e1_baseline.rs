// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! E1: the baseline on unpatched librqbit with its default disk storage.
//! Each test names the `docs/acceptance.md` scenario it covers, or the
//! engine behavior it records for the capability report.

use std::io::SeekFrom;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use engine_spike::{Leecher, Seeder, bounded};
use librqbit::api::TorrentIdOrHash;
use librqbit::{AddTorrent, AddTorrentOptions, ManagedTorrent};
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use torrplay_fixtures::{FixtureSpec, catalog, generator};

/// Reads `len` bytes of file `file_id` from `offset`, or to the end when
/// `len` is `None`.
async fn read_range(
    handle: &Arc<ManagedTorrent>,
    file_id: usize,
    offset: u64,
    len: Option<usize>,
) -> anyhow::Result<Vec<u8>> {
    let mut stream = bounded("opening a stream", handle.clone().stream(file_id)).await?;
    bounded("seeking", async {
        Ok(stream.seek(SeekFrom::Start(offset)).await?)
    })
    .await?;
    let mut bytes = Vec::new();
    match len {
        Some(len) => {
            bytes.resize(len, 0);
            bounded("reading a range", async {
                Ok(stream.read_exact(&mut bytes).await?)
            })
            .await?;
        }
        None => {
            bounded("reading to the end", async {
                Ok(stream.read_to_end(&mut bytes).await?)
            })
            .await?;
        }
    }
    Ok(bytes)
}

/// Starts a seeder for `spec` and a leecher that adds it by `add`, with the
/// seeder as its only peer.
async fn seeded_download(
    spec: &FixtureSpec,
    add: impl FnOnce(&FixtureSpec, &[u8]) -> AddTorrent<'static>,
) -> anyhow::Result<(Seeder, Leecher, Arc<ManagedTorrent>)> {
    let metainfo = spec.metainfo();
    let seeder = Seeder::start(spec, &metainfo.bytes).await?;
    let leecher = Leecher::start().await?;
    let handle = bounded(
        "adding the torrent to the leecher",
        leecher.session.add_torrent(
            add(spec, &metainfo.bytes),
            Some(leecher.add_options(&[seeder.addr])?),
        ),
    )
    .await?
    .into_handle()
    .context("the torrent was not added")?;
    Ok((seeder, leecher, handle))
}

fn by_upload(_: &FixtureSpec, metainfo: &[u8]) -> AddTorrent<'static> {
    AddTorrent::from_bytes(metainfo.to_vec())
}

fn by_magnet(spec: &FixtureSpec, _: &[u8]) -> AddTorrent<'static> {
    AddTorrent::from_url(spec.magnet_uri(&spec.metainfo().info_hash))
}

fn torrent_count(leecher: &Leecher) -> usize {
    leecher.session.with_torrents(|torrents| torrents.count())
}

async fn stop(seeder: Seeder, leecher: Leecher) {
    leecher.session.stop().await;
    seeder.session.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn uploaded_metadata_is_usable_without_peers() -> anyhow::Result<()> {
    let spec = catalog::multi_boundary();
    let leecher = Leecher::start().await?;
    let handle = bounded(
        "adding an uploaded torrent with no peers",
        leecher.session.add_torrent(
            AddTorrent::from_bytes(spec.metainfo().bytes),
            Some(leecher.add_options(&[])?),
        ),
    )
    .await?
    .into_handle()
    .context("the torrent was not added")?;
    let files = handle.with_metadata(|metadata| {
        metadata
            .file_infos
            .iter()
            .map(|file| file.len)
            .collect::<Vec<_>>()
    })?;
    let expected: Vec<u64> = spec.files().iter().map(|file| file.size_bytes).collect();
    assert_eq!(
        files, expected,
        "file sizes come from the uploaded metadata at once"
    );
    assert_eq!(handle.name().as_deref(), Some("multi-boundary"));
    leecher.session.stop().await;
    Ok(())
}

/// LIF-02.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn magnet_metadata_is_retrieved_from_a_local_seeder() -> anyhow::Result<()> {
    let spec = catalog::multi_boundary();
    let (seeder, leecher, handle) = seeded_download(&spec, by_magnet).await?;
    assert_eq!(handle.info_hash().0, spec.metainfo().info_hash);
    let files = handle.with_metadata(|metadata| metadata.file_infos.len())?;
    assert_eq!(files, spec.files().len());
    stop(seeder, leecher).await;
    Ok(())
}

/// Records that a magnet URI is rejected when no peer source exists, rather
/// than waiting for peers as the lifecycle's metadata retrieval does.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn magnet_without_any_peer_source_is_rejected() -> anyhow::Result<()> {
    let spec = catalog::single_small();
    let leecher = Leecher::start().await?;
    let result = bounded(
        "adding a magnet with no peer source",
        leecher.session.add_torrent(
            AddTorrent::from_url(spec.magnet_uri(&spec.metainfo().info_hash)),
            Some(AddTorrentOptions {
                initial_peers: None,
                ..leecher.add_options(&[])?
            }),
        ),
    )
    .await;
    assert!(
        result.is_err(),
        "with DHT and trackers off, librqbit has no way to find peers"
    );
    eprintln!(
        "magnet without peers: {:#}",
        result.err().context("checked above")?
    );
    assert_eq!(torrent_count(&leecher), 0);
    leecher.session.stop().await;
    Ok(())
}

/// The basis for LIF-03. librqbit retrieves a magnet's metadata inside
/// `add_torrent`, before the torrent exists, so pausing during retrieval
/// means canceling that future. Canceling must leave no torrent behind and
/// close the connection to the peer.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn canceling_metadata_retrieval_leaves_no_torrent() -> anyhow::Result<()> {
    let spec = catalog::single_small();
    // A peer that accepts connections and never answers the handshake.
    let silent = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let silent_addr = silent.local_addr()?;
    let leecher = Leecher::start().await?;
    let options = leecher.add_options(&[silent_addr])?;
    let add = leecher.session.add_torrent(
        AddTorrent::from_url(spec.magnet_uri(&spec.metainfo().info_hash)),
        Some(options),
    );
    let accepted = tokio::spawn(async move { silent.accept().await });
    let outcome = tokio::time::timeout(Duration::from_secs(2), add).await;
    assert!(
        outcome.is_err(),
        "retrieval waits for metadata without a deadline"
    );
    assert_eq!(
        torrent_count(&leecher),
        0,
        "a canceled retrieval leaves no torrent"
    );

    let (mut socket, _) = bounded("the silent peer's connection", async {
        Ok(accepted.await??)
    })
    .await?;
    let mut buf = [0; 1024];
    let closed = bounded("the leecher closing the connection", async {
        loop {
            if socket.read(&mut buf).await? == 0 {
                return Ok(());
            }
        }
    })
    .await;
    assert!(
        closed.is_ok(),
        "dropping the retrieval must close its peer connections"
    );
    leecher.session.stop().await;
    Ok(())
}

/// STR-01, through the engine.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn streams_every_file_byte_for_byte() -> anyhow::Result<()> {
    let spec = catalog::multi_boundary();
    let (seeder, leecher, handle) = seeded_download(&spec, by_upload).await?;
    for (index, file) in spec.files().iter().enumerate() {
        let bytes = read_range(&handle, index, 0, None).await?;
        assert_eq!(bytes.len() as u64, file.size_bytes, "file {index}");
        assert_eq!(
            generator::first_mismatch(spec.file_seed(index), 0, &bytes),
            None,
            "file {index}"
        );
    }
    stop(seeder, leecher).await;
    Ok(())
}

/// STR-02, through the engine.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn streams_ranges_across_piece_and_file_boundaries() -> anyhow::Result<()> {
    let spec = catalog::multi_boundary();
    let (seeder, leecher, handle) = seeded_download(&spec, by_upload).await?;
    // part-4.bin starts at torrent offset 824,290, so torrent offset
    // 1,048,576, a piece boundary, is file offset 224,286.
    let file = 3;
    let size = spec.files()[file].size_bytes;
    let seed = spec.file_seed(file);
    for (offset, len) in [
        (0, Some(1)),
        (size - 1, Some(1)),
        (224_276, Some(20)),
        (100_000, None),
        (size - 1000, None),
    ] {
        let bytes = read_range(&handle, file, offset, len).await?;
        let expected_len = len.map_or(size - offset, |len| len as u64);
        assert_eq!(bytes.len() as u64, expected_len, "offset {offset}");
        assert_eq!(
            generator::first_mismatch(seed, offset, &bytes),
            None,
            "offset {offset}"
        );
    }
    // part-2.bin is one byte inside a piece shared with its neighbors.
    let bytes = read_range(&handle, 1, 0, None).await?;
    assert_eq!(bytes, [generator::byte_at(spec.file_seed(1), 0)]);
    stop(seeder, leecher).await;
    Ok(())
}

/// Records how a stream in progress behaves when its torrent is deleted:
/// it must end with an error or end of file within the bound, not hang.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn deleting_a_torrent_ends_its_stream() -> anyhow::Result<()> {
    let spec = catalog::large(8 * 1024 * 1024);
    let (seeder, leecher, handle) = seeded_download(&spec, by_upload).await?;
    let mut stream = bounded("opening a stream", handle.clone().stream(0)).await?;
    let mut first = vec![0; 64 * 1024];
    bounded("reading before deletion", async {
        Ok(stream.read_exact(&mut first).await?)
    })
    .await?;
    assert_eq!(generator::first_mismatch(spec.seed, 0, &first), None);

    bounded(
        "deleting the torrent",
        leecher
            .session
            .delete(TorrentIdOrHash::Id(handle.id()), false),
    )
    .await?;
    let mut rest = Vec::new();
    let after =
        tokio::time::timeout(engine_spike::STEP_TIMEOUT, stream.read_to_end(&mut rest)).await;
    assert!(
        after.is_ok(),
        "a stream must not hang after its torrent is deleted"
    );
    // Recorded for the capability report: how the stream ended.
    eprintln!(
        "stream after deletion: {after:?}, {} more bytes",
        rest.len()
    );
    assert_eq!(torrent_count(&leecher), 0);
    stop(seeder, leecher).await;
    Ok(())
}

/// API-20 and API-06, through the engine: which metainfo librqbit accepts.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn records_which_v2_and_hybrid_torrents_load() -> anyhow::Result<()> {
    let leecher = Leecher::start().await?;
    let add = |bytes: Vec<u8>| {
        let options = leecher.add_options(&[]);
        let session = leecher.session.clone();
        async move {
            let options = AddTorrentOptions {
                paused: true,
                ..options?
            };
            session
                .add_torrent(AddTorrent::from_bytes(bytes), Some(options))
                .await
        }
    };

    let invalid = bounded("adding invalid metainfo", async {
        Ok(add(catalog::invalid_metainfo()).await)
    })
    .await?;
    assert!(invalid.is_err(), "truncated metainfo is rejected");

    let v2_only = bounded("adding a v2-only torrent", async {
        Ok(add(catalog::v2_only().v2_only_metainfo()).await)
    })
    .await?;
    assert!(
        v2_only.is_err(),
        "librqbit reads only the v1 info dictionary"
    );
    eprintln!("v2-only: {:#}", v2_only.err().context("checked above")?);

    let hybrid = catalog::hybrid();
    let metainfo = hybrid.hybrid_metainfo();
    let handle = bounded("adding a hybrid torrent", add(metainfo.bytes))
        .await?
        .into_handle()
        .context("the hybrid torrent was not added")?;
    assert_eq!(
        handle.info_hash().0,
        metainfo.info_hash,
        "a hybrid torrent loads under its v1 hash"
    );

    let equal_padding = bounded("adding a hybrid torrent with equal padding", async {
        Ok(add(catalog::hybrid_equal_padding().hybrid_metainfo().bytes).await)
    })
    .await?;
    assert!(
        equal_padding.is_err(),
        "until upstream PR #701, duplicate .pad names are rejected (ADR 2)"
    );
    eprintln!(
        "equal padding: {:#}",
        equal_padding.err().context("checked above")?
    );
    leecher.session.stop().await;
    Ok(())
}
