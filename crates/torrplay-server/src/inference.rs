// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! Inferred progress, as the lifecycle's Profiles and progress section
//! describes it: how far a DLNA media renderer has watched a file, from the
//! ranges it streams, mapped to playback times through the file's seek
//! index and saved in the device profile.
//!
//! A *viewing* is the streams of one file with gaps shorter than
//! [`VIEWING_GAP`]. Its latest stream that starts inside the media data and
//! has delivered at least [`PROBE_BYTES`] stands for playback; shorter
//! streams, and those outside the media data, are a renderer probing the
//! file's headers and index. Playback runs no faster than real time and no
//! further than what was delivered, so the position is the stream's start
//! time plus the lesser of its elapsed time and the media time it delivered.

use std::collections::{HashMap, VecDeque};
use std::io;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::task::{Context, Poll};
use std::time::{Duration, SystemTime};

use axum::body::{Body, Bytes, HttpBody};
use http_body::{Frame, SizeHint};
use tokio::io::AsyncReadExt;
use torrplay_core::engine::{ByteRange, Engine};
use torrplay_core::id::{FileId, TorrentId};
use torrplay_core::metadata::FileIndex;
use torrplay_media::{SeekIndex, Source};

use crate::app::Clock;

/// How often viewings are estimated, saved, and ended.
pub(crate) const SWEEP_INTERVAL: Duration = Duration::from_secs(5);

/// The least time between two saves of a viewing's progress.
pub(crate) const SAVE_INTERVAL: Duration = Duration::from_secs(15);

/// How long a file goes without a stream before its viewing ends.
pub(crate) const VIEWING_GAP: Duration = Duration::from_secs(120);

/// The least a stream delivers to stand for playback rather than a probe.
pub(crate) const PROBE_BYTES: u64 = 2 << 20;

/// How far before its estimate a viewing that ends unviewed is saved, so
/// resuming never skips what was not seen.
pub(crate) const RESUME_BACK_OFF: Duration = Duration::from_secs(10);

/// The longest reading one file's seek index may take.
pub(crate) const INDEX_TIMEOUT: Duration = Duration::from_secs(60);

/// The most files whose seek index, or its absence, is remembered.
pub(crate) const INDEXES_MAX: usize = 64;

/// The most streams a viewing remembers, the latest.
const STREAMS_KEPT: usize = 8;

/// A file of a torrent.
pub(crate) type FileKey = (TorrentId, FileId);

/// The viewings in progress and the files' seek indexes.
#[derive(Clone, Default)]
pub(crate) struct Inference(Arc<Mutex<State>>);

#[derive(Default)]
struct State {
    viewings: HashMap<FileKey, Viewing>,
    indexes: Indexes,
    next_stream: u64,
}

/// A file's seek index: being read, read, or absent.
#[derive(Clone)]
enum IndexState {
    Pending {
        file: FileIndex,
        name: String,
        size: u64,
    },
    Ready(Arc<SeekIndex>),
    Missing,
}

/// The seek indexes of at most [`INDEXES_MAX`] files, the oldest dropped
/// first.
#[derive(Default)]
struct Indexes {
    entries: HashMap<FileKey, IndexState>,
    order: VecDeque<FileKey>,
}

impl Indexes {
    fn insert(&mut self, key: FileKey, state: IndexState) {
        if self.entries.insert(key.clone(), state).is_none() {
            self.order.push_back(key);
        }
        while self.order.len() > INDEXES_MAX {
            if let Some(oldest) = self.order.pop_front() {
                self.entries.remove(&oldest);
            }
        }
    }

    fn forget_torrent(&mut self, torrent: &TorrentId) {
        self.entries.retain(|(id, _), _| id != torrent);
        self.order.retain(|(id, _)| id != torrent);
    }
}

struct Viewing {
    streams: VecDeque<Stream>,
    /// When its last stream ended, or it started.
    idle_since: SystemTime,
    last_saved: Option<SystemTime>,
    /// The position it last saved.
    last_position: Option<Duration>,
    /// Whether it marked the file viewed, after which it saves nothing.
    finished: bool,
    /// Whether it ended and its last position awaits saving.
    ended: bool,
}

struct Stream {
    number: u64,
    start: u64,
    started_at: SystemTime,
    ended_at: Option<SystemTime>,
    delivered: Arc<AtomicU64>,
}

/// A viewing's position to save.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Due {
    pub(crate) key: FileKey,
    pub(crate) position: Duration,
    pub(crate) duration: Duration,
    /// Whether the viewing ended, so [`Inference::close`] follows the save.
    pub(crate) last: bool,
}

/// A file whose seek index is to be read.
pub(crate) struct PendingIndex {
    pub(crate) key: FileKey,
    pub(crate) file: FileIndex,
    pub(crate) name: String,
    pub(crate) size: u64,
}

impl Inference {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Records a stream of the file from byte `start`, its index to be read
    /// unless known, until the returned guard drops.
    pub(crate) fn begin(
        &self,
        key: FileKey,
        file: FileIndex,
        name: String,
        size: u64,
        start: u64,
        clock: &Clock,
    ) -> Watched {
        let now = clock();
        let delivered = Arc::new(AtomicU64::new(0));
        let mut state = self.lock();
        state.next_stream += 1;
        let number = state.next_stream;
        if !state.indexes.entries.contains_key(&key) {
            state
                .indexes
                .insert(key.clone(), IndexState::Pending { file, name, size });
        }
        let viewing = state.viewings.entry(key.clone()).or_insert(Viewing {
            streams: VecDeque::new(),
            idle_since: now,
            last_saved: None,
            last_position: None,
            finished: false,
            ended: false,
        });
        // A stream before its last save continues the viewing.
        viewing.ended = false;
        viewing.streams.push_back(Stream {
            number,
            start,
            started_at: now,
            ended_at: None,
            delivered: Arc::clone(&delivered),
        });
        if viewing.streams.len() > STREAMS_KEPT {
            viewing.streams.pop_front();
        }
        Watched {
            inference: self.clone(),
            key,
            number,
            delivered,
            clock: Arc::clone(clock),
        }
    }

    fn end(&self, key: &FileKey, number: u64, now: SystemTime) {
        let mut state = self.lock();
        if let Some(viewing) = state.viewings.get_mut(key) {
            if let Some(stream) = viewing
                .streams
                .iter_mut()
                .find(|stream| stream.number == number)
            {
                stream.ended_at = Some(now);
            }
            viewing.idle_since = viewing.idle_since.max(now);
        }
    }

    /// Ends a viewing without saving, as clearing its progress does.
    pub(crate) fn forget(&self, key: &FileKey) {
        self.lock().viewings.remove(key);
    }

    /// Whether the file has a viewing whose positions are still saved.
    pub(crate) fn is_viewing(&self, key: &FileKey) -> bool {
        self.lock().viewings.contains_key(key)
    }

    /// Removes a viewing once its last position is saved, unless a stream
    /// continued it meanwhile.
    pub(crate) fn close(&self, key: &FileKey) {
        let mut state = self.lock();
        if state.viewings.get(key).is_some_and(|viewing| viewing.ended) {
            state.viewings.remove(key);
        }
    }

    /// Ends a deleted torrent's viewings and forgets its indexes.
    pub(crate) fn forget_torrent(&self, torrent: &TorrentId) {
        let mut state = self.lock();
        state.viewings.retain(|(id, _), _| id != torrent);
        state.indexes.forget_torrent(torrent);
    }

    /// Marks a viewing finished once its save marked the file viewed.
    pub(crate) fn finish(&self, key: &FileKey) {
        if let Some(viewing) = self.lock().viewings.get_mut(key) {
            viewing.finished = true;
        }
    }

    /// The files whose seek index is still to be read.
    pub(crate) fn pending_indexes(&self) -> Vec<PendingIndex> {
        self.lock()
            .indexes
            .entries
            .iter()
            .filter_map(|(key, state)| match state {
                IndexState::Pending { file, name, size } => Some(PendingIndex {
                    key: key.clone(),
                    file: *file,
                    name: name.clone(),
                    size: *size,
                }),
                _ => None,
            })
            .collect()
    }

    /// Records a file's seek index, or that it has none.
    pub(crate) fn set_index(&self, key: FileKey, index: Option<SeekIndex>) {
        let state = index.map_or(IndexState::Missing, |index| {
            IndexState::Ready(Arc::new(index))
        });
        self.lock().indexes.insert(key, state);
    }

    /// The positions due to be saved at `now`, ending the viewings past the
    /// gap: each viewing's estimate when it changed, at most every
    /// [`SAVE_INTERVAL`], and its last one when it ends, backed off by
    /// [`RESUME_BACK_OFF`] unless it reached the viewed mark.
    pub(crate) fn due(&self, now: SystemTime) -> Vec<Due> {
        let mut state = self.lock();
        let State {
            viewings, indexes, ..
        } = &mut *state;
        let mut due = Vec::new();
        viewings.retain(|key, viewing| {
            if viewing.ended {
                // Its last save is under way; closing it removes it.
                return true;
            }
            let open = viewing
                .streams
                .iter()
                .any(|stream| stream.ended_at.is_none());
            let ended = !open
                && now
                    .duration_since(viewing.idle_since)
                    .is_ok_and(|idle| idle >= VIEWING_GAP);
            let Some(IndexState::Ready(index)) = indexes.entries.get(key) else {
                return !ended;
            };
            if viewing.finished {
                return !ended;
            }
            let save_now = ended
                || viewing.last_saved.is_none_or(|saved| {
                    now.duration_since(saved)
                        .is_ok_and(|since| since >= SAVE_INTERVAL)
                });
            let estimated = estimate(index, &viewing.streams, now)
                .filter(|position| ended || viewing.last_position != Some(*position));
            let Some(mut position) = estimated.filter(|_| save_now) else {
                return !ended;
            };
            viewing.last_position = Some(position);
            viewing.last_saved = Some(now);
            let duration = index.duration();
            if ended && position.as_millis() * 10 < duration.as_millis() * 9 {
                position = position.saturating_sub(RESUME_BACK_OFF);
            }
            viewing.ended = ended;
            due.push(Due {
                key: key.clone(),
                position,
                duration,
                last: ended,
            });
            true
        });
        due
    }
}

/// The playback position of the viewing's streams at `now`, from its latest
/// stream that starts inside the media data and has delivered at least
/// [`PROBE_BYTES`].
fn estimate(index: &SeekIndex, streams: &VecDeque<Stream>, now: SystemTime) -> Option<Duration> {
    let media = index.media_range();
    let stream = streams
        .iter()
        .filter(|stream| {
            media.contains(&stream.start) && stream.delivered.load(Ordering::Relaxed) >= PROBE_BYTES
        })
        .max_by_key(|stream| (stream.started_at, stream.number))?;
    let start = index.time_at(stream.start)?;
    let reached = stream
        .start
        .saturating_add(stream.delivered.load(Ordering::Relaxed));
    let delivered_to = match index.time_at(reached) {
        Some(time) => time,
        None if reached >= media.end => index.duration(),
        None => start,
    };
    let elapsed = stream
        .ended_at
        .unwrap_or(now)
        .duration_since(stream.started_at)
        .unwrap_or_default();
    let position = start + elapsed.min(delivered_to.saturating_sub(start));
    Some(position.min(index.duration()))
}

/// Keeps a stream in its viewing, counting what it delivers, until it ends.
pub(crate) struct Watched {
    inference: Inference,
    key: FileKey,
    number: u64,
    delivered: Arc<AtomicU64>,
    clock: Clock,
}

impl Drop for Watched {
    fn drop(&mut self) {
        self.inference.end(&self.key, self.number, (self.clock)());
    }
}

/// A response body that counts what it delivers into its viewing, with the
/// inner body's size so lengths stay exact.
pub(crate) struct WatchedBody {
    body: Body,
    watched: Watched,
}

impl WatchedBody {
    pub(crate) fn new(body: Body, watched: Watched) -> Self {
        Self { body, watched }
    }
}

impl HttpBody for WatchedBody {
    type Data = Bytes;
    type Error = axum::Error;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, axum::Error>>> {
        let polled = Pin::new(&mut self.body).poll_frame(cx);
        if let Poll::Ready(Some(Ok(frame))) = &polled
            && let Some(data) = frame.data_ref()
        {
            self.watched
                .delivered
                .fetch_add(data.len() as u64, Ordering::Relaxed);
        }
        polled
    }

    fn is_end_stream(&self) -> bool {
        self.body.is_end_stream()
    }

    fn size_hint(&self) -> SizeHint {
        self.body.size_hint()
    }
}

/// The most bytes a read fetches whole into the block it caches, as the
/// headers a seek index starts from are.
const SMALL_READ_BYTES: usize = 4 << 10;

/// The block a small read fetches.
const BLOCK_BYTES: u64 = 64 << 10;

/// The most blocks kept.
const BLOCKS_KEPT: usize = 4;

/// A file's bytes through engine readers, small reads served from a few
/// cached blocks, so walking headers opens few readers.
pub(crate) struct EngineSource<'a, E> {
    engine: &'a E,
    torrent: TorrentId,
    file: FileIndex,
    size: u64,
    blocks: VecDeque<(u64, Vec<u8>)>,
}

impl<'a, E: Engine> EngineSource<'a, E> {
    pub(crate) fn new(engine: &'a E, torrent: TorrentId, file: FileIndex, size: u64) -> Self {
        Self {
            engine,
            torrent,
            file,
            size,
            blocks: VecDeque::new(),
        }
    }

    async fn fetch(&self, offset: u64, buffer: &mut [u8]) -> io::Result<()> {
        let end = offset
            .checked_add(buffer.len() as u64)
            .ok_or_else(|| io::Error::from(io::ErrorKind::UnexpectedEof))?;
        let range = ByteRange::new(offset, end)
            .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))?;
        let mut reader = self
            .engine
            .open_reader(&self.torrent, self.file, range)
            .await
            .map_err(io::Error::other)?;
        reader.read_exact(buffer).await?;
        Ok(())
    }
}

impl<E: Engine> Source for EngineSource<'_, E> {
    fn size(&self) -> u64 {
        self.size
    }

    async fn read_at(&mut self, offset: u64, buffer: &mut [u8]) -> io::Result<()> {
        let length = buffer.len() as u64;
        if buffer.len() > SMALL_READ_BYTES {
            return self.fetch(offset, buffer).await;
        }
        let cached = self.blocks.iter().find(|(start, block)| {
            offset >= *start && offset + length <= *start + block.len() as u64
        });
        if let Some((start, block)) = cached {
            let at = (offset - start) as usize;
            buffer.copy_from_slice(&block[at..at + buffer.len()]);
            return Ok(());
        }
        let block_end = offset.saturating_add(BLOCK_BYTES).min(self.size);
        if block_end < offset + length {
            return Err(io::Error::from(io::ErrorKind::UnexpectedEof));
        }
        let mut block = vec![0; (block_end - offset) as usize];
        self.fetch(offset, &mut block).await?;
        buffer.copy_from_slice(&block[..buffer.len()]);
        self.blocks.push_back((offset, block));
        if self.blocks.len() > BLOCKS_KEPT {
            self.blocks.pop_front();
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use torrplay_media::SeekPoint;

    fn at(seconds: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(seconds)
    }

    /// A 100-second file whose media data is bytes 1000 to 101,000, a
    /// megabyte a second, with a seek point every 10 seconds.
    fn index() -> SeekIndex {
        let points = (0..10)
            .map(|n| SeekPoint {
                time: Duration::from_secs(10 * n),
                offset: 1000 + 10_000_000 * n,
            })
            .collect();
        SeekIndex::new(Duration::from_secs(100), 1000..100_001_000, points).unwrap()
    }

    fn key() -> FileKey {
        (
            TorrentId::from_random_bytes([1; torrplay_core::id::TORRENT_ID_RANDOM_BYTES]),
            FileId::derive(&["Film.mkv"]),
        )
    }

    struct Harness {
        inference: Inference,
        now: Arc<AtomicU64>,
        clock: Clock,
    }

    impl Harness {
        fn new() -> Self {
            let now = Arc::new(AtomicU64::new(1000));
            let clock: Clock = {
                let now = Arc::clone(&now);
                Arc::new(move || at(now.load(Ordering::SeqCst)))
            };
            let inference = Inference::default();
            inference.set_index(key(), Some(index()));
            Self {
                inference,
                now,
                clock,
            }
        }

        fn now(&self) -> SystemTime {
            (self.clock)()
        }

        fn advance(&self, seconds: u64) {
            self.now.fetch_add(seconds, Ordering::SeqCst);
        }

        fn stream(&self, start: u64) -> Watched {
            self.inference.begin(
                key(),
                FileIndex(0),
                "Film.mkv".into(),
                100_001_000,
                start,
                &self.clock,
            )
        }

        fn positions(&self) -> Vec<u64> {
            self.inference
                .due(self.now())
                .into_iter()
                .map(|due| due.position.as_secs())
                .collect()
        }
    }

    fn deliver(watched: &Watched, bytes: u64) {
        watched.delivered.fetch_add(bytes, Ordering::Relaxed);
    }

    #[test]
    fn playback_runs_at_real_time_up_to_what_was_delivered() {
        let harness = Harness::new();
        let stream = harness.stream(1000);
        deliver(&stream, 30_000_000);
        harness.advance(20);
        assert_eq!(harness.positions(), [20], "20 s played of 30 s delivered");
        harness.advance(5);
        assert!(harness.positions().is_empty(), "saved 5 s ago");
        harness.advance(15);
        assert_eq!(
            harness.positions(),
            [30],
            "a stall stops at what was delivered"
        );
    }

    #[test]
    fn a_seek_restarts_from_its_own_time() {
        let harness = Harness::new();
        let first = harness.stream(1000);
        deliver(&first, 5_000_000);
        harness.advance(5);
        drop(first);
        let seek = harness.stream(1000 + 50_000_000);
        deliver(&seek, 10_000_000);
        harness.advance(4);
        assert_eq!(harness.positions(), [54]);
    }

    #[test]
    fn probes_of_headers_indexes_and_small_ranges_are_ignored() {
        let harness = Harness::new();
        let playing = harness.stream(1000);
        deliver(&playing, 20_000_000);
        harness.advance(10);
        let header = harness.stream(0);
        deliver(&header, 1000);
        let tail = harness.stream(100_001_000);
        deliver(&tail, 50_000);
        let small = harness.stream(90_000_000);
        deliver(&small, PROBE_BYTES - 1);
        assert_eq!(harness.positions(), [10], "the playing stream stands");
    }

    #[test]
    fn a_viewing_ends_after_the_gap_backed_off_unless_viewed() {
        let harness = Harness::new();
        let stream = harness.stream(1000);
        deliver(&stream, 40_000_000);
        harness.advance(40);
        drop(stream);
        assert_eq!(harness.positions(), [40]);
        harness.advance(VIEWING_GAP.as_secs() - 1);
        assert!(harness.positions().is_empty(), "within the gap, and saved");
        harness.advance(1);
        assert_eq!(harness.positions(), [30], "backed off at the end");
        harness.advance(100);
        assert!(harness.positions().is_empty(), "the last save is under way");
        harness.inference.close(&key());
        assert!(harness.positions().is_empty(), "the viewing is over");

        let credits = harness.stream(1000 + 90_000_000);
        deliver(&credits, 10_000_000);
        harness.advance(8);
        drop(credits);
        harness.advance(VIEWING_GAP.as_secs());
        assert_eq!(harness.positions(), [98], "past 90% it is not backed off");
    }

    #[test]
    fn a_finished_or_forgotten_viewing_saves_nothing() {
        let harness = Harness::new();
        let stream = harness.stream(1000);
        deliver(&stream, 10_000_000);
        harness.advance(5);
        harness.inference.finish(&key());
        assert!(harness.positions().is_empty());
        let harness = Harness::new();
        let stream = harness.stream(1000);
        deliver(&stream, 10_000_000);
        harness.advance(5);
        harness.inference.forget(&key());
        assert!(harness.positions().is_empty());
        drop(stream);
    }

    #[test]
    fn a_file_without_an_index_waits_for_it_or_saves_nothing() {
        let harness = Harness::new();
        let other = (key().0, FileId::derive(&["Other.mkv"]));
        let watched = harness.inference.begin(
            other.clone(),
            FileIndex(1),
            "Other.mkv".into(),
            10,
            0,
            &harness.clock,
        );
        deliver(&watched, 10_000_000);
        let pending = harness.inference.pending_indexes();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].key, other);
        harness.advance(20);
        assert!(harness.positions().is_empty(), "the index is pending");
        harness.inference.set_index(other, None);
        assert!(harness.inference.pending_indexes().is_empty());
        assert!(harness.positions().is_empty(), "no index, no estimate");
    }

    #[test]
    fn indexes_past_the_most_drop_the_oldest() {
        let mut indexes = Indexes::default();
        let file = |n: usize| (key().0, FileId::derive(&[&format!("{n}.mkv")]));
        for n in 0..=INDEXES_MAX {
            indexes.insert(file(n), IndexState::Missing);
        }
        assert_eq!(indexes.entries.len(), INDEXES_MAX);
        assert!(!indexes.entries.contains_key(&file(0)));
        assert!(indexes.entries.contains_key(&file(INDEXES_MAX)));
        indexes.forget_torrent(&key().0);
        assert!(indexes.entries.is_empty() && indexes.order.is_empty());
    }
}
