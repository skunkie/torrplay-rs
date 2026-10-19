// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! E4: the peer policy of ADR 3, with the per-torrent upload switch from
//! patch P2. A memory torrent must never tell a peer about a piece the
//! cache may evict. A minimal raw peer connects to the torrent and records
//! the messages it receives, which librqbit's own peers do not expose.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Context;
use engine_spike::{Leecher, Seeder, bounded};
use librqbit::api::LiveStats;
use librqbit::{AddTorrent, AddTorrentOptions, ManagedTorrent};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use torrplay_engine::cache::PieceCache;
use torrplay_engine::storage::{CachedTorrent, MemoryStorageFactory};
use torrplay_fixtures::{FixtureSpec, catalog, generator};

const MIB: u64 = 1024 * 1024;
const BLOCK_BYTES: u32 = 16 * 1024;

// BEP 3 message IDs.
const UNCHOKE: u8 = 1;
const INTERESTED: u8 = 2;
const HAVE: u8 = 4;
const BITFIELD: u8 = 5;
const REQUEST: u8 = 6;
const PIECE: u8 = 7;

/// A peer that speaks just enough of BEP 3 to observe another peer.
struct ProbePeer {
    stream: TcpStream,
}

impl ProbePeer {
    /// Connects and exchanges handshakes, advertising no extensions.
    async fn connect(addr: SocketAddr, info_hash: [u8; 20]) -> anyhow::Result<Self> {
        let mut stream = bounded("connecting the probe", async {
            Ok(TcpStream::connect(addr).await?)
        })
        .await?;
        let mut handshake = Vec::with_capacity(68);
        handshake.push(19);
        handshake.extend_from_slice(b"BitTorrent protocol");
        handshake.extend_from_slice(&[0; 8]);
        handshake.extend_from_slice(&info_hash);
        handshake.extend_from_slice(b"-TP0000-probe-peer-0");
        stream.write_all(&handshake).await?;
        let mut reply = [0; 68];
        bounded("reading the handshake", async {
            Ok(stream.read_exact(&mut reply).await?)
        })
        .await?;
        anyhow::ensure!(
            reply[28..48] == info_hash,
            "the peer answered for another torrent"
        );
        Ok(Self { stream })
    }

    async fn send(&mut self, id: u8, payload: &[u8]) -> anyhow::Result<()> {
        let length = u32::try_from(payload.len() + 1)?;
        self.stream.write_all(&length.to_be_bytes()).await?;
        self.stream.write_u8(id).await?;
        self.stream.write_all(payload).await?;
        Ok(())
    }

    /// Reads the next message, skipping keep-alives. Returns `None` once the
    /// peer closes the connection or resets it.
    async fn next(&mut self) -> anyhow::Result<Option<(u8, Vec<u8>)>> {
        loop {
            let mut length = [0; 4];
            match self.stream.read_exact(&mut length).await {
                Ok(_) => {}
                Err(error) if is_closed(&error) => return Ok(None),
                Err(error) => return Err(error.into()),
            }
            let length = u32::from_be_bytes(length) as usize;
            if length == 0 {
                continue;
            }
            anyhow::ensure!(
                length <= 1 << 20,
                "a {length}-byte message is not plausible"
            );
            let mut message = vec![0; length];
            match self.stream.read_exact(&mut message).await {
                Ok(_) => {}
                Err(error) if is_closed(&error) => return Ok(None),
                Err(error) => return Err(error.into()),
            }
            let id = message.remove(0);
            return Ok(Some((id, message)));
        }
    }

    /// Collects the IDs of the messages received during `window`.
    async fn collect_for(&mut self, window: Duration) -> anyhow::Result<Vec<u8>> {
        let mut ids = Vec::new();
        let deadline = tokio::time::Instant::now() + window;
        loop {
            match tokio::time::timeout_at(deadline, self.next()).await {
                Err(_) => return Ok(ids),
                Ok(message) => match message? {
                    Some((id, _)) => ids.push(id),
                    None => anyhow::bail!("the peer closed the connection after {ids:?}"),
                },
            }
        }
    }

    /// Requests the first block of `piece`.
    async fn request_block(&mut self, piece: u32) -> anyhow::Result<()> {
        let mut payload = Vec::with_capacity(12);
        payload.extend_from_slice(&piece.to_be_bytes());
        payload.extend_from_slice(&0u32.to_be_bytes());
        payload.extend_from_slice(&BLOCK_BYTES.to_be_bytes());
        self.send(REQUEST, &payload).await
    }
}

fn is_closed(error: &std::io::Error) -> bool {
    use std::io::ErrorKind::{BrokenPipe, ConnectionAborted, ConnectionReset, UnexpectedEof};
    matches!(
        error.kind(),
        UnexpectedEof | ConnectionReset | ConnectionAborted | BrokenPipe
    )
}

/// Adds `spec` with the seeder as its only peer, in memory storage from
/// `factory` when given.
async fn add(
    leecher: &Leecher,
    seeder: &Seeder,
    spec: &FixtureSpec,
    factory: Option<&MemoryStorageFactory>,
    disable_upload: bool,
    paused: bool,
) -> anyhow::Result<Arc<ManagedTorrent>> {
    let options = AddTorrentOptions {
        storage_factory: factory.map(|factory| factory.clone().into_boxed()),
        disable_upload,
        paused,
        ..leecher.add_options(&[seeder.addr])?
    };
    bounded(
        "adding a torrent",
        leecher
            .session
            .add_torrent(AddTorrent::from_bytes(spec.metainfo().bytes), Some(options)),
    )
    .await?
    .into_handle()
    .context("the torrent was not added")
}

/// Adds `spec` in memory storage without upload, as TorrPlay's adapter will:
/// paused until attached and initialized, as E3 found it must be.
async fn add_in_memory(
    leecher: &Leecher,
    seeder: &Seeder,
    spec: &FixtureSpec,
    factory: &MemoryStorageFactory,
) -> anyhow::Result<(Arc<ManagedTorrent>, CachedTorrent)> {
    let handle = add(leecher, seeder, spec, Some(factory), true, true).await?;
    let cached = factory.attach(&handle)?;
    bounded("the initial check", handle.wait_until_initialized()).await?;
    bounded("starting the torrent", leecher.session.unpause(&handle)).await?;
    Ok((handle, cached))
}

fn live_stats(handle: &ManagedTorrent) -> anyhow::Result<LiveStats> {
    handle.stats().live.context("the torrent is not live")
}

fn leecher_addr(leecher: &Leecher) -> anyhow::Result<SocketAddr> {
    leecher
        .session
        .listen_addr()
        .context("the leecher session has no listen address")
}

/// The probe sees bitfield, unchoke, and served blocks from a torrent that
/// uploads, so its silence in the next test means something.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_probe_observes_an_uploading_torrent() -> anyhow::Result<()> {
    let spec = catalog::single_small();
    let seeder = Seeder::start(&spec, &spec.metainfo().bytes).await?;
    let leecher = Leecher::start().await?;
    let handle = add(&leecher, &seeder, &spec, None, false, false).await?;
    bounded("completing the download", handle.wait_until_completed()).await?;

    let mut probe = ProbePeer::connect(leecher_addr(&leecher)?, spec.metainfo().info_hash).await?;
    let received = probe.collect_for(Duration::from_millis(500)).await?;
    assert!(received.contains(&BITFIELD), "{received:?}");
    assert!(received.contains(&UNCHOKE), "{received:?}");
    probe.send(INTERESTED, &[]).await?;
    probe.request_block(0).await?;
    let piece = bounded("receiving the block", async {
        loop {
            match probe.next().await? {
                Some((PIECE, payload)) => return Ok(payload),
                Some(_) => {}
                None => anyhow::bail!("the peer closed the connection"),
            }
        }
    })
    .await?;
    assert_eq!(piece.len(), 8 + BLOCK_BYTES as usize);
    assert_eq!(
        generator::first_mismatch(spec.file_seed(0), 0, &piece[8..]),
        None
    );

    leecher.session.stop().await;
    seeder.session.stop().await;
    Ok(())
}

/// PEER-01: a memory torrent that keeps completing pieces sends a
/// connected peer no bitfield, no `have`, and no unchoke; a request closes
/// the connection; upload statistics stay zero.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_memory_torrent_announces_nothing_and_refuses_requests() -> anyhow::Result<()> {
    let capacity = 4 * MIB;
    let spec = catalog::large(capacity);
    let seeder = Seeder::start(&spec, &spec.metainfo().bytes).await?;
    let leecher = Leecher::start().await?;
    let factory = MemoryStorageFactory::new(PieceCache::new(capacity));
    let (handle, _cached) = add_in_memory(&leecher, &seeder, &spec, &factory).await?;

    let mut probe = ProbePeer::connect(leecher_addr(&leecher)?, spec.metainfo().info_hash).await?;
    let before = live_stats(&handle)?.snapshot.downloaded_and_checked_pieces;
    let received = probe.collect_for(Duration::from_secs(1)).await?;
    let completed = live_stats(&handle)?.snapshot.downloaded_and_checked_pieces - before;
    assert!(completed > 0, "pieces completed while the probe listened");
    for id in [BITFIELD, HAVE, UNCHOKE] {
        assert!(!received.contains(&id), "message {id} in {received:?}");
    }

    probe.send(INTERESTED, &[]).await?;
    probe.request_block(0).await?;
    bounded("the connection closing", async {
        while let Some((id, _)) = probe.next().await? {
            anyhow::ensure!(id != PIECE, "the memory torrent served a block");
        }
        Ok(())
    })
    .await?;
    let stats = handle.stats();
    assert_eq!(stats.uploaded_bytes, 0);
    assert_eq!(live_stats(&handle)?.snapshot.uploaded_bytes, 0);

    leecher.session.stop().await;
    seeder.session.stop().await;
    Ok(())
}

/// PEER-02: a finished memory torrent drops its seeder as not needed. When
/// a piece is evicted and a new stream needs it, the seeder reconnects and
/// the stream succeeds. Records time to first byte, including the
/// reconnection, for ADR 3.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_stream_after_an_idle_period_reconnects_and_succeeds() -> anyhow::Result<()> {
    let spec = catalog::single_small();
    let piece_length = u64::from(spec.piece_length);
    let seeder = Seeder::start(&spec, &spec.metainfo().bytes).await?;
    let leecher = Leecher::start().await?;
    let factory = MemoryStorageFactory::new(PieceCache::new(4 * MIB));
    let (handle, cached) = add_in_memory(&leecher, &seeder, &spec, &factory).await?;

    let mut resident = Vec::new();
    let mut after_eviction = Vec::new();
    for round in 0..5u32 {
        bounded("completing the download", handle.wait_until_completed()).await?;
        bounded("the seeder dropped as not needed", async {
            loop {
                let peers = live_stats(&handle)?.snapshot.peer_stats;
                if peers.live == 0 && peers.not_needed == 1 {
                    return Ok(());
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await?;
        let piece = 10 + round * 7;
        let offset = u64::from(piece) * piece_length;
        resident.push(time_to_first_byte(&handle, &spec, offset).await?);

        let started = Instant::now();
        assert_eq!(cached.evict(&[piece])?, 1);
        time_to_first_byte(&handle, &spec, offset).await?;
        after_eviction.push(started.elapsed());
    }
    resident.sort();
    after_eviction.sort();
    eprintln!("PEER-02 time to first byte, resident piece: {resident:?}");
    eprintln!("PEER-02 time to first byte, evicted piece with reconnection: {after_eviction:?}");

    leecher.session.stop().await;
    seeder.session.stop().await;
    Ok(())
}

/// Opens a stream at `offset`, reads one byte, and checks it.
async fn time_to_first_byte(
    handle: &Arc<ManagedTorrent>,
    spec: &FixtureSpec,
    offset: u64,
) -> anyhow::Result<Duration> {
    use std::io::SeekFrom;
    use tokio::io::AsyncSeekExt;

    let started = Instant::now();
    let mut stream = bounded("opening a stream", handle.clone().stream(0)).await?;
    bounded("seeking", async {
        Ok(stream.seek(SeekFrom::Start(offset)).await?)
    })
    .await?;
    let mut byte = [0];
    bounded("reading the first byte", async {
        Ok(stream.read_exact(&mut byte).await?)
    })
    .await?;
    let elapsed = started.elapsed();
    assert_eq!(
        generator::first_mismatch(spec.file_seed(0), offset, &byte),
        None
    );
    Ok(elapsed)
}
