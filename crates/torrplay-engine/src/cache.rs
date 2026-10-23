// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! The global piece cache: one cache capacity shared by every torrent in
//! memory storage mode, and an accountant that counts cache usage as
//! `docs/adr/0005-storage.md` defines it.
//!
//! The engine reserves a piece's full length before it requests the piece,
//! as a pending allocation that becomes the piece buffer on the first
//! write. A reservation that does not fit waits: the [`Evictor`] task evicts
//! verified, unpinned pieces through the engine's completion invalidation,
//! which removes each piece from the cache as it marks it missing, and then
//! readmits waiting pieces in piece priority order across torrents, and in
//! the order they started waiting among equals. While any piece waits, a new
//! reservation joins the queue instead of taking room ahead of it. A write
//! to a piece nobody reserved still reserves it on the spot and evicts from
//! the write path. Partial pieces are never evicted; one that goes back to
//! the engine's queue unverified is released.
//!
//! The cache lock guards only the accounting. Piece buffers have locks of
//! their own: buffers are allocated, and bytes copied in and out, with the
//! cache lock released. A buffer freed while nothing waits for room is
//! retained for reuse, up to [`MAX_RETAINED_BUFFERS`], and counts in cache
//! usage until reused or released to make room.
//!
//! [`Evictor`]: crate::evictor::Evictor

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard};

use tokio::sync::Notify;

/// The most freed piece buffers the cache retains for reuse, as ADR 5's
/// milestone 4 defaults fix it.
pub const MAX_RETAINED_BUFFERS: usize = 4;

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
    /// The bytes counted against capacity: pending allocations, resident
    /// piece buffers, and retained reusable buffers.
    pub usage_bytes: u64,
    /// The highest cache usage since the cache was created.
    pub peak_usage_bytes: u64,
    /// Pieces with a buffer in the cache.
    pub resident_pieces: u64,
    /// Pieces reserved before their first write.
    pub pending_pieces: u64,
    /// Resident pieces that passed their hash check.
    pub verified_pieces: u64,
    /// The full length of every resident piece some reader pins, each once.
    pub pinned_bytes: u64,
    /// The bytes of freed buffers retained for reuse.
    pub retained_bytes: u64,
    /// Pieces evicted since the cache was created.
    pub eviction_count: u64,
    /// Reservations waiting for room.
    pub waiting_pieces: u64,
}

/// The outcome of reserving room for a piece before requesting it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reservation {
    /// The piece has room: reserved now or earlier.
    Reserved,
    /// The cache is full, or other pieces wait ahead of this one; the
    /// evictor makes room and wakes the torrent.
    Waiting,
    /// The piece is longer than the cache capacity and can never be stored.
    TooLarge,
}

/// The engine side of one torrent, which the cache calls without holding its
/// own lock.
pub(crate) trait EngineTorrent: Send + Sync {
    /// Performs completion invalidation for `pieces`, calling
    /// [`PieceCache::evict`] for each piece before marking it missing, and
    /// returns how many were marked.
    fn invalidate(&self, pieces: &[u32]) -> anyhow::Result<usize>;

    /// Asks the torrent to select pieces to request again, because room was
    /// made for a reservation that waited.
    fn reselect(&self);
}

/// Pieces of one torrent to evict, in eviction order.
pub(crate) struct EvictionBatch {
    /// Performs completion invalidation for the torrent.
    pub(crate) engine: Arc<dyn EngineTorrent>,
    /// The pieces to evict.
    pub(crate) pieces: Vec<u32>,
}

/// A piece buffer, locked on its own so that copies never hold the cache
/// lock.
type Buffer = Arc<Mutex<Box<[u8]>>>;

struct Slot {
    length: usize,
    // Absent while the piece is a pending allocation.
    bytes: Option<Buffer>,
    verified: bool,
    // The cache clock at the piece's last read or verification.
    last_use: u64,
}

struct TorrentEntry {
    // Absent until the engine can invalidate the torrent's pieces.
    engine: Option<Arc<dyn EngineTorrent>>,
    started: bool,
    pins: HashSet<u32>,
    // Each planned piece's place in the torrent's priority order, 0 first.
    ranks: HashMap<u32, u32>,
}

/// A reservation waiting for room.
struct Waiting {
    length: usize,
    // When the piece first started waiting, kept across readmissions.
    since: u64,
}

#[derive(Default)]
struct State {
    usage_bytes: u64,
    peak_usage_bytes: u64,
    eviction_count: u64,
    next_key: u64,
    clock: u64,
    torrents: HashMap<TorrentKey, TorrentEntry>,
    slots: HashMap<(TorrentKey, u32), Slot>,
    // Reservations that found no room.
    waiting: HashMap<(TorrentKey, u32), Waiting>,
    // Pieces readmitted from the queue, with the time they started waiting,
    // until the torrent reserves them again.
    admitted: HashMap<(TorrentKey, u32), u64>,
    // Freed buffers kept for reuse, counted in usage.
    retained: Vec<Box<[u8]>>,
}

impl State {
    fn tick(&mut self) -> u64 {
        self.clock += 1;
        self.clock
    }

    fn is_pinned(&self, torrent: TorrentKey, piece: u32) -> bool {
        self.torrents
            .get(&torrent)
            .is_some_and(|entry| entry.pins.contains(&piece))
    }

    fn add(&mut self, length: usize) {
        self.usage_bytes += length as u64;
        self.peak_usage_bytes = self.peak_usage_bytes.max(self.usage_bytes);
    }

    fn retained_bytes(&self) -> u64 {
        self.retained.iter().map(|buffer| buffer.len() as u64).sum()
    }

    /// Releases retained buffers until `needed_bytes` fit, and returns them
    /// for the caller to drop after unlocking.
    fn release_retained(&mut self, needed_bytes: u64, capacity_bytes: u64) -> Vec<Box<[u8]>> {
        let mut released = Vec::new();
        while self.usage_bytes + needed_bytes > capacity_bytes {
            let Some(buffer) = self.retained.pop() else {
                break;
            };
            self.usage_bytes -= buffer.len() as u64;
            released.push(buffer);
        }
        released
    }

    /// Takes a retained buffer of exactly `length` bytes, which leaves the
    /// usage it counted to the slot it moves into.
    fn take_retained(&mut self, length: usize) -> Option<Box<[u8]>> {
        let index = self
            .retained
            .iter()
            .position(|buffer| buffer.len() == length)?;
        let buffer = self.retained.swap_remove(index);
        self.usage_bytes -= length as u64;
        Some(buffer)
    }

    /// Removes a slot and returns its bytes to the capacity. Its buffer is
    /// retained when nothing waits for room, the pool has space, and no copy
    /// still holds it; otherwise it is returned to drop after unlocking.
    fn remove_slot(&mut self, key: (TorrentKey, u32)) -> Option<(Slot, Option<Box<[u8]>>)> {
        let mut slot = self.slots.remove(&key)?;
        self.usage_bytes -= slot.length as u64;
        let mut freed = None;
        if let Some(buffer) = slot.bytes.take() {
            match Arc::try_unwrap(buffer).map(Mutex::into_inner) {
                Ok(Ok(bytes))
                    if self.waiting.is_empty() && self.retained.len() < MAX_RETAINED_BUFFERS =>
                {
                    self.add(bytes.len());
                    self.retained.push(bytes);
                }
                Ok(Ok(bytes)) => freed = Some(bytes),
                // A copy in progress holds it, or a panic poisoned it; it is
                // freed when the last holder drops it.
                _ => {}
            }
        }
        Some((slot, freed))
    }
}

/// The piece cache shared by every torrent in memory storage mode.
pub struct PieceCache {
    capacity_bytes: u64,
    state: Mutex<State>,
    // Wakes the evictor when a reservation waits or room may have appeared.
    room_wanted: Notify,
}

impl PieceCache {
    /// Creates an empty cache with the given capacity.
    pub fn new(capacity_bytes: u64) -> Arc<Self> {
        Arc::new(Self {
            capacity_bytes,
            state: Mutex::new(State::default()),
            room_wanted: Notify::new(),
        })
    }

    fn lock(&self) -> Result<MutexGuard<'_, State>, CacheError> {
        self.state.lock().map_err(|_| CacheError::Poisoned)
    }

    // Called after a change that may let a waiting reservation proceed.
    fn room_may_have_changed(&self, state: &State) {
        if !state.waiting.is_empty() {
            self.room_wanted.notify_one();
        }
    }

    pub(crate) fn room_wanted(&self) -> &Notify {
        &self.room_wanted
    }

    /// Returns a snapshot of the accounting.
    pub fn stats(&self) -> Result<CacheStats, CacheError> {
        let state = self.lock()?;
        Ok(self.snapshot(&state))
    }

    fn snapshot(&self, state: &State) -> CacheStats {
        let resident = || state.slots.values().filter(|slot| slot.bytes.is_some());
        CacheStats {
            capacity_bytes: self.capacity_bytes,
            usage_bytes: state.usage_bytes,
            peak_usage_bytes: state.peak_usage_bytes,
            resident_pieces: resident().count() as u64,
            pending_pieces: state
                .slots
                .values()
                .filter(|slot| slot.bytes.is_none())
                .count() as u64,
            verified_pieces: state.slots.values().filter(|slot| slot.verified).count() as u64,
            pinned_bytes: state
                .slots
                .iter()
                .filter(|((torrent, piece), slot)| {
                    slot.bytes.is_some() && state.is_pinned(*torrent, *piece)
                })
                .map(|(_, slot)| slot.length as u64)
                .sum(),
            retained_bytes: state.retained_bytes(),
            eviction_count: state.eviction_count,
            waiting_pieces: state.waiting.len() as u64,
        }
    }

    /// Recomputes cache usage from pending allocations, resident buffers,
    /// and retained buffers, and checks that it matches the running total
    /// and stays within capacity.
    pub fn audit(&self) -> Result<CacheStats, CacheError> {
        let state = self.lock()?;
        let stats = self.snapshot(&state);
        let resident: u64 = state
            .slots
            .values()
            .map(|slot| slot.length as u64)
            .sum::<u64>()
            + stats.retained_bytes;
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

    /// Whether a piece of a torrent has a buffer in the cache.
    pub(crate) fn is_resident(&self, torrent: TorrentKey, piece: u32) -> Result<bool, CacheError> {
        let state = self.lock()?;
        Ok(state
            .slots
            .get(&(torrent, piece))
            .is_some_and(|slot| slot.bytes.is_some()))
    }

    pub(crate) fn register_torrent(&self) -> Result<TorrentKey, CacheError> {
        let mut state = self.lock()?;
        state.next_key += 1;
        let key = TorrentKey(state.next_key);
        state.torrents.insert(
            key,
            TorrentEntry {
                engine: None,
                started: true,
                pins: HashSet::new(),
                ranks: HashMap::new(),
            },
        );
        Ok(key)
    }

    /// Makes a torrent's verified pieces eviction candidates, invalidated
    /// through `engine`, which the evictor also wakes.
    pub(crate) fn attach_engine(
        &self,
        torrent: TorrentKey,
        engine: Arc<dyn EngineTorrent>,
    ) -> Result<(), CacheError> {
        let mut state = self.lock()?;
        let entry = state
            .torrents
            .get_mut(&torrent)
            .ok_or(CacheError::UnknownTorrent(torrent))?;
        entry.engine = Some(engine);
        self.room_may_have_changed(&state);
        Ok(())
    }

    /// Records whether a torrent is started. Pieces of paused and failed
    /// torrents are evicted before pieces of started ones, and a paused
    /// torrent requests nothing, so its waiting pieces leave the queue.
    pub(crate) fn set_started(&self, torrent: TorrentKey, started: bool) -> Result<(), CacheError> {
        let mut state = self.lock()?;
        let entry = state
            .torrents
            .get_mut(&torrent)
            .ok_or(CacheError::UnknownTorrent(torrent))?;
        entry.started = started;
        if !started {
            state.waiting.retain(|(key, _), _| *key != torrent);
            state.admitted.retain(|(key, _), _| *key != torrent);
            self.room_wanted.notify_one();
        }
        Ok(())
    }

    /// Replaces a torrent's plan: the pieces eviction must skip, and the
    /// pieces it requests, most urgent first, whose places order the waiting
    /// reservations across torrents. Waiting pieces the plan dropped leave
    /// the queue.
    pub(crate) fn set_plan(
        &self,
        torrent: TorrentKey,
        pins: HashSet<u32>,
        priorities: &[u32],
    ) -> Result<(), CacheError> {
        let mut state = self.lock()?;
        let entry = state
            .torrents
            .get_mut(&torrent)
            .ok_or(CacheError::UnknownTorrent(torrent))?;
        entry.pins = pins;
        entry.ranks = priorities
            .iter()
            .enumerate()
            .map(|(rank, &piece)| (piece, u32::try_from(rank).unwrap_or(u32::MAX)))
            .collect();
        let planned: HashSet<u32> = entry.ranks.keys().copied().collect();
        let dropped: Vec<(TorrentKey, u32)> = state
            .waiting
            .keys()
            .chain(state.admitted.keys())
            .filter(|(key, piece)| *key == torrent && !planned.contains(piece))
            .copied()
            .collect();
        for key in dropped {
            state.waiting.remove(&key);
            state.admitted.remove(&key);
        }
        self.room_wanted.notify_one();
        Ok(())
    }

    /// Reserves room for a piece of `length` bytes before the engine
    /// requests it. A reservation that does not fit, or that would pass
    /// pieces already waiting, is recorded as waiting, and the evictor makes
    /// room for it.
    pub(crate) fn reserve(
        &self,
        torrent: TorrentKey,
        piece: u32,
        length: usize,
    ) -> Result<Reservation, CacheError> {
        if length as u64 > self.capacity_bytes {
            return Ok(Reservation::TooLarge);
        }
        let mut state = self.lock()?;
        let state = &mut *state;
        if !state.torrents.contains_key(&torrent) {
            return Err(CacheError::UnknownTorrent(torrent));
        }
        let key = (torrent, piece);
        if state.slots.contains_key(&key) {
            return Ok(Reservation::Reserved);
        }
        if state.waiting.contains_key(&key) {
            // Still queued; only readmission takes it out.
            return Ok(Reservation::Waiting);
        }
        let admitted = state.admitted.remove(&key);
        let queue_ahead = admitted.is_none() && !state.waiting.is_empty();
        let released = if queue_ahead {
            Vec::new()
        } else {
            state.release_retained(length as u64, self.capacity_bytes)
        };
        if queue_ahead || state.usage_bytes + length as u64 > self.capacity_bytes {
            let since = admitted.unwrap_or_else(|| state.tick());
            state.waiting.insert(key, Waiting { length, since });
            self.room_wanted.notify_one();
            drop(released);
            return Ok(Reservation::Waiting);
        }
        state.add(length);
        state.slots.insert(
            key,
            Slot {
                length,
                bytes: None,
                verified: false,
                last_use: 0,
            },
        );
        drop(released);
        Ok(Reservation::Reserved)
    }

    /// Releases a piece that will not complete as it is: its pending
    /// allocation, or its partial buffer. A verified piece stays.
    pub(crate) fn release_unverified(
        &self,
        torrent: TorrentKey,
        piece: u32,
    ) -> Result<(), CacheError> {
        let freed = {
            let mut state = self.lock()?;
            let unverified = state
                .slots
                .get(&(torrent, piece))
                .is_some_and(|slot| !slot.verified);
            let freed = if unverified {
                state.remove_slot((torrent, piece))
            } else {
                None
            };
            self.room_may_have_changed(&state);
            freed
        };
        drop(freed);
        Ok(())
    }

    /// Returns the bytes the waiting reservations need, for
    /// [`eviction_candidates`](Self::eviction_candidates), after releasing
    /// retained buffers toward them, or zero when they fit in the free
    /// capacity.
    pub(crate) fn room_needed(&self) -> Result<u64, CacheError> {
        let (needed, released) = {
            let mut state = self.lock()?;
            let needed: u64 = state
                .waiting
                .values()
                .map(|waiting| waiting.length as u64)
                .sum();
            let released = if needed > 0 {
                state.release_retained(needed, self.capacity_bytes)
            } else {
                Vec::new()
            };
            let free = self.capacity_bytes.saturating_sub(state.usage_bytes);
            (if needed > free { needed } else { 0 }, released)
        };
        drop(released);
        Ok(needed)
    }

    /// Readmits waiting reservations as far as the free capacity goes, in
    /// piece priority order across torrents and, among equal places, in the
    /// order they started waiting. Readmission stops at the first piece that
    /// does not fit, so smaller pieces never pass it indefinitely. Returns
    /// the torrents to wake, which reserve their pieces again when they
    /// select them.
    pub(crate) fn take_admissible(&self) -> Result<Vec<Arc<dyn EngineTorrent>>, CacheError> {
        let mut state = self.lock()?;
        let state = &mut *state;
        let mut queue: Vec<(u32, u64, (TorrentKey, u32), usize)> = state
            .waiting
            .iter()
            .map(|(&key, waiting)| {
                let rank = state
                    .torrents
                    .get(&key.0)
                    .and_then(|entry| entry.ranks.get(&key.1))
                    .copied()
                    .unwrap_or(u32::MAX);
                (rank, waiting.since, key, waiting.length)
            })
            .collect();
        queue.sort_unstable();
        let mut free = self.capacity_bytes.saturating_sub(state.usage_bytes);
        let mut torrents: Vec<TorrentKey> = Vec::new();
        for (_, since, key, length) in queue {
            if length as u64 > free {
                break;
            }
            free -= length as u64;
            state.waiting.remove(&key);
            state.admitted.insert(key, since);
            if !torrents.contains(&key.0) {
                torrents.push(key.0);
            }
        }
        Ok(torrents
            .into_iter()
            .filter_map(|torrent| state.torrents.get(&torrent)?.engine.clone())
            .collect())
    }

    /// Returns verified, unpinned pieces whose eviction would free at least
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
            .slots
            .iter()
            .filter(|(_, slot)| slot.verified)
            .filter(|((torrent, piece), _)| !state.is_pinned(*torrent, *piece))
            .filter_map(|(&(torrent, piece), slot)| {
                let entry = state.torrents.get(&torrent)?;
                entry.engine.as_ref()?;
                Some((entry.started, slot.last_use, torrent, piece, slot.length))
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
                    let Some(engine) = state
                        .torrents
                        .get(&torrent)
                        .and_then(|entry| entry.engine.clone())
                    else {
                        continue;
                    };
                    batches.push((
                        torrent,
                        EvictionBatch {
                            engine,
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
        let freed = {
            let mut state = self.lock()?;
            match state.slots.get(&(torrent, piece)) {
                None => return Err(CacheError::PieceMissing { torrent, piece }),
                Some(slot) if !slot.verified => {
                    return Err(CacheError::NotEvictable { torrent, piece });
                }
                Some(_) => {}
            }
            let freed = state.remove_slot((torrent, piece));
            state.eviction_count += 1;
            self.room_may_have_changed(&state);
            freed
        };
        drop(freed);
        Ok(())
    }

    /// Returns the buffer of a piece of `piece_length` bytes, creating its
    /// slot for a piece nobody reserved, or `None` when the slot has no
    /// buffer yet and the caller must provide one.
    fn buffer_for_write(
        &self,
        torrent: TorrentKey,
        piece: u32,
        piece_length: usize,
        provided: &mut Option<Box<[u8]>>,
    ) -> Result<Option<Buffer>, CacheError> {
        let mut state = self.lock()?;
        let state = &mut *state;
        if !state.torrents.contains_key(&torrent) {
            return Err(CacheError::UnknownTorrent(torrent));
        }
        if !state.slots.contains_key(&(torrent, piece)) {
            let needed_bytes = piece_length as u64;
            // Retained buffers go first; a reused one returns its usage here.
            let released = state.release_retained(needed_bytes, self.capacity_bytes);
            drop(released);
            if state.usage_bytes + needed_bytes > self.capacity_bytes {
                return Err(CacheError::CapacityExceeded {
                    needed_bytes,
                    usage_bytes: state.usage_bytes,
                    capacity_bytes: self.capacity_bytes,
                });
            }
            state.add(piece_length);
            state.slots.insert(
                (torrent, piece),
                Slot {
                    length: piece_length,
                    bytes: None,
                    verified: false,
                    last_use: 0,
                },
            );
        }
        let reused = match state.slots.get(&(torrent, piece)) {
            Some(slot) if slot.bytes.is_none() && provided.is_none() => {
                state.take_retained(piece_length)
            }
            _ => None,
        };
        let slot = state
            .slots
            .get_mut(&(torrent, piece))
            .ok_or(CacheError::PieceMissing { torrent, piece })?;
        if slot.length != piece_length {
            return Err(CacheError::LengthMismatch {
                piece,
                reserved: slot.length,
                requested: piece_length,
            });
        }
        if slot.bytes.is_none()
            && let Some(bytes) = reused.or_else(|| provided.take())
        {
            slot.bytes = Some(Arc::new(Mutex::new(bytes)));
        }
        Ok(slot.bytes.clone())
    }

    /// Writes `data` at `offset` within a piece of `piece_length` bytes. A
    /// reserved piece gets its buffer on the first write, reused from the
    /// retained buffers or allocated with the cache lock released; a piece
    /// nobody reserved is reserved here, or the write fails when it does not
    /// fit.
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
        let mut provided = None;
        let buffer = loop {
            if let Some(buffer) =
                self.buffer_for_write(torrent, piece, piece_length, &mut provided)?
            {
                break buffer;
            }
            provided = Some(vec![0; piece_length].into_boxed_slice());
        };
        // A buffer allocated for a slot that got one meanwhile is dropped
        // here, outside the cache lock.
        drop(provided);
        let mut bytes = buffer.lock().map_err(|_| CacheError::Poisoned)?;
        bytes[offset..end].copy_from_slice(data);
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
        let (buffer, length) = {
            let mut state = self.lock()?;
            let now = state.tick();
            let slot = state
                .slots
                .get_mut(&(torrent, piece))
                .ok_or(CacheError::PieceMissing { torrent, piece })?;
            let buffer = slot
                .bytes
                .clone()
                .ok_or(CacheError::PieceMissing { torrent, piece })?;
            slot.last_use = now;
            (buffer, slot.length)
        };
        let end = offset + buf.len();
        let bytes = buffer.lock().map_err(|_| CacheError::Poisoned)?;
        let source = bytes.get(offset..end).ok_or(CacheError::OutOfBounds {
            piece,
            offset,
            end,
            length,
        })?;
        buf.copy_from_slice(source);
        Ok(())
    }

    /// Records that a resident piece passed its hash check.
    pub(crate) fn mark_verified(&self, torrent: TorrentKey, piece: u32) -> Result<(), CacheError> {
        let mut state = self.lock()?;
        let now = state.tick();
        let slot = state
            .slots
            .get_mut(&(torrent, piece))
            .filter(|slot| slot.bytes.is_some())
            .ok_or(CacheError::PieceMissing { torrent, piece })?;
        slot.verified = true;
        slot.last_use = now;
        self.room_may_have_changed(&state);
        Ok(())
    }

    /// Releases every piece of a torrent and returns their bytes to the
    /// capacity.
    pub(crate) fn release_torrent(&self, torrent: TorrentKey) -> Result<(), CacheError> {
        let freed: Vec<_> = {
            let mut state = self.lock()?;
            state.torrents.remove(&torrent);
            state.waiting.retain(|(key, _), _| *key != torrent);
            state.admitted.retain(|(key, _), _| *key != torrent);
            let keys: Vec<_> = state
                .slots
                .keys()
                .filter(|(key, _)| *key == torrent)
                .copied()
                .collect();
            let freed = keys
                .into_iter()
                .filter_map(|key| state.remove_slot(key))
                .collect();
            self.room_may_have_changed(&state);
            freed
        };
        drop(freed);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct NoEngine;

    impl EngineTorrent for NoEngine {
        fn invalidate(&self, _pieces: &[u32]) -> anyhow::Result<usize> {
            Ok(0)
        }

        fn reselect(&self) {}
    }

    fn evictable(cache: &PieceCache) -> TorrentKey {
        let torrent = cache.register_torrent().unwrap();
        cache.attach_engine(torrent, Arc::new(NoEngine)).unwrap();
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
        assert_eq!(stats.eviction_count, 1);
        assert_eq!(
            (
                stats.usage_bytes,
                stats.retained_bytes,
                stats.resident_pieces
            ),
            (60, 60, 0),
            "the freed buffer is retained and still counted"
        );
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
                stats.usage_bytes - stats.retained_bytes,
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

    #[test]
    fn a_reservation_counts_until_released_and_waits_when_full() {
        let cache = PieceCache::new(100);
        let torrent = evictable(&cache);
        assert_eq!(
            cache.reserve(torrent, 0, 60).unwrap(),
            Reservation::Reserved
        );
        let stats = cache.audit().unwrap();
        assert_eq!(
            (
                stats.usage_bytes,
                stats.pending_pieces,
                stats.resident_pieces
            ),
            (60, 1, 0),
            "a pending allocation counts in full"
        );
        assert_eq!(
            cache.reserve(torrent, 0, 60).unwrap(),
            Reservation::Reserved
        );
        assert_eq!(cache.reserve(torrent, 1, 60).unwrap(), Reservation::Waiting);
        assert_eq!(
            cache.reserve(torrent, 2, 101).unwrap(),
            Reservation::TooLarge
        );
        assert_eq!(cache.stats().unwrap().waiting_pieces, 1);
        assert!(
            cache.take_admissible().unwrap().is_empty(),
            "nothing fits yet"
        );

        cache.write(torrent, 0, 60, 0, &[1]).unwrap();
        assert_eq!(
            cache.audit().unwrap().usage_bytes,
            60,
            "the first write uses the reservation"
        );
        cache.release_unverified(torrent, 0).unwrap();
        assert_eq!(cache.audit().unwrap().usage_bytes, 0);
        assert_eq!(
            cache.take_admissible().unwrap().len(),
            1,
            "the waiting torrent is woken"
        );
        assert_eq!(cache.stats().unwrap().waiting_pieces, 0);
    }

    #[test]
    fn the_room_waiting_pieces_need_selects_enough_to_evict() {
        let cache = PieceCache::new(100);
        let small = evictable(&cache);
        let large = evictable(&cache);
        verified(&cache, small, 0, 30);
        verified(&cache, small, 1, 30);
        assert_eq!(cache.room_needed().unwrap(), 0, "nothing waits");
        assert_eq!(cache.reserve(large, 0, 50).unwrap(), Reservation::Waiting);
        assert_eq!(
            candidates(&cache, cache.room_needed().unwrap()),
            vec![vec![0]],
            "40 bytes are free, so one 30-byte piece makes room for 50"
        );
    }

    #[test]
    fn pinned_pieces_are_not_eviction_candidates() {
        let cache = PieceCache::new(100);
        let torrent = evictable(&cache);
        verified(&cache, torrent, 0, 30);
        verified(&cache, torrent, 1, 30);
        cache.set_plan(torrent, HashSet::from([0]), &[0]).unwrap();
        assert_eq!(cache.stats().unwrap().pinned_bytes, 30);
        assert_eq!(candidates(&cache, 100), vec![vec![1]]);
        cache.release_unverified(torrent, 1).unwrap();
        assert_eq!(
            cache.audit().unwrap().verified_pieces,
            2,
            "releasing never drops a verified piece"
        );
    }

    fn reserved(cache: &PieceCache, torrent: TorrentKey, piece: u32, length: usize) {
        assert_eq!(
            cache.reserve(torrent, piece, length).unwrap(),
            Reservation::Reserved
        );
    }

    #[test]
    fn freed_buffers_are_retained_up_to_the_limit_and_reused() {
        let cache = PieceCache::new(1000);
        let torrent = evictable(&cache);
        for piece in 0..6 {
            verified(&cache, torrent, piece, 100);
        }
        for piece in 0..6 {
            cache.evict(torrent, piece).unwrap();
        }
        let stats = cache.audit().unwrap();
        assert_eq!(
            (stats.retained_bytes, stats.usage_bytes),
            (
                MAX_RETAINED_BUFFERS as u64 * 100,
                MAX_RETAINED_BUFFERS as u64 * 100
            )
        );
        cache.write(torrent, 10, 100, 0, &[1]).unwrap();
        let stats = cache.audit().unwrap();
        assert_eq!(
            (stats.retained_bytes, stats.usage_bytes),
            (300, 400),
            "a write of the same length reuses a retained buffer"
        );
        cache.write(torrent, 11, 50, 0, &[1]).unwrap();
        assert_eq!(
            cache.audit().unwrap().retained_bytes,
            300,
            "other lengths allocate"
        );
    }

    #[test]
    fn retained_buffers_give_way_to_reservations() {
        let cache = PieceCache::new(100);
        let torrent = evictable(&cache);
        verified(&cache, torrent, 0, 60);
        cache.evict(torrent, 0).unwrap();
        reserved(&cache, torrent, 1, 80);
        let stats = cache.audit().unwrap();
        assert_eq!((stats.usage_bytes, stats.retained_bytes), (80, 0));
        cache.write(torrent, 1, 80, 0, &[1]).unwrap();
        cache.mark_verified(torrent, 1).unwrap();
        cache.evict(torrent, 1).unwrap();
        assert_eq!(cache.audit().unwrap().retained_bytes, 80);
        cache.write(torrent, 2, 40, 0, &[1]).unwrap();
        assert_eq!(
            cache.audit().unwrap().retained_bytes,
            0,
            "an unreserved write releases retained buffers too"
        );
    }

    #[test]
    fn nothing_is_retained_while_reservations_wait() {
        let cache = PieceCache::new(100);
        let torrent = evictable(&cache);
        verified(&cache, torrent, 0, 60);
        assert_eq!(cache.reserve(torrent, 1, 60).unwrap(), Reservation::Waiting);
        cache.evict(torrent, 0).unwrap();
        let stats = cache.audit().unwrap();
        assert_eq!((stats.usage_bytes, stats.retained_bytes), (0, 0));
    }

    /// CAC-05: readmission follows piece priority across torrents, then
    /// waiting order, and new reservations queue behind waiting ones.
    #[test]
    fn waiting_pieces_are_readmitted_in_priority_order() {
        let cache = PieceCache::new(100);
        let first = evictable(&cache);
        let second = evictable(&cache);
        let filler = evictable(&cache);
        reserved(&cache, filler, 0, 100);
        cache
            .set_plan(first, HashSet::new(), &[6, 1, 2, 5])
            .unwrap();
        cache.set_plan(second, HashSet::new(), &[7]).unwrap();
        for (torrent, piece) in [(first, 5), (second, 7), (first, 6)] {
            assert_eq!(
                cache.reserve(torrent, piece, 40).unwrap(),
                Reservation::Waiting
            );
        }
        cache.release_unverified(filler, 0).unwrap();
        assert_eq!(
            cache.take_admissible().unwrap().len(),
            2,
            "the two most urgent pieces fit; the third waits"
        );
        assert_eq!(cache.stats().unwrap().waiting_pieces, 1);
        assert_eq!(
            cache.reserve(first, 9, 10).unwrap(),
            Reservation::Waiting,
            "a new reservation does not pass the queue"
        );
        reserved(&cache, second, 7, 40);
        reserved(&cache, first, 6, 40);
        assert_eq!(
            cache.reserve(first, 5, 40).unwrap(),
            Reservation::Waiting,
            "still queued until readmitted"
        );
    }

    #[test]
    fn readmission_stops_at_the_first_piece_that_does_not_fit() {
        let cache = PieceCache::new(100);
        let torrent = evictable(&cache);
        let filler = evictable(&cache);
        reserved(&cache, filler, 0, 60);
        reserved(&cache, filler, 1, 40);
        cache.set_plan(torrent, HashSet::new(), &[0, 1]).unwrap();
        assert_eq!(cache.reserve(torrent, 0, 80).unwrap(), Reservation::Waiting);
        assert_eq!(cache.reserve(torrent, 1, 10).unwrap(), Reservation::Waiting);
        cache.release_unverified(filler, 0).unwrap();
        assert!(
            cache.take_admissible().unwrap().is_empty(),
            "the small piece does not pass the urgent one"
        );
        cache.release_unverified(filler, 1).unwrap();
        cache.take_admissible().unwrap();
        assert_eq!(cache.stats().unwrap().waiting_pieces, 0);
    }

    #[test]
    fn dropped_and_paused_pieces_leave_the_queue() {
        let cache = PieceCache::new(100);
        let torrent = evictable(&cache);
        let filler = evictable(&cache);
        reserved(&cache, filler, 0, 100);
        cache.set_plan(torrent, HashSet::new(), &[0, 1]).unwrap();
        cache.reserve(torrent, 0, 50).unwrap();
        cache.reserve(torrent, 1, 50).unwrap();
        cache.set_plan(torrent, HashSet::new(), &[1]).unwrap();
        assert_eq!(cache.stats().unwrap().waiting_pieces, 1);
        cache.set_started(torrent, false).unwrap();
        assert_eq!(cache.stats().unwrap().waiting_pieces, 0);
    }

    #[test]
    fn concurrent_copies_keep_the_accounting() {
        let cache = PieceCache::new(64 * 1024);
        let torrent = evictable(&cache);
        std::thread::scope(|scope| {
            for worker in 0..8u32 {
                let cache = &cache;
                scope.spawn(move || {
                    for round in 0..200u32 {
                        let piece = worker * 1000 + round % 4;
                        let byte = (worker * 7 + round) as u8;
                        if cache.write(torrent, piece, 1024, 0, &[byte; 1024]).is_ok() {
                            let mut buf = [0; 1024];
                            if cache.read(torrent, piece, 0, &mut buf).is_ok() {
                                assert!(buf.iter().all(|&b| b == buf[0]), "a torn copy");
                            }
                            let _ = cache.mark_verified(torrent, piece);
                            let _ = cache.evict(torrent, piece);
                        }
                    }
                });
            }
        });
        let stats = cache.audit().unwrap();
        assert!(stats.retained_bytes <= MAX_RETAINED_BUFFERS as u64 * 1024);
    }
}
