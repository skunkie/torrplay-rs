// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! One run of a trace against a running target: it creates the trace's
//! torrents, plays every reader's events, samples the server process and
//! its cache every 100 ms, and gathers the benchmark protocol's
//! measurements into a [`RunRecord`].

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::Context;
use reqwest::Client;
use serde::Serialize;
use tokio::task::JoinSet;

use crate::fixtures::FixtureDir;
use crate::player::{self, RequestRecord};
use crate::sampler;
use crate::target::{CacheSample, Target};
use crate::trace::{Action, Source, Trace};

/// How often the server and its cache are sampled.
pub const SAMPLE_INTERVAL: Duration = Duration::from_millis(100);

/// How often a reader checks for metadata it waits for.
const METADATA_POLL: Duration = Duration::from_millis(50);

/// How long a reader waits for metadata before its event fails.
const METADATA_WAIT: Duration = Duration::from_secs(600);

/// A sample of the server and its cache.
#[derive(Clone, Copy, Debug, Serialize, PartialEq)]
pub struct Sample {
    /// Milliseconds since the trace started.
    pub t_ms: f64,
    pub rss_bytes: u64,
    pub user_seconds: f64,
    pub system_seconds: f64,
    pub cache: Option<CacheSample>,
}

/// One torrent of a run.
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct TorrentRecord {
    pub fixture: String,
    pub source: Source,
    /// From the start of the trace until the creation request was sent.
    pub requested_ms: f64,
    /// From the start of the trace until the creation request returned.
    pub created_ms: f64,
    /// From the start of the trace until metadata was available.
    pub metadata_ms: Option<f64>,
    /// Bytes received from peers.
    pub downloaded_bytes: Option<u64>,
    /// Distinct bytes the readers consumed.
    pub unique_consumed_bytes: u64,
    /// Downloaded bytes beyond the distinct bytes consumed.
    pub excess_bytes: Option<u64>,
}

/// The results of one run.
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct RunRecord {
    pub cache_capacity_bytes: u64,
    pub torrents: Vec<TorrentRecord>,
    pub requests: Vec<RequestRecord>,
    pub samples: Vec<Sample>,
    pub summary: Summary,
}

/// The run's process and cache measurements.
#[derive(Clone, Copy, Debug, Default, Serialize, PartialEq)]
pub struct Summary {
    pub user_seconds: f64,
    pub system_seconds: f64,
    pub peak_rss_bytes: u64,
    /// The median resident set size over the second half of the run.
    pub steady_rss_bytes: u64,
    pub peak_cache_usage_bytes: u64,
    /// Evictions over the run, when the target reports them.
    pub eviction_count: Option<u64>,
}

/// Runs `trace` against `target`, whose server process is `pid`.
pub async fn run<T: Target + 'static>(
    target: Arc<T>,
    client: Client,
    pid: u32,
    trace: &Trace,
    fixtures: &FixtureDir,
) -> anyhow::Result<RunRecord> {
    let ticks = sampler::ticks_per_second()?;
    let start = Instant::now();
    let stop = Arc::new(AtomicBool::new(false));
    let sampling = tokio::spawn(sample(target.clone(), pid, ticks, start, stop.clone()));

    let mut torrents = BTreeMap::new();
    let mut records = BTreeMap::new();
    for (fixture, &source) in &trace.fixtures {
        let requested_ms = start.elapsed().as_secs_f64() * 1000.0;
        let torrent = target
            .create(
                source,
                &fixtures.read_torrent(fixture)?,
                &fixtures.read_magnet(fixture)?,
            )
            .await
            .with_context(|| format!("creating {fixture}"))?;
        records.insert(
            fixture.clone(),
            TorrentRecord {
                fixture: fixture.clone(),
                source,
                requested_ms,
                created_ms: start.elapsed().as_secs_f64() * 1000.0,
                metadata_ms: None,
                downloaded_bytes: None,
                unique_consumed_bytes: 0,
                excess_bytes: None,
            },
        );
        torrents.insert(fixture.clone(), torrent);
    }
    let torrents = Arc::new(torrents);

    let mut metadata = JoinSet::new();
    for (fixture, torrent) in torrents.iter() {
        let (target, fixture, torrent) = (target.clone(), fixture.clone(), torrent.clone());
        metadata.spawn(async move {
            let ready = wait_for_metadata(&*target, &torrent).await;
            (
                fixture,
                ready.map(|()| start.elapsed().as_secs_f64() * 1000.0),
            )
        });
    }

    let mut readers = JoinSet::new();
    for (reader, events) in &trace.readers {
        let (target, client, torrents) = (target.clone(), client.clone(), torrents.clone());
        let (reader, events) = (reader.clone(), events.clone());
        readers.spawn(async move {
            let mut played = Vec::with_capacity(events.len());
            let mut requested = false;
            for (index, event) in events.iter().enumerate() {
                tokio::time::sleep_until((start + event.at).into()).await;
                let torrent = &torrents[&event.fixture];
                let url = match wait_for_metadata(&*target, torrent).await {
                    Ok(()) => target.content_url(torrent, &event.file).await,
                    Err(error) => Err(error),
                };
                let url = url.with_context(|| format!("{reader}: event {index}"))?;
                played.push(
                    player::play(&client, &url, &reader, index, event, requested, start).await,
                );
                requested |= matches!(event.action, Action::Request(_));
            }
            anyhow::Ok(played)
        });
    }
    let mut requests = Vec::new();
    while let Some(played) = readers.join_next().await {
        requests.extend(played??);
    }
    while let Some(ready) = metadata.join_next().await {
        let (fixture, ready) = ready?;
        if let Some(record) = records.get_mut(&fixture) {
            record.metadata_ms = ready.ok();
        }
    }
    stop.store(true, Ordering::Release);
    let samples = sampling.await??;
    requests.sort_by(|a, b| (&a.reader, a.index).cmp(&(&b.reader, b.index)));

    for (fixture, record) in &mut records {
        record.unique_consumed_bytes = unique_consumed(&requests, fixture);
        record.downloaded_bytes = target.downloaded_bytes(&torrents[fixture]).await.ok();
        record.excess_bytes = record
            .downloaded_bytes
            .map(|downloaded| downloaded.saturating_sub(record.unique_consumed_bytes));
    }
    Ok(RunRecord {
        cache_capacity_bytes: trace.cache_capacity_bytes,
        torrents: records.into_values().collect(),
        requests,
        summary: summarize(&samples),
        samples,
    })
}

async fn wait_for_metadata<T: Target>(target: &T, torrent: &T::Torrent) -> anyhow::Result<()> {
    tokio::time::timeout(METADATA_WAIT, async {
        while !target.has_metadata(torrent).await? {
            tokio::time::sleep(METADATA_POLL).await;
        }
        anyhow::Ok(())
    })
    .await
    .context("metadata did not arrive")?
}

async fn sample<T: Target>(
    target: Arc<T>,
    pid: u32,
    ticks: f64,
    start: Instant,
    stop: Arc<AtomicBool>,
) -> anyhow::Result<Vec<Sample>> {
    let mut samples = Vec::new();
    let mut interval = tokio::time::interval(SAMPLE_INTERVAL);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    while !stop.load(Ordering::Acquire) {
        interval.tick().await;
        let process = sampler::read(pid, ticks)?;
        samples.push(Sample {
            t_ms: start.elapsed().as_secs_f64() * 1000.0,
            rss_bytes: process.rss_bytes,
            user_seconds: process.user_seconds,
            system_seconds: process.system_seconds,
            cache: target.cache().await.ok(),
        });
    }
    Ok(samples)
}

/// The distinct bytes of `fixture` the requests consumed, over all its
/// files.
pub fn unique_consumed(requests: &[RequestRecord], fixture: &str) -> u64 {
    let mut by_file: BTreeMap<&str, Vec<(u64, u64)>> = BTreeMap::new();
    for request in requests.iter().filter(|request| request.fixture == fixture) {
        if let Some(start) = request.range_start {
            by_file
                .entry(&request.file)
                .or_default()
                .push((start, start + request.body_bytes));
        }
    }
    by_file
        .into_values()
        .map(|mut ranges| {
            ranges.sort_unstable();
            let mut total = 0;
            let mut covered_to = 0;
            for (start, end) in ranges {
                let start = start.max(covered_to);
                if end > start {
                    total += end - start;
                    covered_to = end;
                }
            }
            total
        })
        .sum()
}

/// Peak and steady-state figures over `samples`.
pub fn summarize(samples: &[Sample]) -> Summary {
    let Some(last) = samples.last() else {
        return Summary::default();
    };
    let mut steady: Vec<u64> = samples[samples.len() / 2..]
        .iter()
        .map(|sample| sample.rss_bytes)
        .collect();
    steady.sort_unstable();
    Summary {
        user_seconds: last.user_seconds,
        system_seconds: last.system_seconds,
        peak_rss_bytes: samples.iter().map(|s| s.rss_bytes).max().unwrap_or(0),
        steady_rss_bytes: steady[steady.len() / 2],
        peak_cache_usage_bytes: samples
            .iter()
            .filter_map(|s| s.cache.map(|cache| cache.usage_bytes))
            .max()
            .unwrap_or(0),
        eviction_count: samples
            .iter()
            .filter_map(|s| s.cache.and_then(|cache| cache.eviction_count))
            .max(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(file: &str, start: u64, bytes: u64) -> RequestRecord {
        RequestRecord {
            reader: "r".into(),
            index: 0,
            fixture: "f".into(),
            file: file.into(),
            kind: "request",
            range_start: Some(start),
            range_end: None,
            seek: false,
            sent_ms: 0.0,
            status: Some(206),
            time_to_first_byte_ms: None,
            time_to_first_playable_ms: None,
            body_bytes: bytes,
            duration_ms: 0.0,
            throughput_bytes_per_second: None,
            stalls: Vec::new(),
            error: None,
        }
    }

    #[test]
    fn overlapping_reads_count_once_per_file() {
        let requests = [
            request("a", 0, 100),
            request("a", 50, 100),
            request("a", 300, 10),
            request("b", 0, 100),
        ];
        assert_eq!(unique_consumed(&requests, "f"), 150 + 10 + 100);
        assert_eq!(unique_consumed(&requests, "other"), 0);
    }

    #[test]
    fn steady_rss_is_the_median_of_the_second_half() {
        let samples: Vec<Sample> = [10, 90, 30, 20, 40, 50]
            .into_iter()
            .enumerate()
            .map(|(n, rss)| Sample {
                t_ms: n as f64,
                rss_bytes: rss,
                user_seconds: n as f64,
                system_seconds: 0.5,
                cache: Some(CacheSample {
                    usage_bytes: rss * 2,
                    eviction_count: Some(n as u64),
                }),
            })
            .collect();
        let summary = summarize(&samples);
        assert_eq!(summary.peak_rss_bytes, 90);
        assert_eq!(summary.steady_rss_bytes, 40, "the median of 20, 40, and 50");
        assert_eq!(summary.user_seconds, 5.0);
        assert_eq!(summary.peak_cache_usage_bytes, 180);
        assert_eq!(summary.eviction_count, Some(5));
        assert_eq!(summarize(&[]), Summary::default());
    }
}
