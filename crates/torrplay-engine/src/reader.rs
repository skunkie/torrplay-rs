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
    inner: Take<Reader>,
    piece_wait: Duration,
    // Armed while a read waits for data.
    deadline: Option<Pin<Box<Sleep>>>,
}

impl RangeReader {
    /// Wraps `inner`, already positioned at the range's start, to yield
    /// `length` bytes, each read waiting at most `piece_wait`.
    pub(crate) fn new(inner: Reader, length: u64, piece_wait: Duration) -> Self {
        Self {
            inner: inner.take(length),
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
        if this.inner.limit() == 0 || buf.remaining() == 0 {
            return Poll::Ready(Ok(()));
        }
        let before = buf.filled().len();
        match Pin::new(&mut this.inner).poll_read(cx, buf) {
            Poll::Ready(Ok(())) => {
                this.deadline = None;
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
                let piece_wait = this.piece_wait;
                let deadline = this.deadline.get_or_insert_with(|| {
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
