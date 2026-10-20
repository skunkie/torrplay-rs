// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! Milestone 4's scenarios end to end: the application over HTTP/1.1 and
//! HTTP/2, the librqbit engine in memory storage, and local seeders on
//! loopback sessions that reach no public swarm. The last test checks the
//! milestone's exit criterion.

// Helpers assert as tests do; clippy's test exemption covers only test
// functions.
#![allow(clippy::expect_used)]

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use common::{
    KIB, MIB, Running, STEP, alive_tasks, bounded, eventually, expected, json, magnet, read_body,
    reset_reason, set_desired_state, upload,
};
use reqwest::header::{CONTENT_LENGTH, DATE, HeaderMap, RANGE};
use reqwest::{Client, StatusCode};
use serde_json::Value;
use torrplay::ShutdownTimings;
use torrplay_engine::EngineConfig;
use torrplay_engine::test_support::Seeder;
use torrplay_fixtures::{FixtureSpec, catalog};

/// The cache capacity the acceptance scenarios use.
const CAPACITY: u64 = 8 * MIB;

fn eight_mib(config: &mut EngineConfig) {
    *config = config.clone().with_cache_capacity(CAPACITY);
}

async fn seeded(spec: &FixtureSpec) -> anyhow::Result<Seeder> {
    Seeder::start(spec, &spec.metainfo().bytes).await
}

async fn statistics(running: &Running, client: &Client, path: &str) -> anyhow::Result<Value> {
    json(client.get(running.url(path)).send().await?).await
}

fn without_date(headers: &HeaderMap) -> HeaderMap {
    let mut headers = headers.clone();
    headers.remove(DATE);
    headers
}

/// STR-05: HEAD answers as GET would, without a body, a reader, or a piece.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn head_answers_like_get_without_reading() -> anyhow::Result<()> {
    let spec = catalog::single_small();
    let seeder = seeded(&spec).await?;
    let running = Running::start(vec![seeder.addr], ShutdownTimings::default(), |_| {}).await?;
    let client = Client::new();
    let created = upload(&running, &client, &spec).await?;
    let content = running.url(&created.content[0]);

    let mut heads = Vec::new();
    for range in [None, Some("bytes=100-199")] {
        let mut head = client.head(&content);
        if let Some(range) = range {
            head = head.header(RANGE, range);
        }
        let head = head.send().await?;
        heads.push((head.status(), without_date(head.headers())));
        assert!(head.bytes().await?.is_empty(), "HEAD has no body");
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(running.playback().opened_readers, 0, "HEAD opens no reader");
    let cache = statistics(&running, &client, "/api/v1/cache/statistics").await?;
    assert_eq!(cache["usage_bytes"], 0, "HEAD requests no piece");
    let torrent = statistics(&running, &client, &format!("{}/statistics", created.path)).await?;
    assert_eq!(torrent["downloaded_bytes"], 0);

    for ((status, headers), range) in heads.into_iter().zip([None, Some("bytes=100-199")]) {
        let mut get = client.get(&content);
        if let Some(range) = range {
            get = get.header(RANGE, range);
        }
        let get = get.send().await?;
        assert_eq!(get.status(), status, "{range:?}");
        assert_eq!(without_date(get.headers()), headers, "{range:?}");
        let length = headers[CONTENT_LENGTH].to_str()?.parse::<usize>()?;
        assert_eq!(get.bytes().await?.len(), length);
    }

    running.stop().await?;
    seeder.stop().await;
    Ok(())
}

/// STR-07: a client that disconnects mid-stream leaves an idle reader, and
/// over HTTP/2 the other stream on the connection completes.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn disconnected_streams_leave_idle_readers() -> anyhow::Result<()> {
    let spec = catalog::large(CAPACITY);
    let seeder = seeded(&spec).await?;
    for http2 in [false, true] {
        let running =
            Running::start(vec![seeder.addr], ShutdownTimings::default(), eight_mib).await?;
        let client = if http2 {
            Client::builder().http2_prior_knowledge().build()?
        } else {
            Client::new()
        };
        let created = upload(&running, &client, &spec).await?;
        let content = running.url(&created.content[0]);
        let mut left = client.get(&content).send().await?;
        let other = client
            .get(&content)
            .header(RANGE, format!("bytes={}-{}", 20 * MIB, 21 * MIB - 1))
            .send()
            .await?;
        assert!(left.chunk().await?.is_some_and(|chunk| !chunk.is_empty()));
        drop(left);

        let (bytes, error) = read_body(other).await;
        assert!(error.is_none(), "http2 {http2}: {error:?}");
        assert_eq!(
            bytes,
            expected(&spec, 0, 20 * MIB, 21 * MIB),
            "http2 {http2}"
        );
        eventually("the readers turning idle", async || {
            let playback = running.playback();
            Ok((playback.readers == 2 && playback.idle_readers == 2).then_some(()))
        })
        .await?;
        running.stop().await?;
    }
    seeder.stop().await;
    Ok(())
}

/// STR-08: deleting a torrent resets its HTTP/2 streams with `CANCEL` and
/// leaves another torrent's stream on the same connection alone.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn deleting_resets_only_that_torrents_http2_streams() -> anyhow::Result<()> {
    let kept = catalog::single_small();
    let seeder = seeded(&kept).await?;
    let running = Running::start(vec![seeder.addr], ShutdownTimings::default(), |config| {
        config.piece_wait = Duration::from_secs(600)
    })
    .await?;
    let client = Client::builder().http2_prior_knowledge().build()?;
    // No peer has the deleted torrent, so its streams wait.
    let deleted = upload(&running, &client, &catalog::multi_boundary()).await?;
    let other = upload(&running, &client, &kept).await?;
    let first = client.get(running.url(&deleted.content[0])).send().await?;
    let second = client.get(running.url(&deleted.content[2])).send().await?;
    let continuing = client.get(running.url(&other.content[0])).send().await?;

    let removed = client.delete(running.url(&deleted.path)).send().await?;
    assert_eq!(removed.status(), StatusCode::NO_CONTENT);
    for stream in [first, second] {
        let (_, error) = bounded("the reset stream", read_body(stream)).await?;
        let error = error.expect("deleting resets the stream");
        assert_eq!(reset_reason(&error), Some(h2::Reason::CANCEL), "{error:?}");
    }
    let (bytes, error) = read_body(continuing).await;
    assert!(error.is_none(), "{error:?}");
    let size = kept.total_size_bytes();
    assert_eq!(bytes, expected(&kept, 0, 0, size));

    running.stop().await?;
    seeder.stop().await;
    Ok(())
}

/// STR-09: when the only seeder stops, the stream ends once the piece wait
/// bound expires, and every byte sent before is correct.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_stopped_seeder_ends_the_stream_after_the_piece_wait() -> anyhow::Result<()> {
    let spec = catalog::large(CAPACITY);
    let seeder = seeded(&spec).await?;
    let piece_wait = Duration::from_secs(2);
    let running = Running::start(vec![seeder.addr], ShutdownTimings::default(), |config| {
        eight_mib(config);
        config.piece_wait = piece_wait;
    })
    .await?;
    let client = Client::new();
    let created = upload(&running, &client, &spec).await?;
    let mut stream = client.get(running.url(&created.content[0])).send().await?;
    let mut bytes = Vec::new();
    while (bytes.len() as u64) < 2 * MIB {
        bytes.extend_from_slice(&stream.chunk().await?.expect("more content"));
    }
    let stopped = Instant::now();
    seeder.stop().await;
    let (rest, error) = bounded("the stalled stream", read_body(stream)).await?;
    assert!(error.is_some(), "the stream ends with an error");
    // The bound counts from when the read began waiting, which may be
    // before the seeder stopped.
    assert!(
        stopped.elapsed() <= piece_wait + Duration::from_secs(5),
        "the stream ended within the piece wait bound: {:?}",
        stopped.elapsed()
    );
    bytes.extend_from_slice(&rest);
    let received = bytes.len() as u64;
    assert!(received < spec.total_size_bytes());
    assert_eq!(
        bytes,
        expected(&spec, 0, 0, received),
        "no corrupt byte was sent"
    );

    running.stop().await
}

/// STR-10: a client reading at 64 KiB/s keeps streaming with backpressure:
/// the engine downloads only its readahead ahead of what the client read,
/// and no deadline ends the stream.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_slow_client_streams_with_backpressure() -> anyhow::Result<()> {
    let spec = catalog::large(CAPACITY);
    let seeder = seeded(&spec).await?;
    let running = Running::start(vec![seeder.addr], ShutdownTimings::default(), eight_mib).await?;
    let client = Client::new();
    let created = upload(&running, &client, &spec).await?;
    let mut stream = client.get(running.url(&created.content[0])).send().await?;

    let rate = 64 * KIB;
    let downloaded = async || -> anyhow::Result<u64> {
        let torrent =
            statistics(&running, &client, &format!("{}/statistics", created.path)).await?;
        Ok(torrent["downloaded_bytes"].as_u64().expect("a count"))
    };
    let started = Instant::now();
    let mut bytes = Vec::new();
    let mut early = None;
    while (bytes.len() as u64) < 384 * KIB {
        bytes.extend_from_slice(&stream.chunk().await?.expect("more content"));
        let due = Duration::from_secs_f64(bytes.len() as f64 / rate as f64);
        tokio::time::sleep(due.saturating_sub(started.elapsed())).await;
        // Once socket buffers have filled, downloads follow the client.
        if early.is_none() && bytes.len() as u64 >= 128 * KIB {
            early = Some((bytes.len() as u64, downloaded().await?));
        }
    }
    assert!(
        started.elapsed() >= Duration::from_secs(5),
        "no deadline ended it"
    );
    let received = bytes.len() as u64;
    assert_eq!(bytes, expected(&spec, 0, 0, received));
    let (early_received, early_downloaded) = early.expect("an early sample");
    let late_downloaded = downloaded().await?;
    let piece = u64::from(spec.piece_length);
    assert!(
        late_downloaded - early_downloaded <= received - early_received + 2 * piece,
        "downloads grew by {} bytes while the client read {}",
        late_downloaded - early_downloaded,
        received - early_received
    );
    assert!(late_downloaded < spec.total_size_bytes() / 2);
    assert!(stream.chunk().await?.is_some(), "the stream is still going");
    drop(stream);

    running.stop().await?;
    seeder.stop().await;
    Ok(())
}

/// CAC-01 with API-18: `large` streams byte for byte through an 8 MiB
/// cache; statistics sampled meanwhile only grow, stay within range, never
/// count an upload, and never show usage above capacity; evicted pieces
/// are downloaded again when read again.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn large_streams_through_a_small_cache_with_sound_statistics() -> anyhow::Result<()> {
    let spec = catalog::large(CAPACITY);
    let size = spec.total_size_bytes();
    let seeder = seeded(&spec).await?;
    let running =
        Arc::new(Running::start(vec![seeder.addr], ShutdownTimings::default(), eight_mib).await?);
    let client = Client::new();
    let created = upload(&running, &client, &spec).await?;

    let done = Arc::new(AtomicBool::new(false));
    let sampler = tokio::spawn({
        let (running, client, done) = (running.clone(), client.clone(), done.clone());
        let path = format!("{}/statistics", created.path);
        async move {
            let mut samples = 0;
            let mut downloaded = 0;
            while !done.load(Ordering::Acquire) {
                let torrent = statistics(&running, &client, &path).await?;
                let cache = statistics(&running, &client, "/api/v1/cache/statistics").await?;
                let now = torrent["downloaded_bytes"].as_u64().expect("a count");
                assert!(now >= downloaded, "downloaded bytes only grow");
                downloaded = now;
                assert_eq!(torrent["uploaded_bytes"], 0);
                assert_eq!(torrent["upload_rate_bytes_per_second"], 0);
                if let Some(ratio) = torrent["completion_ratio"].as_f64() {
                    assert!((0.0..=1.0).contains(&ratio), "{ratio}");
                }
                let usage = cache["usage_bytes"].as_u64().expect("a count");
                assert!(usage <= CAPACITY, "usage {usage} above capacity");
                samples += 1;
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            anyhow::Ok(samples)
        }
    });

    let whole = client.get(running.url(&created.content[0])).send().await?;
    let (bytes, error) = bounded("streaming large", read_body(whole)).await?;
    assert!(error.is_none(), "{error:?}");
    assert!(bytes == expected(&spec, 0, 0, size), "byte for byte");
    let again = client
        .get(running.url(&created.content[0]))
        .header(RANGE, format!("bytes=0-{}", MIB - 1))
        .send()
        .await?;
    assert_eq!(again.bytes().await?, expected(&spec, 0, 0, MIB));
    done.store(true, Ordering::Release);
    assert!(
        sampler.await?? > 0,
        "statistics were sampled while streaming"
    );

    let cache = statistics(&running, &client, "/api/v1/cache/statistics").await?;
    assert!(cache["eviction_count"].as_u64() > Some(0));
    let torrent = statistics(&running, &client, &format!("{}/statistics", created.path)).await?;
    assert!(
        torrent["downloaded_bytes"].as_u64() > Some(size),
        "the first piece was downloaded again after its eviction"
    );
    running.engine.cache().expect("a started cache").audit()?;

    Arc::into_inner(running)
        .expect("the sampler ended")
        .stop()
        .await?;
    seeder.stop().await;
    Ok(())
}

/// LIF-04: pausing cancels a stream in progress and releases the idle
/// reader; content answers `409` while metadata, files, and statistics stay
/// readable.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pausing_cancels_streams_and_keeps_reads() -> anyhow::Result<()> {
    let spec = catalog::large(CAPACITY);
    let seeder = seeded(&spec).await?;
    let running = Running::start(vec![seeder.addr], ShutdownTimings::default(), eight_mib).await?;
    let client = Client::new();
    let created = upload(&running, &client, &spec).await?;
    let content = running.url(&created.content[0]);
    // The stream opens first, so the short range cannot continue its
    // reader and becomes idle beside it.
    let mut stream = client
        .get(&content)
        .header(RANGE, format!("bytes={}-", 16 * MIB))
        .send()
        .await?;
    assert!(stream.chunk().await?.is_some());
    let first = client
        .get(&content)
        .header(RANGE, "bytes=0-1023")
        .send()
        .await?;
    assert_eq!(first.bytes().await?, expected(&spec, 0, 0, 1024));
    let playback = running.playback();
    assert_eq!((playback.readers, playback.idle_readers), (2, 1));

    set_desired_state(&running, &client, &created.path, "paused").await?;
    assert_eq!(running.playback().readers, 0, "every reader is released");
    let (rest, _) = bounded("the canceled stream", read_body(stream)).await?;
    assert!(
        (rest.len() as u64) < 16 * MIB,
        "the stream did not complete"
    );
    let refused = client.get(&content).send().await?;
    assert_eq!(refused.status(), StatusCode::CONFLICT);
    for path in [
        created.path.clone(),
        format!("{}/files", created.path),
        format!("{}/statistics", created.path),
    ] {
        let read = client.get(running.url(&path)).send().await?;
        assert_eq!(read.status(), StatusCode::OK, "{path}");
    }

    running.stop().await?;
    seeder.stop().await;
    Ok(())
}

/// LIF-06: a storage write failure fails the torrent and its stream;
/// content answers `409` until pausing and starting clear the failure.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_storage_failure_fails_the_torrent_until_restarted() -> anyhow::Result<()> {
    let spec = catalog::single_small();
    let size = spec.total_size_bytes();
    let seeder = seeded(&spec).await?;
    let running = Running::start(vec![seeder.addr], ShutdownTimings::default(), |config| {
        config.piece_wait = Duration::from_secs(600);
    })
    .await?;
    let client = Client::new();
    let created = upload(&running, &client, &spec).await?;
    running.engine.fail_storage_writes(&created.id)?;
    let content = running.url(&created.content[0]);

    let stream = client.get(&content).send().await?;
    let (bytes, error) = bounded("the failed stream", read_body(stream)).await?;
    assert!(error.is_some() && bytes.is_empty(), "{error:?}");
    let torrent = statistics(&running, &client, &created.path).await?;
    assert_eq!(torrent["observed_state"], "failed");
    assert!(torrent["failure"]["detail"].is_string(), "{torrent}");
    assert_eq!(
        client.get(&content).send().await?.status(),
        StatusCode::CONFLICT
    );

    set_desired_state(&running, &client, &created.path, "paused").await?;
    set_desired_state(&running, &client, &created.path, "started").await?;
    let torrent = statistics(&running, &client, &created.path).await?;
    assert_eq!(torrent["failure"], Value::Null, "{torrent}");
    let (bytes, error) = read_body(client.get(&content).send().await?).await;
    assert!(error.is_none(), "{error:?}");
    assert_eq!(bytes, expected(&spec, 0, 0, size));

    running.stop().await?;
    seeder.stop().await;
    Ok(())
}

/// LIF-07: deleting during metadata retrieval and during a stream answers
/// `204`, cancels the stream, frees cache usage, and leaves `404` for the
/// torrent, every subresource, and a second delete.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn deleting_cancels_and_leaves_nothing_addressable() -> anyhow::Result<()> {
    let spec = catalog::large(CAPACITY);
    let seeder = seeded(&spec).await?;
    let running = Running::start(vec![seeder.addr], ShutdownTimings::default(), eight_mib).await?;
    let client = Client::new();

    // No peer has `multi-boundary`, so its metadata never arrives.
    let retrieving = magnet(&running, &client, &catalog::multi_boundary()).await?;
    let torrent = statistics(&running, &client, &retrieving).await?;
    assert_eq!(torrent["observed_state"], "retrieving_metadata");
    let streaming = upload(&running, &client, &spec).await?;
    let mut stream = client
        .get(running.url(&streaming.content[0]))
        .send()
        .await?;
    let mut read = 0;
    while read < 4 * MIB {
        read += stream.chunk().await?.expect("more content").len() as u64;
    }
    let cache = statistics(&running, &client, "/api/v1/cache/statistics").await?;
    let usage = cache["usage_bytes"].as_u64().expect("a count");

    for path in [&retrieving, &streaming.path] {
        let deleted = client.delete(running.url(path)).send().await?;
        assert_eq!(deleted.status(), StatusCode::NO_CONTENT, "{path}");
    }
    let (rest, error) = bounded("the canceled stream", read_body(stream)).await?;
    assert!(error.is_some() || read + (rest.len() as u64) < spec.total_size_bytes());
    for path in [&retrieving, &streaming.path] {
        assert_eq!(
            client.delete(running.url(path)).send().await?.status(),
            StatusCode::NOT_FOUND
        );
        for resource in ["", "/configuration", "/files", "/statistics"] {
            let gone = client
                .get(running.url(&format!("{path}{resource}")))
                .send()
                .await?;
            assert_eq!(gone.status(), StatusCode::NOT_FOUND, "{path}{resource}");
        }
    }
    eventually("cache usage falling", async || {
        let cache = statistics(&running, &client, "/api/v1/cache/statistics").await?;
        Ok((cache["usage_bytes"].as_u64() < Some(usage)).then_some(()))
    })
    .await?;

    running.stop().await?;
    seeder.stop().await;
    Ok(())
}

/// RDR-01 and RDR-02 over HTTP: sequential range requests continue one
/// reader, which is released with its pins after the idle timeout.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn range_requests_continue_one_reader_until_it_expires() -> anyhow::Result<()> {
    let spec = catalog::single_small();
    let seeder = seeded(&spec).await?;
    let running = Running::start(vec![seeder.addr], ShutdownTimings::default(), |config| {
        config.idle_timeout = Duration::from_millis(500);
    })
    .await?;
    let client = Client::new();
    let created = upload(&running, &client, &spec).await?;
    let content = running.url(&created.content[0]);
    for (start, end) in [(0, 99_999), (100_000, 199_999), (200_000, 299_999)] {
        let range = client
            .get(&content)
            .header(RANGE, format!("bytes={start}-{end}"))
            .send()
            .await?;
        assert_eq!(range.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(range.bytes().await?, expected(&spec, 0, start, end + 1));
    }
    assert_eq!(
        running.playback().opened_readers,
        1,
        "one reader served all"
    );
    eventually("the idle reader expiring", async || {
        let cache = statistics(&running, &client, "/api/v1/cache/statistics").await?;
        Ok((running.playback().readers == 0 && cache["pinned_bytes"] == 0).then_some(()))
    })
    .await?;

    running.stop().await?;
    seeder.stop().await;
    Ok(())
}

/// CAC-09: opening and removing 100 torrents with streams leaves cache
/// usage, readers, peer connections, and tasks at their starting levels.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_hundred_torrents_opened_and_removed_leave_nothing_behind() -> anyhow::Result<()> {
    let spec = catalog::single_small();
    let seeder = seeded(&spec).await?;
    let running = Running::start(vec![seeder.addr], ShutdownTimings::default(), |_| {}).await?;

    let cycle = async |client: &Client| -> anyhow::Result<()> {
        let created = upload(&running, client, &spec).await?;
        let range = client
            .get(running.url(&created.content[0]))
            .header(RANGE, "bytes=0-65535")
            .send()
            .await?;
        assert_eq!(range.bytes().await?, expected(&spec, 0, 0, 65_536));
        let deleted = client.delete(running.url(&created.path)).send().await?;
        assert_eq!(deleted.status(), StatusCode::NO_CONTENT);
        Ok(())
    };
    let settled = async || -> anyhow::Result<(usize, u64)> {
        eventually("the seeder losing its peer", async || {
            let peers = seeder
                .handle
                .stats()
                .live
                .map_or(0, |live| live.snapshot.peer_stats.live);
            Ok((peers == 0 && running.playback().readers == 0).then_some(()))
        })
        .await?;
        let cache = running.engine.cache().expect("a started cache").audit()?;
        Ok((alive_tasks(), cache.usage_bytes - cache.retained_bytes))
    };
    // A first cycle starts what lives as long as the application. Each
    // phase drops its client, whose pooled connections hold tasks on both
    // ends.
    cycle(&Client::new()).await?;
    let (tasks, usage) = settled().await?;
    assert_eq!(usage, 0);
    let started = Instant::now();
    let client = Client::new();
    for _ in 0..100 {
        cycle(&client).await?;
    }
    drop(client);
    let (_, usage) = settled().await?;
    assert_eq!(usage, 0, "cache usage returns to zero");
    eventually("the tasks ending", async || {
        Ok((alive_tasks() <= tasks).then_some(()))
    })
    .await
    .map_err(|error| {
        anyhow::anyhow!(
            "{error}: {} tasks alive, {tasks} at the start",
            alive_tasks()
        )
    })?;
    assert!(started.elapsed() < 10 * STEP);

    running.stop().await?;
    seeder.stop().await;
    Ok(())
}

/// Milestone 4's exit criterion: concurrent streams that seek stay byte
/// for byte correct under sustained eviction, cache usage never exceeds
/// capacity, and deleting the torrent releases its readers, cache usage,
/// and tasks.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn exit_criterion_streams_stay_correct_and_release_everything() -> anyhow::Result<()> {
    let spec = Arc::new(catalog::large(CAPACITY));
    let seeder = seeded(&spec).await?;
    let running = Running::start(vec![seeder.addr], ShutdownTimings::default(), eight_mib).await?;
    let tasks = alive_tasks();
    let client = Client::new();
    let created = upload(&running, &client, &spec).await?;
    let content = running.url(&created.content[0]);

    // Three players, each seeking through the file in its own order, read
    // together 3 MiB more than four times the capacity.
    let mut players = Vec::new();
    for player in 0..3u64 {
        let (client, content, spec) = (client.clone(), content.clone(), spec.clone());
        players.push(tokio::spawn(async move {
            for step in 0..5u64 {
                let start = ((player * 11 + step * 7) % 28) * MIB + player * 1000;
                let end = start + 2 * MIB + 12_345;
                let range = client
                    .get(&content)
                    .header(RANGE, format!("bytes={start}-{}", end - 1))
                    .send()
                    .await?;
                let (bytes, error) = read_body(range).await;
                assert!(error.is_none(), "player {player}, step {step}: {error:?}");
                assert!(
                    bytes == expected(&spec, 0, start, end),
                    "player {player}, step {step}"
                );
            }
            anyhow::Ok(())
        }));
    }
    for player in players {
        bounded("a player", player).await???;
    }
    let cache = running.engine.cache().expect("a started cache");
    // The audit fails if usage ever exceeded capacity.
    let stats = cache.audit()?;
    assert!(stats.eviction_count > 0, "the streams evicted pieces");

    let deleted = client.delete(running.url(&created.path)).send().await?;
    assert_eq!(deleted.status(), StatusCode::NO_CONTENT);
    // Closing the client's pooled connections ends their tasks, which the
    // count before the torrent did not include.
    drop(client);
    eventually("everything released", async || {
        let stats = cache.audit()?;
        let playback = running.playback();
        Ok((stats.usage_bytes == stats.retained_bytes && playback.readers == 0).then_some(()))
    })
    .await?;
    eventually("the torrent's tasks ending", async || {
        Ok((alive_tasks() <= tasks).then_some(()))
    })
    .await
    .map_err(|error| {
        anyhow::anyhow!(
            "{error}: {} tasks alive, {tasks} before the torrent",
            alive_tasks()
        )
    })?;

    running.stop().await?;
    seeder.stop().await;
    Ok(())
}
