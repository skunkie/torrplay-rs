// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! The system resource, the application's statistics, and the metrics for
//! Prometheus, which `docs/observability.md` catalogs.

use std::collections::BTreeMap;
use std::fmt::Write;
use std::sync::Arc;

use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use torrplay_api as api;
use torrplay_core::engine::{
    CacheStatistics, ConnectionOutcomes, Engine, EngineError, EngineStatistics,
};
use torrplay_core::lifecycle::ObservedState;
use torrplay_core::telemetry::HistogramSnapshot;

use crate::app::App;
use crate::headers::negotiate;
use crate::problem::Problem;
use crate::representation::{BASE, JSON, NO_CACHE, count, json};
use crate::telemetry::Process;

/// The media type of the Prometheus text exposition format.
const EXPOSITION: &str = "text/plain; version=0.0.4; charset=utf-8";

/// The application's version.
const VERSION: &str = env!("CARGO_PKG_VERSION");

fn system_path() -> String {
    format!("{BASE}/system")
}

fn statistics_path() -> String {
    format!("{BASE}/statistics")
}

/// The metrics' path, outside the versioned API.
pub(crate) const METRICS_PATH: &str = "/metrics";

pub(crate) async fn system<E: Engine>(
    State(app): State<Arc<App<E>>>,
    headers: HeaderMap,
) -> Result<Response, Problem> {
    negotiate(&headers, JSON)?;
    let process = read_process().await;
    let run = app.run();
    let representation = api::System {
        version: VERSION.to_owned(),
        os: std::env::consts::OS.to_owned(),
        architecture: std::env::consts::ARCH.to_owned(),
        run_id: run.id().to_string(),
        started_at: run.started_at().into(),
        uptime_seconds: count(run.uptime().as_secs()),
        resident_memory_bytes: process.resident_memory_bytes.map(count),
        links: api::SystemLinks {
            self_field: system_path(),
            statistics: statistics_path(),
            metrics: METRICS_PATH.to_owned(),
        },
    };
    uncached(json(StatusCode::OK, &representation)?)
}

pub(crate) async fn statistics<E: Engine>(
    State(app): State<Arc<App<E>>>,
    headers: HeaderMap,
) -> Result<Response, Problem> {
    negotiate(&headers, JSON)?;
    let engine = app.engine().engine_statistics().await?;
    let torrents = torrent_counts(&app).await?;
    let representation = api::Statistics {
        torrent_count: count(torrents.values().sum()),
        torrent_counts_by_observed_state: api::StatisticsTorrentCountsByObservedState {
            additional_properties: torrents
                .iter()
                .map(|(state, number)| (state.as_str().to_owned(), count(*number)))
                .collect(),
        },
        active_reader_count: count(engine.active_reader_count),
        idle_reader_count: count(engine.idle_reader_count),
        waiting_reader_count: count(engine.piece_waits.waiting_reader_count),
        peer_count: count(engine.peer_count),
        downloaded_bytes: count(engine.downloaded_bytes),
        uploaded_bytes: count(engine.uploaded_bytes),
        streamed_bytes: count(app.telemetry().streamed_bytes()),
        download_rate_bytes_per_second: count(engine.download_rate_bytes_per_second),
        upload_rate_bytes_per_second: count(engine.upload_rate_bytes_per_second),
        links: api::StatisticsLinks {
            self_field: statistics_path(),
            system: system_path(),
            cache_statistics: format!("{BASE}/cache/statistics"),
        },
    };
    uncached(json(StatusCode::OK, &representation)?)
}

/// Serves the metrics. While the engine is unavailable or the stored
/// torrents are restored, the metrics that need them are left out, so a
/// scrape still sees the process.
pub(crate) async fn metrics<E: Engine>(
    State(app): State<Arc<App<E>>>,
    headers: HeaderMap,
) -> Result<Response, Problem> {
    negotiate(&headers, "text/plain")?;
    let process = read_process().await;
    let engine = match app.engine().engine_statistics().await {
        Ok(statistics) => Some(statistics),
        Err(EngineError::Unavailable { .. }) => None,
        Err(error) => return Err(error.into()),
    };
    let torrents = torrent_counts(&app).await.ok();
    let cache = app.cache_statistics().await?;
    let mut exposition = Exposition::default();
    exposition.process(&app, &process);
    if let Some(torrents) = &torrents {
        exposition.family(
            "torrplay_torrents",
            "gauge",
            "Torrents in each observed state.",
        );
        for (state, number) in torrents {
            exposition.sample(
                "torrplay_torrents",
                &[("observed_state", state.as_str())],
                *number,
            );
        }
    }
    if let Some(engine) = &engine {
        exposition.engine(engine);
    }
    exposition.transfers(&app, engine.as_ref());
    exposition.cache(&cache);
    exposition.responses(&app);
    exposition.log_records(&app);
    let mut response = (
        [(header::CONTENT_TYPE, HeaderValue::from_static(EXPOSITION))],
        exposition.text,
    )
        .into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, NO_CACHE);
    Ok(response)
}

fn uncached(mut response: Response) -> Result<Response, Problem> {
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, NO_CACHE);
    Ok(response)
}

/// Reads the process's resources on a blocking thread.
async fn read_process() -> Process {
    tokio::task::spawn_blocking(Process::read)
        .await
        .unwrap_or_default()
}

/// The number of torrents in each observed state, every state present.
async fn torrent_counts<E: Engine>(app: &App<E>) -> Result<BTreeMap<ObservedState, u64>, Problem> {
    let mut counts: BTreeMap<_, u64> = ObservedState::ALL
        .into_iter()
        .map(|state| (state, 0))
        .collect();
    for record in app.registry()?.list() {
        let view = app.observe(record).await?;
        *counts.entry(view.observed_state()).or_default() += 1;
    }
    Ok(counts)
}

/// The Prometheus text exposition format, version 0.0.4, being written.
#[derive(Default)]
struct Exposition {
    text: String,
}

impl Exposition {
    fn family(&mut self, name: &str, kind: &str, help: &str) {
        // Writing to a String cannot fail.
        let _ = writeln!(self.text, "# HELP {name} {help}");
        let _ = writeln!(self.text, "# TYPE {name} {kind}");
    }

    fn sample(&mut self, name: &str, labels: &[(&str, &str)], value: impl std::fmt::Display) {
        self.text.push_str(name);
        if !labels.is_empty() {
            self.text.push('{');
            for (index, (label, value)) in labels.iter().enumerate() {
                if index > 0 {
                    self.text.push(',');
                }
                let _ = write!(self.text, "{label}=\"{}\"", escape(value));
            }
            self.text.push('}');
        }
        let _ = writeln!(self.text, " {value}");
    }

    fn single(&mut self, name: &str, kind: &str, help: &str, value: impl std::fmt::Display) {
        self.family(name, kind, help);
        self.sample(name, &[], value);
    }

    fn histogram(&mut self, name: &str, labels: &[(&str, &str)], snapshot: &HistogramSnapshot) {
        let bucket = format!("{name}_bucket");
        for (bound, cumulative) in snapshot.bounds.iter().zip(&snapshot.buckets) {
            let bound = bound.to_string();
            let mut with_bound = labels.to_vec();
            with_bound.push(("le", &bound));
            self.sample(&bucket, &with_bound, cumulative);
        }
        let mut infinite = labels.to_vec();
        infinite.push(("le", "+Inf"));
        self.sample(&bucket, &infinite, snapshot.count);
        self.sample(&format!("{name}_sum"), labels, snapshot.sum.as_secs_f64());
        self.sample(&format!("{name}_count"), labels, snapshot.count);
    }

    fn process<E: Engine>(&mut self, app: &App<E>, process: &Process) {
        self.family(
            "torrplay_build_info",
            "gauge",
            "Always 1; the labels describe the build.",
        );
        self.sample("torrplay_build_info", &[("version", VERSION)], 1);
        let started = app
            .run()
            .started_at()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default();
        self.single(
            "process_start_time_seconds",
            "gauge",
            "Start time of the process since unix epoch in seconds.",
            started.as_secs_f64(),
        );
        if let Some(bytes) = process.resident_memory_bytes {
            self.single(
                "process_resident_memory_bytes",
                "gauge",
                "Resident memory size in bytes.",
                bytes,
            );
        }
        if let Some(seconds) = process.cpu_seconds {
            self.single(
                "process_cpu_seconds_total",
                "counter",
                "Total user and system CPU time spent in seconds.",
                seconds,
            );
        }
        if let Some(open) = process.open_fds {
            self.single(
                "process_open_fds",
                "gauge",
                "Number of open file descriptors.",
                open,
            );
        }
        if let Some(max) = process.max_fds {
            self.single(
                "process_max_fds",
                "gauge",
                "Maximum number of open file descriptors.",
                max,
            );
        }
    }

    fn engine(&mut self, engine: &EngineStatistics) {
        self.family(
            "torrplay_readers",
            "gauge",
            "Readers serving a stream, and idle readers.",
        );
        self.sample(
            "torrplay_readers",
            &[("state", "active")],
            engine.active_reader_count,
        );
        self.sample(
            "torrplay_readers",
            &[("state", "idle")],
            engine.idle_reader_count,
        );
        self.single(
            "torrplay_waiting_readers",
            "gauge",
            "Active readers waiting for a missing piece.",
            engine.piece_waits.waiting_reader_count,
        );
        self.single(
            "torrplay_peers",
            "gauge",
            "Connected peers summed over torrents.",
            engine.peer_count,
        );
        self.family(
            "torrplay_peer_connections_total",
            "counter",
            "Finished attempts to connect to a peer.",
        );
        let connections = &engine.peer_connections;
        for (transport, outcomes) in [
            ("tcp", connections.tcp),
            ("utp", connections.utp),
            ("socks", connections.socks),
        ] {
            let ConnectionOutcomes { succeeded, failed } = outcomes;
            for (outcome, number) in [("succeeded", succeeded), ("failed", failed)] {
                self.sample(
                    "torrplay_peer_connections_total",
                    &[("transport", transport), ("outcome", outcome)],
                    number,
                );
            }
        }
        self.family(
            "torrplay_piece_wait_seconds",
            "histogram",
            "How long a reader waited for a missing piece, observed when the wait ended.",
        );
        let waits = &engine.piece_waits;
        for (outcome, snapshot) in [
            ("ready", &waits.ready),
            ("timed_out", &waits.timed_out),
            ("canceled", &waits.canceled),
        ] {
            self.histogram(
                "torrplay_piece_wait_seconds",
                &[("outcome", outcome)],
                snapshot,
            );
        }
    }

    fn transfers<E: Engine>(&mut self, app: &App<E>, engine: Option<&EngineStatistics>) {
        if let Some(engine) = engine {
            self.single(
                "torrplay_downloaded_bytes_total",
                "counter",
                "Piece data downloaded from peers, without duplicates.",
                engine.downloaded_bytes,
            );
            self.single(
                "torrplay_uploaded_bytes_total",
                "counter",
                "Piece data uploaded to peers.",
                engine.uploaded_bytes,
            );
        }
        self.single(
            "torrplay_streamed_bytes_total",
            "counter",
            "File content sent in streams.",
            app.telemetry().streamed_bytes(),
        );
    }

    fn cache(&mut self, cache: &CacheStatistics) {
        self.single(
            "torrplay_cache_capacity_bytes",
            "gauge",
            "The cache capacity in effect.",
            cache.capacity_bytes,
        );
        self.single(
            "torrplay_cache_usage_bytes",
            "gauge",
            "Bytes counted against the cache capacity.",
            cache.usage_bytes,
        );
        self.single(
            "torrplay_cache_pinned_bytes",
            "gauge",
            "Resident pieces covered by a pinned range.",
            cache.pinned_bytes,
        );
        self.single(
            "torrplay_cache_evictions_total",
            "counter",
            "Pieces evicted from the cache.",
            cache.eviction_count,
        );
    }

    fn responses<E: Engine>(&mut self, app: &App<E>) {
        let (counts, heads) = app.telemetry().responses();
        self.family(
            "torrplay_http_requests_total",
            "counter",
            "Responses, counted when their headers are sent.",
        );
        for response in &counts {
            let status = response.status.to_string();
            self.sample(
                "torrplay_http_requests_total",
                &[
                    ("method", response.method),
                    ("route", &response.route),
                    ("status", &status),
                ],
                response.count,
            );
        }
        self.family(
            "torrplay_http_response_head_seconds",
            "histogram",
            "Time from a request's arrival until its response headers are sent.",
        );
        for head in &heads {
            self.histogram(
                "torrplay_http_response_head_seconds",
                &[("method", head.method), ("route", &head.route)],
                &head.durations,
            );
        }
    }

    fn log_records<E: Engine>(&mut self, app: &App<E>) {
        self.family(
            "torrplay_log_records_total",
            "counter",
            "Log records written, by level.",
        );
        for (level, number) in app.run().log_records().counts() {
            self.sample(
                "torrplay_log_records_total",
                &[("level", level.as_str())],
                number,
            );
        }
    }
}

/// Escapes a label value as the exposition format requires.
fn escape(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use torrplay_core::telemetry::Histogram;

    use super::*;

    #[test]
    fn label_values_are_escaped() {
        assert_eq!(escape(r#"a\b"c"#), r#"a\\b\"c"#);
        assert_eq!(escape("a\nb"), r"a\nb");
    }

    #[test]
    fn histograms_end_with_infinity_sum_and_count() {
        let histogram = Histogram::new(&[0.5, 1.0]);
        histogram.observe(Duration::from_millis(250));
        histogram.observe(Duration::from_secs(2));
        let mut exposition = Exposition::default();
        exposition.histogram(
            "wait_seconds",
            &[("outcome", "ready")],
            &histogram.snapshot(),
        );
        assert_eq!(
            exposition.text,
            "wait_seconds_bucket{outcome=\"ready\",le=\"0.5\"} 1\n\
             wait_seconds_bucket{outcome=\"ready\",le=\"1\"} 1\n\
             wait_seconds_bucket{outcome=\"ready\",le=\"+Inf\"} 2\n\
             wait_seconds_sum{outcome=\"ready\"} 2.25\n\
             wait_seconds_count{outcome=\"ready\"} 2\n"
        );
    }
}
