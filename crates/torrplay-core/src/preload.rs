// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! Preloads, as the lifecycle's Preloads section defines them: what a
//! client asks for, the limits that apply, and what the engine reports.

use std::time::{Duration, SystemTime};

use crate::engine::ByteRange;
use crate::id::PreloadId;
use crate::lifecycle::Failure;
use crate::metadata::FileIndex;

/// The most a preload covers.
pub const MAX_PRELOAD_BYTES: u64 = 32 << 20;

/// The least of a file's end a head-and-tail preload covers; at least one
/// piece.
pub const TAIL_BYTES: u64 = 4 << 20;

/// The time to live of a preload whose request sets none.
pub const DEFAULT_TTL: Duration = Duration::from_secs(120);

/// The shortest time to live a request may set.
pub const MIN_TTL: Duration = Duration::from_secs(10);

/// The longest time to live a request may set.
pub const MAX_TTL: Duration = Duration::from_secs(3600);

/// How long an expired or failed preload stays readable.
pub const RETENTION: Duration = Duration::from_secs(300);

/// The most active preloads one file may have.
pub const PER_FILE_LIMIT: usize = 4;

/// The most active preloads all files may have together.
pub const TOTAL_LIMIT: usize = 16;

/// The most preloads one file lists, active and ended together; the oldest
/// ended ones go first.
pub const LISTED_LIMIT: usize = 16;

/// Where a preload is in its lifecycle.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PreloadState {
    /// Waiting for room in the preloads' share of the readahead budget.
    Queued,
    /// Fetching the pieces of its ranges.
    Downloading,
    /// Holding every piece of its ranges.
    Ready,
    /// Ended at its expiry time.
    Expired,
    /// Ended by a failure, which says why.
    Failed,
}

impl PreloadState {
    /// Returns the contract's enum value.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Downloading => "downloading",
            Self::Ready => "ready",
            Self::Expired => "expired",
            Self::Failed => "failed",
        }
    }

    /// Returns whether the preload ended, releasing its pins and piece
    /// priorities. Only active preloads count toward the limits.
    pub const fn is_ended(self) -> bool {
        matches!(self, Self::Expired | Self::Failed)
    }
}

/// What a client asks a preload to cover.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PreloadRequest {
    /// The offset in the file where playback resumes, for a window there,
    /// or `None` for the file's head and tail. It lies within the file.
    pub offset_bytes: Option<u64>,
    /// How long the preload lasts from its creation, within
    /// [`MIN_TTL`]..=[`MAX_TTL`].
    pub ttl: Duration,
}

/// The engine's view of a preload.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreloadStatus {
    /// The preload's ID.
    pub id: PreloadId,
    /// The file it preloads.
    pub file: FileIndex,
    /// Where it is in its lifecycle.
    pub state: PreloadState,
    /// The offset its request set, if any.
    pub offset_bytes: Option<u64>,
    /// The file's byte ranges it covers, in whole pieces clipped to the
    /// file, in file order and disjoint.
    pub ranges: Vec<ByteRange>,
    /// The bytes of its ranges together.
    pub target_bytes: u64,
    /// The bytes of its ranges the torrent's storage holds now.
    pub completed_bytes: u64,
    /// When it was created.
    pub created_at: SystemTime,
    /// When it expires, or expired.
    pub expires_at: SystemTime,
    /// When it expired or failed.
    pub ended_at: Option<SystemTime>,
    /// Why it failed, if it did.
    pub failure: Option<Failure>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_expired_and_failed_preloads_have_ended() {
        let ended: Vec<_> = [
            PreloadState::Queued,
            PreloadState::Downloading,
            PreloadState::Ready,
            PreloadState::Expired,
            PreloadState::Failed,
        ]
        .into_iter()
        .filter(|state| state.is_ended())
        .map(PreloadState::as_str)
        .collect();
        assert_eq!(ended, ["expired", "failed"]);
    }

    #[test]
    fn the_default_ttl_lies_within_its_bounds() {
        assert!((MIN_TTL..=MAX_TTL).contains(&DEFAULT_TTL));
    }
}
