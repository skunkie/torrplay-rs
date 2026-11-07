// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! The API listener's connections, which shutdown can close by force.
//!
//! Graceful shutdown only asks a connection to finish, and waits for it. A
//! connection may never finish: a peer that sent part of a request head, or
//! one that stopped reading while a write waits for room in its send buffer
//! or for its HTTP/2 flow control window. Canceling a stream does not help,
//! since hyper polls a response body only once it can write. So every
//! connection's I/O can be made to fail, which wakes a task blocked on it and
//! ends the connection whatever it was waiting for.
//!
//! That also drops a handler still running on the connection, so the handlers
//! are counted and shutdown can wait for them first.

use std::collections::HashSet;
use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::{Arc, Mutex, PoisonError};
use std::task::{Context, Poll};

use axum::Router;
use axum::extract::Request;
use axum::middleware::{self, Next};
use axum::serve::Listener;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;
use tokio_util::sync::{CancellationToken, WaitForCancellationFutureOwned};

/// The most peer addresses a report of open connections gives.
const REPORTED_PEER_LIMIT: usize = 16;

/// The connections of a [`ClosableListener`], which outlive it.
#[derive(Clone, Default)]
pub(crate) struct Connections {
    closed: CancellationToken,
    open: Arc<Mutex<HashSet<SocketAddr>>>,
    handlers: watch::Sender<usize>,
}

impl Connections {
    /// Counts `router`'s handlers while they run, until their responses'
    /// headers are ready; a response body in progress is not a handler.
    pub(crate) fn counting_handlers(&self, router: Router) -> Router {
        let connections = self.clone();
        router.layer(middleware::from_fn(move |request: Request, next: Next| {
            let running = connections.handler_began();
            async move {
                let _running = running;
                next.run(request).await
            }
        }))
    }

    fn handler_began(&self) -> RunningHandler {
        self.handlers.send_modify(|count| *count += 1);
        RunningHandler(self.handlers.clone())
    }

    /// How many handlers are running.
    pub(crate) fn running_handlers(&self) -> usize {
        *self.handlers.borrow()
    }

    /// Waits until no handler is running.
    pub(crate) async fn handlers_finished(&self) {
        let mut handlers = self.handlers.subscribe();
        // `self` holds the sender, so waiting cannot fail.
        let _ = handlers.wait_for(|&count| count == 0).await;
    }

    /// Makes the I/O of every open and later connection fail.
    pub(crate) fn close(&self) {
        self.closed.cancel();
    }

    /// How many connections are open, and the addresses of some of their
    /// peers.
    pub(crate) fn open(&self) -> (usize, Vec<SocketAddr>) {
        let open = self.open.lock().unwrap_or_else(PoisonError::into_inner);
        let mut peers: Vec<_> = open.iter().copied().collect();
        peers.sort_unstable();
        peers.truncate(REPORTED_PEER_LIMIT);
        (open.len(), peers)
    }

    fn opened(&self, peer: SocketAddr, stream: TcpStream) -> ClosableStream<TcpStream> {
        self.open
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(peer);
        ClosableStream::new(
            stream,
            &self.closed,
            OpenConnection {
                peer,
                open: Arc::clone(&self.open),
            },
        )
    }
}

/// Counts a handler as running until it is dropped, even if the handler is
/// canceled.
struct RunningHandler(watch::Sender<usize>);

impl Drop for RunningHandler {
    fn drop(&mut self) {
        self.0.send_modify(|count| *count -= 1);
    }
}

/// Counts a connection as open until it is dropped.
struct OpenConnection {
    peer: SocketAddr,
    open: Arc<Mutex<HashSet<SocketAddr>>>,
}

impl Drop for OpenConnection {
    fn drop(&mut self) {
        self.open
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&self.peer);
    }
}

/// A listener whose connections [`Connections`] can close.
pub(crate) struct ClosableListener {
    listener: TcpListener,
    connections: Connections,
}

impl ClosableListener {
    pub(crate) fn new(listener: TcpListener) -> (Self, Connections) {
        let connections = Connections::default();
        (
            Self {
                listener,
                connections: connections.clone(),
            },
            connections,
        )
    }
}

impl Listener for ClosableListener {
    type Io = ClosableStream<TcpStream>;
    type Addr = SocketAddr;

    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        let (stream, peer) = Listener::accept(&mut self.listener).await;
        (self.connections.opened(peer, stream), peer)
    }

    fn local_addr(&self) -> io::Result<Self::Addr> {
        self.listener.local_addr()
    }
}

/// A stream whose reads, writes, and flushes fail once it is closed, even
/// those already waiting.
pub(crate) struct ClosableStream<S> {
    stream: S,
    closed: CancellationToken,
    // One for each direction, so a read and a write from different tasks both
    // wake. Reads and writes register only when the stream itself would wait;
    // a flush registers every time, as `poll_flush` explains.
    read_closed: Pin<Box<WaitForCancellationFutureOwned>>,
    write_closed: Pin<Box<WaitForCancellationFutureOwned>>,
    _open: OpenConnection,
}

impl<S> ClosableStream<S> {
    fn new(stream: S, closed: &CancellationToken, open: OpenConnection) -> Self {
        Self {
            stream,
            closed: closed.clone(),
            read_closed: Box::pin(closed.clone().cancelled_owned()),
            write_closed: Box::pin(closed.clone().cancelled_owned()),
            _open: open,
        }
    }
}

fn closed_error() -> io::Error {
    io::Error::new(
        io::ErrorKind::ConnectionAborted,
        "shutdown closed the connection",
    )
}

/// `result`, or the closed error if `closed` completes while it is pending.
fn unless_closed<T>(
    result: Poll<io::Result<T>>,
    closed: &mut Pin<Box<WaitForCancellationFutureOwned>>,
    cx: &mut Context<'_>,
) -> Poll<io::Result<T>> {
    match result {
        Poll::Pending if closed.as_mut().poll(cx).is_ready() => Poll::Ready(Err(closed_error())),
        result => result,
    }
}

impl<S: AsyncRead + Unpin> AsyncRead for ClosableStream<S> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        if self.closed.is_cancelled() {
            return Poll::Ready(Err(closed_error()));
        }
        let this = &mut *self;
        let result = Pin::new(&mut this.stream).poll_read(cx, buf);
        unless_closed(result, &mut this.read_closed, cx)
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for ClosableStream<S> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        if self.closed.is_cancelled() {
            return Poll::Ready(Err(closed_error()));
        }
        let this = &mut *self;
        let result = Pin::new(&mut this.stream).poll_write(cx, buf);
        unless_closed(result, &mut this.write_closed, cx)
    }

    fn poll_write_vectored(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[io::IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        if self.closed.is_cancelled() {
            return Poll::Ready(Err(closed_error()));
        }
        let this = &mut *self;
        let result = Pin::new(&mut this.stream).poll_write_vectored(cx, bufs);
        unless_closed(result, &mut this.write_closed, cx)
    }

    fn is_write_vectored(&self) -> bool {
        self.stream.is_write_vectored()
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        if self.closed.is_cancelled() {
            return Poll::Ready(Err(closed_error()));
        }
        let this = &mut *self;
        let result = Pin::new(&mut this.stream).poll_flush(cx);
        // While a handler runs, hyper may poll only this, which a socket
        // answers at once, so the wait registers whatever the answer.
        if this.write_closed.as_mut().poll(cx).is_ready() {
            return Poll::Ready(Err(closed_error()));
        }
        result
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.stream).poll_shutdown(cx)
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream};

    use super::*;

    const PEER: &str = "192.0.2.1:1000";

    fn closable() -> (ClosableStream<DuplexStream>, DuplexStream, Connections) {
        let connections = Connections::default();
        let peer = PEER.parse().unwrap();
        connections.open.lock().unwrap().insert(peer);
        let (ours, theirs) = tokio::io::duplex(16);
        let stream = ClosableStream::new(
            ours,
            &connections.closed,
            OpenConnection {
                peer,
                open: Arc::clone(&connections.open),
            },
        );
        (stream, theirs, connections)
    }

    /// Awaits `future`, failing if closing did not end its wait.
    async fn ended<T>(future: impl Future<Output = T>) -> T {
        tokio::time::timeout(Duration::from_secs(10), future)
            .await
            .expect("closing ends the wait")
    }

    #[tokio::test]
    async fn closing_ends_a_write_with_no_room() {
        let (mut stream, _peer, connections) = closable();
        stream.write_all(&[7; 16]).await.unwrap();
        let writing = tokio::spawn(async move { stream.write_all(&[7; 16]).await });
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!writing.is_finished(), "the write waits for room");

        connections.close();
        let error = ended(writing).await.unwrap().unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::ConnectionAborted);
    }

    #[tokio::test]
    async fn closing_ends_a_read_with_no_data() {
        let (mut stream, _peer, connections) = closable();
        let reading = tokio::spawn(async move { stream.read(&mut [0; 16]).await });
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!reading.is_finished(), "the read waits for data");

        connections.close();
        let error = ended(reading).await.unwrap().unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::ConnectionAborted);
    }

    /// A task that polled only its flush, as hyper's does while a handler
    /// runs, is woken too.
    #[tokio::test]
    async fn closing_wakes_a_task_that_only_flushed() {
        let (mut stream, _peer, connections) = closable();
        let waiting = tokio::spawn(async move {
            std::future::poll_fn(|cx| match Pin::new(&mut stream).poll_flush(cx) {
                Poll::Ready(Ok(())) => Poll::Pending,
                other => other,
            })
            .await
        });
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!waiting.is_finished());

        connections.close();
        let error = ended(waiting).await.unwrap().unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::ConnectionAborted);
    }

    #[tokio::test]
    async fn an_open_stream_passes_bytes_both_ways() {
        let (mut stream, mut peer, _connections) = closable();
        stream.write_all(b"out").await.unwrap();
        stream.flush().await.unwrap();
        let mut out = [0; 3];
        peer.read_exact(&mut out).await.unwrap();
        assert_eq!(&out, b"out");
        peer.write_all(b"in").await.unwrap();
        let mut back = [0; 2];
        stream.read_exact(&mut back).await.unwrap();
        assert_eq!(&back, b"in");
    }

    #[tokio::test]
    async fn operations_after_closing_fail() {
        let (mut stream, _peer, connections) = closable();
        connections.close();
        assert!(stream.write_all(b"late").await.is_err());
        assert!(stream.flush().await.is_err());
        assert!(stream.read(&mut [0; 4]).await.is_err());
    }

    #[tokio::test]
    async fn connections_count_until_they_are_dropped() {
        let (stream, _peer, connections) = closable();
        assert_eq!(connections.open(), (1, vec![PEER.parse().unwrap()]));
        drop(stream);
        assert_eq!(connections.open(), (0, Vec::new()));
    }

    #[tokio::test]
    async fn handlers_count_until_they_end_or_are_canceled() {
        let connections = Connections::default();
        connections.handlers_finished().await;
        let first = connections.handler_began();
        let second = connections.handler_began();
        assert_eq!(connections.running_handlers(), 2);
        let waiting = tokio::spawn({
            let connections = connections.clone();
            async move { connections.handlers_finished().await }
        });
        drop(first);
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!waiting.is_finished(), "a handler is still running");
        drop(second);
        ended(waiting).await.unwrap();
    }

    #[test]
    fn a_report_gives_a_bounded_number_of_peers() {
        let connections = Connections::default();
        let mut open = connections.open.lock().unwrap();
        for port in 0..=REPORTED_PEER_LIMIT as u16 {
            open.insert(SocketAddr::from(([192, 0, 2, 1], port)));
        }
        drop(open);
        let (count, peers) = connections.open();
        assert_eq!(count, REPORTED_PEER_LIMIT + 1);
        assert_eq!(peers.len(), REPORTED_PEER_LIMIT);
    }
}
