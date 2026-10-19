// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! The global piece cache: one cache capacity shared by every torrent in
//! memory storage mode, and an accountant that counts cache usage as
//! `docs/adr/0005-storage.md` defines it.
//!
//! A piece's buffer is reserved in full when its first block arrives. A
//! reservation that would exceed capacity fails, and the memory storage makes
//! room by evicting candidates the cache picks through the engine's
//! completion invalidation, which removes each piece from the cache as it
//! marks it missing. Only verified pieces are evicted, so partial pieces stay
//! resident until verified. Pinning, reservation before a piece is requested,
//! and retained reusable buffers come with later experiments.

use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard};

/// Identifies one torrent's pieces in the cache.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TorrentKey(u64);

impl fmt::Display for TorrentKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Failures of cache operations.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum CacheError {
    /// Reserving a piece would exceed cache capacity.
    #[error(
        "reserving {needed_bytes} bytes would exceed the cache capacity of {capacity_bytes} bytes \
         with {usage_bytes} in use"
    )]
    CapacityExceeded {
        /// The size of the reservation.
        needed_bytes: u64,
        /// Cache usage before the reservation.
        usage_bytes: u64,
        /// The cache capacity.
        capacity_bytes: u64,
    },
    /// The piece has no buffer in the cache.
    #[error("piece {piece} of torrent {torrent} is not in the cache")]
    PieceMissing {
        /// The torrent.
        torrent: TorrentKey,
        /// The piece index.
        piece: u32,
    },
    /// A read or write falls outside the piece.
    #[error("bytes {offset}..{end} are outside piece {piece} of {length} bytes")]
    OutOfBounds {
        /// The piece index.
        piece: u32,
        /// The start of the range within the piece.
        offset: usize,
        /// The end of the range within the piece.
        end: usize,
        /// The piece's length.
        length: usize,
    },
    /// A write disagrees with the length of a piece already reserved.
    #[error("piece {piece} was reserved with {reserved} bytes, not {requested}")]
    LengthMismatch {
        /// The piece index.
        piece: u32,
        /// The reserved length.
        reserved: usize,
        /// The length the write assumed.
        requested: usize,
    },
    /// The running total disagrees with the buffers it counts.
    #[error("cache usage is {counted} bytes, but resident buffers hold {resident}")]
    AccountingMismatch {
        /// The running total.
        counted: u64,
        /// The sum of resident buffer lengths.
        resident: u64,
    },
    /// Cache usage has exceeded capacity at some point.
    #[error("cache usage peaked at {peak_bytes} bytes, above the capacity of {capacity_bytes}")]
    CapacityViolated {
        /// The highest cache usage seen.
        peak_bytes: u64,
        /// The cache capacity.
        capacity_bytes: u64,
    },
    /// Only verified pieces can be evicted.
    #[error("piece {piece} of torrent {torrent} is not verified, so it cannot be evicted")]
    NotEvictable {
        /// The torrent.
        torrent: TorrentKey,
        /// The piece index.
        piece: u32,
    },
    /// The torrent is not registered with the cache.
    #[error("torrent {0} is not registered with the cache")]
    UnknownTorrent(TorrentKey),
    /// A thread panicked while holding the cache state.
    #[error("the cache state is unavailable after a panic")]
    Poisoned,
}

/// A snapshot of the cache's accounting.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CacheStats {
    /// The configured cache capacity.
    pub capacity_bytes: u64,
    /// The bytes counted against capacity.
    pub usage_bytes: u64,
    /// The highest cache usage since the cache was created.
    pub peak_usage_bytes: u64,
    /// Pieces with a buffer in the cache.
    pub resident_pieces: u64,
    /// Resident pieces that passed their hash check.
    pub verified_pieces: u64,
    /// Pieces evicted since the cache was created.
    pub eviction_count: u64,
}

/// Marks a torrent's pieces missing in the engine, so the cache can evict
/// them.
pub(crate) trait PieceInvalidator: Send + Sync {
    /// Performs completion invalidation for `pieces`, calling
    /// [`PieceCache::evict`] for each piece before marking it missing, and
    /// returns how many were marked. The cache calls it without holding its
    /// own lock.
    fn invalidate(&self, pieces: &[u32]) -> anyhow::Result<usize>;
}

/// Pieces of one torrent to evict, in eviction order.
pub(crate) struct EvictionBatch {
    /// Performs completion invalidation for the torrent.
    pub(crate) invalidator: Arc<dyn PieceInvalidator>,
    /// The pieces to evict.
    pub(crate) pieces: Vec<u32>,
}

struct PieceBuffer {
    bytes: Box<[u8]>,
    verified: bool,
    // The cache clock at the piece's last read or verification.
    last_use: u64,
}

struct TorrentEntry {
    // Absent until the engine can invalidate the torrent's pieces.
    invalidator: Option<Arc<dyn PieceInvalidator>>,
    started: bool,
}

#[derive(Default)]
struct State {
    usage_bytes: u64,
    peak_usage_bytes: u64,
    eviction_count: u64,
    next_key: u64,
    clock: u64,
    torrents: HashMap<TorrentKey, TorrentEntry>,
    pieces: HashMap<(TorrentKey, u32), PieceBuffer>,
}

impl State {
    fn tick(&mut self) -> u64 {
        self.clock += 1;
        self.clock
    }
}

/// The piece cache shared by every torrent in memory storage mode.
pub struct PieceCache {
    capacity_bytes: u64,
    state: Mutex<State>,
}

impl PieceCache {
    /// Creates an empty cache with the given capacity.
    pub fn new(capacity_bytes: u64) -> Arc<Self> {
        Arc::new(Self {
            capacity_bytes,
            state: Mutex::new(State::default()),
        })
    }

    fn lock(&self) -> Result<MutexGuard<'_, State>, CacheError> {
        self.state.lock().map_err(|_| CacheError::Poisoned)
    }

    /// Returns a snapshot of the accounting.
    pub fn stats(&self) -> Result<CacheStats, CacheError> {
        let state = self.lock()?;
        Ok(self.snapshot(&state))
    }

    fn snapshot(&self, state: &State) -> CacheStats {
        CacheStats {
            capacity_bytes: self.capacity_bytes,
            usage_bytes: state.usage_bytes,
            peak_usage_bytes: state.peak_usage_bytes,
            resident_pieces: state.pieces.len() as u64,
            verified_pieces: state.pieces.values().filter(|piece| piece.verified).count() as u64,
            eviction_count: state.eviction_count,
        }
    }

    /// Recomputes cache usage from the resident buffers and checks that it
    /// matches the running total and stays within capacity.
    pub fn audit(&self) -> Result<CacheStats, CacheError> {
        let state = self.lock()?;
        let stats = self.snapshot(&state);
        let resident: u64 = state
            .pieces
            .values()
            .map(|piece| piece.bytes.len() as u64)
            .sum();
        if resident != stats.usage_bytes {
            return Err(CacheError::AccountingMismatch {
                counted: stats.usage_bytes,
                resident,
            });
        }
        if stats.peak_usage_bytes > self.capacity_bytes {
            return Err(CacheError::CapacityViolated {
                peak_bytes: stats.peak_usage_bytes,
                capacity_bytes: self.capacity_bytes,
            });
        }
        Ok(stats)
    }

    pub(crate) fn register_torrent(&self) -> Result<TorrentKey, CacheError> {
        let mut state = self.lock()?;
        state.next_key += 1;
        let key = TorrentKey(state.next_key);
        state.torrents.insert(
            key,
            TorrentEntry {
                invalidator: None,
                started: true,
            },
        );
        Ok(key)
    }

    /// Makes a torrent's verified pieces eviction candidates, invalidated
    /// through `invalidator`.
    pub(crate) fn attach_invalidator(
        &self,
        torrent: TorrentKey,
        invalidator: Arc<dyn PieceInvalidator>,
    ) -> Result<(), CacheError> {
        let mut state = self.lock()?;
        let entry = state
            .torrents
            .get_mut(&torrent)
            .ok_or(CacheError::UnknownTorrent(torrent))?;
        entry.invalidator = Some(invalidator);
        Ok(())
    }

    /// Records whether a torrent is started. Pieces of paused and failed
    /// torrents are evicted before pieces of started ones.
    pub(crate) fn set_started(&self, torrent: TorrentKey, started: bool) -> Result<(), CacheError> {
        let mut state = self.lock()?;
        let entry = state
            .torrents
            .get_mut(&torrent)
            .ok_or(CacheError::UnknownTorrent(torrent))?;
        entry.started = started;
        Ok(())
    }

    /// Returns verified pieces whose eviction would free at least
    /// `needed_bytes` beyond the capacity left, grouped by torrent: pieces of
    /// paused and failed torrents first, then those of started torrents, the
    /// least recently used first within each class. It may return fewer when
    /// not enough pieces are evictable.
    pub(crate) fn eviction_candidates(
        &self,
        needed_bytes: u64,
    ) -> Result<Vec<EvictionBatch>, CacheError> {
        let state = self.lock()?;
        let free = self.capacity_bytes.saturating_sub(state.usage_bytes);
        let mut shortfall = needed_bytes.saturating_sub(free);
        let mut candidates: Vec<_> = state
            .pieces
            .iter()
            .filter(|(_, buffer)| buffer.verified)
            .filter_map(|(&(torrent, piece), buffer)| {
                let entry = state.torrents.get(&torrent)?;
                entry.invalidator.as_ref()?;
                Some((
                    entry.started,
                    buffer.last_use,
                    torrent,
                    piece,
                    buffer.bytes.len(),
                ))
            })
            .collect();
        candidates.sort_unstable();
        let mut batches: Vec<(TorrentKey, EvictionBatch)> = Vec::new();
        for (_, _, torrent, piece, length) in candidates {
            if shortfall == 0 {
                break;
            }
            shortfall = shortfall.saturating_sub(length as u64);
            match batches.iter_mut().find(|(key, _)| *key == torrent) {
                Some((_, batch)) => batch.pieces.push(piece),
                None => {
                    let Some(invalidator) = state
                        .torrents
                        .get(&torrent)
                        .and_then(|entry| entry.invalidator.clone())
                    else {
                        continue;
                    };
                    batches.push((
                        torrent,
                        EvictionBatch {
                            invalidator,
                            pieces: vec![piece],
                        },
                    ));
                }
            }
        }
        Ok(batches.into_iter().map(|(_, batch)| batch).collect())
    }

    /// Removes a verified piece and returns its bytes to the capacity. The
    /// engine calls it during completion invalidation, while readers cannot
    /// observe the piece as complete.
    pub(crate) fn evict(&self, torrent: TorrentKey, piece: u32) -> Result<(), CacheError> {
        let mut state = self.lock()?;
        match state.pieces.get(&(torrent, piece)) {
            None => return Err(CacheError::PieceMissing { torrent, piece }),
            Some(buffer) if !buffer.verified => {
                return Err(CacheError::NotEvictable { torrent, piece });
            }
            Some(_) => {}
        }
        if let Some(buffer) = state.pieces.remove(&(torrent, piece)) {
            state.usage_bytes -= buffer.bytes.len() as u64;
            state.eviction_count += 1;
        }
        Ok(())
    }

    /// Writes `data` at `offset` within a piece of `piece_length` bytes,
    /// reserving the piece's whole buffer on its first write.
    pub(crate) fn write(
        &self,
        torrent: TorrentKey,
        piece: u32,
        piece_length: usize,
        offset: usize,
        data: &[u8],
    ) -> Result<(), CacheError> {
        let end = offset + data.len();
        if end > piece_length {
            return Err(CacheError::OutOfBounds {
                piece,
                offset,
                end,
                length: piece_length,
            });
        }
        let mut state = self.lock()?;
        let state = &mut *state;
        if !state.torrents.contains_key(&torrent) {
            return Err(CacheError::UnknownTorrent(torrent));
        }
        if !state.pieces.contains_key(&(torrent, piece)) {
            let needed_bytes = piece_length as u64;
            if state.usage_bytes + needed_bytes > self.capacity_bytes {
                return Err(CacheError::CapacityExceeded {
                    needed_bytes,
                    usage_bytes: state.usage_bytes,
                    capacity_bytes: self.capacity_bytes,
                });
            }
            state.usage_bytes += needed_bytes;
            state.peak_usage_bytes = state.peak_usage_bytes.max(state.usage_bytes);
            state.pieces.insert(
                (torrent, piece),
                PieceBuffer {
                    bytes: vec![0; piece_length].into_boxed_slice(),
                    verified: false,
                    last_use: 0,
                },
            );
        }
        let buffer = state
            .pieces
            .get_mut(&(torrent, piece))
            .ok_or(CacheError::PieceMissing { torrent, piece })?;
        if buffer.bytes.len() != piece_length {
            return Err(CacheError::LengthMismatch {
                piece,
                reserved: buffer.bytes.len(),
                requested: piece_length,
            });
        }
        buffer.bytes[offset..end].copy_from_slice(data);
        Ok(())
    }

    /// Reads `buf.len()` bytes at `offset` within a resident piece.
    pub(crate) fn read(
        &self,
        torrent: TorrentKey,
        piece: u32,
        offset: usize,
        buf: &mut [u8],
    ) -> Result<(), CacheError> {
        let mut state = self.lock()?;
        let now = state.tick();
        let buffer = state
            .pieces
            .get_mut(&(torrent, piece))
            .ok_or(CacheError::PieceMissing { torrent, piece })?;
        buffer.last_use = now;
        let end = offset + buf.len();
        let bytes = buffer
            .bytes
            .get(offset..end)
            .ok_or(CacheError::OutOfBounds {
                piece,
                offset,
                end,
                length: buffer.bytes.len(),
            })?;
        buf.copy_from_slice(bytes);
        Ok(())
    }

    /// Records that a resident piece passed its hash check.
    pub(crate) fn mark_verified(&self, torrent: TorrentKey, piece: u32) -> Result<(), CacheError> {
        let mut state = self.lock()?;
        let now = state.tick();
        let buffer = state
            .pieces
            .get_mut(&(torrent, piece))
            .ok_or(CacheError::PieceMissing { torrent, piece })?;
        buffer.verified = true;
        buffer.last_use = now;
        Ok(())
    }

    /// Releases every piece of a torrent and returns their bytes to the
    /// capacity.
    pub(crate) fn release_torrent(&self, torrent: TorrentKey) -> Result<(), CacheError> {
        let mut state = self.lock()?;
        state.torrents.remove(&torrent);
        let mut released = 0;
        state.pieces.retain(|(key, _), piece| {
            if *key == torrent {
                released += piece.bytes.len() as u64;
                false
            } else {
                true
            }
        });
        state.usage_bytes -= released;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct NoEngine;

    impl PieceInvalidator for NoEngine {
        fn invalidate(&self, _pieces: &[u32]) -> anyhow::Result<usize> {
            Ok(0)
        }
    }

    fn evictable(cache: &PieceCache) -> TorrentKey {
        let torrent = cache.register_torrent().unwrap();
        cache
            .attach_invalidator(torrent, Arc::new(NoEngine))
            .unwrap();
        torrent
    }

    fn verified(cache: &PieceCache, torrent: TorrentKey, piece: u32, length: usize) {
        cache.write(torrent, piece, length, 0, &[0]).unwrap();
        cache.mark_verified(torrent, piece).unwrap();
    }

    fn candidates(cache: &PieceCache, needed_bytes: u64) -> Vec<Vec<u32>> {
        cache
            .eviction_candidates(needed_bytes)
            .unwrap()
            .into_iter()
            .map(|batch| batch.pieces)
            .collect()
    }

    #[test]
    fn eviction_takes_paused_torrents_first_then_the_least_recently_used() {
        let cache = PieceCache::new(100);
        let started = evictable(&cache);
        let paused = evictable(&cache);
        cache.set_started(paused, false).unwrap();
        verified(&cache, started, 0, 20);
        verified(&cache, started, 1, 20);
        verified(&cache, paused, 0, 20);
        verified(&cache, paused, 1, 20);
        let mut buf = [0];
        cache.read(started, 0, 0, &mut buf).unwrap();
        cache.read(paused, 0, 0, &mut buf).unwrap();

        assert_eq!(
            candidates(&cache, 20),
            Vec::<Vec<u32>>::new(),
            "20 bytes are free"
        );
        assert_eq!(
            candidates(&cache, 40),
            vec![vec![1]],
            "the paused torrent's older piece"
        );
        assert_eq!(
            candidates(&cache, 100),
            vec![vec![1, 0], vec![1, 0]],
            "the paused torrent's pieces, then the started torrent's, oldest first"
        );
    }

    #[test]
    fn only_verified_pieces_of_attached_torrents_are_candidates() {
        let cache = PieceCache::new(100);
        let attached = evictable(&cache);
        let unattached = cache.register_torrent().unwrap();
        verified(&cache, unattached, 0, 30);
        cache.write(attached, 0, 30, 0, &[0]).unwrap();
        verified(&cache, attached, 1, 30);

        assert_eq!(candidates(&cache, 100), vec![vec![1]]);
        assert_eq!(
            cache.evict(attached, 0),
            Err(CacheError::NotEvictable {
                torrent: attached,
                piece: 0
            }),
            "a partial piece stays resident"
        );
    }

    #[test]
    fn evicting_returns_the_bytes_and_counts_the_eviction() {
        let cache = PieceCache::new(100);
        let torrent = evictable(&cache);
        verified(&cache, torrent, 0, 60);
        cache.evict(torrent, 0).unwrap();
        let stats = cache.audit().unwrap();
        assert_eq!((stats.usage_bytes, stats.eviction_count), (0, 1));
        assert_eq!(
            cache.evict(torrent, 0),
            Err(CacheError::PieceMissing { torrent, piece: 0 })
        );
        cache.write(torrent, 0, 60, 0, &[1]).unwrap();
        cache.write(torrent, 1, 40, 0, &[1]).unwrap();
        assert_eq!(
            cache.audit().unwrap().usage_bytes,
            100,
            "the room is reusable"
        );
    }

    #[test]
    fn a_first_write_reserves_the_whole_piece() {
        let cache = PieceCache::new(100);
        let torrent = cache.register_torrent().unwrap();
        cache.write(torrent, 0, 40, 10, &[1, 2, 3]).unwrap();
        let stats = cache.audit().unwrap();
        assert_eq!(
            stats.usage_bytes, 40,
            "a partial piece counts its whole buffer"
        );
        assert_eq!((stats.resident_pieces, stats.verified_pieces), (1, 0));
        cache.write(torrent, 0, 40, 13, &[4]).unwrap();
        assert_eq!(
            cache.audit().unwrap().usage_bytes,
            40,
            "later writes reuse the buffer"
        );
    }

    #[test]
    fn reads_return_written_bytes_and_fail_for_missing_pieces() {
        let cache = PieceCache::new(100);
        let torrent = cache.register_torrent().unwrap();
        cache.write(torrent, 2, 8, 0, &[9; 8]).unwrap();
        let mut buf = [0; 4];
        cache.read(torrent, 2, 4, &mut buf).unwrap();
        assert_eq!(buf, [9; 4]);
        assert_eq!(
            cache.read(torrent, 3, 0, &mut buf),
            Err(CacheError::PieceMissing { torrent, piece: 3 })
        );
        assert!(matches!(
            cache.read(torrent, 2, 6, &mut buf),
            Err(CacheError::OutOfBounds { .. })
        ));
    }

    #[test]
    fn a_reservation_beyond_capacity_fails_without_counting() {
        let cache = PieceCache::new(100);
        let torrent = cache.register_torrent().unwrap();
        cache.write(torrent, 0, 60, 0, &[0]).unwrap();
        assert_eq!(
            cache.write(torrent, 1, 60, 0, &[0]),
            Err(CacheError::CapacityExceeded {
                needed_bytes: 60,
                usage_bytes: 60,
                capacity_bytes: 100,
            })
        );
        let stats = cache.audit().unwrap();
        assert_eq!((stats.usage_bytes, stats.peak_usage_bytes), (60, 60));
    }

    #[test]
    fn releasing_a_torrent_returns_only_its_bytes() {
        let cache = PieceCache::new(100);
        let first = cache.register_torrent().unwrap();
        let second = cache.register_torrent().unwrap();
        cache.write(first, 0, 30, 0, &[1]).unwrap();
        cache.write(second, 0, 20, 0, &[2]).unwrap();
        cache.mark_verified(first, 0).unwrap();
        cache.release_torrent(first).unwrap();
        let stats = cache.audit().unwrap();
        assert_eq!(
            (
                stats.usage_bytes,
                stats.resident_pieces,
                stats.verified_pieces
            ),
            (20, 1, 0)
        );
        assert_eq!(stats.peak_usage_bytes, 50);
    }

    #[test]
    fn writes_past_the_piece_or_with_another_length_fail() {
        let cache = PieceCache::new(100);
        let torrent = cache.register_torrent().unwrap();
        assert!(matches!(
            cache.write(torrent, 0, 8, 6, &[0; 4]),
            Err(CacheError::OutOfBounds { .. })
        ));
        cache.write(torrent, 0, 8, 0, &[0]).unwrap();
        assert!(matches!(
            cache.write(torrent, 0, 16, 0, &[0]),
            Err(CacheError::LengthMismatch { .. })
        ));
    }
}
