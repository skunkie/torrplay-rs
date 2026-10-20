// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! Access traces: JSON Lines files of timed player events, as
//! `docs/acceptance.md` defines them.
//!
//! A reader's events run in order: each starts at its `t_ms` or once the
//! reader's previous event has ended, whichever is later, so a request that
//! follows another is a seek. A `configure` event, before any other, sets the
//! trace's cache capacity.

use std::collections::BTreeMap;
use std::time::Duration;

use serde::Deserialize;

/// The default cache capacity of a trace, as the benchmark protocol fixes it.
pub const DEFAULT_CACHE_CAPACITY_BYTES: u64 = 256 * 1024 * 1024;

/// How a fixture becomes a torrent.
#[derive(Clone, Copy, Debug, Default, Deserialize, serde::Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    /// From its torrent file.
    #[default]
    Upload,
    /// From a magnet URI.
    Magnet,
}

/// One line of a trace.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct Line {
    t_ms: u64,
    event: EventKind,
    reader: Option<String>,
    fixture: Option<String>,
    file: Option<String>,
    source: Option<Source>,
    range_start: Option<u64>,
    range_end: Option<u64>,
    consume_bytes: Option<u64>,
    consume_rate_bytes_per_second: Option<u64>,
    cache_capacity_bytes: Option<u64>,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum EventKind {
    Configure,
    Request,
    Head,
}

/// A player's request for part of a file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    /// The first byte requested.
    pub range_start: u64,
    /// The last byte requested, inclusive, or `None` for an open-ended
    /// range.
    pub range_end: Option<u64>,
    /// How many body bytes the player reads before closing the response.
    pub consume_bytes: u64,
    /// The playback rate, or `None` to read as fast as bytes arrive.
    pub consume_rate_bytes_per_second: Option<u64>,
}

/// What an event does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    /// A GET of a byte range.
    Request(Request),
    /// A HEAD request.
    Head,
}

/// One event of a reader.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Event {
    /// The earliest start since the trace began.
    pub at: Duration,
    /// The fixture whose file is read.
    pub fixture: String,
    /// The file's name within the fixture.
    pub file: String,
    /// The request.
    pub action: Action,
}

/// A parsed trace.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Trace {
    /// The cache capacity the trace runs with.
    pub cache_capacity_bytes: u64,
    /// How each fixture the trace names becomes a torrent, by fixture name.
    pub fixtures: BTreeMap<String, Source>,
    /// Each reader's events in order, by reader name.
    pub readers: BTreeMap<String, Vec<Event>>,
}

/// Why a trace cannot be used.
#[derive(Debug, thiserror::Error)]
pub enum TraceError {
    /// A line is not a valid event.
    #[error("line {line}: {source}")]
    Json {
        /// The line's number, from 1.
        line: usize,
        /// The parse failure.
        source: serde_json::Error,
    },
    /// A line breaks a rule of the format.
    #[error("line {line}: {message}")]
    Invalid {
        /// The line's number, from 1.
        line: usize,
        /// What is wrong.
        message: &'static str,
    },
}

/// Parses a trace from its JSON Lines text. Blank lines are skipped.
pub fn parse(text: &str) -> Result<Trace, TraceError> {
    let mut trace = Trace {
        cache_capacity_bytes: DEFAULT_CACHE_CAPACITY_BYTES,
        fixtures: BTreeMap::new(),
        readers: BTreeMap::new(),
    };
    let mut previous_t_ms = 0;
    let mut events = 0;
    for (index, text) in text.lines().enumerate() {
        let line = index + 1;
        if text.trim().is_empty() {
            continue;
        }
        let invalid = |message| TraceError::Invalid { line, message };
        let event: Line =
            serde_json::from_str(text).map_err(|source| TraceError::Json { line, source })?;
        if event.t_ms < previous_t_ms {
            return Err(invalid("events must be ordered by t_ms"));
        }
        previous_t_ms = event.t_ms;
        if event.event == EventKind::Configure {
            if events > 0 {
                return Err(invalid("configure must come before every other event"));
            }
            trace.cache_capacity_bytes = event
                .cache_capacity_bytes
                .filter(|&bytes| bytes > 0)
                .ok_or(invalid("configure needs a positive cache_capacity_bytes"))?;
            continue;
        }
        events += 1;
        if event.cache_capacity_bytes.is_some() {
            return Err(invalid("only configure sets cache_capacity_bytes"));
        }
        let (Some(reader), Some(fixture), Some(file)) = (event.reader, event.fixture, event.file)
        else {
            return Err(invalid("requests and heads need reader, fixture, and file"));
        };
        let known = trace.fixtures.get(&fixture).copied();
        match (known, event.source) {
            (None, source) => {
                trace
                    .fixtures
                    .insert(fixture.clone(), source.unwrap_or_default());
            }
            (Some(_), Some(_)) => {
                return Err(invalid(
                    "only the first event naming a fixture has a source",
                ));
            }
            (Some(_), None) => {}
        }
        let action = match event.event {
            EventKind::Head => {
                if event.range_start.is_some() || event.consume_bytes.is_some() {
                    return Err(invalid("a head has no range or consumption"));
                }
                Action::Head
            }
            EventKind::Request | EventKind::Configure => {
                let range_start = event
                    .range_start
                    .ok_or(invalid("a request needs range_start"))?;
                if event.range_end.is_some_and(|end| end < range_start) {
                    return Err(invalid("range_end must not precede range_start"));
                }
                let consume_bytes = event
                    .consume_bytes
                    .filter(|&bytes| bytes > 0)
                    .ok_or(invalid("a request needs a positive consume_bytes"))?;
                if event.consume_rate_bytes_per_second == Some(0) {
                    return Err(invalid("a consumption rate must be positive"));
                }
                Action::Request(Request {
                    range_start,
                    range_end: event.range_end,
                    consume_bytes,
                    consume_rate_bytes_per_second: event.consume_rate_bytes_per_second,
                })
            }
        };
        trace.readers.entry(reader).or_default().push(Event {
            at: Duration::from_millis(event.t_ms),
            fixture,
            file,
            action,
        });
    }
    if events == 0 {
        return Err(TraceError::Invalid {
            line: 0,
            message: "a trace needs at least one request or head",
        });
    }
    Ok(trace)
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXAMPLE: &str = r#"
{"t_ms": 0, "reader": "r1", "fixture": "bench-video", "file": "bench-video.mkv", "event": "request", "source": "magnet", "range_start": 0, "range_end": null, "consume_bytes": 8388608, "consume_rate_bytes_per_second": 1000000}
{"t_ms": 12000, "reader": "r1", "fixture": "bench-video", "file": "bench-video.mkv", "event": "request", "range_start": 1073741824, "range_end": null, "consume_bytes": 4194304, "consume_rate_bytes_per_second": 1000000}
{"t_ms": 20000, "reader": "r1", "fixture": "bench-video", "file": "bench-video.mkv", "event": "head"}
"#;

    #[test]
    fn the_acceptance_example_parses() {
        let trace = parse(EXAMPLE).unwrap();
        assert_eq!(trace.cache_capacity_bytes, DEFAULT_CACHE_CAPACITY_BYTES);
        assert_eq!(trace.fixtures["bench-video"], Source::Magnet);
        let events = &trace.readers["r1"];
        assert_eq!(events.len(), 3);
        assert_eq!(events[1].at, Duration::from_secs(12));
        assert_eq!(
            events[1].action,
            Action::Request(Request {
                range_start: 1_073_741_824,
                range_end: None,
                consume_bytes: 4_194_304,
                consume_rate_bytes_per_second: Some(1_000_000),
            })
        );
        assert_eq!(events[2].action, Action::Head);
    }

    #[test]
    fn configure_sets_the_cache_capacity() {
        let text = format!(
            "{{\"t_ms\": 0, \"event\": \"configure\", \"cache_capacity_bytes\": 67108864}}\n{EXAMPLE}"
        );
        assert_eq!(parse(&text).unwrap().cache_capacity_bytes, 64 * 1024 * 1024);
    }

    #[test]
    fn broken_rules_name_their_line() {
        let request = r#""reader": "r", "fixture": "f", "file": "x", "event": "request""#;
        for (text, line) in [
            (
                format!(
                    "{{\"t_ms\": 5, {request}, \"range_start\": 0, \"consume_bytes\": 1}}\n{{\"t_ms\": 4, {request}, \"range_start\": 0, \"consume_bytes\": 1}}"
                ),
                2,
            ),
            (
                format!(
                    "{{\"t_ms\": 0, {request}, \"range_start\": 0, \"consume_bytes\": 1}}\n{{\"t_ms\": 1, \"event\": \"configure\", \"cache_capacity_bytes\": 1}}"
                ),
                2,
            ),
            (
                format!(
                    "{{\"t_ms\": 0, {request}, \"range_start\": 9, \"range_end\": 8, \"consume_bytes\": 1}}"
                ),
                1,
            ),
            (
                format!("{{\"t_ms\": 0, {request}, \"range_start\": 0, \"consume_bytes\": 0}}"),
                1,
            ),
            (
                format!(
                    "{{\"t_ms\": 0, {request}, \"range_start\": 0, \"consume_bytes\": 1, \"source\": \"upload\"}}\n{{\"t_ms\": 1, {request}, \"range_start\": 0, \"consume_bytes\": 1, \"source\": \"magnet\"}}"
                ),
                2,
            ),
            (
                format!(
                    "{{\"t_ms\": 0, {request}, \"range_start\": 0, \"consume_bytes\": 1, \"speed\": 2}}"
                ),
                1,
            ),
        ] {
            let error = parse(&text).unwrap_err();
            let reported = match error {
                TraceError::Json { line, .. } | TraceError::Invalid { line, .. } => line,
            };
            assert_eq!(reported, line, "{text}: {error}");
        }
        assert!(parse("\n").is_err(), "an empty trace");
    }
}
