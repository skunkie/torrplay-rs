// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! librqbit storage backed by the [`PieceCache`], for torrents in memory
//! storage mode.
//!
//! librqbit addresses storage by file and file-relative offset. Pieces can
//! span files, so each read and write is translated to torrent offsets and
//! split at piece boundaries before it reaches the cache.
//!
//! librqbit asks [`TorrentStorage::reserve_piece`] before it requests a
//! piece, and the cache reserves the piece's room then or makes the request
//! wait for the evictor. Eviction goes through librqbit's completion
//! invalidation, which calls [`TorrentStorage::on_piece_invalidated`] under
//! the torrent's state lock; the piece leaves the cache there, so a reader
//! that finds it gone also finds it marked missing. A write to a piece
//! nobody reserved evicts from the write path.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex, Weak};

use anyhow::Context;
use librqbit::storage::{BoxStorageFactory, StorageFactory, StorageFactoryExt, TorrentStorage};
use librqbit::{ManagedTorrent, ManagedTorrentShared, TorrentMetadata};
use librqbit_core::lengths::{Lengths, ValidPieceIndex};

use crate::cache::{CacheError, EngineTorrent, PieceCache, Reservation, TorrentKey};

/// How many times a write evicts and retries before it fails.
const EVICTION_ROUNDS: usize = 4;

/// Creates a [`MemoryStorage`] for each torrent added with it.
#[derive(Clone)]
pub struct MemoryStorageFactory {
    cache: Arc<PieceCache>,
    // Torrents created but not attached yet, by librqbit torrent ID.
    unattached: Arc<Mutex<HashMap<usize, (TorrentKey, Layout)>>>,
}

/// Where a torrent's pieces and files lie.
#[derive(Clone)]
pub(crate) struct Layout {
    pub(crate) lengths: Lengths,
    /// The offset in the torrent and the length of each file.
    pub(crate) files: Vec<(u64, u64)>,
}

impl MemoryStorageFactory {
    /// Creates a factory whose torrents share `cache`.
    pub fn new(cache: Arc<PieceCache>) -> Self {
        Self {
            cache,
            unattached: Arc::default(),
        }
    }

    /// Returns the factory in the boxed form librqbit's options take.
    pub fn into_boxed(self) -> BoxStorageFactory {
        self.boxed()
    }

    /// Lets the cache evict the pieces of `torrent`, which was added with
    /// this factory, by invalidating their completion in the engine.
    pub fn attach(&self, torrent: &Arc<ManagedTorrent>) -> anyhow::Result<CachedTorrent> {
        let (key, layout) = self
            .unattached
            .lock()
            .map_err(|_| anyhow::anyhow!("the factory was poisoned"))?
            .remove(&torrent.id())
            .context("the torrent has no memory storage from this factory")?;
        let engine = Arc::new(EngineHandle {
            torrent: Arc::downgrade(torrent),
            lengths: layout.lengths,
        });
        self.cache.attach_engine(key, engine.clone())?;
        Ok(CachedTorrent {
            cache: self.cache.clone(),
            key,
            layout,
            engine,
        })
    }
}

/// A torrent whose pieces the cache may evict.
pub struct CachedTorrent {
    pub(crate) cache: Arc<PieceCache>,
    pub(crate) key: TorrentKey,
    pub(crate) layout: Layout,
    engine: Arc<EngineHandle>,
}

impl CachedTorrent {
    /// Records whether the torrent is started. The pieces of paused and
    /// failed torrents are evicted first.
    pub fn set_started(&self, started: bool) -> Result<(), CacheError> {
        self.cache.set_started(self.key, started)
    }

    /// Releases every piece of the torrent and returns their bytes to the
    /// capacity. Call it once the torrent is deleted: librqbit drops a
    /// deleted torrent's storage only when its last peer task ends, and until
    /// then the pieces would hold capacity that nothing can evict. Later
    /// reads and writes of the torrent fail.
    pub fn release(&self) -> Result<(), CacheError> {
        self.cache.release_torrent(self.key)
    }

    /// Evicts the given verified pieces through completion invalidation and
    /// returns how many the engine marked missing.
    pub fn evict(&self, pieces: &[u32]) -> anyhow::Result<usize> {
        self.engine.invalidate(pieces)
    }

    /// Whether a piece of the torrent has a buffer in the cache.
    pub fn is_resident(&self, piece: u32) -> Result<bool, CacheError> {
        self.cache.is_resident(self.key, piece)
    }
}

struct EngineHandle {
    // Weak, so the cache never keeps a deleted torrent alive.
    torrent: Weak<ManagedTorrent>,
    lengths: Lengths,
}

impl EngineTorrent for EngineHandle {
    fn reselect(&self) {
        if let Some(torrent) = self.torrent.upgrade() {
            torrent.reselect_pieces();
        }
    }

    fn invalidate(&self, pieces: &[u32]) -> anyhow::Result<usize> {
        let torrent = self.torrent.upgrade().context("the torrent was deleted")?;
        let pieces: Vec<_> = pieces
            .iter()
            .filter_map(|&piece| self.lengths.validate_piece_index(piece))
            .collect();
        torrent.invalidate_pieces(&pieces)
    }
}

impl StorageFactory for MemoryStorageFactory {
    type Storage = MemoryStorage;

    fn create(
        &self,
        shared: &ManagedTorrentShared,
        metadata: &TorrentMetadata,
    ) -> anyhow::Result<MemoryStorage> {
        let key = self.cache.register_torrent()?;
        self.unattached
            .lock()
            .map_err(|_| anyhow::anyhow!("the factory was poisoned"))?
            .insert(
                shared.id,
                (
                    key,
                    Layout {
                        lengths: *metadata.lengths(),
                        files: metadata
                            .file_infos
                            .iter()
                            .map(|file| (file.offset_in_torrent, file.len))
                            .collect(),
                    },
                ),
            );
        let pieces = TorrentPieces {
            cache: self.cache.clone(),
            key,
            lengths: *metadata.lengths(),
            file_offsets: metadata
                .file_infos
                .iter()
                .map(|file| file.offset_in_torrent)
                .collect(),
        };
        Ok(MemoryStorage {
            pieces: Mutex::new(Some(Arc::new(pieces))),
        })
    }

    fn clone_box(&self) -> BoxStorageFactory {
        self.clone().boxed()
    }
}

/// One torrent's view of the cache. Dropping the last holder, when the
/// torrent is deleted, releases its pieces.
struct TorrentPieces {
    cache: Arc<PieceCache>,
    key: TorrentKey,
    lengths: Lengths,
    file_offsets: Vec<u64>,
}

impl TorrentPieces {
    /// Calls `op` for each piece that bytes `offset..offset + len` of file
    /// `file_id` touch, with the piece index, the piece's length, the offset
    /// within the piece, and the range of the caller's buffer it covers.
    fn for_each_piece(
        &self,
        file_id: usize,
        offset: u64,
        len: usize,
        mut op: impl FnMut(u32, usize, usize, std::ops::Range<usize>) -> anyhow::Result<()>,
    ) -> anyhow::Result<()> {
        let file_offset = *self
            .file_offsets
            .get(file_id)
            .with_context(|| format!("no file {file_id} in the torrent"))?;
        let piece_length = u64::from(self.lengths.default_piece_length());
        let mut position = file_offset + offset;
        let mut done = 0;
        while done < len {
            let index = u32::try_from(position / piece_length)?;
            let piece = self
                .lengths
                .validate_piece_index(index)
                .with_context(|| format!("torrent offset {position} is past the last piece"))?;
            let this_length = self.lengths.piece_length(piece) as usize;
            let in_piece = usize::try_from(position % piece_length)?;
            let take = (len - done).min(this_length.saturating_sub(in_piece));
            if take == 0 {
                anyhow::bail!("torrent offset {position} is past the end of piece {index}");
            }
            op(index, this_length, in_piece, done..done + take)?;
            done += take;
            position += take as u64;
        }
        Ok(())
    }
}

impl TorrentPieces {
    fn write(
        &self,
        piece: u32,
        piece_length: usize,
        offset: usize,
        data: &[u8],
    ) -> anyhow::Result<()> {
        for _ in 0..EVICTION_ROUNDS {
            match self
                .cache
                .write(self.key, piece, piece_length, offset, data)
            {
                Err(CacheError::CapacityExceeded { needed_bytes, .. }) => {
                    let batches = self.cache.eviction_candidates(needed_bytes)?;
                    if batches.is_empty() {
                        break;
                    }
                    for batch in batches {
                        // A torrent that cannot invalidate, for example
                        // because it was deleted, frees nothing; the next
                        // round picks other pieces, and the last write
                        // reports the shortfall.
                        let _ = batch.engine.invalidate(&batch.pieces);
                    }
                }
                result => return Ok(result?),
            }
        }
        Ok(self
            .cache
            .write(self.key, piece, piece_length, offset, data)?)
    }
}

impl Drop for TorrentPieces {
    fn drop(&mut self) {
        // A poisoned cache cannot be repaired here; its later operations
        // report the poisoning instead.
        let _ = self.cache.release_torrent(self.key);
    }
}

/// A torrent's storage in the piece cache.
pub struct MemoryStorage {
    // `take` moves the pieces to a new storage when librqbit pauses the
    // torrent, which leaves this one unusable, as the trait requires.
    pieces: Mutex<Option<Arc<TorrentPieces>>>,
}

impl MemoryStorage {
    fn pieces(&self) -> anyhow::Result<Arc<TorrentPieces>> {
        self.pieces
            .lock()
            .map_err(|_| anyhow::anyhow!("the storage was poisoned"))?
            .clone()
            .context("the storage was taken by a later state of the torrent")
    }
}

impl TorrentStorage for MemoryStorage {
    fn init(
        &mut self,
        _shared: &ManagedTorrentShared,
        _metadata: &TorrentMetadata,
    ) -> anyhow::Result<()> {
        Ok(())
    }

    fn pread_exact(&self, file_id: usize, offset: u64, buf: &mut [u8]) -> anyhow::Result<()> {
        let pieces = self.pieces()?;
        pieces.for_each_piece(file_id, offset, buf.len(), |piece, _, in_piece, range| {
            Ok(pieces
                .cache
                .read(pieces.key, piece, in_piece, &mut buf[range])?)
        })
    }

    fn pwrite_all(&self, file_id: usize, offset: u64, buf: &[u8]) -> anyhow::Result<()> {
        let pieces = self.pieces()?;
        pieces.for_each_piece(
            file_id,
            offset,
            buf.len(),
            |piece, length, in_piece, range| pieces.write(piece, length, in_piece, &buf[range]),
        )
    }

    fn remove_file(&self, _file_id: usize, _filename: &Path) -> anyhow::Result<()> {
        Ok(())
    }

    fn remove_directory_if_empty(&self, _path: &Path) -> anyhow::Result<()> {
        Ok(())
    }

    fn ensure_file_length(&self, _file_id: usize, _length: u64) -> anyhow::Result<()> {
        Ok(())
    }

    fn take(&self) -> anyhow::Result<Box<dyn TorrentStorage>> {
        let pieces = self
            .pieces
            .lock()
            .map_err(|_| anyhow::anyhow!("the storage was poisoned"))?
            .take()
            .context("the storage was already taken")?;
        Ok(Box::new(MemoryStorage {
            pieces: Mutex::new(Some(pieces)),
        }))
    }

    fn on_piece_completed(&self, piece: ValidPieceIndex) -> anyhow::Result<()> {
        let pieces = self.pieces()?;
        Ok(pieces.cache.mark_verified(pieces.key, piece.get())?)
    }

    fn on_piece_invalidated(&self, piece: ValidPieceIndex) -> anyhow::Result<()> {
        let pieces = self.pieces()?;
        Ok(pieces.cache.evict(pieces.key, piece.get())?)
    }

    fn reserve_piece(&self, piece: ValidPieceIndex) -> bool {
        let Ok(pieces) = self.pieces() else {
            return false;
        };
        let length = pieces.lengths.piece_length(piece) as usize;
        // An error, such as a released torrent, leaves the piece unrequested.
        matches!(
            pieces.cache.reserve(pieces.key, piece.get(), length),
            Ok(Reservation::Reserved)
        )
    }

    fn release_piece(&self, piece: ValidPieceIndex) {
        if let Ok(pieces) = self.pieces() {
            // A poisoned cache reports itself on its next operation.
            let _ = pieces.cache.release_unverified(pieces.key, piece.get());
        }
    }
}
