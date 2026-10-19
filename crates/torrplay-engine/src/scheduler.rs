// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! Readers and the pieces they need.
//!
//! A [`TorrentScheduler`] is a torrent's librqbit [`PieceSelector`]: the
//! torrent requests only the pieces its readers need, most urgent first,
//! and downloads nothing in the background. Each reader's readahead window
//! starts at its current piece; windows share the readahead budget, so they
//! shrink as readers are added. A piece several windows cover takes its
//! most urgent place.
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
use std::io::SeekFrom;
use std::pin::Pin;
use std::sync::{Arc, Mutex, Weak};
use std::task::{Context, Poll};

use librqbit::{FileStream, ManagedTorrent, PieceSelector};
use librqbit_core::lengths::ValidPieceIndex;
use tokio::io::{AsyncRead, AsyncSeek, ReadBuf};

use crate::cache::{PieceCache, TorrentKey};
use crate::storage::{CachedTorrent, Layout};

/// A reader's place in a torrent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ReaderPosition {
    /// The file the reader reads.
    pub(crate) file: usize,
    /// The reader's offset in that file.
    pub(crate) offset: u64,
}

/// The pieces a torrent's readers need.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Plan {
    /// The pieces to request, most urgent first, each once.
    pub(crate) priorities: Vec<u32>,
    /// The pieces eviction must skip.
    pub(crate) pins: HashSet<u32>,
}

/// Plans the pieces for `readers`, whose readahead windows share
/// `readahead_bytes`.
pub(crate) fn plan(layout: &Layout, readers: &[ReaderPosition], readahead_bytes: u64) -> Plan {
    struct Span {
        current: u64,
        first: u64,
        last: u64,
    }

    let piece_length = u64::from(layout.lengths.default_piece_length());
    let spans: Vec<Span> = readers
        .iter()
        .filter_map(|reader| {
            let &(file_offset, file_length) = layout.files.get(reader.file)?;
            let last_byte = file_offset + file_length.checked_sub(1)?;
            Some(Span {
                current: (file_offset + reader.offset).min(last_byte) / piece_length,
                first: file_offset / piece_length,
                last: last_byte / piece_length,
            })
        })
        .collect();
    let Some(share) = readahead_bytes.checked_div(spans.len() as u64) else {
        return Plan::default();
    };
    let window = (share / piece_length).max(2);

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
    for span in &spans {
        add(&mut plan, span.first, false, true);
        add(&mut plan, span.last, false, true);
    }
    for distance in 1..window {
        for span in &spans {
            let piece = span.current + distance;
            if piece <= span.last {
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

#[derive(Default)]
struct State {
    binding: Option<Binding>,
    readers: BTreeMap<u64, ReaderPosition>,
    next_reader: u64,
    priorities: Vec<ValidPieceIndex>,
}

/// Selects the pieces a torrent requests from its readers' positions.
pub struct TorrentScheduler {
    readahead_bytes: u64,
    state: Mutex<State>,
}

impl TorrentScheduler {
    /// Creates a scheduler whose readers share `readahead_bytes` of
    /// readahead. Pass it as the torrent's `piece_selector` when adding it.
    pub fn new(readahead_bytes: u64) -> Arc<Self> {
        Arc::new(Self {
            readahead_bytes,
            state: Mutex::new(State::default()),
        })
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

    /// Opens a reader at the start of file `file`.
    pub async fn open(
        self: &Arc<Self>,
        torrent: &Arc<ManagedTorrent>,
        file: usize,
    ) -> anyhow::Result<Reader> {
        let stream = torrent.clone().stream(file).await?;
        let id = {
            let mut state = self.lock()?;
            let id = state.next_reader;
            state.next_reader += 1;
            state.readers.insert(id, ReaderPosition { file, offset: 0 });
            id
        };
        self.replan()?;
        Ok(Reader {
            stream,
            id,
            file,
            piece: None,
            scheduler: self.clone(),
        })
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

    fn move_reader(&self, id: u64, offset: u64) -> anyhow::Result<()> {
        if let Some(reader) = self.lock()?.readers.get_mut(&id) {
            reader.offset = offset;
        }
        self.replan()
    }

    fn remove_reader(&self, id: u64) -> anyhow::Result<()> {
        self.lock()?.readers.remove(&id);
        self.replan()
    }

    // Recomputes the plan, pins its pieces, and asks the torrent to select
    // pieces again. The cache lock is taken under the scheduler's, which
    // nothing takes the other way round; the torrent is called with neither
    // held, because it calls priority_pieces() under its own lock.
    fn replan(&self) -> anyhow::Result<()> {
        let torrent = {
            let mut state = self.lock()?;
            let state = &mut *state;
            let Some(binding) = &state.binding else {
                return Ok(());
            };
            let readers: Vec<_> = state.readers.values().copied().collect();
            let plan = plan(&binding.layout, &readers, self.readahead_bytes);
            state.priorities = plan
                .priorities
                .iter()
                .filter_map(|&piece| binding.layout.lengths.validate_piece_index(piece))
                .collect();
            binding.cache.set_pins(binding.key, plan.pins)?;
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
}

/// A stream of one file whose position steers what the torrent downloads.
pub struct Reader {
    stream: FileStream,
    id: u64,
    file: usize,
    // The piece last reported to the scheduler, relative to the file start.
    piece: Option<u64>,
    scheduler: Arc<TorrentScheduler>,
}

impl Reader {
    /// The file the reader reads.
    pub fn file(&self) -> usize {
        self.file
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
            let _ = self.scheduler.move_reader(self.id, position);
        }
    }
}

impl AsyncRead for Reader {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let result = Pin::new(&mut self.stream).poll_read(cx, buf);
        if let Poll::Ready(Ok(())) = result {
            self.moved();
        }
        result
    }
}

impl AsyncSeek for Reader {
    fn start_seek(mut self: Pin<&mut Self>, position: SeekFrom) -> std::io::Result<()> {
        Pin::new(&mut self.stream).start_seek(position)?;
        self.moved();
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

    /// Two files: 0..1000 and 1000..2050, in pieces of 100 bytes.
    fn layout() -> Layout {
        Layout {
            lengths: Lengths::new(2050, PIECE as u32).unwrap(),
            files: vec![(0, 1000), (1000, 1050)],
        }
    }

    fn at(file: usize, offset: u64) -> ReaderPosition {
        ReaderPosition { file, offset }
    }

    #[test]
    fn a_reader_pins_its_minimum_window_and_file_boundaries() {
        let plan = plan(&layout(), &[at(0, 350)], 5 * PIECE);
        assert_eq!(plan.priorities, [3, 0, 9, 4, 5, 6, 7]);
        assert_eq!(plan.pins, HashSet::from([3, 4, 0, 9]));
    }

    #[test]
    fn windows_share_the_budget_and_stop_at_the_file_end() {
        let plan = plan(&layout(), &[at(0, 850), at(1, 0)], 8 * PIECE);
        assert_eq!(
            plan.priorities,
            [8, 10, 0, 9, 20, 11, 12, 13],
            "four pieces each; the first reader's file ends at piece 9"
        );
        let crowded = super::plan(&layout(), &[at(0, 0), at(0, 300), at(1, 0)], PIECE);
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
        let before = plan(&layout(), &readers, 8 * PIECE);
        assert_eq!(before.priorities, [0, 2, 9, 1, 3, 4, 5]);
        let after = plan(&layout(), &[at(0, 0), at(0, 0)], 8 * PIECE);
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
        let plan = plan(&layout(), &readers, 2 * PIECE);
        assert_eq!(plan.pins, HashSet::from([0, 3, 6, 10]));
        assert_eq!(
            &plan.priorities[..4],
            [0, 3, 6, 10],
            "current pieces still come first"
        );
        let roomy = super::plan(&layout(), &readers, 12 * PIECE);
        assert_eq!(
            roomy.pins,
            HashSet::from([0, 3, 6, 10, 9, 20, 1, 4, 7, 11]),
            "with room, boundaries and next pieces are pinned too"
        );
    }

    #[test]
    fn no_readers_need_nothing() {
        assert_eq!(plan(&layout(), &[], 8 * PIECE), Plan::default());
    }
}
