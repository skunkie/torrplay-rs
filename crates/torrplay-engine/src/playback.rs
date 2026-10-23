// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! Readers across torrents: the shared readahead budget, idle readers, and
//! the active playback signal.
//!
//! Every [`TorrentScheduler`] of an engine shares one [`Playback`]. A torrent's
//! readahead budget is its readers' share of the global budget, so windows
//! shrink as readers are added anywhere and grow back as they are released.
//!
//! A reader whose stream ends cleanly, including when its client
//! disconnects, becomes an idle reader: it keeps its position, pins, and
//! piece priorities, and the next stream of the same file continues it. Idle
//! readers are released after the idle timeout, when a torrent or the whole
//! pool exceeds its idle reader limit (the oldest in that scope first), when
//! the cache's bounded fallback needs their pins, and when their torrent
//! stops.
//!
//! Active playback, the presence of any active or idle reader, is published
//! through a [`watch`] channel as soon as reader counts change, so
//! background work can react without polling.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::time::Duration;

use tokio::sync::{Notify, watch};
use tokio::task::JoinHandle;
use tokio::time::Instant;

use crate::scheduler::{Reader, TorrentScheduler};

/// The default idle timeout, as the lifecycle defines it.
pub const DEFAULT_IDLE_TIMEOUT: Duration = Duration::from_secs(30);

/// The default least readahead of a ramping reader, as Go TorrPlay's. A
/// 16 MiB start, tried against the benchmark's slow-profile stalls, delayed
/// the first byte there instead: the urgent piece then shared the links with
/// more of the window's pieces.
pub const DEFAULT_READAHEAD_RAMP_BYTES: u64 = 1024 * 1024;

/// The default limit of idle readers per torrent.
pub const DEFAULT_IDLE_READERS_PER_TORRENT: usize = 4;

/// The default limit of idle readers across torrents.
pub const DEFAULT_IDLE_READERS_TOTAL: usize = 16;

/// How readers share readahead and how long idle readers stay.
#[derive(Clone, Copy, Debug)]
pub struct PlaybackConfig {
    /// The readahead budget every reader of every torrent shares.
    pub readahead_bytes: u64,
    /// The least a new reader, or one after a seek, reads ahead while its
    /// window ramps up to its share; zero gives every reader its share at
    /// once.
    pub readahead_ramp_bytes: u64,
    /// How long a reader stays idle after its stream ends.
    pub idle_timeout: Duration,
    /// The most idle readers one torrent keeps.
    pub idle_readers_per_torrent: usize,
    /// The most idle readers all torrents keep together.
    pub idle_readers_total: usize,
}

impl PlaybackConfig {
    /// The lifecycle's idle reader defaults with `readahead_bytes` shared.
    pub fn new(readahead_bytes: u64) -> Self {
        Self {
            readahead_bytes,
            readahead_ramp_bytes: DEFAULT_READAHEAD_RAMP_BYTES,
            idle_timeout: DEFAULT_IDLE_TIMEOUT,
            idle_readers_per_torrent: DEFAULT_IDLE_READERS_PER_TORRENT,
            idle_readers_total: DEFAULT_IDLE_READERS_TOTAL,
        }
    }
}

/// Counts of readers across torrents.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PlaybackStats {
    /// Active and idle readers.
    pub readers: u64,
    /// Idle readers.
    pub idle_readers: u64,
    /// Readers opened since the playback was created; continuing an idle
    /// reader opens none.
    pub opened_readers: u64,
}

/// Readers across the torrents of one engine.
pub struct Playback {
    config: PlaybackConfig,
    // The readahead budget, which settings change with cache capacity.
    readahead_bytes: AtomicU64,
    // Active and idle readers. Schedulers change it under their own lock,
    // so it never counts a reader twice or misses one.
    readers: AtomicU64,
    // Those of torrents in memory storage, which share the readahead budget.
    cache_readers: AtomicU64,
    opened: AtomicU64,
    active: watch::Sender<bool>,
    next_scheduler: AtomicU64,
    state: Mutex<State>,
    // Wakes the reaper when the earliest idle deadline may have changed.
    idle_changed: Arc<Notify>,
}

struct State {
    schedulers: Vec<Weak<TorrentScheduler>>,
    idle: IdlePool<Reader>,
}

impl Playback {
    /// Creates the playback state for one engine's schedulers.
    pub fn new(config: PlaybackConfig) -> Arc<Self> {
        Arc::new(Self {
            readahead_bytes: AtomicU64::new(config.readahead_bytes),
            readers: AtomicU64::new(0),
            cache_readers: AtomicU64::new(0),
            opened: AtomicU64::new(0),
            active: watch::Sender::new(false),
            next_scheduler: AtomicU64::new(0),
            state: Mutex::new(State {
                schedulers: Vec::new(),
                idle: IdlePool::new(&config),
            }),
            idle_changed: Arc::new(Notify::new()),
            config,
        })
    }

    /// The readahead budget all readers share.
    pub fn readahead_bytes(&self) -> u64 {
        self.readahead_bytes.load(Ordering::Acquire)
    }

    /// Sets the readahead budget all readers share and rebalances every
    /// torrent's readahead to it.
    pub fn set_readahead_bytes(&self, readahead_bytes: u64) {
        self.readahead_bytes
            .store(readahead_bytes, Ordering::Release);
        self.replan_all();
    }

    /// The least a ramping reader reads ahead, or zero for no ramp.
    pub fn readahead_ramp_bytes(&self) -> u64 {
        self.config.readahead_ramp_bytes
    }

    /// Returns a receiver of active playback: `true` while any active or
    /// idle reader exists. It changes as soon as reader counts do.
    pub fn subscribe(&self) -> watch::Receiver<bool> {
        self.active.subscribe()
    }

    /// Returns the current reader counts.
    pub fn stats(&self) -> PlaybackStats {
        PlaybackStats {
            readers: self.readers.load(Ordering::Acquire),
            idle_readers: self.lock().idle.len() as u64,
            opened_readers: self.opened.load(Ordering::Acquire),
        }
    }

    /// Releases every idle reader, as shutdown does.
    pub fn release_idle_readers(&self) {
        let released = self.lock().idle.drain();
        drop(released);
    }

    /// Releases the oldest idle reader, returning whether there was one.
    /// The cache's bounded fallback calls it when a reservation finds no
    /// evictable piece.
    pub fn release_oldest_idle_reader(&self) -> bool {
        let released = self.lock().idle.release_oldest();
        let found = released.is_some();
        drop(released);
        if found {
            self.idle_changed.notify_one();
        }
        found
    }

    /// Starts the task that releases idle readers when their idle timeout
    /// expires.
    pub fn spawn_reaper(self: &Arc<Self>) -> Reaper {
        let stop = Arc::new(Notify::new());
        let task = tokio::spawn(reap(
            Arc::downgrade(self),
            self.idle_changed.clone(),
            stop.clone(),
        ));
        Reaper { stop, task }
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Registers a new scheduler, so reader changes elsewhere rebalance its
    /// readahead.
    pub(crate) fn register(&self, scheduler: Weak<TorrentScheduler>) {
        let mut state = self.lock();
        state
            .schedulers
            .retain(|scheduler| scheduler.strong_count() > 0);
        state.schedulers.push(scheduler);
    }

    pub(crate) fn next_scheduler_id(&self) -> u64 {
        self.next_scheduler.fetch_add(1, Ordering::Relaxed)
    }

    /// The number of active and idle readers of torrents in memory storage,
    /// which share the readahead budget.
    pub(crate) fn cache_readers(&self) -> u64 {
        self.cache_readers.load(Ordering::Acquire)
    }

    pub(crate) fn opened(&self) {
        self.opened.fetch_add(1, Ordering::AcqRel);
    }

    /// Records readers added or removed, of a torrent in memory storage if
    /// `cached`, rebalances every torrent's readahead, and then publishes
    /// active playback, so a receiver that sees it end also sees the released
    /// pins gone. Callers hold no scheduler lock.
    pub(crate) fn readers_changed(&self, cached: bool, added: u64, removed: u64) {
        let counts = if cached {
            &[&self.readers, &self.cache_readers][..]
        } else {
            &[&self.readers][..]
        };
        for count in counts {
            if added >= removed {
                count.fetch_add(added - removed, Ordering::AcqRel);
            } else {
                count.fetch_sub(removed - added, Ordering::AcqRel);
            }
        }
        self.replan_all();
        // The count is read under the channel's lock, so concurrent changes
        // cannot publish a stale value last.
        self.active.send_if_modified(|active| {
            let now = self.readers.load(Ordering::Acquire) > 0;
            let changed = *active != now;
            *active = now;
            changed
        });
    }

    // Rebalances every torrent's readahead. Callers hold no scheduler lock.
    fn replan_all(&self) {
        let schedulers: Vec<_> = self
            .lock()
            .schedulers
            .iter()
            .filter_map(Weak::upgrade)
            .collect();
        for scheduler in schedulers {
            // A failure leaves that torrent's old plan in place; its next
            // reader change tries again.
            let _ = scheduler.replan();
        }
    }

    /// Keeps `reader` idle, releasing whichever idle readers the limits
    /// push out.
    pub(crate) fn park(&self, reader: Reader) {
        let (scheduler, file, offset) = (reader.scheduler_id(), reader.file(), reader.position());
        let released = self
            .lock()
            .idle
            .park(Instant::now(), scheduler, file, offset, reader);
        drop(released);
        self.idle_changed.notify_one();
    }

    /// Takes the idle reader of `file` in the scheduler's torrent nearest to
    /// `offset`, if there is one.
    pub(crate) fn take_idle(&self, scheduler: u64, file: usize, offset: u64) -> Option<Reader> {
        let reader = self.lock().idle.take(scheduler, file, offset);
        if reader.is_some() {
            self.idle_changed.notify_one();
        }
        reader
    }

    /// Sets the idle timeout and limits, releasing at once the idle readers
    /// past the new timeout or over a new limit, oldest first.
    pub fn set_idle_limits(&self, timeout: Duration, per_torrent: usize, total: usize) {
        let released = self
            .lock()
            .idle
            .set_limits(Instant::now(), timeout, per_torrent, total);
        drop(released);
        self.idle_changed.notify_one();
    }

    /// Releases the idle readers of one scheduler's torrent.
    pub(crate) fn release_idle_of(&self, scheduler: u64) {
        let released = self.lock().idle.release_scheduler(scheduler);
        drop(released);
        self.idle_changed.notify_one();
    }

    /// Releases expired idle readers and returns the next deadline.
    fn expire(&self, now: Instant) -> Option<Instant> {
        let (released, next) = {
            let mut state = self.lock();
            let released = state.idle.expire(now);
            (released, state.idle.next_deadline())
        };
        drop(released);
        next
    }
}

/// The task that releases idle readers after their idle timeout.
pub struct Reaper {
    stop: Arc<Notify>,
    task: JoinHandle<()>,
}

impl Reaper {
    /// Stops the task and waits for it to end, failing if it panicked.
    pub async fn shutdown(self) -> anyhow::Result<()> {
        self.stop.notify_one();
        Ok(self.task.await?)
    }
}

async fn reap(playback: Weak<Playback>, idle_changed: Arc<Notify>, stop: Arc<Notify>) {
    loop {
        // Upgrading only to expire lets the playback go once its engine
        // drops it. A change while the task sleeps leaves a permit in
        // `idle_changed`, so none is missed.
        let Some(deadline) = playback
            .upgrade()
            .map(|playback| playback.expire(Instant::now()))
        else {
            return;
        };
        tokio::select! {
            () = async {
                match deadline {
                    Some(deadline) => tokio::time::sleep_until(deadline).await,
                    None => std::future::pending().await,
                }
            } => {}
            () = idle_changed.notified() => {}
            () = stop.notified() => return,
        }
    }
}

struct Idle<T> {
    scheduler: u64,
    file: usize,
    offset: u64,
    since: Instant,
    item: T,
}

/// Idle readers in the order they became idle, under the idle timeout and
/// limits.
pub(crate) struct IdlePool<T> {
    entries: VecDeque<Idle<T>>,
    timeout: Duration,
    per_torrent: usize,
    total: usize,
}

impl<T> IdlePool<T> {
    pub(crate) fn new(config: &PlaybackConfig) -> Self {
        Self {
            entries: VecDeque::new(),
            timeout: config.idle_timeout,
            per_torrent: config.idle_readers_per_torrent,
            total: config.idle_readers_total,
        }
    }

    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }

    /// Adds an idle reader at `now` and returns those the limits release:
    /// the oldest of its torrent past the per-torrent limit, then the oldest
    /// of all past the total limit.
    pub(crate) fn park(
        &mut self,
        now: Instant,
        scheduler: u64,
        file: usize,
        offset: u64,
        item: T,
    ) -> Vec<T> {
        self.entries.push_back(Idle {
            scheduler,
            file,
            offset,
            since: now,
            item,
        });
        let mut released = Vec::new();
        self.limit_torrent(scheduler, &mut released);
        while self.entries.len() > self.total {
            released.extend(self.entries.pop_front().map(|idle| idle.item));
        }
        released
    }

    /// Sets the timeout and limits and returns the idle readers they
    /// release at `now`: those past the new timeout, then the oldest of each
    /// torrent over the per-torrent limit, then the oldest of all over the
    /// total.
    pub(crate) fn set_limits(
        &mut self,
        now: Instant,
        timeout: Duration,
        per_torrent: usize,
        total: usize,
    ) -> Vec<T> {
        self.timeout = timeout;
        self.per_torrent = per_torrent;
        self.total = total;
        let mut released = self.expire(now);
        let mut schedulers: Vec<u64> = self.entries.iter().map(|idle| idle.scheduler).collect();
        schedulers.sort_unstable();
        schedulers.dedup();
        for scheduler in schedulers {
            self.limit_torrent(scheduler, &mut released);
        }
        while self.entries.len() > self.total {
            released.extend(self.entries.pop_front().map(|idle| idle.item));
        }
        released
    }

    /// Releases the oldest idle readers of `scheduler` over the per-torrent
    /// limit into `released`.
    fn limit_torrent(&mut self, scheduler: u64, released: &mut Vec<T>) {
        while self
            .entries
            .iter()
            .filter(|idle| idle.scheduler == scheduler)
            .count()
            > self.per_torrent
        {
            let Some(oldest) = self
                .entries
                .iter()
                .position(|idle| idle.scheduler == scheduler)
            else {
                break;
            };
            released.extend(self.entries.remove(oldest).map(|idle| idle.item));
        }
    }

    /// Takes the idle reader of `file` in `scheduler` nearest to `offset`,
    /// the most recent among equally near ones.
    pub(crate) fn take(&mut self, scheduler: u64, file: usize, offset: u64) -> Option<T> {
        let (index, _) = self
            .entries
            .iter()
            .enumerate()
            .filter(|(_, idle)| idle.scheduler == scheduler && idle.file == file)
            .min_by_key(|(index, idle)| (idle.offset.abs_diff(offset), usize::MAX - index))?;
        self.entries.remove(index).map(|idle| idle.item)
    }

    /// Removes the readers idle since the timeout before `now`.
    pub(crate) fn expire(&mut self, now: Instant) -> Vec<T> {
        let mut released = Vec::new();
        while self
            .entries
            .front()
            .is_some_and(|idle| idle.since + self.timeout <= now)
        {
            released.extend(self.entries.pop_front().map(|idle| idle.item));
        }
        released
    }

    /// When the oldest idle reader expires.
    pub(crate) fn next_deadline(&self) -> Option<Instant> {
        self.entries.front().map(|idle| idle.since + self.timeout)
    }

    pub(crate) fn release_scheduler(&mut self, scheduler: u64) -> Vec<T> {
        let (released, kept) = std::mem::take(&mut self.entries)
            .into_iter()
            .partition(|idle| idle.scheduler == scheduler);
        self.entries = kept;
        released
            .into_iter()
            .map(|idle: Idle<T>| idle.item)
            .collect()
    }

    pub(crate) fn release_oldest(&mut self) -> Option<T> {
        self.entries.pop_front().map(|idle| idle.item)
    }

    pub(crate) fn drain(&mut self) -> Vec<T> {
        self.entries.drain(..).map(|idle| idle.item).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pool(per_torrent: usize, total: usize) -> IdlePool<&'static str> {
        IdlePool::new(&PlaybackConfig {
            idle_readers_per_torrent: per_torrent,
            idle_readers_total: total,
            ..PlaybackConfig::new(0)
        })
    }

    fn later(start: Instant, millis: u64) -> Instant {
        start + Duration::from_millis(millis)
    }

    /// RDR-03: the oldest idle reader in the exceeded scope goes first.
    #[test]
    fn limits_release_the_oldest_idle_reader_in_their_scope() {
        let start = Instant::now();
        let mut pool = pool(4, 6);
        for (n, name) in ["a1", "a2", "a3", "a4"].into_iter().enumerate() {
            assert!(pool.park(later(start, n as u64), 1, 0, 0, name).is_empty());
        }
        assert_eq!(
            pool.park(later(start, 4), 2, 0, 0, "b1"),
            Vec::<&str>::new()
        );
        assert_eq!(
            pool.park(later(start, 5), 1, 0, 0, "a5"),
            ["a1"],
            "a fifth idle reader in one torrent releases that torrent's oldest"
        );
        assert_eq!(
            pool.park(later(start, 6), 3, 0, 0, "c1"),
            Vec::<&str>::new()
        );
        assert_eq!(
            pool.park(later(start, 7), 3, 0, 0, "c2"),
            ["a2"],
            "past the total limit, the oldest of all goes"
        );
        assert_eq!(pool.len(), 6);
    }

    /// SET-06: lowering the timeout and limits releases at once the idle
    /// readers past the timeout, then the oldest of each torrent over the
    /// per-torrent limit, then the oldest of all over the total.
    #[test]
    fn new_limits_release_idle_readers_at_once() {
        let start = Instant::now();
        let mut pool = pool(4, 16);
        for (n, (scheduler, name)) in [(1, "a1"), (1, "a2"), (2, "b1"), (1, "a3"), (2, "b2")]
            .into_iter()
            .enumerate()
        {
            pool.park(later(start, n as u64 * 1000), scheduler, 0, 0, name);
        }
        assert_eq!(
            pool.set_limits(later(start, 1500), Duration::from_secs(30), 4, 16),
            Vec::<&str>::new(),
            "limits as before release nothing"
        );
        let mut released = pool.set_limits(later(start, 2500), Duration::from_secs(2), 1, 2);
        released.sort_unstable();
        assert_eq!(
            released,
            ["a1", "a2", "b1"],
            "a1 is past the new timeout, a2 over torrent 1's limit, b1 over torrent 2's"
        );
        assert_eq!(pool.len(), 2);
        assert_eq!(
            pool.set_limits(later(start, 2500), Duration::from_secs(2), 1, 1),
            ["a3"],
            "then the oldest of all over the total"
        );
        assert_eq!(
            pool.next_deadline(),
            Some(later(start, 4000) + Duration::from_secs(2))
        );
    }

    /// RDR-02: idle readers expire after the idle timeout, oldest first.
    #[test]
    fn idle_readers_expire_after_the_timeout() {
        let start = Instant::now();
        let mut pool = pool(4, 16);
        pool.park(start, 1, 0, 0, "first");
        pool.park(later(start, 10_000), 1, 0, 0, "second");
        assert_eq!(pool.next_deadline(), Some(start + DEFAULT_IDLE_TIMEOUT));
        assert!(pool.expire(later(start, 29_999)).is_empty());
        assert_eq!(pool.expire(start + DEFAULT_IDLE_TIMEOUT), ["first"]);
        assert_eq!(
            pool.next_deadline(),
            Some(later(start, 10_000) + DEFAULT_IDLE_TIMEOUT)
        );
        assert_eq!(pool.expire(later(start, 60_000)), ["second"]);
        assert_eq!(pool.next_deadline(), None);
    }

    /// RDR-01: a stream continues the idle reader of its file nearest to
    /// where it starts.
    #[test]
    fn streams_continue_the_nearest_idle_reader_of_their_file() {
        let start = Instant::now();
        let mut pool = pool(4, 16);
        pool.park(start, 1, 0, 1000, "near the start");
        pool.park(start, 1, 0, 9000, "far along");
        pool.park(start, 1, 1, 5000, "other file");
        pool.park(start, 2, 0, 5000, "other torrent");
        assert_eq!(pool.take(1, 0, 8000), Some("far along"));
        assert_eq!(pool.take(1, 0, 8000), Some("near the start"));
        assert_eq!(pool.take(1, 0, 8000), None);
        pool.park(start, 1, 2, 0, "older");
        pool.park(start, 1, 2, 0, "newer");
        assert_eq!(
            pool.take(1, 2, 0),
            Some("newer"),
            "ties take the most recent"
        );
    }

    #[test]
    fn a_torrent_releases_only_its_own_idle_readers() {
        let start = Instant::now();
        let mut pool = pool(4, 16);
        pool.park(start, 1, 0, 0, "a");
        pool.park(start, 2, 0, 0, "b");
        pool.park(start, 1, 0, 0, "c");
        assert_eq!(pool.release_scheduler(1), ["a", "c"]);
        assert_eq!(pool.release_oldest(), Some("b"));
        assert_eq!(pool.release_oldest(), None);
    }

    #[test]
    fn zero_limits_keep_no_idle_reader() {
        let mut pool = pool(0, 16);
        assert_eq!(pool.park(Instant::now(), 1, 0, 0, "gone"), ["gone"]);
        assert_eq!(pool.len(), 0);
    }
}
