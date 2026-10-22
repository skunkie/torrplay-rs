// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! Readers and the pieces they need.
//!
//! A [`TorrentScheduler`] is a torrent's librqbit [`PieceSelector`]: the
//! torrent requests only the pieces its readers need, most urgent first,
//! and downloads nothing in the background. Each reader's readahead window
//! starts at its current piece; windows share the [`Playback`]'s readahead
//! budget across every torrent, so they shrink as readers are added anywhere
//! and grow back as readers are released. A piece several windows cover
//! takes its most urgent place.
//!
//! A new reader, and one after a seek, ramps its window up: it reads ahead
//! at most twice what it has read since, and at least the playback's ramp
//! bytes, until its share allows no more, as Go TorrPlay's readers do. A
//! player that reads a container header and seeks, or plays a few seconds
//! at each of several positions, then downloads little it never plays. A
//! reader that continues where it stopped, as an idle reader taken for the
//! next range request does, keeps its ramp.
//!
//! librqbit downloads a piece from one peer unless the scheduler shares it,
//! so a small window leaves peers idle between pieces. A window therefore
//! spans at least one piece more than the torrent has live peers, and each
//! reader's current and next pieces are shared: free peers join them,
//! fetching their chunks from the other end, before starting later pieces,
//! so the piece a reader needs now arrives at several peers' rate. Every
//! reader's current piece is listed before any next piece, and librqbit
//! gives each shared piece a peer before helping any, so one reader's
//! pieces cannot keep every peer from another's.
//!
//! Closing the scheduler, when its torrent pauses, fails, or is deleted,
//! cancels its streams and releases its readers, idle ones included, at
//! once: their pins and priorities go even while a canceled stream's
//! response is not being polled.
//!
//! Each reader pins its current piece. Within the readahead budget, readers
//! also pin the first and last pieces of their files, which players read
//! repeatedly for container headers and indexes, and then their next
//! pieces. The rest of a window has priority but no pin: it stays resident
//! through eviction order rather than a guarantee. This is ADR 5's bounded
//! fallback taken ahead of time: every pinned piece beyond the budget is a
//! reader's current piece, which that reader can read and leave, so pins
//! never deadlock the cache however many readers there are.

use std::collections::{BTreeMap, HashSet};
use std::future::Future;
use std::io::SeekFrom;
use std::pin::Pin;
use std::sync::{Arc, Mutex, Weak};
use std::task::{Context, Poll};

use librqbit::{FileStream, ManagedTorrent, PieceSelector};
use librqbit_core::lengths::ValidPieceIndex;
use tokio::io::{AsyncRead, AsyncSeek, ReadBuf};
use tokio_util::sync::{CancellationToken, WaitForCancellationFutureOwned};

use crate::cache::{CacheError, PieceCache, TorrentKey};
use crate::playback::Playback;
use crate::storage::{CachedTorrent, Layout};

/// A reader's place in a torrent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ReaderPosition {
    /// The file the reader reads.
    pub(crate) file: usize,
    /// The reader's offset in that file.
    pub(crate) offset: u64,
    /// Where the reader's current run of sequential reading started: where
    /// it opened or last seeked to.
    pub(crate) run_start: u64,
}

impl ReaderPosition {
    /// A reader of `file` starting a run at `offset`.
    pub(crate) fn new(file: usize, offset: u64) -> Self {
        Self {
            file,
            offset,
            run_start: offset,
        }
    }

    /// The most this reader reads ahead while it ramps up from at least
    /// `ramp_bytes`; zero means no ramp.
    fn ramp_limit(&self, ramp_bytes: u64) -> u64 {
        if ramp_bytes == 0 {
            return u64::MAX;
        }
        (2 * self.offset.saturating_sub(self.run_start)).max(ramp_bytes)
    }
}

/// The pieces a torrent's readers need.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Plan {
    /// The pieces to request, most urgent first, each once.
    pub(crate) priorities: Vec<u32>,
    /// The pieces eviction must skip.
    pub(crate) pins: HashSet<u32>,
    /// Each reader's current and next pieces, which several peers may
    /// fetch at once: every current piece first, then the next ones.
    pub(crate) shared: Vec<u32>,
}

/// Returns a torrent's share of the global readahead budget: its readers'
/// part of all readers.
pub(crate) fn torrent_budget(readahead_bytes: u64, readers: u64, all_readers: u64) -> u64 {
    let all_readers = all_readers.max(readers);
    if all_readers == 0 {
        return 0;
    }
    (u128::from(readahead_bytes) * u128::from(readers) / u128::from(all_readers)) as u64
}

/// Plans the pieces for `readers`, whose readahead windows share
/// `readahead_bytes`, ramp up from `ramp_bytes`, and span at least one piece
/// more than the torrent's `live_peers` while their shares allow.
pub(crate) fn plan(
    layout: &Layout,
    readers: &[ReaderPosition],
    readahead_bytes: u64,
    ramp_bytes: u64,
    live_peers: u64,
) -> Plan {
    struct Span {
        current: u64,
        first: u64,
        last: u64,
        // The pieces the window spans, the current one included.
        window: u64,
    }

    let piece_length = u64::from(layout.lengths.default_piece_length());
    let Some(share) = readahead_bytes.checked_div(readers.len() as u64) else {
        return Plan::default();
    };
    let spans: Vec<Span> = readers
        .iter()
        .filter_map(|reader| {
            let &(file_offset, file_length) = layout.files.get(reader.file)?;
            let last_byte = file_offset + file_length.checked_sub(1)?;
            Some(Span {
                current: (file_offset + reader.offset).min(last_byte) / piece_length,
                first: file_offset / piece_length,
                last: last_byte / piece_length,
                window: (share / piece_length)
                    .min(
                        reader
                            .ramp_limit(ramp_bytes)
                            .div_ceil(piece_length)
                            .max(live_peers + 1),
                    )
                    .max(2),
            })
        })
        .collect();
    let window = spans.iter().map(|span| span.window).max().unwrap_or(0);

    let pin_budget = (readahead_bytes / piece_length) as usize;
    let mut plan = Plan::default();
    // Pins `piece` if `always` or the budget allows it.
    let add = |plan: &mut Plan, piece: u64, always: bool, pin: bool| {
        let Ok(piece) = u32::try_from(piece) else {
            return;
        };
        if !plan.priorities.contains(&piece) {
            plan.priorities.push(piece);
        }
        if always || (pin && plan.pins.len() < pin_budget) {
            plan.pins.insert(piece);
        }
    };
    for span in &spans {
        add(&mut plan, span.current, true, true);
    }
    // Every reader's current piece comes before any next piece, so peers
    // help the reader that waits now before the one reading ahead.
    for ahead in [0, 1] {
        for span in &spans {
            if let Ok(piece) = u32::try_from(span.current + ahead)
                && piece as u64 <= span.last
                && !plan.shared.contains(&piece)
            {
                plan.shared.push(piece);
            }
        }
    }
    for span in &spans {
        add(&mut plan, span.first, false, true);
        add(&mut plan, span.last, false, true);
    }
    for distance in 1..window {
        for span in &spans {
            let piece = span.current + distance;
            if distance < span.window && piece <= span.last {
                add(&mut plan, piece, false, distance == 1);
            }
        }
    }
    plan
}

struct Binding {
    cache: Arc<PieceCache>,
    key: TorrentKey,
    layout: Layout,
    torrent: Weak<ManagedTorrent>,
}

struct State {
    binding: Option<Binding>,
    readers: BTreeMap<u64, ReaderPosition>,
    next_reader: u64,
    priorities: Vec<ValidPieceIndex>,
    // The plan's shared pieces.
    shared: Vec<ValidPieceIndex>,
    closed: bool,
    // Canceled when the scheduler closes; readers hold its child tokens.
    streams: CancellationToken,
}

/// Why a reader could not be opened.
#[derive(Debug, thiserror::Error)]
pub enum OpenError {
    /// The scheduler is closed because its torrent paused, failed, or was
    /// deleted.
    #[error("the torrent's readers are closed")]
    Closed,
    /// librqbit could not open a stream.
    #[error(transparent)]
    Engine(#[from] anyhow::Error),
}

/// Selects the pieces a torrent requests from its readers' positions.
pub struct TorrentScheduler {
    id: u64,
    playback: Arc<Playback>,
    state: Mutex<State>,
}

impl TorrentScheduler {
    /// Creates a scheduler whose readers share `playback`'s readahead
    /// budget with every other torrent's. Pass it as the torrent's
    /// `piece_selector` when adding it.
    pub fn new(playback: &Arc<Playback>) -> Arc<Self> {
        let scheduler = Arc::new(Self {
            id: playback.next_scheduler_id(),
            playback: playback.clone(),
            state: Mutex::new(State {
                binding: None,
                readers: BTreeMap::new(),
                next_reader: 0,
                priorities: Vec::new(),
                shared: Vec::new(),
                closed: false,
                streams: CancellationToken::new(),
            }),
        });
        playback.register(Arc::downgrade(&scheduler));
        scheduler
    }

    fn lock(&self) -> anyhow::Result<std::sync::MutexGuard<'_, State>> {
        self.state
            .lock()
            .map_err(|_| anyhow::anyhow!("the scheduler was poisoned"))
    }

    /// Connects the scheduler to its torrent once the torrent is attached
    /// to the cache.
    pub fn bind(
        &self,
        cached: &CachedTorrent,
        torrent: &Arc<ManagedTorrent>,
    ) -> anyhow::Result<()> {
        self.lock()?.binding = Some(Binding {
            cache: cached.cache.clone(),
            key: cached.key,
            layout: cached.layout.clone(),
            torrent: Arc::downgrade(torrent),
        });
        self.replan()
    }

    /// Opens a reader at the start of file `file`, failing with
    /// [`OpenError::Closed`] while the scheduler is closed.
    pub async fn open(
        self: &Arc<Self>,
        torrent: &Arc<ManagedTorrent>,
        file: usize,
    ) -> Result<Reader, OpenError> {
        let stream = torrent.clone().stream(file).await?;
        let (id, streams) = {
            let mut state = self.lock()?;
            if state.closed {
                return Err(OpenError::Closed);
            }
            let id = state.next_reader;
            state.next_reader += 1;
            state.readers.insert(id, ReaderPosition::new(file, 0));
            (id, state.streams.child_token())
        };
        self.playback.opened();
        self.playback.readers_changed(1, 0);
        Ok(Reader {
            stream,
            id,
            file,
            piece: None,
            canceled: Box::pin(streams.clone().cancelled_owned()),
            streams,
            scheduler: self.clone(),
        })
    }

    /// Takes an idle reader of `file` nearest to `offset`, if one exists.
    pub fn take_idle(&self, file: usize, offset: u64) -> Option<Reader> {
        self.playback.take_idle(self.id, file, offset)
    }

    /// Cancels every stream of the torrent and releases its readers, idle
    /// ones included, with their pins and priorities. Readers cannot be
    /// opened until [`reopen`](Self::reopen).
    pub fn close(&self) -> anyhow::Result<()> {
        let (removed, streams) = {
            let mut state = self.lock()?;
            state.closed = true;
            let streams = std::mem::take(&mut state.streams);
            let removed = state.readers.len() as u64;
            state.readers.clear();
            (removed, streams)
        };
        // Released idle readers find their entries gone, so they count
        // nothing again.
        self.playback.release_idle_of(self.id);
        let result = if removed > 0 {
            self.playback.readers_changed(0, removed);
            Ok(())
        } else {
            self.replan()
        };
        // Streams are canceled last, so whoever sees a stream end also sees
        // its reader released in the playback counts.
        streams.cancel();
        result
    }

    /// Lets readers be opened again after [`close`](Self::close).
    pub fn reopen(&self) -> anyhow::Result<()> {
        self.lock()?.closed = false;
        Ok(())
    }

    /// The pieces the torrent requests first, most urgent first.
    pub fn priorities(&self) -> anyhow::Result<Vec<u32>> {
        Ok(self
            .lock()?
            .priorities
            .iter()
            .map(|piece| piece.get())
            .collect())
    }

    /// The torrent's active and idle readers.
    pub fn readers(&self) -> anyhow::Result<usize> {
        Ok(self.lock()?.readers.len())
    }

    /// Moves reader `id` to `offset`, starting a new run there if
    /// `restart`.
    fn move_reader(&self, id: u64, offset: u64, restart: bool) -> anyhow::Result<()> {
        if let Some(reader) = self.lock()?.readers.get_mut(&id) {
            reader.offset = offset;
            if restart {
                reader.run_start = offset;
            }
        }
        self.replan()
    }

    fn remove_reader(&self, id: u64) -> anyhow::Result<()> {
        let removed = self.lock()?.readers.remove(&id).is_some();
        if removed {
            self.playback.readers_changed(0, 1);
        }
        Ok(())
    }

    // Recomputes the plan, pins its pieces, and asks the torrent to select
    // pieces again. The cache lock is taken under the scheduler's, which
    // nothing takes the other way round; the torrent is called with neither
    // held, because it calls priority_pieces() under its own lock.
    pub(crate) fn replan(&self) -> anyhow::Result<()> {
        // The torrent's live state is read before the scheduler's lock is
        // taken: the torrent calls priority_pieces() under its own lock.
        let bound = self
            .lock()?
            .binding
            .as_ref()
            .map(|binding| binding.torrent.clone());
        let live_peers = bound
            .and_then(|torrent| torrent.upgrade()?.live())
            .map_or(0, |live| live.stats_snapshot().peer_stats.live as u64);
        let torrent = {
            let mut state = self.lock()?;
            let state = &mut *state;
            let Some(binding) = &state.binding else {
                return Ok(());
            };
            let readers: Vec<_> = state.readers.values().copied().collect();
            let budget = torrent_budget(
                self.playback.readahead_bytes(),
                readers.len() as u64,
                self.playback.readers(),
            );
            let plan = plan(
                &binding.layout,
                &readers,
                budget,
                self.playback.readahead_ramp_bytes(),
                live_peers,
            );
            state.priorities = plan
                .priorities
                .iter()
                .filter_map(|&piece| binding.layout.lengths.validate_piece_index(piece))
                .collect();
            state.shared = plan
                .shared
                .iter()
                .filter_map(|&piece| binding.layout.lengths.validate_piece_index(piece))
                .collect();
            tracing::debug!(
                readers = ?readers,
                budget,
                live_peers,
                priorities = plan.priorities.len(),
                first_priorities = ?&plan.priorities[..plan.priorities.len().min(8)],
                shared = ?plan.shared,
                "replanned"
            );
            match binding
                .cache
                .set_plan(binding.key, plan.pins, &plan.priorities)
            {
                // The torrent's storage left the cache, as librqbit's failure
                // or deletion does; there is nothing left to pin.
                Ok(()) | Err(CacheError::UnknownTorrent(_)) => {}
                Err(error) => return Err(error.into()),
            }
            binding.torrent.clone()
        };
        if let Some(torrent) = torrent.upgrade() {
            torrent.reselect_pieces();
        }
        Ok(())
    }
}

impl PieceSelector for TorrentScheduler {
    fn priority_pieces(&self, f: &mut dyn FnMut(ValidPieceIndex) -> bool) {
        // A poisoned scheduler selects nothing; its owner sees the poisoning
        // on its next call.
        let Ok(state) = self.state.lock() else {
            return;
        };
        for &piece in &state.priorities {
            if !f(piece) {
                break;
            }
        }
    }

    fn download_other_pieces(&self) -> bool {
        false
    }

    // librqbit fetches a piece from one peer unless it is shared; sharing
    // the pieces readers need now keeps every peer busy on them.
    fn shared_pieces(&self, f: &mut dyn FnMut(ValidPieceIndex) -> bool) {
        let Ok(state) = self.state.lock() else {
            return;
        };
        for &piece in &state.shared {
            if !f(piece) {
                break;
            }
        }
    }
}

/// A stream of one file whose position steers what the torrent downloads.
///
/// Once its scheduler closes, reads fail with
/// [`std::io::ErrorKind::ConnectionAborted`].
pub struct Reader {
    stream: FileStream,
    id: u64,
    file: usize,
    // The piece last reported to the scheduler, relative to the file start.
    piece: Option<u64>,
    streams: CancellationToken,
    canceled: Pin<Box<WaitForCancellationFutureOwned>>,
    scheduler: Arc<TorrentScheduler>,
}

impl Reader {
    /// The file the reader reads.
    pub fn file(&self) -> usize {
        self.file
    }

    /// The reader's offset in its file.
    pub fn position(&self) -> u64 {
        self.stream.position()
    }

    /// Whether the reader's torrent canceled its streams.
    pub fn is_canceled(&self) -> bool {
        self.streams.is_cancelled()
    }

    /// Keeps the reader idle, with its position, pins, and priorities, so a
    /// later stream of the same file can continue it. A canceled reader is
    /// released instead.
    pub fn park(self) {
        if !self.is_canceled() {
            self.scheduler.playback.clone().park(self);
        }
    }

    pub(crate) fn scheduler_id(&self) -> u64 {
        self.scheduler.id
    }

    fn canceled_error() -> std::io::Error {
        std::io::Error::new(
            std::io::ErrorKind::ConnectionAborted,
            "the torrent's streams were canceled",
        )
    }

    /// Starts a new run at the reader's position, ramping its window up
    /// again.
    fn seeked(&mut self) {
        self.piece = None;
        let position = self.stream.position();
        // A failure leaves the old plan in place; the reader still reads.
        let _ = self.scheduler.move_reader(self.id, position, true);
        self.moved();
    }

    // Reports the position when it reaches another piece.
    fn moved(&mut self) {
        let position = self.stream.position();
        let piece = {
            let Ok(state) = self.scheduler.state.lock() else {
                return;
            };
            let Some(binding) = &state.binding else {
                return;
            };
            let Some(&(file_offset, _)) = binding.layout.files.get(self.file) else {
                return;
            };
            (file_offset + position) / u64::from(binding.layout.lengths.default_piece_length())
        };
        if self.piece != Some(piece) {
            self.piece = Some(piece);
            // A failure leaves the old plan in place; the reader still reads.
            let _ = self.scheduler.move_reader(self.id, position, false);
        }
    }
}

impl AsyncRead for Reader {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        if self.is_canceled() {
            return Poll::Ready(Err(Self::canceled_error()));
        }
        let result = Pin::new(&mut self.stream).poll_read(cx, buf);
        match result {
            Poll::Ready(Ok(())) => self.moved(),
            Poll::Pending => {
                // Registers for cancellation while the stream waits.
                if self.canceled.as_mut().poll(cx).is_ready() {
                    return Poll::Ready(Err(Self::canceled_error()));
                }
            }
            Poll::Ready(Err(_)) => {}
        }
        result
    }
}

impl AsyncSeek for Reader {
    fn start_seek(mut self: Pin<&mut Self>, position: SeekFrom) -> std::io::Result<()> {
        if self.is_canceled() {
            return Err(Self::canceled_error());
        }
        let before = self.stream.position();
        Pin::new(&mut self.stream).start_seek(position)?;
        if self.stream.position() == before {
            // Continuing where it stopped keeps the reader's run.
            self.moved();
        } else {
            self.seeked();
        }
        Ok(())
    }

    fn poll_complete(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<u64>> {
        Pin::new(&mut self.stream).poll_complete(cx)
    }
}

impl Drop for Reader {
    fn drop(&mut self) {
        let _ = self.scheduler.remove_reader(self.id);
    }
}

#[cfg(test)]
mod tests {
    use librqbit_core::lengths::Lengths;

    use super::*;

    const PIECE: u64 = 100;

    /// Wakes by recording how many readers playback counts at that moment.
    struct CountOnWake {
        playback: Arc<Playback>,
        readers: Mutex<Option<u64>>,
    }

    impl std::task::Wake for CountOnWake {
        fn wake(self: Arc<Self>) {
            *self.readers.lock().unwrap() = Some(self.playback.stats().readers);
        }
    }

    #[test]
    fn a_stream_closing_cancels_sees_its_reader_released() {
        let playback = Playback::new(crate::playback::PlaybackConfig::new(1000));
        let scheduler = TorrentScheduler::new(&playback);
        // Registers a reader as `open` does, without a librqbit stream.
        let streams = {
            let mut state = scheduler.lock().unwrap();
            state.readers.insert(0, ReaderPosition::new(0, 0));
            state.streams.child_token()
        };
        playback.readers_changed(1, 0);
        let mut canceled = Box::pin(streams.cancelled_owned());
        let wake = Arc::new(CountOnWake {
            playback: playback.clone(),
            readers: Mutex::new(None),
        });
        let waker = std::task::Waker::from(wake.clone());
        assert!(
            canceled
                .as_mut()
                .poll(&mut Context::from_waker(&waker))
                .is_pending()
        );

        scheduler.close().unwrap();
        assert_eq!(
            *wake.readers.lock().unwrap(),
            Some(0),
            "the reader is released before its stream is canceled"
        );
    }

    /// Two files: 0..1000 and 1000..2050, in pieces of 100 bytes.
    fn layout() -> Layout {
        Layout {
            lengths: Lengths::new(2050, PIECE as u32).unwrap(),
            files: vec![(0, 1000), (1000, 1050)],
        }
    }

    /// A reader whose run started long enough ago to read ahead in full.
    fn at(file: usize, offset: u64) -> ReaderPosition {
        ReaderPosition {
            run_start: 0,
            ..ReaderPosition::new(file, offset)
        }
    }

    const RAMP: u64 = 1024 * 1024;

    /// One 64-piece file in pieces of [`RAMP`] bytes.
    fn ramp_layout() -> Layout {
        let piece = RAMP;
        Layout {
            lengths: Lengths::new(64 * piece, piece as u32).unwrap(),
            files: vec![(0, 64 * piece)],
        }
    }

    /// The pieces after a reader's current one that it reads ahead.
    fn readahead(position: ReaderPosition) -> Vec<u32> {
        let piece = RAMP;
        let current = (position.offset / piece) as u32;
        plan(&ramp_layout(), &[position], 32 * piece, RAMP, 0)
            .priorities
            .into_iter()
            .filter(|&p| p > current && p != 63)
            .collect()
    }

    #[test]
    fn readahead_ramps_up_with_what_a_run_has_read() {
        let piece = RAMP;
        assert_eq!(
            readahead(ReaderPosition::new(0, 0)),
            [1],
            "a new reader reads ahead its two-piece minimum"
        );
        let read_four = ReaderPosition {
            run_start: 0,
            ..ReaderPosition::new(0, 4 * piece)
        };
        assert_eq!(
            readahead(read_four),
            (5..12).collect::<Vec<_>>(),
            "after 4 pieces, twice that, the current piece included"
        );
        let read_twenty = ReaderPosition {
            run_start: 0,
            ..ReaderPosition::new(0, 20 * piece)
        };
        assert_eq!(
            readahead(read_twenty).len(),
            31,
            "the share caps the ramp at 32 pieces"
        );
        assert_eq!(
            readahead(ReaderPosition::new(0, 40 * piece)),
            [41],
            "a seek starts the ramp again"
        );
    }

    #[test]
    fn windows_keep_every_live_peer_busy() {
        let piece = RAMP;
        let ahead = |live_peers: u64| -> usize {
            plan(
                &ramp_layout(),
                &[ReaderPosition::new(0, 0)],
                32 * piece,
                RAMP,
                live_peers,
            )
            .priorities
            .into_iter()
            .filter(|&p| p > 0 && p != 63)
            .count()
        };
        assert_eq!(ahead(0), 1, "the two-piece minimum");
        assert_eq!(
            ahead(5),
            5,
            "six pieces, one more than five live peers, the current one included"
        );
        assert_eq!(ahead(40), 31, "the share still caps the floor");
    }

    #[test]
    fn readers_share_their_current_and_next_pieces() {
        // Pieces 3 and 4 for the first reader, 8 and 9 for the second, whose
        // file ends at piece 9; the third shares the first one's. Current
        // pieces come first.
        let plan = plan(
            &layout(),
            &[at(0, 350), at(0, 850), at(0, 399)],
            8 * PIECE,
            0,
            0,
        );
        assert_eq!(plan.shared, [3, 8, 4, 9]);
        let at_end = super::plan(&layout(), &[at(0, 950)], 8 * PIECE, 0, 0);
        assert_eq!(at_end.shared, [9], "nothing past the file's last piece");
    }

    #[test]
    fn a_reader_pins_its_minimum_window_and_file_boundaries() {
        let plan = plan(&layout(), &[at(0, 350)], 5 * PIECE, 0, 0);
        assert_eq!(plan.priorities, [3, 0, 9, 4, 5, 6, 7]);
        assert_eq!(plan.pins, HashSet::from([3, 4, 0, 9]));
    }

    #[test]
    fn windows_share_the_budget_and_stop_at_the_file_end() {
        let plan = plan(&layout(), &[at(0, 850), at(1, 0)], 8 * PIECE, 0, 0);
        assert_eq!(
            plan.priorities,
            [8, 10, 0, 9, 20, 11, 12, 13],
            "four pieces each; the first reader's file ends at piece 9"
        );
        let crowded = super::plan(&layout(), &[at(0, 0), at(0, 300), at(1, 0)], PIECE, 0, 0);
        assert_eq!(
            crowded.priorities.len(),
            8,
            "a reader keeps a two-piece window however small its share"
        );
    }

    /// RDR-05: overlapping windows give each piece its most urgent place,
    /// and priorities follow a backward seek.
    #[test]
    fn overlapping_windows_take_the_most_urgent_place() {
        let readers = [at(0, 0), at(0, 200)];
        let before = plan(&layout(), &readers, 8 * PIECE, 0, 0);
        assert_eq!(before.priorities, [0, 2, 9, 1, 3, 4, 5]);
        let after = plan(&layout(), &[at(0, 0), at(0, 0)], 8 * PIECE, 0, 0);
        assert_eq!(
            after.priorities,
            [0, 9, 1, 2, 3],
            "after the second reader seeks back, it shares the first one's pieces"
        );
    }

    /// CAC-05: when the minimum windows exceed the budget, only the
    /// readers' current pieces stay pinned.
    #[test]
    fn pins_beyond_the_budget_shrink_to_current_pieces() {
        let readers = [at(0, 0), at(0, 300), at(0, 600), at(1, 0)];
        let plan = plan(&layout(), &readers, 2 * PIECE, 0, 0);
        assert_eq!(plan.pins, HashSet::from([0, 3, 6, 10]));
        assert_eq!(
            &plan.priorities[..4],
            [0, 3, 6, 10],
            "current pieces still come first"
        );
        let roomy = super::plan(&layout(), &readers, 12 * PIECE, 0, 0);
        assert_eq!(
            roomy.pins,
            HashSet::from([0, 3, 6, 10, 9, 20, 1, 4, 7, 11]),
            "with room, boundaries and next pieces are pinned too"
        );
    }

    /// RDR-06: a torrent's budget is its readers' share of all readers, so
    /// it shrinks as readers open elsewhere and grows back as they close.
    #[test]
    fn torrents_share_the_readahead_budget_by_reader_count() {
        assert_eq!(torrent_budget(1200, 1, 1), 1200);
        assert_eq!(torrent_budget(1200, 1, 3), 400);
        assert_eq!(torrent_budget(1200, 2, 3), 800);
        assert_eq!(torrent_budget(1200, 0, 3), 0);
        assert_eq!(
            torrent_budget(1200, 2, 1),
            1200,
            "a count that lags behind never grants more than the budget"
        );
        assert_eq!(torrent_budget(u64::MAX, 2, 4), u64::MAX / 2, "no overflow");
    }

    #[test]
    fn no_readers_need_nothing() {
        assert_eq!(plan(&layout(), &[], 8 * PIECE, 0, 0), Plan::default());
    }
}
