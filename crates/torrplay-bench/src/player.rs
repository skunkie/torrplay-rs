// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! A simulated player: it sends one request, reads its body, and measures it
//! as the benchmark protocol defines.
//!
//! Like a real player, it buffers the first [`PLAYABLE_BYTES`] as fast as
//! they arrive and then reads at the trace's playback rate: a later read is
//! due when playback, starting at the first body byte, reaches it. A stall is
//! a wait longer than [`STALL_THRESHOLD`] past the time the next bytes were
//! due, when the player had consumed everything it had received; an unpaced
//! read is due at once.

use std::time::{Duration, Instant};

use reqwest::Client;
use reqwest::header::RANGE;
use serde::Serialize;

use crate::trace::{Action, Event, Request};

/// The first bytes a player needs before it can play.
pub const PLAYABLE_BYTES: u64 = 2 * 1024 * 1024;

/// The shortest wait that counts as a stall.
pub const STALL_THRESHOLD: Duration = Duration::from_millis(500);

/// A stall of a player.
#[derive(Clone, Copy, Debug, Serialize, PartialEq)]
pub struct Stall {
    /// When it began, in milliseconds since the trace started.
    pub start_ms: f64,
    pub duration_ms: f64,
}

/// What one event measured.
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct RequestRecord {
    pub reader: String,
    /// The event's position among the reader's events.
    pub index: usize,
    pub fixture: String,
    pub file: String,
    /// `"request"` or `"head"`.
    pub kind: &'static str,
    pub range_start: Option<u64>,
    pub range_end: Option<u64>,
    /// Whether a request of the same reader preceded it, which makes its
    /// time to first byte a seek latency.
    pub seek: bool,
    /// When it was sent, in milliseconds since the trace started.
    pub sent_ms: f64,
    pub status: Option<u16>,
    pub time_to_first_byte_ms: Option<f64>,
    /// Until the first [`PLAYABLE_BYTES`] arrived.
    pub time_to_first_playable_ms: Option<f64>,
    /// Body bytes read.
    pub body_bytes: u64,
    pub duration_ms: f64,
    /// Body bytes per second, for unpaced requests that read anything.
    pub throughput_bytes_per_second: Option<f64>,
    pub stalls: Vec<Stall>,
    /// Why the request did not read what it should, if it did not.
    pub error: Option<String>,
}

/// How long playback takes to need `bytes` read, with the first
/// [`PLAYABLE_BYTES`] buffered at once.
fn playback(bytes: u64, rate: u64) -> Duration {
    Duration::from_secs_f64(bytes.saturating_sub(PLAYABLE_BYTES) as f64 / rate as f64)
}

fn millis(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}

/// Sends `event` to `url` and measures it. `trace_start` is when the trace
/// began; `seek` says whether the reader made a request before.
pub async fn play(
    client: &Client,
    url: &str,
    reader: &str,
    index: usize,
    event: &Event,
    seek: bool,
    trace_start: Instant,
) -> RequestRecord {
    let mut record = RequestRecord {
        reader: reader.to_owned(),
        index,
        fixture: event.fixture.clone(),
        file: event.file.clone(),
        kind: "head",
        range_start: None,
        range_end: None,
        seek,
        sent_ms: millis(trace_start.elapsed()),
        status: None,
        time_to_first_byte_ms: None,
        time_to_first_playable_ms: None,
        body_bytes: 0,
        duration_ms: 0.0,
        throughput_bytes_per_second: None,
        stalls: Vec::new(),
        error: None,
    };
    let sent = Instant::now();
    match &event.action {
        Action::Head => match client.head(url).send().await {
            Ok(response) => record.status = Some(response.status().as_u16()),
            Err(error) => record.error = Some(error.to_string()),
        },
        Action::Request(request) => {
            record.kind = "request";
            record.range_start = Some(request.range_start);
            record.range_end = request.range_end;
            if let Err(error) = read(client, url, request, sent, trace_start, &mut record).await {
                record.error = Some(format!("{error:#}"));
            }
        }
    }
    record.duration_ms = millis(sent.elapsed());
    record
}

async fn read(
    client: &Client,
    url: &str,
    request: &Request,
    sent: Instant,
    trace_start: Instant,
    record: &mut RequestRecord,
) -> anyhow::Result<()> {
    let range = match request.range_end {
        Some(end) => format!("bytes={}-{end}", request.range_start),
        None => format!("bytes={}-", request.range_start),
    };
    let mut response = client.get(url).header(RANGE, range).send().await?;
    record.status = Some(response.status().as_u16());
    anyhow::ensure!(
        response.status().is_success(),
        "the response was {}",
        response.status()
    );
    let mut first_byte = None;
    while record.body_bytes < request.consume_bytes {
        let waiting = Instant::now();
        // Playback has consumed everything received once it reaches the
        // bytes read so far; an unpaced player needs them at once.
        let due = match (first_byte, request.consume_rate_bytes_per_second) {
            (Some(first), Some(rate)) => first + playback(record.body_bytes, rate),
            _ => waiting,
        };
        tokio::time::sleep_until(due.into()).await;
        let Some(chunk) = response.chunk().await? else {
            anyhow::bail!(
                "the body ended after {} of {} bytes",
                record.body_bytes,
                request.consume_bytes
            );
        };
        let arrived = Instant::now();
        let waited_from = due.max(waiting);
        if first_byte.is_some() && arrived - waited_from > STALL_THRESHOLD {
            record.stalls.push(Stall {
                start_ms: millis(waited_from - trace_start),
                duration_ms: millis(arrived - waited_from),
            });
        }
        let first = *first_byte.get_or_insert(arrived);
        if record.time_to_first_byte_ms.is_none() {
            record.time_to_first_byte_ms = Some(millis(first - sent));
        }
        let before = record.body_bytes;
        record.body_bytes = (before + chunk.len() as u64).min(request.consume_bytes);
        if before < PLAYABLE_BYTES && record.body_bytes >= PLAYABLE_BYTES {
            record.time_to_first_playable_ms = Some(millis(arrived - sent));
        }
    }
    if request.consume_rate_bytes_per_second.is_none()
        && let Some(first) = first_byte
    {
        let elapsed = first.elapsed().as_secs_f64();
        record.throughput_bytes_per_second =
            (elapsed > 0.0).then(|| record.body_bytes as f64 / elapsed);
    }
    // A paced player plays what it received before it closes the response.
    if let (Some(first), Some(rate)) = (first_byte, request.consume_rate_bytes_per_second) {
        let played = first + playback(record.body_bytes, rate);
        tokio::time::sleep_until(played.into()).await;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use axum::Router;
    use axum::body::Body;
    use axum::routing::get;
    use tokio::net::TcpListener;

    use super::*;

    const MIB: usize = 1024 * 1024;

    /// Serves 3 MiB that pauses for `pause` after its first 2.5 MiB.
    async fn server(pause: Duration) -> String {
        let router = Router::new().route(
            "/content",
            get(move || async move {
                let parts = futures_util::stream::unfold(0, move |part| async move {
                    match part {
                        0 => Some((Ok::<_, std::io::Error>(vec![1u8; 5 * MIB / 2]), 1)),
                        1 => {
                            tokio::time::sleep(pause).await;
                            Some((Ok(vec![2u8; MIB / 2]), 2))
                        }
                        _ => None,
                    }
                });
                Body::from_stream(parts)
            }),
        );
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/content", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, router).await });
        url
    }

    fn event(rate: Option<u64>) -> Event {
        Event {
            at: Duration::ZERO,
            fixture: "f".into(),
            file: "x".into(),
            action: Action::Request(Request {
                range_start: 0,
                range_end: None,
                consume_bytes: 3 * MIB as u64,
                consume_rate_bytes_per_second: rate,
            }),
        }
    }

    #[tokio::test]
    async fn a_wait_longer_than_the_threshold_is_a_stall() {
        let url = server(Duration::from_millis(800)).await;
        let start = Instant::now();
        let record = play(&Client::new(), &url, "r", 0, &event(None), false, start).await;
        assert_eq!(record.error, None);
        assert_eq!(record.body_bytes, 3 * MIB as u64);
        assert!(record.time_to_first_playable_ms.is_some());
        assert!(record.throughput_bytes_per_second.is_some());
        let [stall] = record.stalls.as_slice() else {
            panic!("one stall expected: {:?}", record.stalls);
        };
        assert!((700.0..1500.0).contains(&stall.duration_ms), "{stall:?}");
    }

    #[tokio::test]
    async fn a_short_wait_or_a_full_buffer_is_no_stall() {
        let url = server(Duration::from_millis(200)).await;
        let quick = play(
            &Client::new(),
            &url,
            "r",
            0,
            &event(None),
            false,
            Instant::now(),
        )
        .await;
        assert_eq!(quick.stalls, []);
        // The first 2 MiB are buffered at once; playing the next 0.5 MiB at
        // 1 MB/s covers all but about 280 ms of the pause, under the
        // threshold.
        let url = server(Duration::from_millis(800)).await;
        let paced = play(
            &Client::new(),
            &url,
            "r",
            0,
            &Event {
                action: Action::Request(Request {
                    range_start: 0,
                    range_end: None,
                    consume_bytes: 5 * MIB as u64 / 2 + 1,
                    consume_rate_bytes_per_second: Some(1_000_000),
                }),
                ..event(None)
            },
            false,
            Instant::now(),
        )
        .await;
        assert_eq!(paced.error, None);
        assert_eq!(
            paced.stalls,
            [],
            "playback was still busy most of the pause"
        );
        assert_eq!(paced.throughput_bytes_per_second, None);
        assert!(
            paced.duration_ms >= 500.0,
            "playing 0.5 MiB at 1 MB/s takes over 500 ms: {}",
            paced.duration_ms
        );
    }
}
