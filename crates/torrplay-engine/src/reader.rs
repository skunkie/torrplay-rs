// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! The byte-range readers the adapter hands out.
//!
//! A [`RangeReader`] yields exactly one range of a file from a scheduler
//! [`Reader`], whose position steers what the torrent downloads. A read that
//! makes no progress within the piece wait bound fails with
//! [`io::ErrorKind::TimedOut`], which ends the stream; the bound restarts
//! with every byte delivered, so a slow but moving stream is never cut off.
//!
//! Each wait for data is counted while it lasts and recorded when it ends,
//! by how it ended, for the piece wait metrics.
//!
//! Dropping a range reader that has not failed, whether its range is done or
//! its client went away, keeps the scheduler reader idle for the next stream
//! of the file; a reader that failed or was canceled is released.

use std::future::Future;
use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt, ReadBuf, Take};
use tokio::time::{Instant, Sleep};
use torrplay_core::telemetry::{PieceWaitRecorder, WaitOutcome};

use crate::scheduler::Reader;

/// Waits at least this long are logged at debug level, for diagnosing stalls.
const LOGGED_WAIT: Duration = Duration::from_millis(500);

/// A reader of one byte range of a file.
pub struct RangeReader {
    // Taken by `drop` to park the reader.
    inner: Option<Take<Reader>>,
    failed: bool,
    piece_wait: Duration,
    // Armed while a read waits for data.
    deadline: Option<Pin<Box<Sleep>>>,
    waits: Arc<PieceWaitRecorder>,
}

impl RangeReader {
    /// Wraps `inner`, already positioned at the range's start, to yield
    /// `length` bytes, each read waiting at most `piece_wait`, with its
    /// waits recorded in `waits`.
    pub(crate) fn new(
        inner: Reader,
        length: u64,
        piece_wait: Duration,
        waits: Arc<PieceWaitRecorder>,
    ) -> Self {
        Self {
            inner: Some(inner.take(length)),
            failed: false,
            piece_wait,
            deadline: None,
            waits,
        }
    }
}

impl AsyncRead for RangeReader {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = &mut *self;
        let Some(inner) = this.inner.as_mut() else {
            return Poll::Ready(Ok(()));
        };
        if inner.limit() == 0 || buf.remaining() == 0 {
            return Poll::Ready(Ok(()));
        }
        let before = buf.filled().len();
        let waiting_since = this
            .deadline
            .as_ref()
            .map(|deadline| deadline.deadline() - this.piece_wait);
        let result = Self::poll_inner(inner, &mut this.deadline, this.piece_wait, cx, buf, before);
        if waiting_since.is_none() && result.is_pending() {
            this.waits.begin();
        }
        if let (Poll::Ready(outcome), Some(since)) = (&result, waiting_since) {
            let waited = since.elapsed();
            this.waits.end(wait_outcome(outcome), waited);
            if waited >= LOGGED_WAIT {
                let reader = inner.get_ref();
                tracing::debug!(
                    file_index = reader.file(),
                    position = reader.position(),
                    waited_ms = waited.as_millis() as u64,
                    failed = outcome.is_err(),
                    "read waited for data"
                );
            }
        }
        if let Poll::Ready(Err(_)) = &result {
            this.failed = true;
            // A failed read leaves its deadline, which no longer marks a wait.
            this.deadline = None;
        }
        result
    }
}

/// How a wait that ended with `outcome` is recorded.
fn wait_outcome(outcome: &io::Result<()>) -> WaitOutcome {
    match outcome {
        Ok(()) => WaitOutcome::Ready,
        Err(error) if error.kind() == io::ErrorKind::TimedOut => WaitOutcome::TimedOut,
        Err(_) => WaitOutcome::Canceled,
    }
}

impl RangeReader {
    fn poll_inner(
        inner: &mut Take<Reader>,
        deadline: &mut Option<Pin<Box<Sleep>>>,
        piece_wait: Duration,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
        before: usize,
    ) -> Poll<io::Result<()>> {
        match Pin::new(inner).poll_read(cx, buf) {
            Poll::Ready(Ok(())) => {
                *deadline = None;
                if buf.filled().len() == before {
                    // The file ended before the range did, which the range
                    // check before opening rules out.
                    return Poll::Ready(Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "the file ended before the range",
                    )));
                }
                Poll::Ready(Ok(()))
            }
            Poll::Ready(Err(error)) => Poll::Ready(Err(error)),
            Poll::Pending => {
                let deadline = deadline.get_or_insert_with(|| {
                    Box::pin(tokio::time::sleep_until(Instant::now() + piece_wait))
                });
                match deadline.as_mut().poll(cx) {
                    Poll::Ready(()) => Poll::Ready(Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "no piece data arrived within the piece wait bound",
                    ))),
                    Poll::Pending => Poll::Pending,
                }
            }
        }
    }
}

impl Drop for RangeReader {
    fn drop(&mut self) {
        // A wait still armed ends with its stream.
        if let Some(deadline) = self.deadline.take() {
            let since = deadline.deadline() - self.piece_wait;
            self.waits.end(WaitOutcome::Canceled, since.elapsed());
        }
        if let Some(inner) = self.inner.take()
            && !self.failed
        {
            inner.into_inner().park();
        }
    }
}
