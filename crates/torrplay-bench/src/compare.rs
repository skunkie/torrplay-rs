// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! A candidate implementation's runs checked against a baseline's by the
//! thresholds in `docs/acceptance.md`.
//!
//! Runs of the same network profile, from their `profile` label, and trace
//! form a group. Each group's measurements are compared one by one:
//!
//! - **Latencies** (time to metadata, first byte, and first playable data,
//!   and seek latency): the median within 110 percent of the baseline's plus
//!   50 ms and, when the baseline pools 20 or more values, the 90th
//!   percentile within 125 percent of the baseline's plus 100 ms.
//! - **Total stall duration per run:** the median within 110 percent plus
//!   1 s, except in traces with no consumption-limited request, where every
//!   wait for the network counts as a stall.
//! - **CPU seconds and peak RSS:** the median within 110 percent.
//! - **Excess downloaded bytes:** the median within 125 percent plus 16 MiB.
//! - **Throughput:** reads of ranges no earlier request of the run covered
//!   reach 90 percent of the baseline's median, cached ones 75 percent.
//!
//! Every candidate run must also answer every request without an error,
//! keep sampled cache usage within the trace's capacity, and stop cleanly.

use std::collections::BTreeMap;

use serde::Serialize;
use serde_json::Value;

use crate::summary::Distribution;
use crate::trace::{Action, Trace};

const MIB: f64 = 1024.0 * 1024.0;

/// What a measurement is, which picks its rule and unit.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Kind {
    Latency,
    Stall,
    Resource,
    Excess,
    UncachedThroughput,
    CachedThroughput,
}

/// The measurements compared, in report order.
const MEASUREMENTS: [(&str, Kind); 10] = [
    ("time_to_metadata_ms", Kind::Latency),
    ("time_to_first_byte_ms", Kind::Latency),
    ("time_to_first_playable_ms", Kind::Latency),
    ("seek_latency_ms", Kind::Latency),
    ("stall_total_ms", Kind::Stall),
    ("cpu_seconds", Kind::Resource),
    ("peak_rss_bytes", Kind::Resource),
    ("excess_bytes", Kind::Excess),
    (
        "uncached_throughput_bytes_per_second",
        Kind::UncachedThroughput,
    ),
    ("cached_throughput_bytes_per_second", Kind::CachedThroughput),
];

/// One check of a candidate's measurement.
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct Check {
    pub profile: String,
    pub trace: String,
    pub measurement: String,
    /// `median`, `p90`, or `every run` for the checks every run must pass.
    pub statistic: &'static str,
    /// The baseline's value of the statistic, absent for checks against a
    /// fixed limit.
    pub baseline: Option<f64>,
    pub candidate: f64,
    pub limit: f64,
    /// Whether the limit is an upper bound rather than a floor.
    pub at_most: bool,
    pub passed: bool,
}

/// The values one group of runs pools, by measurement, and what every run
/// must satisfy.
#[derive(Default)]
struct Group {
    values: BTreeMap<&'static str, Vec<f64>>,
    request_errors: f64,
    over_capacity: f64,
    unclean_stops: f64,
}

impl Group {
    fn add(&mut self, name: &'static str, value: Option<f64>) {
        if let Some(value) = value {
            self.values.entry(name).or_default().push(value);
        }
    }

    fn distribution(&self, name: &str) -> Option<Distribution> {
        Distribution::of(self.values.get(name)?.clone())
    }

    /// Adds one run's measurements.
    fn add_run(&mut self, run: &Value) {
        let record = &run["run"];
        let empty = Vec::new();
        for torrent in record["torrents"].as_array().unwrap_or(&empty) {
            if torrent["source"] == "magnet" {
                self.add(
                    "time_to_metadata_ms",
                    torrent["metadata_ms"]
                        .as_f64()
                        .zip(torrent["requested_ms"].as_f64())
                        .map(|(ready, requested)| ready - requested),
                );
            }
            self.add("excess_bytes", torrent["excess_bytes"].as_f64());
        }
        // Ranges each file's earlier requests read, to tell cached reads.
        let mut read: BTreeMap<(String, String), Vec<(u64, u64)>> = BTreeMap::new();
        let mut stall_ms = 0.0;
        let mut requests: Vec<&Value> = record["requests"]
            .as_array()
            .unwrap_or(&empty)
            .iter()
            .collect();
        requests.sort_by(|a, b| {
            a["sent_ms"]
                .as_f64()
                .unwrap_or(0.0)
                .total_cmp(&b["sent_ms"].as_f64().unwrap_or(0.0))
        });
        for request in requests {
            if !request["error"].is_null() {
                self.request_errors += 1.0;
            }
            if request["kind"] != "request" {
                continue;
            }
            let first_byte = request["time_to_first_byte_ms"].as_f64();
            if request["seek"] == true {
                self.add("seek_latency_ms", first_byte);
            } else {
                self.add("time_to_first_byte_ms", first_byte);
                self.add(
                    "time_to_first_playable_ms",
                    request["time_to_first_playable_ms"].as_f64(),
                );
            }
            for stall in request["stalls"].as_array().unwrap_or(&empty) {
                stall_ms += stall["duration_ms"].as_f64().unwrap_or(0.0);
            }
            let file = (
                request["fixture"].as_str().unwrap_or("").to_owned(),
                request["file"].as_str().unwrap_or("").to_owned(),
            );
            let start = request["range_start"].as_u64().unwrap_or(0);
            let end = start + request["body_bytes"].as_u64().unwrap_or(0);
            let ranges = read.entry(file).or_default();
            if let Some(throughput) = request["throughput_bytes_per_second"].as_f64() {
                let name = if covered(ranges, start, end) {
                    "cached_throughput_bytes_per_second"
                } else {
                    "uncached_throughput_bytes_per_second"
                };
                self.add(name, Some(throughput));
            }
            ranges.push((start, end));
        }
        self.add("stall_total_ms", Some(stall_ms));
        let summary = &record["summary"];
        self.add(
            "cpu_seconds",
            summary["user_seconds"]
                .as_f64()
                .zip(summary["system_seconds"].as_f64())
                .map(|(user, system)| user + system),
        );
        self.add("peak_rss_bytes", summary["peak_rss_bytes"].as_f64());
        let capacity = record["cache_capacity_bytes"].as_f64();
        let peak = summary["peak_cache_usage_bytes"].as_f64();
        if let (Some(capacity), Some(peak)) = (capacity, peak)
            && peak > capacity
        {
            self.over_capacity += 1.0;
        }
        if run["server"]["exit_success"] != true {
            self.unclean_stops += 1.0;
        }
    }
}

/// Whether `ranges` together cover `start..end`.
fn covered(ranges: &[(u64, u64)], start: u64, end: u64) -> bool {
    let mut sorted = ranges.to_vec();
    sorted.sort_unstable();
    let mut reached = start;
    for (from, to) in sorted {
        if from <= reached && to > reached {
            reached = to;
        }
    }
    reached >= end
}

/// Whether every request of `trace` reads as fast as bytes arrive.
fn unpaced(trace: &Trace) -> bool {
    trace
        .readers
        .values()
        .flatten()
        .all(|event| match &event.action {
            Action::Request(request) => request.consume_rate_bytes_per_second.is_none(),
            Action::Head => true,
        })
}

/// Checks the candidate's runs against the baseline's, group by group,
/// with `traces` giving each trace's definition by name.
pub fn compare(
    runs: &[Value],
    baseline: &str,
    candidate: &str,
    traces: &BTreeMap<String, Trace>,
) -> Vec<Check> {
    let mut groups: BTreeMap<(String, String), (Group, Group)> = BTreeMap::new();
    for run in runs {
        let text = |value: &Value| value.as_str().unwrap_or("").to_owned();
        let key = (text(&run["labels"]["profile"]), text(&run["trace"]));
        let implementation = run["implementation"].as_str().unwrap_or("");
        let (base, cand) = groups.entry(key).or_default();
        if implementation == baseline {
            base.add_run(run);
        } else if implementation == candidate {
            cand.add_run(run);
        }
    }
    let mut checks = Vec::new();
    for ((profile, trace), (base, cand)) in &groups {
        if cand.values.is_empty() || base.values.is_empty() {
            continue;
        }
        let check = |measurement: &str,
                     statistic: &'static str,
                     baseline: Option<f64>,
                     candidate: f64,
                     limit: f64,
                     at_most: bool| Check {
            profile: profile.clone(),
            trace: trace.clone(),
            measurement: measurement.to_owned(),
            statistic,
            baseline,
            candidate,
            limit,
            at_most,
            passed: if at_most {
                candidate <= limit
            } else {
                candidate >= limit
            },
        };
        let stalls_count = traces.get(trace).is_none_or(|trace| !unpaced(trace));
        for (name, kind) in MEASUREMENTS {
            let (Some(b), Some(c)) = (base.distribution(name), cand.distribution(name)) else {
                continue;
            };
            match kind {
                Kind::Latency => {
                    let limit = 1.10 * b.median + 50.0;
                    checks.push(check(name, "median", Some(b.median), c.median, limit, true));
                    if b.n >= 20 {
                        let limit = 1.25 * b.p90 + 100.0;
                        checks.push(check(name, "p90", Some(b.p90), c.p90, limit, true));
                    }
                }
                Kind::Stall if stalls_count => {
                    let limit = 1.10 * b.median + 1000.0;
                    checks.push(check(name, "median", Some(b.median), c.median, limit, true));
                }
                Kind::Stall => {}
                Kind::Resource => {
                    let limit = 1.10 * b.median;
                    checks.push(check(name, "median", Some(b.median), c.median, limit, true));
                }
                Kind::Excess => {
                    let limit = 1.25 * b.median + 16.0 * MIB;
                    checks.push(check(name, "median", Some(b.median), c.median, limit, true));
                }
                Kind::UncachedThroughput => {
                    let limit = 0.90 * b.median;
                    checks.push(check(
                        name,
                        "median",
                        Some(b.median),
                        c.median,
                        limit,
                        false,
                    ));
                }
                Kind::CachedThroughput => {
                    let limit = 0.75 * b.median;
                    checks.push(check(
                        name,
                        "median",
                        Some(b.median),
                        c.median,
                        limit,
                        false,
                    ));
                }
            }
        }
        for (name, count) in [
            ("request_errors", cand.request_errors),
            ("runs_over_cache_capacity", cand.over_capacity),
            ("unclean_stops", cand.unclean_stops),
        ] {
            checks.push(check(name, "every run", None, count, 0.0, true));
        }
    }
    checks
}

/// A value in the unit its measurement reads best in.
fn display(measurement: &str, value: f64) -> String {
    if measurement.ends_with("_ms") {
        if value >= 1000.0 {
            format!("{:.2} s", value / 1000.0)
        } else {
            format!("{value:.0} ms")
        }
    } else if measurement.ends_with("_bytes_per_second") {
        format!("{:.1} MB/s", value / 1e6)
    } else if measurement.ends_with("_bytes") {
        format!("{:.0} MiB", value / MIB)
    } else if measurement == "cpu_seconds" {
        format!("{value:.1} s")
    } else {
        format!("{value:.0}")
    }
}

/// A Markdown report of `checks`, misses first, for reading.
pub fn markdown(checks: &[Check], baseline: &str, candidate: &str) -> String {
    let passed = checks.iter().filter(|check| check.passed).count();
    let mut out = format!(
        "{passed} of {} checks of {candidate} against {baseline} passed.\n\n",
        checks.len()
    );
    out.push_str(
        "| Result | Profile | Trace | Measurement | Statistic | Baseline | Candidate | Limit |\n",
    );
    out.push_str("|---|---|---|---|---|---|---|---|\n");
    let mut ordered: Vec<&Check> = checks.iter().collect();
    ordered.sort_by_key(|check| check.passed);
    for check in ordered {
        let baseline = check.baseline.map_or_else(
            || "—".to_owned(),
            |value| display(&check.measurement, value),
        );
        out.push_str(&format!(
            "| {} | {} | {} | {} | {} | {} | {} | {} {} |\n",
            if check.passed { "ok" } else { "MISS" },
            check.profile,
            check.trace,
            check.measurement,
            check.statistic,
            baseline,
            display(&check.measurement, check.candidate),
            if check.at_most { "≤" } else { "≥" },
            display(&check.measurement, check.limit),
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn request(seek: bool, start: u64, bytes: u64, ttfb: f64, throughput: Option<f64>) -> Value {
        json!({
            "kind": "request", "seek": seek, "fixture": "f", "file": "a.mkv",
            "range_start": start, "body_bytes": bytes, "sent_ms": ttfb,
            "time_to_first_byte_ms": ttfb, "time_to_first_playable_ms": ttfb + 1.0,
            "throughput_bytes_per_second": throughput,
            "stalls": [{ "start_ms": 0.0, "duration_ms": 2000.0 }], "error": null,
        })
    }

    fn run(implementation: &str, ttfb: f64, cpu: f64, requests: Vec<Value>) -> Value {
        json!({
            "trace": "t", "implementation": implementation, "labels": { "profile": "fast" },
            "server": { "exit_success": true },
            "run": {
                "cache_capacity_bytes": 100,
                "torrents": [{ "source": "magnet", "requested_ms": 0.0,
                               "metadata_ms": ttfb, "excess_bytes": 10 }],
                "requests": requests,
                "summary": { "user_seconds": cpu, "system_seconds": 0.0,
                             "peak_rss_bytes": 50, "peak_cache_usage_bytes": 100 },
            },
        })
    }

    fn find<'a>(checks: &'a [Check], measurement: &str, statistic: &str) -> &'a Check {
        checks
            .iter()
            .find(|check| check.measurement == measurement && check.statistic == statistic)
            .unwrap_or_else(|| panic!("no {statistic} check of {measurement}"))
    }

    #[test]
    fn latencies_and_resources_follow_the_rules() {
        let runs = [
            run(
                "go",
                1000.0,
                10.0,
                vec![request(false, 0, 10, 1000.0, None)],
            ),
            run(
                "rust",
                1160.0,
                11.5,
                vec![request(false, 0, 10, 1160.0, None)],
            ),
        ];
        let checks = compare(&runs, "go", "rust", &BTreeMap::new());
        let first_byte = find(&checks, "time_to_first_byte_ms", "median");
        assert_eq!(first_byte.limit, 1150.0, "110 percent plus 50 ms");
        assert!(!first_byte.passed);
        let cpu = find(&checks, "cpu_seconds", "median");
        assert!(!cpu.passed, "11.5 s is beyond 110 percent of 10 s");
        assert!(
            checks.iter().all(|check| check.statistic != "p90"),
            "fewer than 20 values need no 90th percentile"
        );
        let stall = find(&checks, "stall_total_ms", "median");
        assert_eq!(stall.limit, 1.10 * 2000.0 + 1000.0);
        assert!(find(&checks, "unclean_stops", "every run").passed);
    }

    #[test]
    fn rereading_a_range_is_a_cached_read() {
        assert!(covered(&[(0, 5), (5, 10)], 2, 10));
        assert!(!covered(&[(0, 5), (6, 10)], 0, 10));
        let reads = |rate: f64| {
            vec![
                request(false, 0, 100, 1.0, Some(rate)),
                request(true, 0, 100, 2.0, Some(10.0 * rate)),
            ]
        };
        let runs = [
            run("go", 1.0, 1.0, reads(100.0)),
            run("rust", 1.0, 1.0, reads(80.0)),
        ];
        let checks = compare(&runs, "go", "rust", &BTreeMap::new());
        let uncached = find(&checks, "uncached_throughput_bytes_per_second", "median");
        assert_eq!((uncached.limit, uncached.passed), (90.0, false));
        let cached = find(&checks, "cached_throughput_bytes_per_second", "median");
        assert_eq!((cached.limit, cached.passed), (750.0, true));
    }

    #[test]
    fn stalls_of_unpaced_traces_are_not_compared() {
        let trace = crate::trace::parse(
            r#"{"t_ms": 0, "reader": "r1", "fixture": "f", "file": "a.mkv", "event": "request", "range_start": 0, "range_end": null, "consume_bytes": 10, "consume_rate_bytes_per_second": null}"#,
        )
        .unwrap();
        let traces = BTreeMap::from([("t".to_owned(), trace)]);
        let runs = [
            run("go", 1.0, 1.0, vec![request(false, 0, 10, 1.0, None)]),
            run("rust", 1.0, 1.0, vec![request(false, 0, 10, 1.0, None)]),
        ];
        let checks = compare(&runs, "go", "rust", &traces);
        assert!(
            checks
                .iter()
                .all(|check| check.measurement != "stall_total_ms")
        );
    }
}
