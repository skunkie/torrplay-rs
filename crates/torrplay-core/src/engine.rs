// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! The torrent engine facade the server talks to.
//!
//! [`Engine`] hides the torrent engine behind operations on torrent IDs,
//! file indexes, and byte ranges, so the server never sees an engine handle
//! or type. The librqbit adapter implements it in `torrplay-engine`; the
//! `fake` feature provides a scripted implementation for tests.
//!
//! The server registers a torrent in the [`Registry`](crate::registry::Registry)
//! before adding it to the engine, keeps desired state in the registry's
//! configuration, and derives observed state from the configuration and
//! [`TorrentStatus`]. Engine methods therefore take IDs the registry issued
//! and never decide whether a torrent exists from the client's point of
//! view.

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use tokio::io::AsyncRead;

use crate::id::{InfoHashV1, TorrentId};
use crate::lifecycle::{DesiredState, Failure, Readiness};
use crate::metadata::{FileIndex, ReportedMetadata};

/// The input a torrent is created from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    /// The bytes of an uploaded torrent file.
    TorrentFile(Vec<u8>),
    /// A magnet URI.
    MagnetUri(String),
}

impl Source {
    /// Returns the JSON Pointer of the request member that carries the
    /// source, for the `errors` entry of a `validation-failed` problem.
    pub fn pointer(&self) -> &'static str {
        match self {
            Self::TorrentFile(_) => "/torrent_file",
            Self::MagnetUri(_) => "/magnet_uri",
        }
    }
}

/// What the engine learned from a source without adding it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Inspection {
    /// The v1 info hash.
    pub info_hash: InfoHashV1,
    /// The metadata of a torrent file; `None` for a magnet URI.
    pub metadata: Option<Arc<ReportedMetadata>>,
}

/// A source the engine cannot use.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum SourceRejection {
    /// The source is not valid torrent metainfo or a parsable magnet URI: a
    /// `validation-failed` problem whose error entry has the source's
    /// [`pointer`](Source::pointer) and this detail.
    #[error("{detail}")]
    Invalid {
        /// What is wrong with the value, phrased for an error entry, such as
        /// "Must be a valid torrent file."
        detail: String,
    },
    /// The source is a valid torrent the engine cannot handle, such as a
    /// v2-only torrent: an `unsupported-torrent` problem.
    #[error("{detail}")]
    Unsupported {
        /// Why the torrent is unsupported.
        detail: String,
    },
}

/// An engine operation that failed.
#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    /// The engine is not ready: `engine-unavailable`.
    #[error("the torrent engine is unavailable")]
    Unavailable {
        /// When a retry may succeed, if known.
        retry_after: Option<Duration>,
    },
    /// The source cannot be used.
    #[error(transparent)]
    Rejected(#[from] SourceRejection),
    /// The engine has no torrent with the ID.
    #[error("the engine has no torrent with this ID")]
    UnknownTorrent,
    /// The engine already has a torrent with the ID.
    #[error("the engine already has a torrent with this ID")]
    DuplicateTorrent,
    /// The supplied metadata belongs to another info hash.
    #[error("the metadata does not match the torrent's info hash")]
    MetadataMismatch,
    /// The torrent's metadata is not available yet.
    #[error("the torrent's metadata is not available")]
    MetadataUnavailable,
    /// The torrent is paused or failed, so its content cannot be read.
    #[error("the torrent is not started")]
    NotStarted,
    /// The torrent's metadata has no file at the index.
    #[error("the torrent has no file at this index")]
    UnknownFile,
    /// The byte range does not lie within the file.
    #[error("the byte range lies outside the file")]
    RangeOutOfBounds,
    /// An unexpected engine failure: `internal-error`.
    #[error("the torrent engine failed")]
    Internal(#[source] Box<dyn std::error::Error + Send + Sync>),
}

/// The engine's view of a torrent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TorrentStatus {
    /// The metadata, once uploaded or retrieved. The server validates it
    /// with [`Metadata::identify`](crate::metadata::Metadata::identify).
    pub metadata: Option<Arc<ReportedMetadata>>,
    /// Why the torrent failed, if it did.
    pub failure: Option<Failure>,
}

/// A half-open range of byte offsets within a file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ByteRange {
    start: u64,
    end: u64,
}

impl ByteRange {
    /// Returns the range from `start` up to, but excluding, `end`, or `None`
    /// if `start` exceeds `end`.
    pub fn new(start: u64, end: u64) -> Option<Self> {
        (start <= end).then_some(Self { start, end })
    }

    /// Returns the first offset.
    pub fn start(self) -> u64 {
        self.start
    }

    /// Returns the offset after the last byte.
    pub fn end(self) -> u64 {
        self.end
    }

    /// Returns the number of bytes.
    pub fn len_bytes(self) -> u64 {
        self.end - self.start
    }

    /// Returns whether the range has no bytes.
    pub fn is_empty(self) -> bool {
        self.start == self.end
    }
}

/// A fraction from 0.0 to 1.0 inclusive.
#[derive(Clone, Copy, Debug, PartialEq, PartialOrd)]
pub struct Ratio(f64);

impl Ratio {
    /// Returns the ratio, or `None` outside 0.0 to 1.0 or for NaN.
    pub fn new(value: f64) -> Option<Self> {
        (0.0..=1.0).contains(&value).then_some(Self(value))
    }

    /// Returns the value.
    pub fn get(self) -> f64 {
        self.0
    }
}

/// Transfer and peer measurements for a torrent, the contract's
/// `TorrentStatistics` without links.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct TorrentStatistics {
    /// Bytes downloaded from peers since the torrent was created.
    pub downloaded_bytes: u64,
    /// Bytes uploaded to peers since the torrent was created; 0 in memory
    /// storage mode.
    pub uploaded_bytes: u64,
    /// The current download rate.
    pub download_rate_bytes_per_second: u64,
    /// The current upload rate; 0 in memory storage mode.
    pub upload_rate_bytes_per_second: u64,
    /// The number of connected peers.
    pub peer_count: u64,
    /// The fraction of the torrent's pieces currently held, or `None` until
    /// metadata is available.
    pub completion_ratio: Option<Ratio>,
}

/// The global piece cache's capacity and usage, the contract's
/// `CacheStatistics` without links.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CacheStatistics {
    /// The configured cache capacity.
    pub capacity_bytes: u64,
    /// The bytes counted against capacity, including pending allocations
    /// and retained reusable buffers.
    pub usage_bytes: u64,
    /// The full length of each resident piece covered by at least one
    /// pinned range, counted once.
    pub pinned_bytes: u64,
    /// The number of pieces evicted since the process started.
    pub eviction_count: u64,
}

/// The torrent engine facade.
///
/// Every asynchronous method returns [`EngineError::Unavailable`] while
/// [`readiness`](Self::readiness) is not [`Readiness::Ready`]. Methods that
/// take a torrent ID return [`EngineError::UnknownTorrent`] for an ID the
/// engine does not hold. Implementations bound their waits, keep blocking
/// work off the async runtime, and never block a caller on another torrent's
/// operation.
pub trait Engine: Send + Sync + 'static {
    /// A byte-range reader over a file's content.
    type Reader: AsyncRead + Send + Unpin + 'static;

    /// Returns whether the engine can serve requests. It answers from state
    /// the engine already holds and never waits.
    fn readiness(&self) -> Readiness;

    /// Inspects a source without adding a torrent, contacting peers, or
    /// prefetching. Returns the v1 info hash and, for a torrent file, its
    /// metadata as the engine would report it, or
    /// [`EngineError::Rejected`]: [`SourceRejection::Invalid`] for input
    /// that is not valid metainfo or a parsable magnet URI, and
    /// [`SourceRejection::Unsupported`] for a v2-only torrent. A hybrid
    /// torrent is accepted as a v1 torrent.
    fn inspect(
        &self,
        source: &Source,
    ) -> impl Future<Output = Result<Inspection, EngineError>> + Send;

    /// Adds a torrent under `id`, which the registry issued, from a source
    /// [`inspect`](Self::inspect) accepted. A torrent file's metadata is used
    /// immediately; a magnet URI starts metadata retrieval while the torrent
    /// is started. Returns once the engine holds the torrent, without waiting
    /// for peers. Returns [`EngineError::DuplicateTorrent`] if `id` is taken.
    fn add(
        &self,
        id: &TorrentId,
        source: &Source,
        desired_state: DesiredState,
    ) -> impl Future<Output = Result<(), EngineError>> + Send;

    /// Gives a torrent without metadata the metadata of an uploaded torrent
    /// file with the same info hash, ending metadata retrieval without
    /// dropping its peers. Does nothing if the torrent already has metadata.
    /// Returns [`EngineError::MetadataMismatch`] for another info hash and
    /// [`EngineError::Rejected`] for bytes `inspect` would reject.
    fn supply_metadata(
        &self,
        id: &TorrentId,
        torrent_file: &[u8],
    ) -> impl Future<Output = Result<(), EngineError>> + Send;

    /// Applies a desired state and returns once the engine accepted it; the
    /// status follows when the change is applied. Pausing closes peer
    /// connections, stops metadata retrieval, and releases the torrent's
    /// readers. Starting a failed torrent that was paused since it failed
    /// clears its failure and restarts it. Setting the current value again
    /// does nothing.
    fn set_desired_state(
        &self,
        id: &TorrentId,
        desired_state: DesiredState,
    ) -> impl Future<Output = Result<(), EngineError>> + Send;

    /// Removes a torrent: cancels its readers, stops metadata retrieval,
    /// closes its peer connections, and releases its cache usage, partial
    /// and verified pieces alike, without counting evictions. The release
    /// happens before this returns, not when the engine's own tasks end, as
    /// ADR 5 requires.
    fn remove(&self, id: &TorrentId) -> impl Future<Output = Result<(), EngineError>> + Send;

    /// Returns a torrent's metadata, once available, and failure.
    fn status(
        &self,
        id: &TorrentId,
    ) -> impl Future<Output = Result<TorrentStatus, EngineError>> + Send;

    /// Opens a reader for `range` of the file at `file` in the engine's file
    /// list. The reader yields exactly the range's bytes, waiting for each
    /// missing piece for at most the piece wait bound and then failing with
    /// an I/O error; dropping it releases its pins and piece priorities.
    /// Returns [`EngineError::MetadataUnavailable`] without metadata,
    /// [`EngineError::NotStarted`] for a paused or failed torrent,
    /// [`EngineError::UnknownFile`] for an index outside the file list, and
    /// [`EngineError::RangeOutOfBounds`] for a range past the file's end.
    fn open_reader(
        &self,
        id: &TorrentId,
        file: FileIndex,
        range: ByteRange,
    ) -> impl Future<Output = Result<Self::Reader, EngineError>> + Send;

    /// Returns a torrent's transfer and peer statistics.
    fn torrent_statistics(
        &self,
        id: &TorrentId,
    ) -> impl Future<Output = Result<TorrentStatistics, EngineError>> + Send;

    /// Returns the piece cache's statistics.
    fn cache_statistics(&self)
    -> impl Future<Output = Result<CacheStatistics, EngineError>> + Send;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn byte_ranges_are_half_open() {
        let range = ByteRange::new(10, 15).unwrap();
        assert_eq!(range.len_bytes(), 5);
        assert!(!range.is_empty());
        assert!(ByteRange::new(7, 7).unwrap().is_empty());
        assert_eq!(ByteRange::new(8, 7), None);
    }

    #[test]
    fn ratios_lie_between_zero_and_one() {
        for valid in [0.0, 0.5, 1.0] {
            assert_eq!(Ratio::new(valid).map(Ratio::get), Some(valid));
        }
        for invalid in [-0.1, 1.000_001, f64::NAN, f64::INFINITY] {
            assert_eq!(Ratio::new(invalid), None, "{invalid}");
        }
    }

    #[test]
    fn sources_name_their_request_member() {
        assert_eq!(Source::TorrentFile(Vec::new()).pointer(), "/torrent_file");
        assert_eq!(Source::MagnetUri(String::new()).pointer(), "/magnet_uri");
    }
}
