// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! Preloads across the torrents of one engine, as the lifecycle's Preloads
//! section defines them.
//!
//! [`Preloads`] holds every preload in creation order. Each torrent's
//! scheduler requests the pieces of its torrent's downloading and ready
//! preloads after its readers' pieces, and pins them in memory storage;
//! [`Preloads`] tells it which, admits queued preloads as the preloads'
//! share of the readahead budget allows, and moves preloads through their
//! states on a timer: as their pieces arrive, at their expiry, and when they
//! stall or their torrents stop. The newest queued preload starts first, and
//! reclaims older preloads' pins when they leave it no room, since a client
//! switching files wants the one it asked for last.

use std::collections::HashSet;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::time::{Duration, SystemTime};

use tokio::sync::Notify;
use tokio::task::JoinHandle;
use tokio::time::Instant;
use torrplay_core::engine::{ByteRange, EngineError};
use torrplay_core::id::{PreloadId, TorrentId};
use torrplay_core::lifecycle::Failure;
use torrplay_core::metadata::FileIndex;
use torrplay_core::preload::{
    self, PRELOAD_MAX_BYTES, PreloadRequest, PreloadState, PreloadStatus, TAIL_BYTES,
};

use crate::playback::Playback;
use crate::scheduler::{ScheduledPreload, TorrentScheduler};
use crate::storage::Layout;

/// How often downloading preloads check which of their pieces arrived.
const PROGRESS_INTERVAL: Duration = Duration::from_millis(250);

/// The failure of a preload whose torrent was paused.
const PAUSED: &str = "The torrent was paused.";

/// The failure of a preload whose torrent failed.
const TORRENT_FAILED: &str = "The torrent failed.";

/// The failure of a preload whose pieces stopped arriving.
const STALLED: &str = "No data of the preload arrived within the piece wait bound.";

/// The failure of a preload the cache's bounded fallback ended.
const CACHE_NEEDED_ROOM: &str = "The piece cache needed the preload's pins for active readers.";

/// The failure of a preload a newer one reclaimed.
const RECLAIMED: &str = "A newer preload needed the room this preload held.";

/// What a preload covers: its pieces, in ascending order, and the file's
/// byte ranges they hold.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Coverage {
    pub(crate) pieces: Vec<u32>,
    pub(crate) ranges: Vec<ByteRange>,
    // The file's offset in the torrent, which the ranges are relative to.
    file_start: u64,
}

/// Plans what a preload of file `file` covers, in whole pieces, within
/// `limit_bytes` and at least one piece: the whole file if it fits, its
/// head and tail without `offset`, and its first and last pieces with a
/// window at `offset` otherwise. `None` for an empty or unknown file.
pub(crate) fn coverage(
    layout: &Layout,
    file: usize,
    offset: Option<u64>,
    limit_bytes: u64,
) -> Option<Coverage> {
    let &(file_start, file_length) = layout.files.get(file)?;
    let file_end = file_start.checked_add(file_length.checked_sub(1)?)?;
    let piece_length = u64::from(layout.lengths.default_piece_length());
    let first = file_start / piece_length;
    let last = file_end / piece_length;
    let limit = (limit_bytes / piece_length).max(1);
    let span = last - first + 1;
    let mut pieces: Vec<u64> = if span <= limit {
        (first..=last).collect()
    } else if let Some(offset) = offset.filter(|_| limit >= 3) {
        // The first and last pieces hold the container's header and index;
        // the rest is a window that starts an eighth of itself before the
        // offset, so a player's first reads land inside it.
        let window = limit - 2;
        let lead = window * piece_length / 8;
        let start = (file_start + offset.saturating_sub(lead)) / piece_length;
        let start = start.min(last + 1 - window).max(first);
        let mut pieces = vec![first, last];
        pieces.extend(start..start + window);
        pieces
    } else {
        // At least one head piece; a single piece is the head alone.
        let tail = match limit {
            1 => 0,
            _ => TAIL_BYTES.div_ceil(piece_length).clamp(1, limit - 1),
        };
        let head = limit - tail;
        (first..first + head)
            .chain(last + 1 - tail..=last)
            .collect()
    };
    pieces.sort_unstable();
    pieces.dedup();
    let mut ranges: Vec<ByteRange> = Vec::new();
    for &piece in &pieces {
        let start = (piece * piece_length).max(file_start) - file_start;
        let end = ((piece + 1) * piece_length).min(file_end + 1) - file_start;
        match ranges.last_mut() {
            Some(range) if range.end() == start => {
                *range = ByteRange::new(range.start(), end)?;
            }
            _ => ranges.push(ByteRange::new(start, end)?),
        }
    }
    Some(Coverage {
        pieces: pieces
            .into_iter()
            .map(u32::try_from)
            .collect::<Result<_, _>>()
            .ok()?,
        ranges,
        file_start,
    })
}

/// One preload and what it needs to change state.
struct Entry {
    status: PreloadStatus,
    torrent: TorrentId,
    scheduler: Weak<TorrentScheduler>,
    coverage: Coverage,
    // The bytes its pieces take in the cache, whole pieces.
    pin_bytes: u64,
    // Whether its torrent is in memory storage, so it pins and queues.
    cached: bool,
    expires: Instant,
    // When a piece last arrived, or it last started downloading.
    progress_at: Instant,
    ended: Option<Instant>,
    // When it first became ready; falling back does not start this again.
    first_ready: Option<Instant>,
    // Whether a reader had its file open when last observed.
    read: bool,
}

impl Entry {
    fn is_active(&self) -> bool {
        !self.status.state.is_ended()
    }

    /// Whether it requests and, in memory storage, pins its pieces.
    fn is_scheduled(&self) -> bool {
        matches!(
            self.status.state,
            PreloadState::Downloading | PreloadState::Ready
        )
    }

    /// Whether a newer preload may reclaim its pins at `now`: it pins some,
    /// and its reader's own pins hold what it fetched, or it is not within
    /// its protection.
    fn is_reclaimable(&self, now: Instant) -> bool {
        self.cached
            && self.is_scheduled()
            && (self.read
                || self
                    .first_ready
                    .is_none_or(|at| now.duration_since(at) >= preload::READY_PROTECTION))
    }

    /// Records what has arrived. Progress is counted by chunk, so a large piece
    /// arriving slowly does not count as a stall, and readiness by verified
    /// pieces, so a reader finds what it was promised.
    fn observe_progress(&mut self, received: u64, verified: u64, now: Instant) {
        if received > self.status.completed_bytes {
            self.progress_at = now;
        }
        self.status.completed_bytes = received;
        if self.is_scheduled() {
            self.status.state = if verified >= self.status.target_bytes {
                self.first_ready.get_or_insert(now);
                PreloadState::Ready
            } else {
                PreloadState::Downloading
            };
        }
    }

    fn end(&mut self, state: PreloadState, failure: Option<&str>, now: Instant) {
        self.status.state = state;
        self.status.ended_at = Some(SystemTime::now());
        self.status.failure = failure.map(|detail| Failure {
            detail: detail.to_owned(),
        });
        self.ended = Some(now);
    }
}

struct State {
    entries: Vec<Entry>,
    piece_wait: Duration,
}

/// Every preload of one engine; see the [module documentation](self).
pub struct Preloads {
    playback: Arc<Playback>,
    state: Mutex<State>,
    // Wakes the timer when a preload was created or changed.
    changed: Arc<Notify>,
}

impl Preloads {
    /// Creates the preloads of an engine whose readers share `playback`'s
    /// readahead budget, failing stalled preloads after `piece_wait`.
    pub fn new(playback: &Arc<Playback>, piece_wait: Duration) -> Arc<Self> {
        let preloads = Arc::new(Self {
            playback: playback.clone(),
            state: Mutex::new(State {
                entries: Vec::new(),
                piece_wait,
            }),
            changed: Arc::new(Notify::new()),
        });
        playback.set_preloads(Arc::downgrade(&preloads));
        preloads
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Sets how long a downloading preload may go without a piece.
    pub fn set_piece_wait(&self, piece_wait: Duration) {
        self.lock().piece_wait = piece_wait;
    }

    /// The most a preload of a torrent in memory storage, if `cached`, or in
    /// disk storage may cover.
    fn limit_bytes(&self, cached: bool) -> u64 {
        if cached {
            PRELOAD_MAX_BYTES.min(self.playback.readahead_bytes() / 2)
        } else {
            PRELOAD_MAX_BYTES
        }
    }

    /// Creates a preload of `file` of the torrent `torrent`, scheduled by
    /// `scheduler` over `layout`. The caller checked that the torrent is
    /// started and that `offset` lies within the file.
    pub(crate) fn create(
        &self,
        torrent: &TorrentId,
        scheduler: &Arc<TorrentScheduler>,
        layout: &Layout,
        file: FileIndex,
        id: PreloadId,
        request: PreloadRequest,
    ) -> Result<PreloadStatus, EngineError> {
        let cached = scheduler.is_cached();
        let coverage = coverage(
            layout,
            file.0,
            request.offset_bytes,
            self.limit_bytes(cached),
        )
        .ok_or(EngineError::UnknownFile)?;
        let pin_bytes = coverage
            .pieces
            .iter()
            .filter_map(|&piece| layout.piece_length(piece))
            .map(|length| length as u64)
            .sum();
        let status = {
            let mut state = self.lock();
            let active = state.entries.iter().filter(|entry| entry.is_active());
            let (of_file, total) = active.fold((0, 0), |(of_file, total), entry| {
                let same = entry.torrent == *torrent && entry.status.file == file;
                (of_file + usize::from(same), total + 1)
            });
            if of_file >= preload::PER_FILE_LIMIT || total >= preload::TOTAL_LIMIT {
                return Err(EngineError::TooManyPreloads);
            }
            let now = Instant::now();
            let created_at = SystemTime::now();
            let status = PreloadStatus {
                id,
                file,
                state: if cached {
                    PreloadState::Queued
                } else {
                    PreloadState::Downloading
                },
                offset_bytes: request.offset_bytes,
                target_bytes: coverage.ranges.iter().map(|range| range.len_bytes()).sum(),
                ranges: coverage.ranges.clone(),
                completed_bytes: 0,
                created_at,
                expires_at: created_at + request.expires_in,
                ended_at: None,
                failure: None,
            };
            state.entries.push(Entry {
                status,
                torrent: torrent.clone(),
                scheduler: Arc::downgrade(scheduler),
                coverage,
                pin_bytes,
                cached,
                expires: now + request.expires_in,
                progress_at: now,
                ended: None,
                first_ready: None,
                read: false,
            });
            trim_ended(&mut state.entries, torrent, file);
            self.admit(&mut state, now);
            state.entries.last().map(|entry| entry.status.clone())
        };
        self.reschedule(None);
        self.changed.notify_one();
        status.ok_or_else(|| EngineError::Internal("the new preload is missing".into()))
    }

    /// The file's preloads, in creation order.
    pub(crate) fn of_file(&self, torrent: &TorrentId, file: FileIndex) -> Vec<PreloadStatus> {
        self.lock()
            .entries
            .iter()
            .filter(|entry| entry.torrent == *torrent && entry.status.file == file)
            .map(|entry| entry.status.clone())
            .collect()
    }

    /// Cancels and removes one of the file's preloads.
    pub(crate) fn cancel(
        &self,
        torrent: &TorrentId,
        file: FileIndex,
        id: &PreloadId,
    ) -> Result<(), EngineError> {
        {
            let mut state = self.lock();
            let before = state.entries.len();
            state.entries.retain(|entry| {
                !(entry.torrent == *torrent && entry.status.file == file && entry.status.id == *id)
            });
            if state.entries.len() == before {
                return Err(EngineError::UnknownPreload);
            }
            self.admit(&mut state, Instant::now());
        }
        self.reschedule(None);
        Ok(())
    }

    /// Removes a deleted torrent's preloads.
    pub(crate) fn remove_torrent(&self, torrent: &TorrentId) {
        {
            let mut state = self.lock();
            state.entries.retain(|entry| entry.torrent != *torrent);
            self.admit(&mut state, Instant::now());
        }
        self.reschedule(None);
    }

    /// Fails a torrent's active preloads with `detail`.
    pub(crate) fn fail_torrent(&self, torrent: &TorrentId, detail: &str) {
        {
            let mut state = self.lock();
            let now = Instant::now();
            for entry in &mut state.entries {
                if entry.torrent == *torrent && entry.is_active() {
                    entry.end(PreloadState::Failed, Some(detail), now);
                }
            }
            self.admit(&mut state, now);
        }
        self.reschedule(None);
    }

    /// Fails the newest preload that pins pieces in the cache, for the
    /// cache's bounded fallback, and returns whether there was one.
    pub(crate) fn end_newest_pinning(&self) -> bool {
        let ended = {
            let mut state = self.lock();
            let now = Instant::now();
            let newest = state
                .entries
                .iter_mut()
                .rev()
                .find(|entry| entry.cached && entry.is_scheduled());
            let Some(newest) = newest else {
                return false;
            };
            newest.end(PreloadState::Failed, Some(CACHE_NEEDED_ROOM), now);
            self.admit(&mut state, now);
            true
        };
        self.reschedule(None);
        ended
    }

    /// Starts queued preloads as [`admit_within`] does, within the preloads'
    /// share of the readahead budget. Returns the schedulers of the preloads
    /// it started or reclaimed, which need their plans again.
    fn admit(&self, state: &mut State, now: Instant) -> HashSet<*const TorrentScheduler> {
        admit_within(&mut state.entries, self.playback.readahead_bytes() / 2, now)
    }

    /// Publishes the bytes preloads pin, which readers' readahead leaves
    /// them, and gives every scheduler its scheduled preloads, or only the
    /// schedulers in `only`. Both replan torrents, which takes scheduler locks, so this
    /// runs outside the lock.
    fn reschedule(&self, only: Option<&HashSet<*const TorrentScheduler>>) {
        let mut plans: Vec<(Arc<TorrentScheduler>, Vec<ScheduledPreload>)> = Vec::new();
        let pinned = {
            let state = self.lock();
            for entry in &state.entries {
                let Some(scheduler) = entry.scheduler.upgrade() else {
                    continue;
                };
                if only.is_some_and(|only| !only.contains(&Arc::as_ptr(&scheduler))) {
                    continue;
                }
                let index = match plans
                    .iter()
                    .position(|(other, _)| Arc::ptr_eq(other, &scheduler))
                {
                    Some(index) => index,
                    None => {
                        plans.push((scheduler, Vec::new()));
                        plans.len() - 1
                    }
                };
                if entry.is_scheduled() {
                    plans[index].1.push(ScheduledPreload {
                        pieces: entry.coverage.pieces.clone(),
                        pinned: entry.cached,
                    });
                }
            }
            state
                .entries
                .iter()
                .filter(|entry| entry.cached && entry.is_scheduled())
                .map(|entry| entry.pin_bytes)
                .sum()
        };
        self.playback.set_preload_pinned_bytes(pinned);
        for scheduler in self.playback.schedulers() {
            if only.is_some_and(|only| !only.contains(&Arc::as_ptr(&scheduler))) {
                continue;
            }
            let preloads = plans
                .iter()
                .find(|(other, _)| Arc::ptr_eq(other, &scheduler))
                .map(|(_, preloads)| preloads.clone())
                .unwrap_or_default();
            // A failure leaves the old plan in place; the next change tries
            // again.
            let _ = scheduler.set_preloads(preloads);
        }
    }

    /// Moves every preload on as its torrent and pieces did, then returns
    /// when the next timer is due.
    fn tick(&self, now: Instant) -> Option<Instant> {
        // The torrents are read outside the lock: reading pieces takes the
        // torrent's lock. Preloads canceled meanwhile are no longer found.
        let observed: Vec<(PreloadId, Observation)> = {
            let state = self.lock();
            state
                .entries
                .iter()
                .filter(|entry| entry.is_active())
                .map(|entry| {
                    let id = entry.status.id.clone();
                    let file = entry.status.file;
                    (id, entry.scheduler.clone(), entry.coverage.clone(), file)
                })
                .collect::<Vec<_>>()
        }
        .into_iter()
        .map(|(id, scheduler, coverage, file)| (id, observe(&scheduler, &coverage, file)))
        .collect();
        let mut changed = HashSet::new();
        let next = {
            let mut state = self.lock();
            let piece_wait = state.piece_wait;
            for (id, observation) in observed {
                let Some(entry) = state.entries.iter_mut().find(|entry| entry.status.id == id)
                else {
                    continue;
                };
                if !entry.is_active() {
                    continue;
                }
                let before = (entry.status.state, entry.cached);
                match observation {
                    Observation::Closed => entry.end(PreloadState::Failed, Some(PAUSED), now),
                    Observation::Failed => {
                        entry.end(PreloadState::Failed, Some(TORRENT_FAILED), now);
                    }
                    Observation::Swapping => {}
                    Observation::Live {
                        cached,
                        received,
                        verified,
                        read,
                    } => {
                        entry.read = read;
                        if cached != entry.cached {
                            // A storage mode transition: memory storage
                            // queues it for room, disk storage pins nothing.
                            entry.cached = cached;
                            if entry.is_scheduled() {
                                entry.status.state = if cached {
                                    PreloadState::Queued
                                } else {
                                    PreloadState::Downloading
                                };
                            }
                        }
                        entry.observe_progress(received, verified, now);
                    }
                }
                if entry.is_active() && now >= entry.expires {
                    entry.end(PreloadState::Expired, None, now);
                } else if entry.status.state == PreloadState::Downloading
                    && now.duration_since(entry.progress_at) >= piece_wait
                {
                    entry.end(PreloadState::Failed, Some(STALLED), now);
                }
                if (entry.status.state, entry.cached) != before
                    && let Some(scheduler) = entry.scheduler.upgrade()
                {
                    changed.insert(Arc::as_ptr(&scheduler));
                }
            }
            state.entries.retain(|entry| {
                entry
                    .ended
                    .is_none_or(|ended| now.duration_since(ended) < preload::ENDED_LIFETIME)
            });
            // An ending preload can start another torrent's queued one,
            // whose scheduler has not changed otherwise.
            changed.extend(self.admit(&mut state, now));
            next_due(&state, now)
        };
        self.reschedule(Some(&changed));
        next
    }

    /// Starts the timer that moves preloads on, which runs until
    /// [`PreloadTimer::shutdown`].
    pub fn spawn_timer(self: &Arc<Self>) -> PreloadTimer {
        let stop = Arc::new(Notify::new());
        let task = tokio::spawn(run(
            Arc::downgrade(self),
            self.changed.clone(),
            stop.clone(),
        ));
        PreloadTimer { stop, task }
    }
}

/// Removes the file's oldest ended preloads while it lists more than
/// [`preload::LISTED_LIMIT`].
fn trim_ended(entries: &mut Vec<Entry>, torrent: &TorrentId, file: FileIndex) {
    let of_file = |entry: &Entry| entry.torrent == *torrent && entry.status.file == file;
    let mut excess = entries
        .iter()
        .filter(|entry| of_file(entry))
        .count()
        .saturating_sub(preload::LISTED_LIMIT);
    entries.retain(|entry| {
        if excess > 0 && of_file(entry) && !entry.is_active() {
            excess -= 1;
            return false;
        }
        true
    });
}

/// What a tick found of a preload's torrent.
enum Observation {
    /// The scheduler closed: the torrent paused, or is being deleted.
    Closed,
    /// librqbit stopped the torrent with an error.
    Failed,
    /// The torrent is being added again with other storage.
    Swapping,
    /// The torrent runs, in memory storage if `cached`, having received
    /// `received` bytes of the preload's ranges, of which it holds `verified`
    /// in pieces that passed their hash checks.
    /// A reader has its file open if `read`.
    Live {
        cached: bool,
        received: u64,
        verified: u64,
        read: bool,
    },
}

fn observe(
    scheduler: &Weak<TorrentScheduler>,
    coverage: &Coverage,
    file: FileIndex,
) -> Observation {
    let Some(scheduler) = scheduler.upgrade() else {
        return Observation::Closed;
    };
    if scheduler.is_closed() {
        return Observation::Closed;
    }
    let (Some(torrent), Some(layout)) = (scheduler.torrent(), scheduler.layout()) else {
        return Observation::Swapping;
    };
    if matches!(torrent.stats().state, librqbit::TorrentStatsState::Error) {
        return Observation::Failed;
    }
    let Ok(have) = torrent.have_pieces() else {
        return Observation::Swapping;
    };
    let Ok(pieces) = torrent.received_piece_bytes(&coverage.pieces) else {
        return Observation::Swapping;
    };
    let verified = verified_bytes(&layout, coverage, &have);
    Observation::Live {
        cached: scheduler.is_cached(),
        // The two reads are not atomic: a piece can pass its check between them.
        received: received_bytes(&layout, coverage, &pieces).max(verified),
        verified,
        read: scheduler.has_reader_of(file.0),
    }
}

/// The bytes of `coverage`'s ranges received, given the bytes received of
/// each of its pieces, in their order. A piece's received chunks count in
/// proportion, since they arrive in no particular order within it.
fn received_bytes(layout: &Layout, coverage: &Coverage, received: &[u64]) -> u64 {
    let piece_length = u64::from(layout.lengths.default_piece_length());
    coverage
        .pieces
        .iter()
        .zip(received)
        .map(|(&piece, &received)| {
            let covered = covered_bytes(layout, coverage, piece);
            (u128::from(covered) * u128::from(received) / u128::from(piece_length)) as u64
        })
        .sum()
}

/// The bytes of `coverage`'s ranges in the pieces `have` marks, a bitfield
/// in BEP 3's order.
fn verified_bytes(layout: &Layout, coverage: &Coverage, have: &[u8]) -> u64 {
    coverage
        .pieces
        .iter()
        .filter(|&&piece| {
            have.get((piece / 8) as usize)
                .is_some_and(|byte| byte & (0x80 >> (piece % 8)) != 0)
        })
        .map(|&piece| covered_bytes(layout, coverage, piece))
        .sum()
}

/// The bytes of `piece` within `coverage`'s ranges.
fn covered_bytes(layout: &Layout, coverage: &Coverage, piece: u32) -> u64 {
    let piece_length = u64::from(layout.lengths.default_piece_length());
    // The piece's span relative to the file, as the ranges are.
    let start = (u64::from(piece) * piece_length).saturating_sub(coverage.file_start);
    let end = ((u64::from(piece) + 1) * piece_length).saturating_sub(coverage.file_start);
    coverage
        .ranges
        .iter()
        .map(|range| overlap(start, end, range))
        .sum()
}

/// Starts queued preloads of torrents in memory storage, newest first, while
/// their pins fit in `share`, or while no preload pins anything. The newest
/// reclaims older preloads to fit; older ones wait behind it and only take
/// room that is free. Returns the schedulers of the preloads it changed.
fn admit_within(
    entries: &mut [Entry],
    share: u64,
    now: Instant,
) -> HashSet<*const TorrentScheduler> {
    let mut changed = HashSet::new();
    let mut pinned: u64 = entries
        .iter()
        .filter(|entry| entry.cached && entry.is_scheduled())
        .map(|entry| entry.pin_bytes)
        .sum();
    let queued: Vec<usize> = (0..entries.len())
        .rev()
        .filter(|&index| entries[index].status.state == PreloadState::Queued)
        .collect();
    for (turn, index) in queued.into_iter().enumerate() {
        let pin = entries[index].pin_bytes;
        let fits = |pinned: u64| pinned == 0 || pinned + pin <= share;
        if !fits(pinned) {
            let reclaimed = (turn == 0)
                .then(|| reclaimable(entries, index, pinned, now, fits))
                .flatten();
            let Some(reclaimed) = reclaimed else {
                break;
            };
            for older in reclaimed {
                let entry = &mut entries[older];
                pinned -= entry.pin_bytes;
                entry.end(PreloadState::Failed, Some(RECLAIMED), now);
                if let Some(scheduler) = entry.scheduler.upgrade() {
                    changed.insert(Arc::as_ptr(&scheduler));
                }
            }
        }
        let entry = &mut entries[index];
        pinned += pin;
        entry.status.state = PreloadState::Downloading;
        entry.progress_at = now;
        if let Some(scheduler) = entry.scheduler.upgrade() {
            changed.insert(Arc::as_ptr(&scheduler));
        }
    }
    changed
}

/// The preloads older than `newest` to reclaim for it to fit, those being read
/// first, then the oldest; `None` if reclaiming all it may would not be
/// enough, when it reclaims nothing and waits.
fn reclaimable(
    entries: &[Entry],
    newest: usize,
    pinned: u64,
    now: Instant,
    fits: impl Fn(u64) -> bool,
) -> Option<Vec<usize>> {
    let mut candidates: Vec<usize> = (0..newest)
        .filter(|&older| entries[older].is_reclaimable(now))
        .collect();
    candidates.sort_by_key(|&older| !entries[older].read);
    let mut left = pinned;
    let mut reclaimed = Vec::new();
    for older in candidates {
        if fits(left) {
            break;
        }
        left -= entries[older].pin_bytes;
        reclaimed.push(older);
    }
    fits(left).then_some(reclaimed)
}

fn overlap(start: u64, end: u64, range: &ByteRange) -> u64 {
    end.min(range.end())
        .saturating_sub(start.max(range.start()))
}

/// When the next tick is due: soon while a preload is active, since its
/// pieces and torrent change without notice, and otherwise at the end of
/// the next retention.
fn next_due(state: &State, now: Instant) -> Option<Instant> {
    let timers = state.entries.iter().map(|entry| match entry.ended {
        Some(ended) => ended + preload::ENDED_LIFETIME,
        None => entry.expires,
    });
    let soon = state
        .entries
        .iter()
        .any(Entry::is_active)
        .then_some(now + PROGRESS_INTERVAL);
    timers.chain(soon).min()
}

async fn run(preloads: Weak<Preloads>, changed: Arc<Notify>, stop: Arc<Notify>) {
    loop {
        let next = match preloads.upgrade() {
            Some(preloads) => preloads.tick(Instant::now()),
            None => return,
        };
        let wait = async {
            match next {
                Some(next) => tokio::time::sleep_until(next).await,
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            () = stop.notified() => return,
            () = changed.notified() => {}
            () = wait => {}
        }
    }
}

/// The task that moves preloads on.
pub struct PreloadTimer {
    stop: Arc<Notify>,
    task: JoinHandle<()>,
}

impl PreloadTimer {
    /// Stops the timer and waits for its task.
    pub async fn shutdown(self) -> anyhow::Result<()> {
        // A stored permit stops the task even if it is not waiting yet.
        self.stop.notify_one();
        self.task
            .await
            .map_err(|error| anyhow::anyhow!("the preload timer panicked: {error}"))
    }
}

#[cfg(test)]
mod tests {
    use librqbit_core::lengths::Lengths;

    use super::*;

    const PIECE: u64 = 1 << 20;

    fn layout(files: &[u64]) -> Layout {
        let total: u64 = files.iter().sum();
        let mut offset = 0;
        Layout {
            lengths: Lengths::new(total, PIECE as u32).unwrap(),
            files: files
                .iter()
                .map(|&length| {
                    let file = (offset, length);
                    offset += length;
                    file
                })
                .collect(),
        }
    }

    fn bytes(coverage: &Coverage) -> Vec<(u64, u64)> {
        coverage
            .ranges
            .iter()
            .map(|range| (range.start(), range.end()))
            .collect()
    }

    #[test]
    fn a_file_that_fits_is_covered_whole() {
        let layout = layout(&[PIECE / 2, 3 * PIECE]);
        let covered = coverage(&layout, 1, None, 32 * PIECE).unwrap();
        assert_eq!(covered.pieces, [0, 1, 2, 3]);
        assert_eq!(
            bytes(&covered),
            [(0, 3 * PIECE)],
            "clipped to the file, which starts mid-piece"
        );
    }

    #[test]
    fn a_larger_file_is_covered_at_its_head_and_tail() {
        let layout = layout(&[100 * PIECE]);
        let covered = coverage(&layout, 0, None, 32 * PIECE).unwrap();
        assert_eq!(covered.pieces.len(), 32);
        assert_eq!(
            bytes(&covered),
            [(0, 28 * PIECE), (96 * PIECE, 100 * PIECE)],
            "the last 4 MiB, and the rest of the size at the start"
        );
    }

    #[test]
    fn an_offset_takes_the_first_and_last_pieces_and_a_window() {
        let layout = layout(&[100 * PIECE]);
        let covered = coverage(&layout, 0, Some(50 * PIECE), 10 * PIECE).unwrap();
        assert_eq!(
            bytes(&covered),
            [
                (0, PIECE),
                (49 * PIECE, 57 * PIECE),
                (99 * PIECE, 100 * PIECE)
            ],
            "an eight-piece window starting an eighth of itself early"
        );
        let near_end = coverage(&layout, 0, Some(99 * PIECE), 10 * PIECE).unwrap();
        assert_eq!(
            bytes(&near_end),
            [(0, PIECE), (92 * PIECE, 100 * PIECE)],
            "a window near the end shifts back to stay in the file"
        );
    }

    #[test]
    fn a_limit_below_one_piece_still_covers_one() {
        let layout = layout(&[10 * PIECE]);
        let covered = coverage(&layout, 0, None, PIECE / 4).unwrap();
        assert_eq!(covered.pieces, [0]);
        assert_eq!(
            coverage(&layout, 0, Some(5 * PIECE), PIECE).unwrap().pieces,
            [0]
        );
    }

    fn entry(torrent: u8, file: usize, n: u8, state: PreloadState) -> Entry {
        let now = Instant::now();
        let at = SystemTime::now();
        Entry {
            status: PreloadStatus {
                id: PreloadId::from_random_bytes([n; 16]),
                file: FileIndex(file),
                state,
                offset_bytes: None,
                ranges: Vec::new(),
                target_bytes: 0,
                completed_bytes: 0,
                created_at: at,
                expires_at: at,
                ended_at: None,
                failure: None,
            },
            torrent: TorrentId::from_random_bytes([torrent; 16]),
            scheduler: Weak::new(),
            coverage: Coverage {
                pieces: Vec::new(),
                ranges: Vec::new(),
                file_start: 0,
            },
            pin_bytes: 0,
            cached: true,
            expires: now,
            progress_at: now,
            ended: None,
            first_ready: None,
            read: false,
        }
    }

    /// A preload pinning `pin_bytes` of a memory torrent.
    fn pinning(n: u8, state: PreloadState, pin_bytes: u64) -> Entry {
        let mut entry = entry(n, 0, n, state);
        entry.pin_bytes = pin_bytes;
        entry
    }

    fn states(entries: &[Entry]) -> Vec<PreloadState> {
        entries.iter().map(|entry| entry.status.state).collect()
    }

    use PreloadState::{Downloading, Failed, Queued, Ready};

    #[test]
    fn a_newer_preload_reclaims_one_still_downloading() {
        let now = Instant::now();
        let mut entries = [pinning(1, Downloading, 2), pinning(2, Queued, 2)];
        admit_within(&mut entries, 2, now);
        assert_eq!(states(&entries), [Failed, Downloading]);
        assert_eq!(
            entries[0]
                .status
                .failure
                .as_ref()
                .map(|f| f.detail.as_str()),
            Some(RECLAIMED)
        );
    }

    #[test]
    fn a_ready_preload_is_protected_from_when_it_first_became_ready() {
        let now = Instant::now();
        let mut entries = [pinning(1, Ready, 2), pinning(2, Queued, 2)];
        entries[0].first_ready = Some(now);

        let almost = now + preload::READY_PROTECTION - Duration::from_secs(1);
        admit_within(&mut entries, 2, almost);
        assert_eq!(states(&entries), [Ready, Queued], "still protected");

        admit_within(&mut entries, 2, now + preload::READY_PROTECTION);
        assert_eq!(states(&entries), [Failed, Downloading]);
    }

    #[test]
    fn falling_back_from_ready_keeps_the_first_time_it_was_ready() {
        let now = Instant::now();
        let mut entry = pinning(1, Downloading, 2);
        entry.status.target_bytes = 100;
        entry.observe_progress(100, 100, now);
        let later = now + Duration::from_secs(5);
        entry.observe_progress(50, 50, later);
        assert_eq!(entry.status.state, Downloading);
        entry.observe_progress(100, 100, later);
        assert_eq!(entry.first_ready, Some(now));
    }

    #[test]
    fn a_preload_being_read_needs_no_protection_and_goes_first() {
        let now = Instant::now();
        let mut entries = [
            pinning(1, Downloading, 2),
            pinning(2, Ready, 2),
            pinning(3, Queued, 2),
        ];
        entries[1].first_ready = Some(now);
        entries[1].read = true;
        admit_within(&mut entries, 4, now);
        assert_eq!(states(&entries), [Downloading, Failed, Downloading]);
    }

    #[test]
    fn only_what_is_needed_is_reclaimed_oldest_first() {
        let now = Instant::now();
        let mut entries = [
            pinning(1, Downloading, 2),
            pinning(2, Downloading, 2),
            pinning(3, Queued, 2),
        ];
        admit_within(&mut entries, 4, now);
        assert_eq!(states(&entries), [Failed, Downloading, Downloading]);
    }

    #[test]
    fn nothing_is_reclaimed_when_it_would_not_make_enough_room() {
        let now = Instant::now();
        let mut entries = [
            pinning(1, Downloading, 2),
            pinning(2, Ready, 2),
            pinning(3, Queued, 2),
        ];
        entries[1].first_ready = Some(now);
        admit_within(&mut entries, 2, now);
        assert_eq!(states(&entries), [Downloading, Ready, Queued]);
    }

    #[test]
    fn a_disk_preload_is_never_reclaimed_and_a_newer_preload_is_safe_from_an_older() {
        let now = Instant::now();
        let mut disk = pinning(1, Downloading, 2);
        disk.cached = false;
        let mut entries = [disk, pinning(2, Queued, 2), pinning(3, Downloading, 2)];
        admit_within(&mut entries, 2, now);
        assert_eq!(states(&entries), [Downloading, Queued, Downloading]);
    }

    #[test]
    fn the_newest_starts_first_and_older_ones_take_only_free_room() {
        let now = Instant::now();
        let mut entries = [pinning(1, Queued, 2), pinning(2, Queued, 2)];
        admit_within(&mut entries, 2, now);
        assert_eq!(states(&entries), [Queued, Downloading], "the older waits");

        let mut entries = [pinning(1, Queued, 2), pinning(2, Queued, 2)];
        admit_within(&mut entries, 4, now);
        assert_eq!(states(&entries), [Downloading, Downloading]);
    }

    #[test]
    fn a_file_lists_at_most_its_limit_dropping_the_oldest_ended() {
        let mut entries: Vec<Entry> = (0..17)
            .map(|n| {
                let state = if n < 3 {
                    PreloadState::Expired
                } else {
                    PreloadState::Ready
                };
                entry(1, 0, n, state)
            })
            .collect();
        entries.push(entry(1, 1, 99, PreloadState::Failed));
        entries.push(entry(2, 0, 98, PreloadState::Failed));
        trim_ended(
            &mut entries,
            &TorrentId::from_random_bytes([1; 16]),
            FileIndex(0),
        );
        assert_eq!(entries.len(), 18, "one of the file's 17 goes");
        assert!(
            !entries
                .iter()
                .any(|entry| entry.status.id == PreloadId::from_random_bytes([0; 16])),
            "the oldest ended one"
        );
    }

    #[test]
    fn chunks_arriving_are_progress_but_only_verified_pieces_are_ready() {
        let mut entry = entry(1, 0, 1, PreloadState::Downloading);
        entry.status.target_bytes = 100;
        let start = entry.progress_at;
        let later = start + Duration::from_secs(30);

        entry.observe_progress(40, 0, later);
        assert_eq!(entry.progress_at, later, "a chunk is not a stall");
        assert_eq!(entry.status.completed_bytes, 40);

        let much_later = later + Duration::from_secs(30);
        entry.observe_progress(40, 0, much_later);
        assert_eq!(entry.progress_at, later, "nothing new arrived");

        entry.observe_progress(100, 0, much_later);
        assert_eq!(
            entry.status.state,
            PreloadState::Downloading,
            "every chunk arrived, but no piece has passed its check"
        );

        entry.observe_progress(100, 100, much_later);
        assert_eq!(entry.status.state, PreloadState::Ready);
    }

    #[test]
    fn received_bytes_count_chunks_in_proportion_within_the_ranges() {
        let layout = layout(&[PIECE / 2, 3 * PIECE]);
        let covered = coverage(&layout, 1, None, 32 * PIECE).unwrap();
        // Half of piece 0 lies in the file, and a quarter of it has arrived.
        let mut received = vec![0; covered.pieces.len()];
        received[0] = PIECE / 4;
        received[1] = PIECE / 2;
        assert_eq!(
            received_bytes(&layout, &covered, &received),
            PIECE / 8 + PIECE / 2
        );
    }

    #[test]
    fn verified_bytes_count_held_pieces_within_the_ranges() {
        let layout = layout(&[PIECE / 2, 3 * PIECE]);
        let covered = coverage(&layout, 1, None, 32 * PIECE).unwrap();
        // Pieces 0 and 2 held: half of piece 0 lies in the file.
        let have = [0b1010_0000];
        assert_eq!(verified_bytes(&layout, &covered, &have), PIECE / 2 + PIECE);
    }
}
