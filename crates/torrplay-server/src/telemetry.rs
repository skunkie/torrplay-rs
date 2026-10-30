// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! What the server measures about itself, as `docs/observability.md`
//! describes: the run, responses by route, streamed bytes, the process's
//! resources, and the request ID every response carries.

use std::collections::BTreeMap;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::task::{Context, Poll};
use std::time::{Duration, Instant, SystemTime};

use axum::body::Body;
use axum::extract::Request;
use axum::http::{HeaderName, HeaderValue, Method, StatusCode};
use axum::middleware::Next;
use axum::response::Response;
use bytes::Bytes;
use http_body::{Frame, SizeHint};
use torrplay_core::id::{RunId, TORRENT_ID_RANDOM_BYTES, random_torrent_id_bytes};
use torrplay_core::telemetry::{Histogram, HistogramSnapshot, LogRecordCounts};
use tracing::Instrument;

/// The header that carries a request's ID.
pub(crate) const REQUEST_ID: HeaderName = HeaderName::from_static("x-request-id");

/// The prefix of the request IDs the application chooses.
const REQUEST_ID_PREFIX: &str = "req_";

/// The longest request ID a request may bring.
const REQUEST_ID_MAX_BYTES: usize = 128;

/// The upper bounds of the response head histogram, in seconds.
pub(crate) const RESPONSE_HEAD_BOUNDS: &[f64] = &[
    0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0,
];

/// The methods that label responses as themselves; others are `other`, so a
/// client cannot add series by inventing methods.
const LABELED_METHODS: &[&str] = &[
    "GET",
    "HEAD",
    "POST",
    "PUT",
    "PATCH",
    "DELETE",
    "OPTIONS",
    "SUBSCRIBE",
    "UNSUBSCRIBE",
];

/// The process run the server belongs to: its ID, when it started, and the
/// count of its log records, which the logging layer keeps.
#[derive(Clone, Debug)]
pub struct Run {
    id: RunId,
    started_at: SystemTime,
    started: Instant,
    log_records: Arc<LogRecordCounts>,
}

impl Run {
    /// A run that started now, with the ID `id`, whose log records are
    /// counted in `log_records`.
    pub fn new(id: RunId, log_records: Arc<LogRecordCounts>) -> Self {
        Self {
            id,
            started_at: SystemTime::now(),
            started: Instant::now(),
            log_records,
        }
    }

    /// A run that started now with a random ID, or one of zeros if the
    /// operating system's random number generator fails, since a run ID only
    /// tells runs apart.
    pub fn draw() -> Self {
        let bytes = random_torrent_id_bytes().unwrap_or([0; TORRENT_ID_RANDOM_BYTES]);
        Self::new(RunId::from_random_bytes(bytes), Arc::default())
    }

    /// The run's ID.
    pub fn id(&self) -> &RunId {
        &self.id
    }

    pub(crate) fn started_at(&self) -> SystemTime {
        self.started_at
    }

    /// How long the process has run, by the monotonic clock.
    pub(crate) fn uptime(&self) -> Duration {
        self.started.elapsed()
    }

    /// The counts of the run's log records, which the logging layer adds
    /// to.
    pub fn log_records(&self) -> Arc<LogRecordCounts> {
        Arc::clone(&self.log_records)
    }
}

impl Default for Run {
    fn default() -> Self {
        Self::draw()
    }
}

/// The server's measurements of its responses and streams.
#[derive(Debug, Default)]
pub(crate) struct Telemetry {
    streamed_bytes: Arc<AtomicU64>,
    responses: Mutex<Responses>,
}

#[derive(Debug, Default)]
struct Responses {
    /// Responses by method, route, and status.
    counts: BTreeMap<(&'static str, String, u16), u64>,
    /// Response head durations by method and route.
    heads: BTreeMap<(&'static str, String), Histogram>,
}

/// The responses with one method, route, and status.
pub(crate) struct ResponseCount {
    pub(crate) method: &'static str,
    pub(crate) route: String,
    pub(crate) status: u16,
    pub(crate) count: u64,
}

/// The response heads of one method and route.
pub(crate) struct ResponseHeads {
    pub(crate) method: &'static str,
    pub(crate) route: String,
    pub(crate) durations: HistogramSnapshot,
}

impl Telemetry {
    /// Records a response whose headers were ready `elapsed` after its
    /// request arrived.
    pub(crate) fn record_response(
        &self,
        method: &Method,
        route: &str,
        status: StatusCode,
        elapsed: Duration,
    ) {
        let method = method_label(method);
        let mut responses = self
            .responses
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        *responses
            .counts
            .entry((method, route.to_owned(), status.as_u16()))
            .or_default() += 1;
        responses
            .heads
            .entry((method, route.to_owned()))
            .or_insert_with(|| Histogram::new(RESPONSE_HEAD_BOUNDS))
            .observe(elapsed);
    }

    /// The responses so far, ordered by method, route, and status.
    pub(crate) fn responses(&self) -> (Vec<ResponseCount>, Vec<ResponseHeads>) {
        let responses = self
            .responses
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let counts = responses
            .counts
            .iter()
            .map(|((method, route, status), count)| ResponseCount {
                method,
                route: route.clone(),
                status: *status,
                count: *count,
            })
            .collect();
        let heads = responses
            .heads
            .iter()
            .map(|((method, route), histogram)| ResponseHeads {
                method,
                route: route.clone(),
                durations: histogram.snapshot(),
            })
            .collect();
        (counts, heads)
    }

    /// The counter of file content sent in streams.
    pub(crate) fn streamed(&self) -> Arc<AtomicU64> {
        Arc::clone(&self.streamed_bytes)
    }

    pub(crate) fn streamed_bytes(&self) -> u64 {
        self.streamed_bytes.load(Ordering::Relaxed)
    }
}

fn method_label(method: &Method) -> &'static str {
    LABELED_METHODS
        .iter()
        .find(|name| **name == method.as_str())
        .copied()
        .unwrap_or("other")
}

tokio::task_local! {
    /// The ID of the request being served.
    static CURRENT_REQUEST: String;
}

/// The ID of the request the caller serves, if any.
pub(crate) fn current_request_id() -> Option<String> {
    CURRENT_REQUEST.try_with(String::clone).ok()
}

/// Gives the request an ID, which its log records, its problems, and its
/// response's `X-Request-Id` carry: the one the request brought, when it is
/// usable, or a new one.
pub(crate) async fn assign_request_id(request: Request, next: Next) -> Response {
    let id = request
        .headers()
        .get(&REQUEST_ID)
        .and_then(|value| value.to_str().ok())
        .filter(|id| is_usable_request_id(id))
        .map_or_else(new_request_id, str::to_owned);
    // An error span is enabled whatever the log level, so every record of
    // the request names it.
    let span = tracing::error_span!("request", request_id = %id);
    let mut response = CURRENT_REQUEST
        .scope(id.clone(), next.run(request).instrument(span.clone()))
        .await;
    // Records logged while the body streams name the request too.
    let response_body = std::mem::take(response.body_mut());
    *response.body_mut() = Body::new(SpannedBody {
        inner: response_body,
        span,
    });
    if let Ok(value) = HeaderValue::try_from(id) {
        response.headers_mut().insert(REQUEST_ID, value);
    }
    response
}

/// Whether a request's own ID may be kept: 1 to 128 letters, digits, and
/// `. _ : -`, which cannot forge a log line or a header.
fn is_usable_request_id(id: &str) -> bool {
    (1..=REQUEST_ID_MAX_BYTES).contains(&id.len())
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'-'))
}

/// A new request ID: `req_` and 128 random bits in lowercase base32, or a
/// counter's value when the random number generator fails.
fn new_request_id() -> String {
    static FALLBACK: AtomicU64 = AtomicU64::new(0);
    let bytes = random_torrent_id_bytes().unwrap_or_else(|_| {
        let mut bytes = [0; TORRENT_ID_RANDOM_BYTES];
        bytes[..8].copy_from_slice(&FALLBACK.fetch_add(1, Ordering::Relaxed).to_be_bytes());
        bytes
    });
    // A run ID has the same shape, so its encoding serves.
    let encoded = RunId::from_random_bytes(bytes);
    format!(
        "{REQUEST_ID_PREFIX}{}",
        &encoded.as_str()[RunId::PREFIX.len()..]
    )
}

/// A body polled inside its request's span.
struct SpannedBody {
    inner: Body,
    span: tracing::Span,
}

impl http_body::Body for SpannedBody {
    type Data = Bytes;
    type Error = axum::Error;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        let this = &mut *self;
        let _entered = this.span.enter();
        Pin::new(&mut this.inner).poll_frame(cx)
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> SizeHint {
        self.inner.size_hint()
    }
}

/// The process's resources, where the platform reports them.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct Process {
    pub(crate) resident_memory_bytes: Option<u64>,
    pub(crate) cpu_seconds: Option<f64>,
    pub(crate) open_fds: Option<u64>,
    pub(crate) max_fds: Option<u64>,
}

impl Process {
    /// Reads the process's resources. It reads files, so callers run it on a
    /// blocking thread.
    pub(crate) fn read() -> Self {
        #[cfg(any(target_os = "linux", target_os = "android"))]
        {
            linux::read()
        }
        #[cfg(not(any(target_os = "linux", target_os = "android")))]
        {
            Self::default()
        }
    }
}

#[cfg(any(target_os = "linux", target_os = "android"))]
mod linux {
    //! The process's resources from `/proc`, which needs no system calls
    //! beyond reading files.

    use super::Process;

    pub(super) fn read() -> Process {
        let status = std::fs::read_to_string("/proc/self/status").ok();
        let schedstat = std::fs::read_to_string("/proc/self/schedstat").ok();
        let limits = std::fs::read_to_string("/proc/self/limits").ok();
        Process {
            resident_memory_bytes: status.as_deref().and_then(resident_memory_bytes),
            cpu_seconds: schedstat.as_deref().and_then(cpu_seconds),
            // Reading the directory opens one descriptor more, which is
            // counted as the Prometheus client libraries count it.
            open_fds: std::fs::read_dir("/proc/self/fd")
                .ok()
                .map(|entries| entries.count() as u64),
            max_fds: limits.as_deref().and_then(max_fds),
        }
    }

    /// `VmRSS` from `/proc/self/status`, which gives it in KiB.
    pub(super) fn resident_memory_bytes(status: &str) -> Option<u64> {
        let line = status.lines().find(|line| line.starts_with("VmRSS:"))?;
        let kib: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
        kib.checked_mul(1024)
    }

    /// The time on the CPU from `/proc/self/schedstat`, whose first field
    /// is in nanoseconds, so no clock tick rate is needed.
    pub(super) fn cpu_seconds(schedstat: &str) -> Option<f64> {
        let nanoseconds: u64 = schedstat.split_whitespace().next()?.parse().ok()?;
        Some(nanoseconds as f64 / 1e9)
    }

    /// The soft limit on open files from `/proc/self/limits`.
    pub(super) fn max_fds(limits: &str) -> Option<u64> {
        let line = limits
            .lines()
            .find(|line| line.starts_with("Max open files"))?;
        line.split_whitespace().nth(3)?.parse().ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_keep_only_ids_that_cannot_forge_a_line() {
        for usable in ["abc", "req_7ndq", "9f1c-4e.2:x_y", &"a".repeat(128)] {
            assert!(is_usable_request_id(usable), "{usable}");
        }
        for unusable in ["", "a b", "a\nb", "ä", "a\"b", &"a".repeat(129)] {
            assert!(!is_usable_request_id(unusable), "{unusable:?}");
        }
    }

    #[test]
    fn new_request_ids_are_distinct_and_usable() {
        let (first, second) = (new_request_id(), new_request_id());
        assert_ne!(first, second);
        assert!(first.starts_with(REQUEST_ID_PREFIX), "{first}");
        assert_eq!(first.len(), REQUEST_ID_PREFIX.len() + 26);
        assert!(is_usable_request_id(&first));
    }

    #[test]
    fn unknown_methods_share_one_label() {
        assert_eq!(method_label(&Method::GET), "GET");
        assert_eq!(
            method_label(&Method::from_bytes(b"SUBSCRIBE").unwrap()),
            "SUBSCRIBE"
        );
        assert_eq!(method_label(&Method::from_bytes(b"BREW").unwrap()), "other");
    }

    #[test]
    fn responses_are_counted_by_route_and_status() {
        let telemetry = Telemetry::default();
        let route = "/api/v1/torrents/{torrent_id}";
        for status in [StatusCode::OK, StatusCode::OK, StatusCode::NOT_FOUND] {
            telemetry.record_response(&Method::GET, route, status, Duration::from_millis(3));
        }
        let (counts, heads) = telemetry.responses();
        let counts: Vec<_> = counts
            .iter()
            .map(|count| {
                (
                    count.method,
                    count.route.as_str(),
                    count.status,
                    count.count,
                )
            })
            .collect();
        assert_eq!(counts, [("GET", route, 200, 2), ("GET", route, 404, 1)]);
        assert_eq!(heads.len(), 1, "one histogram per method and route");
        assert_eq!(heads[0].durations.count, 3);
    }

    #[cfg(any(target_os = "linux", target_os = "android"))]
    #[test]
    fn proc_files_give_the_process_resources() {
        let status = "Name:\ttorrplay\nVmRSS:\t   402432 kB\nThreads:\t9\n";
        assert_eq!(linux::resident_memory_bytes(status), Some(402_432 * 1024));
        assert_eq!(linux::cpu_seconds("1500000000 20000 7\n"), Some(1.5));
        let limits = "Limit                     Soft Limit           Hard Limit           Units\n\
                      Max open files            1024                 524288               files\n";
        assert_eq!(linux::max_fds(limits), Some(1024));
        assert_eq!(linux::resident_memory_bytes("Name:\tx\n"), None);
    }
}
