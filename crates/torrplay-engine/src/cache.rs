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
//! reservation joins the queue instead of taking room ahead of it. An
//! unreserved write can claim available space or evict for a shortfall that
//! pending teardown cannot cover. Storage drops the block for a later fetch
//! if the cache still cannot hold it. Partial pieces are protected from
//! eviction and stay resident until verified or discarded.
//!
//! Cache capacity can change while the cache runs. Growing it lets waiting
//! reservations in; shrinking it below cache usage makes the evictor evict
//! down to the new capacity, as ADR 5's capacity changes describe, while
//! nothing new is admitted. A capacity below the piece length of a
//! registered torrent is refused.
//!
//! The cache lock guards only the accounting. Piece buffers have locks of
//! their own: buffers are allocated, and bytes copied in and out, with the
//! cache lock released. A buffer freed while nothing waits for room is
//! retained for reuse, up to [`RETAINED_BUFFER_LIMIT`], and counts in cache
//! usage until reused or released to make room.
//!
//! [`Evictor`]: crate::evictor::Evictor

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::ops::{Deref, DerefMut};
#[cfg(target_os = "linux")]
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock, Weak};
use std::time::Duration;

use tokio::sync::Notify;
use tokio::time::Instant;

/// The most freed piece buffers the cache retains for reuse, as ADR 5's
/// milestone 4 defaults fix it.
pub const RETAINED_BUFFER_LIMIT: usize = 4;
// A notified peer may disappear or never select an admitted piece.
const ADMISSION_TIMEOUT: Duration = Duration::from_secs(1);
// A detached piece may still be in use by an in-flight copy.
const TEARDOWN_RETRY: Duration = Duration::from_millis(10);

/// Small buffers stay on the heap to avoid a mapping per small piece.
#[cfg(target_os = "linux")]
const MAPPED_PIECE_MIN_BYTES: usize = 1 << 20;

/// Leaves room for the rest of the process below Linux's default VMA limit.
#[cfg(target_os = "linux")]
static AVAILABLE_PIECE_MAPPINGS: AtomicUsize = AtomicUsize::new(4096);

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
    /// Linux could not allocate the piece buffer.
    #[cfg(target_os = "linux")]
    #[error("allocating a {length}-byte piece buffer failed: {kind}")]
    AllocationFailed {
        /// The piece buffer's length.
        length: usize,
        /// The allocation error.
        kind: std::io::ErrorKind,
    },
    /// The running total disagrees with the recomputed cache usage.
    #[error("cache usage mismatch with {counted} bytes tracked and {recomputed} bytes recomputed")]
    AccountingMismatch {
        /// The running total.
        counted: u64,
        /// The sum of pending allocations and all charged buffers.
        recomputed: u64,
    },
    /// Cache usage rose above capacity at some point.
    #[error("cache usage rose to {peak_bytes} bytes, above the capacity of {capacity_bytes}")]
    CapacityViolated {
        /// The highest cache usage above capacity.
        peak_bytes: u64,
        /// The cache capacity it rose above.
        capacity_bytes: u64,
    },
    /// A new capacity is below a registered torrent's piece length.
    #[error("{piece_length_bytes}-byte pieces exceed a cache capacity of {capacity_bytes} bytes")]
    CapacityBelowPieceLength {
        /// The longest piece length among registered torrents.
        piece_length_bytes: u64,
        /// The capacity refused.
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
    /// The cache capacity in effect.
    pub capacity_bytes: u64,
    /// The bytes counted against capacity: pending allocations, resident
    /// piece buffers, retained reusable buffers, and buffers being freed.
    /// Only a capacity that shrank leaves it above capacity.
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
    /// The bytes of reusable buffers, including those being released.
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
    /// The piece is longer than the current cache capacity.
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
type Buffer = Arc<PieceBuffer>;

struct PieceBuffer {
    bytes: Mutex<PieceBytes>,
    // Field order frees the bytes before returning their charge.
    shutdown_charge: OnceLock<ShutdownCharge>,
}

struct ShutdownCharge {
    cache: Weak<PieceCache>,
    length: u64,
}

impl Drop for ShutdownCharge {
    fn drop(&mut self) {
        if let Some(cache) = self.cache.upgrade()
            && let Ok(mut state) = cache.lock()
        {
            cache.finish_teardown(&mut state, self.length);
        }
    }
}

enum PieceBytes {
    Heap(Box<[u8]>),
    #[cfg(target_os = "linux")]
    Mapped(MappedPiece),
}

#[cfg(target_os = "linux")]
struct MappedPiece {
    bytes: memmap2::MmapMut,
    // Drop the mapping before returning its budget slot.
    _permit: MappingPermit,
}

#[cfg(target_os = "linux")]
struct MappingPermit(&'static AtomicUsize);

#[cfg(target_os = "linux")]
impl MappingPermit {
    fn acquire(available: &'static AtomicUsize) -> Option<Self> {
        available
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |count| {
                count.checked_sub(1)
            })
            .ok()?;
        Some(Self(available))
    }
}

#[cfg(target_os = "linux")]
impl Drop for MappingPermit {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::Relaxed);
    }
}

impl PieceBytes {
    fn allocate(length: usize) -> Result<Self, CacheError> {
        #[cfg(target_os = "linux")]
        if length >= MAPPED_PIECE_MIN_BYTES {
            return Self::allocate_large(
                length,
                &AVAILABLE_PIECE_MAPPINGS,
                memmap2::MmapMut::map_anon,
            );
        }
        Ok(Self::Heap(vec![0; length].into_boxed_slice()))
    }

    #[cfg(target_os = "linux")]
    fn allocate_large(
        length: usize,
        available: &'static AtomicUsize,
        map: impl FnOnce(usize) -> std::io::Result<memmap2::MmapMut>,
    ) -> Result<Self, CacheError> {
        if let Some(permit) = MappingPermit::acquire(available) {
            // The general-purpose heap can keep freed piece buffers resident.
            if let Ok(bytes) = map(length) {
                return Ok(Self::Mapped(MappedPiece {
                    bytes,
                    _permit: permit,
                }));
            }
            // The kernel's mapping limit also includes other allocations.
        }
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(length)
            .map_err(|_| CacheError::AllocationFailed {
                length,
                kind: std::io::ErrorKind::OutOfMemory,
            })?;
        bytes.resize(length, 0);
        Ok(Self::Heap(bytes.into_boxed_slice()))
    }
}

impl Deref for PieceBytes {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        match self {
            Self::Heap(bytes) => bytes,
            #[cfg(target_os = "linux")]
            Self::Mapped(piece) => &piece.bytes,
        }
    }
}

impl DerefMut for PieceBytes {
    fn deref_mut(&mut self) -> &mut [u8] {
        match self {
            Self::Heap(bytes) => bytes,
            #[cfg(target_os = "linux")]
            Self::Mapped(piece) => &mut piece.bytes,
        }
    }
}

struct Slot {
    length: usize,
    // Absent while the piece is a pending allocation.
    bytes: Option<Buffer>,
    verified: bool,
    // The cache clock at the piece's last read or verification.
    last_use: u64,
}

struct DetachedPiece {
    length: usize,
    copying: Option<Buffer>,
    owned: Option<PieceBytes>,
}

struct TorrentEntry {
    // Absent until the engine can invalidate the torrent's pieces.
    engine: Option<Arc<dyn EngineTorrent>>,
    // The capacity may not shrink below it.
    piece_length: u64,
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
    // Keep its place while the notified peers decide whether they still need it.
    rescan_deadline: Option<Instant>,
}

struct Retry {
    since: u64,
    // An admission holds a pending slot until claimed or canceled.
    deadline: Option<Instant>,
}

struct State {
    capacity_bytes: u64,
    usage_bytes: u64,
    peak_usage_bytes: u64,
    // The highest usage that an addition took above capacity, with that
    // capacity: an accounting bug, which the audit reports.
    exceeded: Option<(u64, u64)>,
    eviction_count: u64,
    next_key: u64,
    clock: u64,
    torrents: HashMap<TorrentKey, TorrentEntry>,
    slots: HashMap<(TorrentKey, u32), Slot>,
    // Reservations that found no room.
    waiting: HashMap<(TorrentKey, u32), Waiting>,
    // Readmitted pieces keep their original wait time until claimed.
    retry: HashMap<(TorrentKey, u32), Retry>,
    // Freed buffers kept for reuse, counted in usage.
    retained: Vec<PieceBytes>,
    // Detached retained buffers stay charged until their teardown finishes.
    releasing_retained_bytes: u64,
    // Detached piece buffers waiting for the eviction worker.
    teardown: Vec<DetachedPiece>,
    teardown_bytes: u64,
    // Once the evictor stops, late releases clean up on the releasing thread.
    teardown_after_shutdown: Option<Weak<PieceCache>>,
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
        if self.usage_bytes > self.capacity_bytes {
            let peak = self.exceeded.map_or(0, |(peak, _)| peak);
            if self.usage_bytes > peak {
                self.exceeded = Some((self.usage_bytes, self.capacity_bytes));
            }
        }
    }

    // What the capacity lacks for current usage plus `needed_bytes`.
    fn shortfall(&self, needed_bytes: u64) -> u64 {
        (self.usage_bytes + needed_bytes).saturating_sub(self.capacity_bytes)
    }

    fn free_bytes(&self) -> u64 {
        self.capacity_bytes.saturating_sub(self.usage_bytes)
    }

    fn retained_bytes(&self) -> u64 {
        self.retained
            .iter()
            .map(|buffer| buffer.len() as u64)
            .sum::<u64>()
            + self.releasing_retained_bytes
    }

    fn wait_order(&self, key: (TorrentKey, u32), waiting: &Waiting) -> (u32, u64) {
        let rank = self
            .torrents
            .get(&key.0)
            .and_then(|entry| entry.ranks.get(&key.1))
            .copied()
            .unwrap_or(u32::MAX);
        (rank, waiting.since)
    }

    fn waiting_bytes(&self) -> u64 {
        let first_rescan = self
            .waiting
            .iter()
            .filter(|(_, waiting)| waiting.rescan_deadline.is_some())
            .map(|(&key, waiting)| self.wait_order(key, waiting))
            .min();
        self.waiting
            .iter()
            .filter(|&(&key, waiting)| {
                first_rescan.is_none_or(|first| self.wait_order(key, waiting) < first)
            })
            .map(|(_, waiting)| waiting.length as u64)
            .sum()
    }

    fn shortfall_after_teardown(&self, needed_bytes: u64) -> u64 {
        self.shortfall(needed_bytes)
            .saturating_sub(self.releasing_retained_bytes + self.teardown_bytes)
    }

    fn cancel_retry(&mut self, key: (TorrentKey, u32)) {
        if self
            .retry
            .remove(&key)
            .is_some_and(|retry| retry.deadline.is_some())
            && let Some(slot) = self.slots.remove(&key)
        {
            self.usage_bytes -= slot.length as u64;
        }
    }

    fn add_pending(&mut self, key: (TorrentKey, u32), length: usize) {
        self.add(length);
        self.slots.insert(
            key,
            Slot {
                length,
                bytes: None,
                verified: false,
                last_use: 0,
            },
        );
    }

    /// Detaches only buffers whose release would help cover the shortfall.
    fn detach_retained(
        &mut self,
        needed_bytes: u64,
        deferred_teardown_bytes: u64,
    ) -> (Vec<PieceBytes>, u64) {
        let shortfall = self
            .shortfall(needed_bytes)
            .saturating_sub(self.releasing_retained_bytes + deferred_teardown_bytes);
        let mut buffers = Vec::new();
        let mut detached_bytes = 0;
        while detached_bytes < shortfall {
            let Some(buffer) = self.retained.pop() else {
                break;
            };
            detached_bytes += buffer.len() as u64;
            buffers.push(buffer);
        }
        self.releasing_retained_bytes += detached_bytes;
        (buffers, detached_bytes)
    }

    /// Takes a retained buffer of exactly `length` bytes, which leaves the
    /// usage it counted to the slot it moves into.
    fn take_retained(&mut self, length: usize) -> Option<PieceBytes> {
        let index = self
            .retained
            .iter()
            .position(|buffer| buffer.len() == length)?;
        let buffer = self.retained.swap_remove(index);
        self.usage_bytes -= length as u64;
        Some(buffer)
    }

    /// Removes a slot. A pending allocation returns its bytes immediately.
    /// A buffer stays charged while retained or detached. It is
    /// retained when nothing waits for room, the pool has space, usage is
    /// within capacity, and no copy still holds it; otherwise it is returned
    /// to be dropped after unlocking.
    fn remove_slot(&mut self, key: (TorrentKey, u32)) -> Option<DetachedPiece> {
        let Slot { length, bytes, .. } = self.slots.remove(&key)?;
        let Some(buffer) = bytes else {
            self.usage_bytes -= length as u64;
            return None;
        };
        let (owned, copying) = match Arc::try_unwrap(buffer).map(|buffer| buffer.bytes.into_inner())
        {
            Ok(Ok(bytes))
                if self.waiting.is_empty()
                    && self.retained.len() < RETAINED_BUFFER_LIMIT
                    && self.shortfall(0) == 0 =>
            {
                self.retained.push(bytes);
                return None;
            }
            Ok(Ok(bytes)) => (Some(bytes), None),
            Ok(Err(error)) => (Some(error.into_inner()), None),
            Err(buffer) => (None, Some(buffer)),
        };
        self.teardown_bytes += length as u64;
        Some(DetachedPiece {
            length,
            copying,
            owned,
        })
    }

    /// Removes a slot, keeping any detached buffer charged until teardown.
    fn defer_slot_release(&mut self, key: (TorrentKey, u32)) -> bool {
        let Some(detached) = self.remove_slot(key) else {
            return false;
        };
        self.teardown.push(detached);
        true
    }
}

/// The piece cache shared by every torrent in memory storage mode.
pub struct PieceCache {
    state: Mutex<State>,
    // An unreserved write can wait for retained buffers to be released.
    retained_released: Condvar,
    // Storage transitions wait asynchronously for detached buffers.
    seed_room_changed: Notify,
    // Wakes the evictor when a reservation waits or room may have appeared.
    room_wanted: Notify,
}

impl PieceCache {
    /// Creates an empty cache with the given capacity.
    pub fn new(capacity_bytes: u64) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(State {
                capacity_bytes,
                usage_bytes: 0,
                peak_usage_bytes: 0,
                exceeded: None,
                eviction_count: 0,
                next_key: 0,
                clock: 0,
                torrents: HashMap::new(),
                slots: HashMap::new(),
                waiting: HashMap::new(),
                retry: HashMap::new(),
                retained: Vec::new(),
                releasing_retained_bytes: 0,
                teardown: Vec::new(),
                teardown_bytes: 0,
                teardown_after_shutdown: None,
            }),
            retained_released: Condvar::new(),
            seed_room_changed: Notify::new(),
            room_wanted: Notify::new(),
        })
    }

    fn lock(&self) -> Result<MutexGuard<'_, State>, CacheError> {
        self.state.lock().map_err(|_| CacheError::Poisoned)
    }

    fn lock_reclaiming_retained(
        &self,
        needed: impl Fn(&State) -> Option<u64>,
        wait_for_release: bool,
    ) -> Result<MutexGuard<'_, State>, CacheError> {
        let mut state = self.lock()?;
        loop {
            if state.retained.is_empty()
                && (!wait_for_release || state.releasing_retained_bytes == 0)
            {
                return Ok(state);
            }
            let Some(needed_bytes) = needed(&state) else {
                return Ok(state);
            };
            // The evictor retries after piece buffer teardown. A writer only
            // waits for releases already in progress from the retained pool.
            let deferred_teardown_bytes = if wait_for_release {
                0
            } else {
                state.teardown_bytes
            };
            let (buffers, detached_bytes) =
                state.detach_retained(needed_bytes, deferred_teardown_bytes);
            if buffers.is_empty() {
                if !wait_for_release
                    || state.releasing_retained_bytes == 0
                    || state.shortfall(needed_bytes) == 0
                {
                    return Ok(state);
                }
                state = self
                    .retained_released
                    .wait(state)
                    .map_err(|_| CacheError::Poisoned)?;
                continue;
            }
            drop(state);
            drop(buffers);
            state = self.finish_retained_release(detached_bytes)?;
        }
    }

    fn finish_retained_release(&self, bytes: u64) -> Result<MutexGuard<'_, State>, CacheError> {
        let mut state = self.lock()?;
        state.releasing_retained_bytes -= bytes;
        state.usage_bytes -= bytes;
        self.retained_released.notify_all();
        self.room_may_have_changed(&state);
        Ok(state)
    }

    /// Waits only when detached buffers account for the cache shortfall
    /// after reclaiming reusable buffers. Returns false if the wait expires.
    /// Returns true when seeding can proceed to its own capacity check,
    /// including when teardown cannot cover the shortfall.
    pub(crate) async fn wait_for_seed_room(
        &self,
        seed_bytes: u64,
        timeout: Duration,
    ) -> Result<bool, CacheError> {
        if seed_bytes == 0 {
            return Ok(true);
        }
        let wait = async {
            loop {
                let room_changed = self.seed_room_changed.notified();
                // Register before reading the accounting so a concurrent
                // release cannot slip between the check and the wait.
                tokio::pin!(room_changed);
                room_changed.as_mut().enable();
                {
                    let state = self.lock()?;
                    let shortfall = state
                        .shortfall(seed_bytes)
                        .saturating_sub(state.retained_bytes());
                    if shortfall == 0 || shortfall > state.teardown_bytes {
                        return Ok(true);
                    }
                }
                room_changed.await;
            }
        };
        tokio::time::timeout(timeout, wait)
            .await
            .unwrap_or(Ok(false))
    }

    pub(crate) fn maintenance_deadline(&self) -> Result<Option<Instant>, CacheError> {
        let state = self.lock()?;
        Ok(state
            .retry
            .values()
            .filter_map(|retry| retry.deadline)
            .chain(
                state
                    .waiting
                    .values()
                    .filter_map(|waiting| waiting.rescan_deadline),
            )
            .chain((!state.teardown.is_empty()).then(|| Instant::now() + TEARDOWN_RETRY))
            .min())
    }

    pub(crate) fn expire_reservations(&self, now: Instant) -> Result<(), CacheError> {
        let mut state = self.lock()?;
        let previous_usage = state.usage_bytes;
        let state = &mut *state;
        state.waiting.retain(|_, waiting| {
            waiting
                .rescan_deadline
                .is_none_or(|deadline| deadline > now)
        });
        for (key, retry) in &mut state.retry {
            if retry.deadline.is_some_and(|deadline| deadline <= now) {
                retry.deadline = None;
                if let Some(slot) = state.slots.remove(key) {
                    state.usage_bytes -= slot.length as u64;
                }
            }
        }
        if state.usage_bytes < previous_usage {
            self.notify_seed_waiters(state);
        }
        Ok(())
    }

    pub(crate) fn stop_teardown_worker(self: &Arc<Self>) -> Result<(), CacheError> {
        self.lock()?.teardown_after_shutdown = Some(Arc::downgrade(self));
        self.drain_teardown()
    }

    fn schedule_teardown(&self, state: MutexGuard<'_, State>) -> Result<(), CacheError> {
        if state.teardown_after_shutdown.is_some() {
            drop(state);
            self.drain_teardown()
        } else {
            self.room_wanted.notify_one();
            Ok(())
        }
    }

    pub(crate) fn drain_teardown(&self) -> Result<(), CacheError> {
        let (pieces, after_shutdown) = {
            let mut state = self.lock()?;
            (
                std::mem::take(&mut state.teardown),
                state.teardown_after_shutdown.clone(),
            )
        };
        if pieces.is_empty() {
            return Ok(());
        }
        let mut busy = Vec::new();
        let mut released = 0;
        for piece in pieces {
            // Once detached, only copies which already hold a reference can
            // share this buffer. Keep its last reference on the worker
            // until shutdown hands cleanup to the remaining copies.
            if let Some(buffer) = piece
                .copying
                .as_ref()
                .filter(|buffer| Arc::strong_count(buffer) > 1)
            {
                if let Some(cache) = &after_shutdown {
                    // The last copy now owns cleanup, even after the worker
                    // has returned. The queue's reference keeps the buffer
                    // alive while the charge is attached.
                    buffer.shutdown_charge.get_or_init(|| ShutdownCharge {
                        cache: cache.clone(),
                        length: piece.length as u64,
                    });
                } else {
                    busy.push(piece);
                }
            } else {
                released += piece.length as u64;
                drop(piece);
            }
        }
        let mut state = self.lock()?;
        state.teardown.append(&mut busy);
        self.finish_teardown(&mut state, released);
        Ok(())
    }

    fn finish_teardown(&self, state: &mut State, released: u64) {
        if released > 0 {
            state.teardown_bytes -= released;
            state.usage_bytes -= released;
            self.seed_room_changed.notify_waiters();
        }
    }

    fn notify_seed_waiters(&self, state: &State) {
        // A transition can only be asleep while detached buffers are
        // charged. Completing teardown wakes it even after the last drop.
        if state.teardown_bytes > 0 {
            self.seed_room_changed.notify_waiters();
        }
    }

    // Called after a change that may let a waiting reservation proceed, or
    // let usage above a shrunk capacity come down.
    fn room_may_have_changed(&self, state: &State) {
        if !state.waiting.is_empty() || state.shortfall(0) > 0 {
            self.room_wanted.notify_one();
        }
    }

    pub(crate) fn room_wanted(&self) -> &Notify {
        &self.room_wanted
    }

    /// The cache capacity in effect.
    pub fn capacity_bytes(&self) -> Result<u64, CacheError> {
        Ok(self.lock()?.capacity_bytes)
    }

    /// Changes the cache capacity, unless it is below the piece length of a
    /// registered torrent. A smaller capacity than cache usage leaves usage
    /// above it, admits nothing new, and wakes the evictor to evict down to
    /// it; a larger one wakes the evictor to admit waiting reservations.
    pub fn set_capacity(&self, capacity_bytes: u64) -> Result<(), CacheError> {
        let mut state = self.lock()?;
        let longest = state
            .torrents
            .values()
            .map(|entry| entry.piece_length)
            .max()
            .unwrap_or(0);
        if longest > capacity_bytes {
            return Err(CacheError::CapacityBelowPieceLength {
                piece_length_bytes: longest,
                capacity_bytes,
            });
        }
        state.capacity_bytes = capacity_bytes;
        self.room_wanted.notify_one();
        self.seed_room_changed.notify_waiters();
        Ok(())
    }

    /// Returns a snapshot of the accounting.
    pub fn stats(&self) -> Result<CacheStats, CacheError> {
        let state = self.lock()?;
        Ok(self.snapshot(&state))
    }

    fn snapshot(&self, state: &State) -> CacheStats {
        let resident = || state.slots.values().filter(|slot| slot.bytes.is_some());
        CacheStats {
            capacity_bytes: state.capacity_bytes,
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
    /// retained and detached buffers, and checks it against the running total
    /// and that nothing ever added to it beyond capacity.
    pub fn audit(&self) -> Result<CacheStats, CacheError> {
        let state = self.lock()?;
        let stats = self.snapshot(&state);
        let recomputed: u64 = state
            .slots
            .values()
            .map(|slot| slot.length as u64)
            .sum::<u64>()
            + stats.retained_bytes
            + state.teardown_bytes;
        if recomputed != stats.usage_bytes {
            return Err(CacheError::AccountingMismatch {
                counted: stats.usage_bytes,
                recomputed,
            });
        }
        if let Some((peak_bytes, capacity_bytes)) = state.exceeded {
            return Err(CacheError::CapacityViolated {
                peak_bytes,
                capacity_bytes,
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

    /// Registers a torrent of `piece_length`-byte pieces, which the
    /// capacity may not shrink below while it is registered.
    pub(crate) fn register_torrent(&self, piece_length: u32) -> Result<TorrentKey, CacheError> {
        let mut state = self.lock()?;
        state.next_key += 1;
        let key = TorrentKey(state.next_key);
        state.torrents.insert(
            key,
            TorrentEntry {
                engine: None,
                piece_length: piece_length.into(),
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
            let previous_usage = state.usage_bytes;
            state.waiting.retain(|(key, _), _| *key != torrent);
            let retries: Vec<_> = state
                .retry
                .keys()
                .copied()
                .filter(|(key, _)| *key == torrent)
                .collect();
            for key in retries {
                state.cancel_retry(key);
            }
            if state.usage_bytes < previous_usage {
                self.notify_seed_waiters(&state);
            }
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
            .chain(state.retry.keys())
            .filter(|(key, piece)| *key == torrent && !planned.contains(piece))
            .copied()
            .collect();
        let previous_usage = state.usage_bytes;
        for key in dropped {
            state.waiting.remove(&key);
            state.cancel_retry(key);
        }
        if state.usage_bytes < previous_usage {
            self.notify_seed_waiters(&state);
        }
        self.room_wanted.notify_one();
        Ok(())
    }

    /// Reserves room for a piece of `length` bytes before the engine
    /// requests it. A reservation that does not fit, including one that
    /// needs retained-buffer reclamation, or would pass pieces already
    /// waiting, is queued for the evictor.
    pub(crate) fn reserve(
        &self,
        torrent: TorrentKey,
        piece: u32,
        length: usize,
    ) -> Result<Reservation, CacheError> {
        let key = (torrent, piece);
        let mut state = self.lock()?;
        if length as u64 > state.capacity_bytes {
            return Ok(Reservation::TooLarge);
        }
        if !state.torrents.contains_key(&torrent) {
            return Err(CacheError::UnknownTorrent(torrent));
        }
        if state.slots.contains_key(&key) {
            state.retry.remove(&key);
            return Ok(Reservation::Reserved);
        }
        if let Some(waiting) = state.waiting.get_mut(&key) {
            if waiting.rescan_deadline.take().is_some() {
                self.room_wanted.notify_one();
            }
            return Ok(Reservation::Waiting);
        }
        let retry = state.retry.remove(&key);
        if !state.waiting.is_empty() || state.shortfall(length as u64) > 0 {
            let since = retry.map_or_else(|| state.tick(), |retry| retry.since);
            state.waiting.insert(
                key,
                Waiting {
                    length,
                    since,
                    rescan_deadline: None,
                },
            );
            self.room_wanted.notify_one();
            return Ok(Reservation::Waiting);
        }
        state.add_pending(key, length);
        Ok(Reservation::Reserved)
    }

    /// Releases a piece that will not complete as it is: its pending
    /// allocation, or its partial buffer. A verified piece stays.
    pub(crate) fn release_unverified(
        &self,
        torrent: TorrentKey,
        piece: u32,
    ) -> Result<(), CacheError> {
        let key = (torrent, piece);
        let mut state = self.lock()?;
        if state.slots.get(&key).is_some_and(|slot| !slot.verified) {
            state.retry.remove(&key);
            if state.defer_slot_release(key) {
                return self.schedule_teardown(state);
            } else {
                self.notify_seed_waiters(&state);
            }
        }
        self.room_may_have_changed(&state);
        Ok(())
    }

    /// Reclaims retained buffers and returns the shortfall that eviction
    /// still needs to cover, crediting the space already being released.
    pub(crate) fn room_needed(&self) -> Result<u64, CacheError> {
        let state = self.lock_reclaiming_retained(|state| Some(state.waiting_bytes()), false)?;
        Ok(state.shortfall_after_teardown(state.waiting_bytes()))
    }

    /// Readmits waiting reservations as far as the free capacity goes, in
    /// piece priority order across torrents and, among equal places, in the
    /// order they started waiting. Readmission stops at the first piece that
    /// does not fit, so smaller pieces never pass it indefinitely. Returns
    /// the torrents to wake. Each admitted piece holds a pending allocation
    /// until claimed, canceled, or expired.
    pub(crate) fn take_admissible(&self) -> Result<Vec<Arc<dyn EngineTorrent>>, CacheError> {
        self.readmit_waiting(false)
    }

    /// Admits the prefix that already fits. Torrents woken here will scan
    /// again, so any later pieces they queued in the same scan must ask
    /// again before the evictor makes room for them. They keep their place
    /// until asked for again or the rescan deadline expires.
    pub(crate) fn take_admissible_before_eviction(
        &self,
    ) -> Result<Vec<Arc<dyn EngineTorrent>>, CacheError> {
        self.readmit_waiting(true)
    }

    fn readmit_waiting(&self, rescan: bool) -> Result<Vec<Arc<dyn EngineTorrent>>, CacheError> {
        let mut state = self.lock()?;
        let state = &mut *state;
        let mut free = state.free_bytes();
        if state.usage_bytes > state.capacity_bytes
            || (free == 0 && state.waiting.values().all(|waiting| waiting.length > 0))
        {
            return Ok(Vec::new());
        }
        let mut queue: Vec<_> = state
            .waiting
            .iter()
            .map(|(&key, waiting)| {
                (
                    state.wait_order(key, waiting),
                    key,
                    waiting.length,
                    waiting.rescan_deadline.is_some(),
                )
            })
            .collect();
        queue.sort_unstable();
        let mut torrents: Vec<TorrentKey> = Vec::new();
        let deadline = Instant::now() + ADMISSION_TIMEOUT;
        for ((_, since), key, length, awaiting_rescan) in queue {
            if awaiting_rescan || length as u64 > free {
                break;
            }
            free -= length as u64;
            state.waiting.remove(&key);
            state.add_pending(key, length);
            state.retry.insert(
                key,
                Retry {
                    since,
                    deadline: Some(deadline),
                },
            );
            if !torrents.contains(&key.0) {
                torrents.push(key.0);
            }
        }
        if rescan && !torrents.is_empty() {
            for (&(key, _), waiting) in &mut state.waiting {
                if torrents.contains(&key)
                    && state
                        .torrents
                        .get(&key)
                        .is_some_and(|entry| entry.engine.is_some())
                {
                    waiting.rescan_deadline.get_or_insert(deadline);
                }
            }
        }
        Ok(torrents
            .into_iter()
            .filter_map(|torrent| state.torrents.get(&torrent)?.engine.clone())
            .collect())
    }

    /// Selects verified, unpinned pieces to make room for an unreserved
    /// write, accounting for buffers already being freed.
    pub(crate) fn write_eviction_candidates(
        &self,
        needed_bytes: u64,
    ) -> Result<Vec<EvictionBatch>, CacheError> {
        let state = self.lock()?;
        let shortfall = state.shortfall_after_teardown(needed_bytes);
        Ok(Self::select_eviction_candidates(&state, shortfall))
    }

    /// Selects eviction candidates using the current queue and teardown
    /// accounting. Returns None when no eviction or fallback is needed.
    pub(crate) fn waiting_eviction_candidates(
        &self,
    ) -> Result<Option<Vec<EvictionBatch>>, CacheError> {
        let state = self.lock()?;
        let shortfall = state.shortfall_after_teardown(state.waiting_bytes());
        Ok((shortfall > 0).then(|| Self::select_eviction_candidates(&state, shortfall)))
    }

    /// Orders eligible pieces by whether their torrent is started, then by
    /// least recent use, and groups the selected pieces by torrent.
    fn select_eviction_candidates(state: &State, mut shortfall: u64) -> Vec<EvictionBatch> {
        if shortfall == 0 {
            return Vec::new();
        }
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
        batches.into_iter().map(|(_, batch)| batch).collect()
    }

    /// Removes a verified piece during completion invalidation. Its buffer
    /// stays charged until it is freed, or remains charged for reuse.
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
            state.eviction_count += 1;
            let Some(detached) = state.remove_slot((torrent, piece)) else {
                self.room_may_have_changed(&state);
                self.notify_seed_waiters(&state);
                return Ok(());
            };
            if detached.copying.is_some() {
                state.teardown.push(detached);
                return self.schedule_teardown(state);
            }
            detached
        };
        let released = freed.length as u64;
        drop(freed.owned);
        let mut state = self.lock()?;
        self.finish_teardown(&mut state, released);
        self.room_may_have_changed(&state);
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
        provided: &mut Option<PieceBytes>,
    ) -> Result<Option<Buffer>, CacheError> {
        let mut state = self.lock_reclaiming_retained(
            |state| {
                (state.shortfall(piece_length as u64) > 0
                    && state.torrents.contains_key(&torrent)
                    && !state.slots.contains_key(&(torrent, piece)))
                .then_some(piece_length as u64)
            },
            true,
        )?;
        if !state.torrents.contains_key(&torrent) {
            return Err(CacheError::UnknownTorrent(torrent));
        }
        let key = (torrent, piece);
        if !state.slots.contains_key(&key) {
            let needed_bytes = piece_length as u64;
            if state.shortfall(needed_bytes) > 0 {
                return Err(CacheError::CapacityExceeded {
                    needed_bytes,
                    usage_bytes: state.usage_bytes,
                    capacity_bytes: state.capacity_bytes,
                });
            }
            state.add_pending(key, piece_length);
            if state.waiting.remove(&key).is_some() {
                self.room_may_have_changed(&state);
            }
        }
        let reused = match state.slots.get(&key) {
            Some(slot)
                if slot.length == piece_length && slot.bytes.is_none() && provided.is_none() =>
            {
                state.take_retained(piece_length)
            }
            _ => None,
        };
        let slot = state
            .slots
            .get(&key)
            .ok_or(CacheError::PieceMissing { torrent, piece })?;
        if slot.length != piece_length {
            return Err(CacheError::LengthMismatch {
                piece,
                reserved: slot.length,
                requested: piece_length,
            });
        }
        state.retry.remove(&key);
        let slot = state
            .slots
            .get_mut(&key)
            .ok_or(CacheError::PieceMissing { torrent, piece })?;
        if slot.bytes.is_none()
            && let Some(bytes) = reused.or_else(|| provided.take())
        {
            slot.bytes = Some(Arc::new(PieceBuffer {
                bytes: Mutex::new(bytes),
                shutdown_charge: OnceLock::new(),
            }));
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
            provided = Some(PieceBytes::allocate(piece_length)?);
        };
        // A buffer allocated for a slot that got one meanwhile is dropped
        // here, outside the cache lock.
        drop(provided);
        let mut bytes = buffer.bytes.lock().map_err(|_| CacheError::Poisoned)?;
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
        self.read_slot(torrent, piece, offset, buf, false)
    }

    /// Reads a whole piece into `buf`, only if it is verified. A piece
    /// evicted and requested again since it was verified is partial, and
    /// reads as missing; readers rely on the engine's piece completion for
    /// that instead.
    pub(crate) fn read_verified(
        &self,
        torrent: TorrentKey,
        piece: u32,
        buf: &mut [u8],
    ) -> Result<(), CacheError> {
        self.read_slot(torrent, piece, 0, buf, true)
    }

    fn read_slot(
        &self,
        torrent: TorrentKey,
        piece: u32,
        offset: usize,
        buf: &mut [u8],
        verified_only: bool,
    ) -> Result<(), CacheError> {
        // A verified piece's buffer never changes: it is only removed, and
        // the clone taken here keeps it from being reused.
        let (buffer, length) = {
            let mut state = self.lock()?;
            let now = state.tick();
            let slot = state
                .slots
                .get_mut(&(torrent, piece))
                .filter(|slot| slot.verified || !verified_only)
                .ok_or(CacheError::PieceMissing { torrent, piece })?;
            let buffer = slot
                .bytes
                .clone()
                .ok_or(CacheError::PieceMissing { torrent, piece })?;
            slot.last_use = now;
            (buffer, slot.length)
        };
        let end = offset + buf.len();
        let bytes = buffer.bytes.lock().map_err(|_| CacheError::Poisoned)?;
        let source = bytes.get(offset..end).ok_or(CacheError::OutOfBounds {
            piece,
            offset,
            end,
            length,
        })?;
        buf.copy_from_slice(source);
        Ok(())
    }

    /// The verified pieces of a torrent that the cache holds.
    pub(crate) fn verified_pieces(&self, torrent: TorrentKey) -> Result<Vec<u32>, CacheError> {
        let state = self.lock()?;
        let mut pieces: Vec<u32> = state
            .slots
            .iter()
            .filter(|((key, _), slot)| *key == torrent && slot.verified)
            .map(|((_, piece), _)| *piece)
            .collect();
        pieces.sort_unstable();
        Ok(pieces)
    }

    /// Stores `bytes` as verified piece `piece` of a torrent, as a storage
    /// mode transition does with pieces read from disk. It fails, storing
    /// nothing, when the piece does not fit in the room left.
    pub(crate) fn seed(
        &self,
        torrent: TorrentKey,
        piece: u32,
        bytes: &[u8],
    ) -> Result<(), CacheError> {
        self.write(torrent, piece, bytes.len(), 0, bytes)?;
        self.mark_verified(torrent, piece)
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

    /// Detaches every piece of a torrent. The evictor frees discarded
    /// buffers before their bytes become available for new allocations.
    pub(crate) fn release_torrent(&self, torrent: TorrentKey) -> Result<(), CacheError> {
        let mut state = self.lock()?;
        state.torrents.remove(&torrent);
        state.waiting.retain(|(key, _), _| *key != torrent);
        state.retry.retain(|(key, _), _| *key != torrent);
        let keys: Vec<_> = state
            .slots
            .keys()
            .filter(|(key, _)| *key == torrent)
            .copied()
            .collect();
        let mut scheduled = false;
        for key in keys {
            scheduled |= state.defer_slot_release(key);
        }
        self.notify_seed_waiters(&state);
        if scheduled {
            return self.schedule_teardown(state);
        }
        self.room_may_have_changed(&state);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "linux")]
    #[test]
    fn large_piece_uses_a_zero_initialized_mapping() {
        let length = MAPPED_PIECE_MIN_BYTES;
        let cache = PieceCache::new(length as u64);
        let torrent = cache.register_torrent(length as u32).unwrap();
        cache.write(torrent, 0, length, length - 2, &[7]).unwrap();

        let mut tail = [255; 4];
        cache
            .read(torrent, 0, length - tail.len(), &mut tail)
            .unwrap();
        assert_eq!(tail, [0, 0, 7, 0]);
        let piece = cache.lock().unwrap().slots[&(torrent, 0)]
            .bytes
            .clone()
            .unwrap();
        assert!(matches!(
            *piece.bytes.lock().unwrap(),
            PieceBytes::Mapped(_)
        ));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn large_pieces_fall_back_to_the_heap_until_a_mapping_is_released() {
        static AVAILABLE: AtomicUsize = AtomicUsize::new(1);
        let length = MAPPED_PIECE_MIN_BYTES;
        let mapped =
            PieceBytes::allocate_large(length, &AVAILABLE, memmap2::MmapMut::map_anon).unwrap();
        assert!(matches!(mapped, PieceBytes::Mapped(_)));

        let mut heap = PieceBytes::allocate_large(length, &AVAILABLE, |_| {
            panic!("the exhausted budget should skip mmap")
        })
        .unwrap();
        assert!(matches!(heap, PieceBytes::Heap(_)));
        assert_eq!(heap[length - 1], 0);
        heap[length - 1] = 7;

        drop(mapped);
        let oversized = isize::MAX as usize + 1;
        assert!(matches!(
            PieceBytes::allocate_large(oversized, &AVAILABLE, memmap2::MmapMut::map_anon),
            Err(CacheError::AllocationFailed { .. })
        ));
        let mapped =
            PieceBytes::allocate_large(length, &AVAILABLE, memmap2::MmapMut::map_anon).unwrap();
        assert!(matches!(mapped, PieceBytes::Mapped(_)));
        assert_eq!(heap[length - 1], 7);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn refused_mapping_uses_a_zeroed_heap_buffer_and_returns_its_permit() {
        static AVAILABLE: AtomicUsize = AtomicUsize::new(1);
        let length = MAPPED_PIECE_MIN_BYTES;
        let heap = PieceBytes::allocate_large(length, &AVAILABLE, |_| {
            Err(std::io::ErrorKind::OutOfMemory.into())
        })
        .unwrap();
        assert!(matches!(heap, PieceBytes::Heap(_)));
        assert_eq!(heap[length - 1], 0);
        let mapped =
            PieceBytes::allocate_large(length, &AVAILABLE, memmap2::MmapMut::map_anon).unwrap();
        assert!(matches!(mapped, PieceBytes::Mapped(_)));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn failed_piece_allocation_leaves_only_a_releasable_reservation() {
        // The mapping library rejects lengths that cannot be Rust slices.
        let length = isize::MAX as usize + 1;
        let cache = PieceCache::new(length as u64);
        let torrent = cache.register_torrent(1).unwrap();

        assert!(matches!(
            cache.write(torrent, 0, length, 0, &[7]),
            Err(CacheError::AllocationFailed { .. })
        ));
        let stats = cache.audit().unwrap();
        assert_eq!((stats.pending_pieces, stats.resident_pieces), (1, 0));
        cache.release_unverified(torrent, 0).unwrap();
        assert_eq!(cache.audit().unwrap().usage_bytes, 0);
        cache.seed(torrent, 0, &[7]).unwrap();
        assert_eq!(cache.audit().unwrap().usage_bytes, 1);
    }

    #[test]
    fn retained_buffers_stay_charged_during_teardown() {
        let cache = PieceCache::new(80);
        let torrent = cache.register_torrent(80).unwrap();
        cache.seed(torrent, 0, &[7; 80]).unwrap();
        cache.evict(torrent, 0).unwrap();
        let (buffers, bytes) = cache.lock().unwrap().detach_retained(80, 0);

        let stats = cache.audit().unwrap();
        assert_eq!((stats.usage_bytes, stats.retained_bytes), (80, 80));
        assert_eq!(cache.reserve(torrent, 1, 80).unwrap(), Reservation::Waiting);
        assert!(cache.take_admissible().unwrap().is_empty());
        assert_eq!(cache.audit().unwrap().waiting_pieces, 1);
        assert_eq!(cache.room_needed().unwrap(), 0);

        drop(buffers);
        drop(cache.finish_retained_release(bytes).unwrap());
        assert_eq!(cache.audit().unwrap().usage_bytes, 0);
        cache.take_admissible().unwrap();
        assert_eq!(
            cache.reserve(torrent, 1, 80).unwrap(),
            Reservation::Reserved
        );
    }

    struct NoEngine;

    impl EngineTorrent for NoEngine {
        fn invalidate(&self, _pieces: &[u32]) -> anyhow::Result<usize> {
            Ok(0)
        }

        fn reselect(&self) {}
    }

    fn evictable(cache: &PieceCache) -> TorrentKey {
        let torrent = cache.register_torrent(0).unwrap();
        cache.attach_engine(torrent, Arc::new(NoEngine)).unwrap();
        torrent
    }

    fn verified(cache: &PieceCache, torrent: TorrentKey, piece: u32, length: usize) {
        cache.write(torrent, piece, length, 0, &[0]).unwrap();
        cache.mark_verified(torrent, piece).unwrap();
    }

    fn candidates(cache: &PieceCache, needed_bytes: u64) -> Vec<Vec<u32>> {
        cache
            .write_eviction_candidates(needed_bytes)
            .unwrap()
            .into_iter()
            .map(|batch| batch.pieces)
            .collect()
    }

    /// CAC-08, and CAC-07's partial pieces, which stay resident.
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
        let unattached = cache.register_torrent(0).unwrap();
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
    fn eviction_counts_and_retains_a_reusable_buffer() {
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
            "the evicted buffer is retained and still counted"
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
        let torrent = cache.register_torrent(0).unwrap();
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
        let torrent = cache.register_torrent(0).unwrap();
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
        let torrent = cache.register_torrent(0).unwrap();
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
        let first = cache.register_torrent(0).unwrap();
        let second = cache.register_torrent(0).unwrap();
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
    fn bulk_removal_keeps_discarded_buffers_charged_until_teardown() {
        let cache = PieceCache::new(100);
        let removed = evictable(&cache);
        let remaining = evictable(&cache);
        verified(&cache, removed, 0, 30);
        verified(&cache, removed, 1, 30);
        reserved(&cache, removed, 2, 20);
        reserved(&cache, remaining, 0, 20);
        let copy = cache.lock().unwrap().slots[&(removed, 0)]
            .bytes
            .clone()
            .unwrap();
        assert_eq!(
            cache.reserve(remaining, 1, 60).unwrap(),
            Reservation::Waiting
        );

        cache.release_torrent(removed).unwrap();
        let stats = cache.audit().unwrap();
        assert_eq!((stats.usage_bytes, stats.resident_pieces), (80, 0));
        assert!(cache.take_admissible().unwrap().is_empty());
        cache.drain_teardown().unwrap();
        assert_eq!(cache.audit().unwrap().usage_bytes, 50);
        assert!(cache.take_admissible().unwrap().is_empty());

        drop(copy);
        cache.drain_teardown().unwrap();
        assert_eq!(cache.audit().unwrap().usage_bytes, 20);
        assert_eq!(cache.take_admissible().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn seed_room_waits_for_removed_buffers_to_be_released() {
        let cache = PieceCache::new(160);
        let removed = evictable(&cache);
        let target = evictable(&cache);
        for piece in 0..16 {
            verified(&cache, removed, piece, 10);
        }
        cache.release_torrent(removed).unwrap();
        assert_eq!(cache.audit().unwrap().retained_bytes, 40);

        let wait = cache.wait_for_seed_room(50, Duration::from_secs(1));
        tokio::pin!(wait);
        tokio::select! {
            biased;
            result = &mut wait => panic!("the seed batch still needs detached capacity: {result:?}"),
            () = std::future::ready(()) => {}
        }
        cache.drain_teardown().unwrap();
        assert!(wait.await.unwrap());
        for piece in 0..5 {
            cache.seed(target, piece, &[7; 10]).unwrap();
        }
        assert_eq!(cache.verified_pieces(target).unwrap(), [0, 1, 2, 3, 4]);
        assert_eq!(cache.audit().unwrap().usage_bytes, 50);
    }

    #[tokio::test]
    async fn seed_room_wakes_when_a_pending_reservation_is_released() {
        let cache = PieceCache::new(100);
        let removed = evictable(&cache);
        let active = evictable(&cache);
        reserved(&cache, active, 0, 60);
        verified(&cache, removed, 0, 40);
        let copy = cache.lock().unwrap().slots[&(removed, 0)]
            .bytes
            .clone()
            .unwrap();
        cache.release_torrent(removed).unwrap();

        let wait = cache.wait_for_seed_room(40, Duration::from_secs(1));
        tokio::pin!(wait);
        tokio::select! {
            biased;
            result = &mut wait => panic!("seed capacity is still occupied: {result:?}"),
            () = std::future::ready(()) => {}
        }

        cache.release_unverified(active, 0).unwrap();
        tokio::select! {
            biased;
            result = &mut wait => assert!(result.unwrap()),
            () = std::future::ready(()) => panic!("releasing the reservation did not wake the seed wait"),
        }
        assert_eq!(cache.audit().unwrap().usage_bytes, 40);
        cache.seed(active, 1, &[7; 40]).unwrap();
        drop(copy);
        cache.drain_teardown().unwrap();
        assert_eq!(cache.audit().unwrap().usage_bytes, 40);
    }

    #[tokio::test]
    async fn seed_room_wait_is_bounded_and_skips_unrecoverable_shortfalls() {
        let cache = PieceCache::new(100);
        let removed = evictable(&cache);
        let active = evictable(&cache);
        reserved(&cache, active, 0, 60);
        verified(&cache, removed, 0, 40);
        let copy = cache.lock().unwrap().slots[&(removed, 0)]
            .bytes
            .clone()
            .unwrap();
        cache.release_torrent(removed).unwrap();

        assert!(!cache.wait_for_seed_room(40, Duration::ZERO).await.unwrap());
        assert!(cache.wait_for_seed_room(41, Duration::ZERO).await.unwrap());
        assert!(cache.wait_for_seed_room(0, Duration::ZERO).await.unwrap());
        drop(copy);
        cache.drain_teardown().unwrap();
    }

    #[test]
    fn writes_past_the_piece_or_with_another_length_fail() {
        let cache = PieceCache::new(100);
        let torrent = cache.register_torrent(0).unwrap();
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

    /// CAC-07: a piece being written counts as a pending allocation.
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
        cache.drain_teardown().unwrap();
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
        assert_eq!(cache.room_needed().unwrap(), 10);
        assert_eq!(
            cache.waiting_eviction_candidates().unwrap().unwrap()[0].pieces,
            [0],
            "40 bytes are free, so one 30-byte piece makes room for 50"
        );
    }

    #[test]
    fn eviction_covers_the_shortfall_while_a_detached_buffer_is_held() {
        let cache = PieceCache::new(100);
        let torrent = evictable(&cache);
        verified(&cache, torrent, 0, 30);
        verified(&cache, torrent, 1, 30);
        reserved(&cache, torrent, 2, 30);
        cache.write(torrent, 3, 10, 0, &[1]).unwrap();
        let copy = cache.lock().unwrap().slots[&(torrent, 3)]
            .bytes
            .clone()
            .unwrap();
        cache.release_unverified(torrent, 3).unwrap();
        assert_eq!(cache.reserve(torrent, 4, 40).unwrap(), Reservation::Waiting);
        cache.drain_teardown().unwrap();
        assert_eq!(cache.audit().unwrap().usage_bytes, 100);

        assert_eq!(cache.room_needed().unwrap(), 30);
        let batches = cache.waiting_eviction_candidates().unwrap().unwrap();
        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0].pieces, [0]);
        cache.evict(torrent, 0).unwrap();
        assert!(cache.is_resident(torrent, 1).unwrap());
        assert_eq!(cache.audit().unwrap().usage_bytes, 70);
        assert_eq!(cache.room_needed().unwrap(), 0);
        assert!(cache.waiting_eviction_candidates().unwrap().is_none());
        assert!(cache.take_admissible().unwrap().is_empty());

        drop(copy);
        cache.drain_teardown().unwrap();
        assert_eq!(cache.take_admissible().unwrap().len(), 1);
        assert_eq!(cache.audit().unwrap().usage_bytes, 100);
    }

    #[test]
    fn verified_eviction_keeps_capacity_charged_during_an_in_flight_read() {
        let cache = PieceCache::new(40);
        let original = evictable(&cache);
        let waiting = evictable(&cache);
        cache.seed(original, 0, &[7; 40]).unwrap();

        std::thread::scope(|scope| {
            let buffer = cache.lock().unwrap().slots[&(original, 0)]
                .bytes
                .clone()
                .unwrap();
            let locked = buffer.bytes.lock().unwrap();
            let read = scope.spawn(|| {
                let mut bytes = [0; 40];
                cache.read_verified(original, 0, &mut bytes).unwrap();
                bytes
            });
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            while Arc::strong_count(&buffer) < 3 {
                assert!(
                    std::time::Instant::now() < deadline,
                    "the reader did not acquire the buffer"
                );
                std::thread::yield_now();
            }

            cache.evict(original, 0).unwrap();
            let resident = cache.is_resident(original, 0).unwrap();
            cache.drain_teardown().unwrap();
            let usage = cache.audit().unwrap().usage_bytes;
            let reservation = cache.reserve(waiting, 0, 40).unwrap();
            let admitted = cache.take_admissible().unwrap();

            drop(locked);
            drop(buffer);
            assert_eq!(read.join().unwrap(), [7; 40]);
            assert!(!resident);
            assert_eq!(usage, 40);
            assert_eq!(reservation, Reservation::Waiting);
            assert!(admitted.is_empty());
        });

        cache.drain_teardown().unwrap();
        assert_eq!(cache.audit().unwrap().usage_bytes, 0);
        assert_eq!(cache.take_admissible().unwrap().len(), 1);
        reserved(&cache, waiting, 0, 40);
    }

    #[tokio::test]
    async fn detached_buffers_release_capacity_after_the_evictor_stops() {
        let cache = PieceCache::new(80);
        let torrent = evictable(&cache);
        cache.seed(torrent, 0, &[7; 40]).unwrap();
        cache.seed(torrent, 1, &[8; 40]).unwrap();
        let (first, second) = {
            let state = cache.lock().unwrap();
            (
                state.slots[&(torrent, 0)].bytes.clone().unwrap(),
                state.slots[&(torrent, 1)].bytes.clone().unwrap(),
            )
        };
        let evictor = crate::evictor::Evictor::spawn(cache.clone());
        cache.evict(torrent, 0).unwrap();
        evictor.shutdown().await.unwrap();
        cache.release_torrent(torrent).unwrap();
        assert_eq!(cache.audit().unwrap().usage_bytes, 80);

        drop(first);
        assert_eq!(cache.audit().unwrap().usage_bytes, 40);
        drop(second);
        assert_eq!(cache.audit().unwrap().usage_bytes, 0);
    }

    #[test]
    fn a_smaller_capacity_admits_nothing_until_eviction_brings_usage_down() {
        let cache = PieceCache::new(300);
        let torrent = evictable(&cache);
        for piece in 0..3 {
            verified(&cache, torrent, piece, 100);
        }
        cache.set_capacity(150).unwrap();
        let stats = cache.audit().unwrap();
        assert_eq!((stats.capacity_bytes, stats.usage_bytes), (150, 300));
        assert_eq!(
            cache.reserve(torrent, 3, 50).unwrap(),
            Reservation::Waiting,
            "nothing is admitted above capacity"
        );
        assert_eq!(
            cache.room_needed().unwrap(),
            200,
            "the 150 bytes above capacity and the waiting 50"
        );
        let batches = cache.waiting_eviction_candidates().unwrap().unwrap();
        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0].pieces, [0, 1]);
        cache.evict(torrent, 0).unwrap();
        cache.evict(torrent, 1).unwrap();
        let stats = cache.audit().unwrap();
        assert_eq!(
            (stats.usage_bytes, stats.retained_bytes),
            (100, 0),
            "nothing is retained while usage is above capacity or pieces wait"
        );
        assert_eq!(cache.take_admissible().unwrap().len(), 1);
        reserved(&cache, torrent, 3, 50);
        assert_eq!(cache.audit().unwrap().usage_bytes, 150);
    }

    #[test]
    fn usage_above_a_smaller_capacity_wants_eviction_without_waiting_pieces() {
        let cache = PieceCache::new(200);
        let torrent = evictable(&cache);
        verified(&cache, torrent, 0, 100);
        cache.write(torrent, 1, 100, 0, &[0]).unwrap();
        cache.set_capacity(100).unwrap();
        assert_eq!(cache.room_needed().unwrap(), 100);
        let batches = cache.waiting_eviction_candidates().unwrap().unwrap();
        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0].pieces, [0]);
        cache.evict(torrent, 0).unwrap();
        assert_eq!(
            cache.room_needed().unwrap(),
            0,
            "the partial piece alone fits"
        );
        assert_eq!(
            cache.write(torrent, 2, 100, 0, &[0]),
            Err(CacheError::CapacityExceeded {
                needed_bytes: 100,
                usage_bytes: 100,
                capacity_bytes: 100
            }),
            "an unreserved write is checked against the new capacity"
        );
    }

    #[test]
    fn a_larger_capacity_admits_waiting_pieces() {
        let cache = PieceCache::new(100);
        let torrent = evictable(&cache);
        verified(&cache, torrent, 0, 100);
        cache
            .set_plan(torrent, HashSet::from([0]), &[0, 1])
            .unwrap();
        assert_eq!(
            cache.reserve(torrent, 1, 100).unwrap(),
            Reservation::Waiting
        );
        assert!(cache.take_admissible().unwrap().is_empty());
        cache.set_capacity(200).unwrap();
        assert_eq!(cache.take_admissible().unwrap().len(), 1);
        reserved(&cache, torrent, 1, 100);
        assert_eq!(
            cache.reserve(torrent, 2, 150).unwrap(),
            Reservation::Waiting,
            "a piece that was too large now fits the capacity"
        );
        cache.audit().unwrap();
    }

    #[test]
    fn a_capacity_below_a_registered_piece_length_is_refused() {
        let cache = PieceCache::new(300);
        let torrent = cache.register_torrent(200).unwrap();
        assert_eq!(
            cache.set_capacity(150),
            Err(CacheError::CapacityBelowPieceLength {
                piece_length_bytes: 200,
                capacity_bytes: 150
            })
        );
        assert_eq!(cache.capacity_bytes().unwrap(), 300, "nothing changed");
        cache.set_capacity(200).unwrap();
        cache.release_torrent(torrent).unwrap();
        cache.set_capacity(150).unwrap();
        assert_eq!(cache.capacity_bytes().unwrap(), 150);
    }

    #[test]
    fn the_audit_reports_usage_added_beyond_capacity() {
        let cache = PieceCache::new(100);
        let torrent = evictable(&cache);
        verified(&cache, torrent, 0, 100);
        cache.set_capacity(50).unwrap();
        cache
            .audit()
            .expect("usage left above a smaller capacity is no violation");
        {
            // An addition beyond capacity, undone so the totals still agree.
            let mut state = cache.lock().unwrap();
            state.add(10);
            state.usage_bytes -= 10;
        }
        assert_eq!(
            cache.audit(),
            Err(CacheError::CapacityViolated {
                peak_bytes: 110,
                capacity_bytes: 50
            })
        );
    }

    #[test]
    fn a_piece_downloaded_again_reads_as_missing_until_verified() {
        let cache = PieceCache::new(100);
        let torrent = evictable(&cache);
        verified(&cache, torrent, 0, 10);
        let mut buf = [0; 10];
        cache.read_verified(torrent, 0, &mut buf).unwrap();
        cache.evict(torrent, 0).unwrap();
        // The engine requests it again, and its first block arrives.
        cache.write(torrent, 0, 10, 0, &[1]).unwrap();
        cache
            .read(torrent, 0, 0, &mut buf)
            .expect("readers read partial pieces, which the engine guards");
        assert_eq!(
            cache.read_verified(torrent, 0, &mut buf),
            Err(CacheError::PieceMissing { torrent, piece: 0 }),
            "a partial piece is not a verified one"
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
                RETAINED_BUFFER_LIMIT as u64 * 100,
                RETAINED_BUFFER_LIMIT as u64 * 100
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
    fn reservations_leave_retained_buffers_for_the_evictor() {
        let cache = PieceCache::new(100);
        let torrent = evictable(&cache);
        verified(&cache, torrent, 0, 60);
        cache.evict(torrent, 0).unwrap();
        assert_eq!(cache.reserve(torrent, 1, 80).unwrap(), Reservation::Waiting);
        let stats = cache.audit().unwrap();
        assert_eq!((stats.usage_bytes, stats.retained_bytes), (60, 60));
        assert_eq!(
            cache.room_needed().unwrap(),
            0,
            "reclamation made enough room"
        );
        assert_eq!(cache.take_admissible().unwrap().len(), 1);
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
    fn eviction_skips_retention_while_a_reservation_waits() {
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
    fn an_admission_holds_room_before_the_peer_claims_it() {
        let cache = PieceCache::new(20);
        let first = evictable(&cache);
        let second = evictable(&cache);
        let filler = evictable(&cache);
        reserved(&cache, filler, 0, 20);
        for (torrent, piece) in [(first, 0), (first, 1), (second, 0)] {
            assert_eq!(
                cache.reserve(torrent, piece, 20).unwrap(),
                Reservation::Waiting
            );
        }
        cache.release_unverified(filler, 0).unwrap();
        assert_eq!(cache.take_admissible_before_eviction().unwrap().len(), 1);
        assert_eq!(cache.audit().unwrap().usage_bytes, 20);
        assert!(cache.take_admissible().unwrap().is_empty());
        assert_eq!(cache.reserve(first, 1, 20).unwrap(), Reservation::Waiting);
        reserved(&cache, first, 0, 20);
        assert_eq!(cache.audit().unwrap().usage_bytes, 20);
    }

    #[test]
    fn unclaimed_admissions_release_room_on_plan_change_or_expiry() {
        let cache = PieceCache::new(20);
        let torrent = evictable(&cache);
        let filler = evictable(&cache);
        reserved(&cache, filler, 0, 20);
        cache.set_plan(torrent, HashSet::new(), &[0, 1]).unwrap();
        for piece in 0..2 {
            assert_eq!(
                cache.reserve(torrent, piece, 20).unwrap(),
                Reservation::Waiting
            );
        }
        cache.release_unverified(filler, 0).unwrap();
        assert_eq!(cache.take_admissible().unwrap().len(), 1);
        cache.set_plan(torrent, HashSet::new(), &[1]).unwrap();
        assert_eq!(cache.audit().unwrap().usage_bytes, 0);
        assert_eq!(cache.take_admissible().unwrap().len(), 1);
        cache
            .expire_reservations(cache.maintenance_deadline().unwrap().unwrap())
            .unwrap();
        assert_eq!(cache.audit().unwrap().usage_bytes, 0);
        assert!(cache.take_admissible().unwrap().is_empty());
    }

    #[test]
    fn discarded_partial_buffers_stay_charged_until_the_worker_can_free_them() {
        let cache = PieceCache::new(100);
        let torrent = evictable(&cache);
        reserved(&cache, torrent, 0, 80);
        cache.write(torrent, 0, 80, 0, &[1]).unwrap();
        let copy = cache.lock().unwrap().slots[&(torrent, 0)]
            .bytes
            .clone()
            .unwrap();
        assert_eq!(cache.reserve(torrent, 1, 40).unwrap(), Reservation::Waiting);
        cache.release_unverified(torrent, 0).unwrap();
        assert_eq!(cache.audit().unwrap().usage_bytes, 80);
        cache.drain_teardown().unwrap();
        assert_eq!(cache.audit().unwrap().usage_bytes, 80);
        assert!(cache.take_admissible().unwrap().is_empty());

        drop(copy);
        cache.drain_teardown().unwrap();
        assert_eq!(cache.audit().unwrap().usage_bytes, 0);
        assert_eq!(cache.take_admissible().unwrap().len(), 1);
    }

    #[test]
    fn a_late_write_claims_a_waiting_piece_before_readmission() {
        let cache = PieceCache::new(100);
        let torrent = evictable(&cache);
        let filler = evictable(&cache);
        reserved(&cache, filler, 0, 80);
        assert_eq!(cache.reserve(torrent, 0, 30).unwrap(), Reservation::Waiting);
        cache.release_unverified(filler, 0).unwrap();

        cache.write(torrent, 0, 30, 0, &[7]).unwrap();
        assert!(cache.take_admissible().unwrap().is_empty());
        let mut data = [0];
        cache.read(torrent, 0, 0, &mut data).unwrap();
        assert_eq!(data, [7]);
        assert_eq!(cache.audit().unwrap().usage_bytes, 30);
    }

    #[test]
    fn an_unreserved_write_reclaims_retained_room_during_partial_teardown() {
        let cache = PieceCache::new(100);
        let torrent = evictable(&cache);
        verified(&cache, torrent, 0, 40);
        cache.evict(torrent, 0).unwrap();
        cache.write(torrent, 1, 60, 0, &[1]).unwrap();
        let copy = cache.lock().unwrap().slots[&(torrent, 1)]
            .bytes
            .clone()
            .unwrap();
        cache.release_unverified(torrent, 1).unwrap();
        assert_eq!(cache.audit().unwrap().retained_bytes, 40);

        cache.write(torrent, 2, 30, 0, &[2]).unwrap();
        let stats = cache.audit().unwrap();
        assert_eq!((stats.usage_bytes, stats.retained_bytes), (90, 0));
        drop(copy);
        cache.drain_teardown().unwrap();
        assert_eq!(cache.audit().unwrap().usage_bytes, 30);
    }

    #[test]
    fn rescanned_requests_keep_their_place_among_equally_ranked_torrents() {
        let cache = PieceCache::new(100);
        let first = evictable(&cache);
        let second = evictable(&cache);
        let filler = evictable(&cache);
        reserved(&cache, filler, 0, 60);
        reserved(&cache, filler, 1, 20);
        verified(&cache, filler, 2, 20);
        cache.evict(filler, 2).unwrap();
        cache.set_plan(first, HashSet::new(), &[0, 1]).unwrap();
        cache.set_plan(second, HashSet::new(), &[0, 1]).unwrap();
        for (torrent, piece) in [(first, 0), (first, 1), (second, 1)] {
            assert_eq!(
                cache.reserve(torrent, piece, 20).unwrap(),
                Reservation::Waiting
            );
        }

        cache.room_needed().unwrap();
        assert_eq!(cache.take_admissible_before_eviction().unwrap().len(), 1);
        assert_eq!(cache.stats().unwrap().waiting_pieces, 2);
        reserved(&cache, first, 0, 20);
        assert_eq!(cache.reserve(first, 1, 20).unwrap(), Reservation::Waiting);
        cache.release_unverified(filler, 1).unwrap();
        assert_eq!(cache.take_admissible().unwrap().len(), 1);

        assert_eq!(cache.reserve(second, 1, 20).unwrap(), Reservation::Waiting);
        reserved(&cache, first, 1, 20);
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
        assert!(stats.retained_bytes <= RETAINED_BUFFER_LIMIT as u64 * 1024);
    }
}
