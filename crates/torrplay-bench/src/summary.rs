// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! Distributions across runs: runs of the same trace, implementation, and
//! labels form a group, and each measurement is reported as its median,
//! 90th percentile, minimum, and maximum, as the benchmark protocol asks.
//!
//! Measurements a run makes once, such as its CPU time, give one value per
//! run; those it makes per request or torrent, such as seek latencies, pool
//! every value of the group's runs.

use std::collections::BTreeMap;

use serde::Serialize;
use serde_json::Value;

/// A distribution of values.
#[derive(Clone, Copy, Debug, Serialize, PartialEq)]
pub struct Distribution {
    pub n: usize,
    pub median: f64,
    /// The 90th percentile, by the nearest-rank method.
    pub p90: f64,
    pub min: f64,
    pub max: f64,
}

impl Distribution {
    /// The distribution of `values`, or `None` if there are none.
    pub fn of(mut values: Vec<f64>) -> Option<Self> {
        if values.is_empty() {
            return None;
        }
        values.sort_by(f64::total_cmp);
        let rank = |fraction: f64| {
            let index = (fraction * values.len() as f64).ceil() as usize;
            values[index.clamp(1, values.len()) - 1]
        };
        let n = values.len();
        let median = if n % 2 == 1 {
            values[n / 2]
        } else {
            (values[n / 2 - 1] + values[n / 2]) / 2.0
        };
        Some(Self {
            n,
            median,
            p90: rank(0.9),
            min: values[0],
            max: values[n - 1],
        })
    }
}

/// One group's summary.
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct Group {
    pub trace: String,
    pub implementation: String,
    pub labels: BTreeMap<String, String>,
    pub runs: usize,
    pub metrics: BTreeMap<&'static str, Distribution>,
}

/// What runs of one group share: trace, implementation, and labels.
type GroupKey = (String, String, BTreeMap<String, String>);

/// The measurements of one run, by name.
fn measurements(run: &Value) -> BTreeMap<&'static str, Vec<f64>> {
    let mut values: BTreeMap<&'static str, Vec<f64>> = BTreeMap::new();
    let mut add = |name, value: Option<f64>| {
        if let Some(value) = value {
            values.entry(name).or_default().push(value);
        }
    };
    let record = &run["run"];
    let empty = Vec::new();
    let torrents = record["torrents"].as_array().unwrap_or(&empty);
    let requests = record["requests"].as_array().unwrap_or(&empty);
    for torrent in torrents {
        if torrent["source"] == "magnet" {
            add(
                "time_to_metadata_ms",
                torrent["metadata_ms"]
                    .as_f64()
                    .zip(torrent["requested_ms"].as_f64())
                    .map(|(ready, requested)| ready - requested),
            );
        }
        add("downloaded_bytes", torrent["downloaded_bytes"].as_f64());
        add("excess_bytes", torrent["excess_bytes"].as_f64());
    }
    let mut stalls = 0.0;
    let mut stall_ms = 0.0;
    let mut errors = 0.0;
    for request in requests {
        if !request["error"].is_null() {
            errors += 1.0;
        }
        if request["kind"] != "request" {
            continue;
        }
        let first_byte = request["time_to_first_byte_ms"].as_f64();
        if request["seek"] == true {
            add("seek_latency_ms", first_byte);
        } else {
            add("time_to_first_byte_ms", first_byte);
            add(
                "time_to_first_playable_ms",
                request["time_to_first_playable_ms"].as_f64(),
            );
        }
        add(
            "throughput_bytes_per_second",
            request["throughput_bytes_per_second"].as_f64(),
        );
        for stall in request["stalls"].as_array().unwrap_or(&empty) {
            stalls += 1.0;
            stall_ms += stall["duration_ms"].as_f64().unwrap_or(0.0);
        }
    }
    add("stall_count", Some(stalls));
    add("stall_total_ms", Some(stall_ms));
    add("request_errors", Some(errors));
    let summary = &record["summary"];
    add(
        "cpu_seconds",
        summary["user_seconds"]
            .as_f64()
            .zip(summary["system_seconds"].as_f64())
            .map(|(user, system)| user + system),
    );
    add("peak_rss_bytes", summary["peak_rss_bytes"].as_f64());
    add("steady_rss_bytes", summary["steady_rss_bytes"].as_f64());
    add(
        "peak_cache_usage_bytes",
        summary["peak_cache_usage_bytes"].as_f64(),
    );
    add("eviction_count", summary["eviction_count"].as_f64());
    values
}

/// Groups `runs`, files `run` writes, and summarizes each group.
pub fn summarize(runs: &[Value]) -> Vec<Group> {
    let mut groups: BTreeMap<GroupKey, Vec<&Value>> = BTreeMap::new();
    for run in runs {
        let labels = run["labels"]
            .as_object()
            .map(|labels| {
                labels
                    .iter()
                    .map(|(key, value)| (key.clone(), value.as_str().unwrap_or("").to_owned()))
                    .collect()
            })
            .unwrap_or_default();
        let text = |field: &str| run[field].as_str().unwrap_or("").to_owned();
        groups
            .entry((text("trace"), text("implementation"), labels))
            .or_default()
            .push(run);
    }
    groups
        .into_iter()
        .map(|((trace, implementation, labels), runs)| {
            let mut pooled: BTreeMap<&'static str, Vec<f64>> = BTreeMap::new();
            for run in &runs {
                for (name, values) in measurements(run) {
                    pooled.entry(name).or_default().extend(values);
                }
            }
            Group {
                trace,
                implementation,
                labels,
                runs: runs.len(),
                metrics: pooled
                    .into_iter()
                    .filter_map(|(name, values)| Some((name, Distribution::of(values)?)))
                    .collect(),
            }
        })
        .collect()
}

/// A Markdown table per group, for reading.
pub fn markdown(groups: &[Group]) -> String {
    let mut out = String::new();
    for group in groups {
        let labels: Vec<String> = group
            .labels
            .iter()
            .map(|(key, value)| format!("{key}={value}"))
            .collect();
        out.push_str(&format!(
            "## {} on {} ({} runs{}{})\n\n| Measurement | n | Median | p90 | Min | Max |\n|---|---|---|---|---|---|\n",
            group.trace,
            group.implementation,
            group.runs,
            if labels.is_empty() { "" } else { ", " },
            labels.join(", ")
        ));
        for (name, d) in &group.metrics {
            out.push_str(&format!(
                "| {name} | {} | {:.1} | {:.1} | {:.1} | {:.1} |\n",
                d.n, d.median, d.p90, d.min, d.max
            ));
        }
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn distributions_use_nearest_rank_percentiles() {
        let d = Distribution::of((1..=10).map(f64::from).collect()).unwrap();
        assert_eq!(
            (d.n, d.median, d.p90, d.min, d.max),
            (10, 5.5, 9.0, 1.0, 10.0)
        );
        let one = Distribution::of(vec![3.0]).unwrap();
        assert_eq!((one.median, one.p90), (3.0, 3.0));
        assert_eq!(Distribution::of(Vec::new()), None);
    }

    fn run(trace: &str, commit: &str, cpu: f64, seek_ms: f64) -> Value {
        json!({
            "trace": trace,
            "implementation": "torrplay",
            "labels": { "commit": commit },
            "run": {
                "torrents": [{
                    "fixture": "bench-video", "source": "magnet", "requested_ms": 1.0,
                    "metadata_ms": 51.0, "downloaded_bytes": 100, "excess_bytes": 10,
                }],
                "requests": [
                    { "kind": "request", "seek": false, "time_to_first_byte_ms": 20.0,
                      "time_to_first_playable_ms": 30.0, "throughput_bytes_per_second": null,
                      "stalls": [{ "start_ms": 0.0, "duration_ms": 600.0 }], "error": null },
                    { "kind": "request", "seek": true, "time_to_first_byte_ms": seek_ms,
                      "time_to_first_playable_ms": null, "throughput_bytes_per_second": null,
                      "stalls": [], "error": null },
                    { "kind": "head", "seek": false, "time_to_first_byte_ms": null,
                      "stalls": [], "error": "refused" },
                ],
                "summary": { "user_seconds": cpu, "system_seconds": 1.0, "peak_rss_bytes": 5,
                             "steady_rss_bytes": 4, "peak_cache_usage_bytes": 3,
                             "eviction_count": null },
            },
        })
    }

    #[test]
    fn runs_group_by_trace_implementation_and_labels() {
        let runs = [
            run("far-seeks", "a", 1.0, 100.0),
            run("far-seeks", "a", 3.0, 300.0),
            run("far-seeks", "b", 9.0, 900.0),
            run("cold-start", "a", 2.0, 200.0),
        ];
        let groups = summarize(&runs);
        assert_eq!(groups.len(), 3);
        let seeks = groups
            .iter()
            .find(|group| group.trace == "far-seeks" && group.labels["commit"] == "a")
            .unwrap();
        assert_eq!(seeks.runs, 2);
        assert_eq!(seeks.metrics["cpu_seconds"].median, 3.0, "1+1 and 3+1");
        assert_eq!(seeks.metrics["seek_latency_ms"].max, 300.0);
        assert_eq!(seeks.metrics["time_to_metadata_ms"].median, 50.0);
        assert_eq!(seeks.metrics["stall_total_ms"].median, 600.0);
        assert_eq!(seeks.metrics["request_errors"].max, 1.0);
        assert!(!seeks.metrics.contains_key("eviction_count"));
        assert!(!seeks.metrics.contains_key("throughput_bytes_per_second"));
        assert!(markdown(&groups).contains("## far-seeks on torrplay (2 runs, commit=a)"));
    }
}
