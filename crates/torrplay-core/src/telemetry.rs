// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! Measurements the application keeps about itself for
//! `docs/observability.md`: histograms of durations and counts of log
//! records by level.
//!
//! Recording takes no lock, so the engine's readers and the server's request
//! path can record on every wait and response.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use crate::settings::LogLevel;

/// A histogram of durations over fixed bucket bounds, in seconds.
#[derive(Debug)]
pub struct Histogram {
    bounds: &'static [f64],
    // One count per bound, then the count above the last bound.
    counts: Box<[AtomicU64]>,
    sum_nanoseconds: AtomicU64,
}

impl Histogram {
    /// Creates an empty histogram with the upper bounds `bounds`, which must
    /// increase.
    pub fn new(bounds: &'static [f64]) -> Self {
        Self {
            bounds,
            counts: (0..=bounds.len()).map(|_| AtomicU64::new(0)).collect(),
            sum_nanoseconds: AtomicU64::new(0),
        }
    }

    /// Records one observation of `duration`.
    pub fn observe(&self, duration: Duration) {
        let seconds = duration.as_secs_f64();
        let bucket = self
            .bounds
            .iter()
            .position(|bound| seconds <= *bound)
            .unwrap_or(self.bounds.len());
        self.counts[bucket].fetch_add(1, Ordering::Relaxed);
        let nanoseconds = u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX);
        self.sum_nanoseconds
            .fetch_add(nanoseconds, Ordering::Relaxed);
    }

    /// Returns the observations so far.
    pub fn snapshot(&self) -> HistogramSnapshot {
        let mut total = 0;
        let cumulative = self
            .counts
            .iter()
            .map(|count| {
                total += count.load(Ordering::Relaxed);
                total
            })
            .collect::<Vec<_>>();
        HistogramSnapshot {
            bounds: self.bounds,
            buckets: cumulative[..self.bounds.len()].to_vec(),
            count: total,
            sum: Duration::from_nanos(self.sum_nanoseconds.load(Ordering::Relaxed)),
        }
    }
}

/// A histogram's observations at one moment, in Prometheus's form.
#[derive(Clone, Debug, PartialEq)]
pub struct HistogramSnapshot {
    /// The buckets' upper bounds, in seconds.
    pub bounds: &'static [f64],
    /// The observations at or below each bound, cumulative.
    pub buckets: Vec<u64>,
    /// Every observation, including those above the last bound.
    pub count: u64,
    /// The sum of the observed durations.
    pub sum: Duration,
}

impl HistogramSnapshot {
    /// An empty snapshot over `bounds`.
    pub fn empty(bounds: &'static [f64]) -> Self {
        Self {
            bounds,
            buckets: vec![0; bounds.len()],
            count: 0,
            sum: Duration::ZERO,
        }
    }
}

/// The upper bounds of the piece wait histograms, in seconds.
pub const PIECE_WAIT_BOUNDS: &[f64] = &[
    0.01, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0, 60.0, 120.0,
];

/// How a reader's wait for a missing piece ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WaitOutcome {
    /// The piece arrived.
    Ready,
    /// The wait reached the piece wait bound, which cancels the stream.
    TimedOut,
    /// The stream ended otherwise: the client went away, or the torrent
    /// paused, failed, or was removed.
    Canceled,
}

/// Records readers' waits for missing pieces.
#[derive(Debug)]
pub struct PieceWaitRecorder {
    waiting: AtomicU64,
    ready: Histogram,
    timed_out: Histogram,
    canceled: Histogram,
}

impl Default for PieceWaitRecorder {
    fn default() -> Self {
        Self {
            waiting: AtomicU64::new(0),
            ready: Histogram::new(PIECE_WAIT_BOUNDS),
            timed_out: Histogram::new(PIECE_WAIT_BOUNDS),
            canceled: Histogram::new(PIECE_WAIT_BOUNDS),
        }
    }
}

impl PieceWaitRecorder {
    /// Counts a reader that starts waiting. Each call is followed by one
    /// [`end`](Self::end).
    pub fn begin(&self) {
        self.waiting.fetch_add(1, Ordering::Relaxed);
    }

    /// Records a wait of `waited` that ended with `outcome`.
    pub fn end(&self, outcome: WaitOutcome, waited: Duration) {
        self.waiting.fetch_sub(1, Ordering::Relaxed);
        match outcome {
            WaitOutcome::Ready => &self.ready,
            WaitOutcome::TimedOut => &self.timed_out,
            WaitOutcome::Canceled => &self.canceled,
        }
        .observe(waited);
    }

    /// Returns the waits so far.
    pub fn snapshot(&self) -> PieceWaits {
        PieceWaits {
            waiting_reader_count: self.waiting.load(Ordering::Relaxed),
            ready: self.ready.snapshot(),
            timed_out: self.timed_out.snapshot(),
            canceled: self.canceled.snapshot(),
        }
    }
}

/// Readers' waits for missing pieces at one moment.
#[derive(Clone, Debug, PartialEq)]
pub struct PieceWaits {
    /// The readers waiting now.
    pub waiting_reader_count: u64,
    /// The waits that ended with the piece.
    pub ready: HistogramSnapshot,
    /// The waits that reached the piece wait bound.
    pub timed_out: HistogramSnapshot,
    /// The waits their stream's end cut short.
    pub canceled: HistogramSnapshot,
}

impl Default for PieceWaits {
    fn default() -> Self {
        Self {
            waiting_reader_count: 0,
            ready: HistogramSnapshot::empty(PIECE_WAIT_BOUNDS),
            timed_out: HistogramSnapshot::empty(PIECE_WAIT_BOUNDS),
            canceled: HistogramSnapshot::empty(PIECE_WAIT_BOUNDS),
        }
    }
}

/// The number of log records written at each level since the process
/// started.
#[derive(Debug, Default)]
pub struct LogRecordCounts {
    counts: [AtomicU64; LogLevel::ALL.len()],
}

impl LogRecordCounts {
    /// Counts one record at `level`.
    pub fn record(&self, level: LogLevel) {
        self.counts[index(level)].fetch_add(1, Ordering::Relaxed);
    }

    /// Returns the count of every level, most severe first.
    pub fn counts(&self) -> [(LogLevel, u64); LogLevel::ALL.len()] {
        LogLevel::ALL.map(|level| (level, self.counts[index(level)].load(Ordering::Relaxed)))
    }
}

fn index(level: LogLevel) -> usize {
    LogLevel::ALL
        .iter()
        .position(|candidate| *candidate == level)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    const BOUNDS: &[f64] = &[0.1, 1.0];

    #[test]
    fn buckets_are_cumulative_and_count_what_exceeds_the_last_bound() {
        let histogram = Histogram::new(BOUNDS);
        for milliseconds in [50, 100, 500, 2000] {
            histogram.observe(Duration::from_millis(milliseconds));
        }
        let snapshot = histogram.snapshot();
        assert_eq!(snapshot.buckets, [2, 3], "a bound includes its own value");
        assert_eq!(snapshot.count, 4);
        assert_eq!(snapshot.sum, Duration::from_millis(2650));
    }

    #[test]
    fn an_empty_histogram_matches_an_empty_snapshot() {
        assert_eq!(
            Histogram::new(BOUNDS).snapshot(),
            HistogramSnapshot::empty(BOUNDS)
        );
    }

    #[test]
    fn waits_are_counted_while_they_last_and_recorded_by_outcome() {
        let waits = PieceWaitRecorder::default();
        waits.begin();
        waits.begin();
        assert_eq!(waits.snapshot().waiting_reader_count, 2);
        waits.end(WaitOutcome::Ready, Duration::from_millis(20));
        waits.end(WaitOutcome::TimedOut, Duration::from_secs(60));
        let snapshot = waits.snapshot();
        assert_eq!(snapshot.waiting_reader_count, 0);
        assert_eq!(snapshot.ready.count, 1);
        assert_eq!(snapshot.timed_out.count, 1);
        assert_eq!(snapshot.canceled.count, 0);
    }

    #[test]
    fn records_are_counted_by_level() {
        let counts = LogRecordCounts::default();
        counts.record(LogLevel::Warn);
        counts.record(LogLevel::Warn);
        counts.record(LogLevel::Trace);
        assert_eq!(
            counts.counts(),
            [
                (LogLevel::Error, 0),
                (LogLevel::Warn, 2),
                (LogLevel::Info, 0),
                (LogLevel::Debug, 0),
                (LogLevel::Trace, 1),
            ]
        );
    }
}
