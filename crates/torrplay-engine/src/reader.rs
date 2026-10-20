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
//! Dropping a range reader that has not failed, whether its range is done or
//! its client went away, keeps the scheduler reader idle for the next stream
//! of the file; a reader that failed or was canceled is released.

use std::future::Future;
use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt, ReadBuf, Take};
use tokio::time::{Instant, Sleep};

use crate::scheduler::Reader;

/// A reader of one byte range of a file.
pub struct RangeReader {
    // Taken by `drop` to park the reader.
    inner: Option<Take<Reader>>,
    failed: bool,
    piece_wait: Duration,
    // Armed while a read waits for data.
    deadline: Option<Pin<Box<Sleep>>>,
}

impl RangeReader {
    /// Wraps `inner`, already positioned at the range's start, to yield
    /// `length` bytes, each read waiting at most `piece_wait`.
    pub(crate) fn new(inner: Reader, length: u64, piece_wait: Duration) -> Self {
        Self {
            inner: Some(inner.take(length)),
            failed: false,
            piece_wait,
            deadline: None,
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
        let result = Self::poll_inner(inner, &mut this.deadline, this.piece_wait, cx, buf, before);
        if let Poll::Ready(Err(_)) = &result {
            this.failed = true;
        }
        result
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
        if let Some(inner) = self.inner.take()
            && !self.failed
        {
            inner.into_inner().park();
        }
    }
}
