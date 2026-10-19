// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! librqbit storage backed by the [`PieceCache`], for torrents in memory
//! storage mode.
//!
//! librqbit addresses storage by file and file-relative offset. Pieces can
//! span files, so each read and write is translated to torrent offsets and
//! split at piece boundaries before it reaches the cache.

use std::path::Path;
use std::sync::{Arc, Mutex};

use anyhow::Context;
use librqbit::storage::{BoxStorageFactory, StorageFactory, StorageFactoryExt, TorrentStorage};
use librqbit::{ManagedTorrentShared, TorrentMetadata};
use librqbit_core::lengths::{Lengths, ValidPieceIndex};

use crate::cache::{PieceCache, TorrentKey};

/// Creates a [`MemoryStorage`] for each torrent added with it.
#[derive(Clone)]
pub struct MemoryStorageFactory {
    cache: Arc<PieceCache>,
}

impl MemoryStorageFactory {
    /// Creates a factory whose torrents share `cache`.
    pub fn new(cache: Arc<PieceCache>) -> Self {
        Self { cache }
    }

    /// Returns the factory in the boxed form librqbit's options take.
    pub fn into_boxed(self) -> BoxStorageFactory {
        self.boxed()
    }
}

impl StorageFactory for MemoryStorageFactory {
    type Storage = MemoryStorage;

    fn create(
        &self,
        _shared: &ManagedTorrentShared,
        metadata: &TorrentMetadata,
    ) -> anyhow::Result<MemoryStorage> {
        let pieces = TorrentPieces {
            cache: self.cache.clone(),
            key: self.cache.register_torrent()?,
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
            |piece, length, in_piece, range| {
                Ok(pieces
                    .cache
                    .write(pieces.key, piece, length, in_piece, &buf[range])?)
            },
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
}
