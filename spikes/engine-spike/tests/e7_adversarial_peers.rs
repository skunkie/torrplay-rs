// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! E7: peers that send bad data. A seeder whose content is corrupted after
//! its initial check serves corrupt blocks, because librqbit does not hash
//! what it uploads (CAC-11). A minimal BEP 9 peer serves metadata that does
//! not match the info hash (LIF-12).

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use anyhow::Context;
use engine_spike::{Leecher, MemoryCache, Seeder, bounded};
use librqbit::{AddTorrent, ManagedTorrent, TorrentStatsState};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinSet;
use torrplay_fixtures::{FixtureSpec, catalog, generator};

const KIB: u64 = 1024;
const MIB: u64 = 1024 * KIB;

/// BEP 3 message ID of extension messages (BEP 10).
const EXTENDED: u8 = 20;
/// The ID this file's fake peer gives `ut_metadata`.
const UT_METADATA: u8 = 1;
/// BEP 9 metadata piece size.
const METADATA_PIECE: usize = 16 * 1024;

fn live_snapshot(handle: &ManagedTorrent) -> anyhow::Result<librqbit::api::LiveStats> {
    handle.stats().live.context("the torrent is not live")
}

/// CAC-11: a peer that sends corrupt blocks has them fail the hash check;
/// the pieces come again from a valid peer, and the reader receives only
/// correct bytes.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn corrupt_blocks_fail_and_come_again_from_a_valid_peer() -> anyhow::Result<()> {
    let spec = catalog::single_small();
    let metainfo = spec.metainfo();
    let corrupt = Seeder::start(&spec, &metainfo.bytes).await?;
    // librqbit hashes the content once, at its initial check, and serves
    // later reads unchecked: flipping it now makes every block corrupt.
    let mut content = tokio::fs::read(&corrupt.root).await?;
    content.iter_mut().for_each(|byte| *byte ^= 0xff);
    tokio::fs::write(&corrupt.root, &content).await?;

    let leecher = Leecher::start().await?;
    let memory = MemoryCache::start(4 * MIB);
    let torrent = memory
        .add_scheduled(&leecher, &[corrupt.addr], &spec, 256 * KIB)
        .await?;
    let mut reader = bounded(
        "opening a reader",
        torrent.scheduler.open(&torrent.handle, 0),
    )
    .await?;
    let read = tokio::spawn(async move {
        let mut bytes = Vec::new();
        bounded("reading the file", async {
            Ok(reader.read_to_end(&mut bytes).await?)
        })
        .await?;
        anyhow::Ok(bytes)
    });

    // Only the corrupt peer is known yet: blocks arrive, and none verify.
    bounded("corrupt blocks arriving", async {
        loop {
            let snapshot = live_snapshot(&torrent.handle)?.snapshot;
            if snapshot.fetched_bytes >= u64::from(spec.piece_length) {
                anyhow::ensure!(
                    snapshot.downloaded_and_checked_bytes == 0,
                    "a corrupt piece passed its hash check"
                );
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await?;

    let leecher_addr = leecher
        .session
        .listen_addr()
        .context("the leecher has no listen address")?;
    let valid = Seeder::start_with_peers(&spec, &metainfo.bytes, &[leecher_addr]).await?;
    let bytes = read.await??;
    assert_eq!(bytes.len() as u64, spec.total_size_bytes());
    assert_eq!(
        generator::first_mismatch(spec.file_seed(0), 0, &bytes),
        None
    );
    let snapshot = live_snapshot(&torrent.handle)?.snapshot;
    assert_eq!(
        snapshot.downloaded_and_checked_bytes,
        spec.total_size_bytes()
    );
    assert!(
        snapshot.fetched_bytes > snapshot.downloaded_and_checked_bytes,
        "the corrupt blocks were fetched and discarded"
    );
    assert!(!matches!(
        torrent.handle.stats().state,
        TorrentStatsState::Error
    ));
    memory.cache.audit()?;

    leecher.session.stop().await;
    corrupt.session.stop().await;
    valid.session.stop().await;
    memory.shutdown().await?;
    Ok(())
}

/// A peer that offers metadata through BEP 9 and serves `metadata` for it,
/// whatever the info hash.
struct FakeMetadataPeer {
    addr: SocketAddr,
    served: Arc<AtomicUsize>,
    tasks: JoinSet<anyhow::Result<()>>,
}

impl FakeMetadataPeer {
    async fn start(metadata: Vec<u8>) -> anyhow::Result<Self> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?;
        let addr = listener.local_addr()?;
        let served = Arc::new(AtomicUsize::new(0));
        let metadata = Arc::new(metadata);
        let mut tasks = JoinSet::new();
        let counter = served.clone();
        tasks.spawn(async move {
            let mut connections = JoinSet::new();
            loop {
                let (stream, _) = listener.accept().await?;
                connections.spawn(serve_metadata(stream, metadata.clone(), counter.clone()));
            }
        });
        Ok(Self {
            addr,
            served,
            tasks,
        })
    }

    async fn shutdown(mut self) {
        self.tasks.abort_all();
        while self.tasks.join_next().await.is_some() {}
    }
}

async fn send(stream: &mut TcpStream, id: u8, payload: &[u8]) -> anyhow::Result<()> {
    stream
        .write_all(&u32::try_from(payload.len() + 1)?.to_be_bytes())
        .await?;
    stream.write_u8(id).await?;
    stream.write_all(payload).await?;
    Ok(())
}

/// Returns the integer after `key` in a bencoded dictionary.
fn bencoded_int(message: &[u8], key: &[u8]) -> Option<u64> {
    let start = message
        .windows(key.len())
        .position(|window| window == key)?
        + key.len();
    let digits = message.get(start..)?.strip_prefix(b"i")?;
    let end = digits.iter().position(|&byte| byte == b'e')?;
    std::str::from_utf8(&digits[..end]).ok()?.parse().ok()
}

async fn serve_metadata(
    mut stream: TcpStream,
    metadata: Arc<Vec<u8>>,
    served: Arc<AtomicUsize>,
) -> anyhow::Result<()> {
    let mut handshake = [0; 68];
    stream.read_exact(&mut handshake).await?;
    let mut reply = handshake;
    reply[20..28].copy_from_slice(&[0, 0, 0, 0, 0, 0x10, 0, 0]);
    reply[48..68].copy_from_slice(b"-TP0000-fake-meta-00");
    stream.write_all(&reply).await?;
    let mut extended = vec![0];
    extended.extend_from_slice(
        format!(
            "d1:md11:ut_metadatai{UT_METADATA}ee13:metadata_sizei{}ee",
            metadata.len()
        )
        .as_bytes(),
    );
    send(&mut stream, EXTENDED, &extended).await?;

    let mut their_ut_metadata = None;
    loop {
        let mut length = [0; 4];
        if stream.read_exact(&mut length).await.is_err() {
            return Ok(());
        }
        let mut message = vec![0; u32::from_be_bytes(length) as usize];
        if stream.read_exact(&mut message).await.is_err() {
            return Ok(());
        }
        let [EXTENDED, extension, body @ ..] = message.as_slice() else {
            continue;
        };
        if *extension == 0 {
            their_ut_metadata =
                bencoded_int(body, b"11:ut_metadata").and_then(|id| u8::try_from(id).ok());
            continue;
        }
        let (Some(reply_id), true, Some(0)) = (
            their_ut_metadata,
            *extension == UT_METADATA,
            bencoded_int(body, b"8:msg_type"),
        ) else {
            continue;
        };
        let piece = usize::try_from(bencoded_int(body, b"5:piece").unwrap_or_default())?;
        let chunk = metadata
            .chunks(METADATA_PIECE)
            .nth(piece)
            .context("a request for a metadata piece out of range")?;
        let mut data = vec![reply_id];
        data.extend_from_slice(
            format!(
                "d8:msg_typei1e5:piecei{piece}e10:total_sizei{}ee",
                metadata.len()
            )
            .as_bytes(),
        );
        data.extend_from_slice(chunk);
        send(&mut stream, EXTENDED, &data).await?;
        served.fetch_add(1, Ordering::Relaxed);
    }
}

/// Forwards connections to `target` after `delay`, so the peer behind it
/// answers last.
async fn delaying_proxy(
    target: SocketAddr,
    delay: Duration,
) -> anyhow::Result<(SocketAddr, JoinSet<anyhow::Result<()>>)> {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?;
    let addr = listener.local_addr()?;
    let mut tasks = JoinSet::new();
    tasks.spawn(async move {
        let mut connections = JoinSet::new();
        loop {
            let (mut inbound, _) = listener.accept().await?;
            connections.spawn(async move {
                tokio::time::sleep(delay).await;
                let mut outbound = TcpStream::connect(target).await?;
                let _ = tokio::io::copy_bidirectional(&mut inbound, &mut outbound).await;
                anyhow::Ok(())
            });
        }
    });
    Ok((addr, tasks))
}

/// The info dictionary of `spec`'s metainfo, which holds nothing else.
fn info_bytes(spec: &FixtureSpec) -> anyhow::Result<Vec<u8>> {
    let bytes = spec.metainfo().bytes;
    let info = bytes
        .strip_prefix(b"d4:info")
        .and_then(|rest| rest.strip_suffix(b"e"))
        .context("the metainfo holds more than its info dictionary")?;
    Ok(info.to_vec())
}

/// LIF-12: metadata that does not match the info hash is discarded, the
/// torrent does not fail, and the metadata from a valid peer is used.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mismatched_metadata_is_discarded_for_a_valid_peer() -> anyhow::Result<()> {
    let spec = catalog::multi_boundary();
    let metainfo = spec.metainfo();
    let mut wrong = info_bytes(&spec)?;
    // Same length, different hash: flip a byte of the piece hashes.
    let last = wrong.len() - 2;
    wrong[last] ^= 0xff;
    let fake = FakeMetadataPeer::start(wrong).await?;
    let seeder = Seeder::start(&spec, &metainfo.bytes).await?;
    let (proxy, mut proxy_tasks) = delaying_proxy(seeder.addr, Duration::from_millis(500)).await?;
    let leecher = Leecher::start().await?;

    let handle = bounded(
        "retrieving metadata",
        leecher.session.add_torrent(
            AddTorrent::from_url(spec.magnet_uri(&metainfo.info_hash)),
            Some(leecher.add_options(&[fake.addr, proxy])?),
        ),
    )
    .await?
    .into_handle()
    .context("the torrent was not added")?;
    assert!(
        fake.served.load(Ordering::Relaxed) > 0,
        "the fake peer served its metadata"
    );
    assert_eq!(handle.info_hash().0, metainfo.info_hash);
    assert_eq!(handle.name().as_deref(), Some(spec.torrent_name.as_str()));
    let mut stream = bounded("opening a stream", handle.clone().stream(2)).await?;
    let mut bytes = Vec::new();
    bounded("reading a file", async {
        Ok(stream.read_to_end(&mut bytes).await?)
    })
    .await?;
    assert_eq!(
        generator::first_mismatch(spec.file_seed(2), 0, &bytes),
        None
    );
    assert!(!matches!(handle.stats().state, TorrentStatsState::Error));

    drop(stream);
    leecher.session.stop().await;
    seeder.session.stop().await;
    fake.shutdown().await;
    proxy_tasks.abort_all();
    while proxy_tasks.join_next().await.is_some() {}
    Ok(())
}
