// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! librqbit storage in files, for torrents in disk storage mode, as ADR 9
//! decides.
//!
//! Each disk torrent has a directory in the download directory, named by
//! its torrent ID, which holds its files at the paths [`paths`] maps from
//! its metadata; padding files have no file. Its [resume
//! bitfield](bitfield) sits beside the directory and records each piece as
//! it passes its hash check, so the torrent starts again without rereading
//! its data.
//!
//! The storage never deletes anything: TorrPlay deletes a torrent's
//! directory and bitfield itself, after closing the files, when the torrent
//! is deleted or moves to memory storage.

pub mod bitfield;
mod io;
pub mod paths;

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::Context;
use librqbit::storage::{BoxStorageFactory, StorageFactory, StorageFactoryExt, TorrentStorage};
use librqbit::{ManagedTorrentShared, TorrentMetadata};
use librqbit_core::hash_id::Id20;
use librqbit_core::lengths::ValidPieceIndex;
use torrplay_core::metadata::ReportedMetadata;

use self::bitfield::ResumeBitfield;
use self::io::FileAt;
use crate::inspect;

/// Creates a [`DiskStorage`] for each torrent added with it, in the
/// directory [`expect`](Self::expect) named for the torrent.
#[derive(Clone, Default)]
pub struct DiskStorageFactory {
    // Torrents about to be added, by info hash.
    expected: Arc<Mutex<HashMap<Id20, Expected>>>,
}

/// Where a torrent about to be added keeps its files, and its bitfield.
struct Expected {
    directory: PathBuf,
    bits: Vec<u8>,
}

impl DiskStorageFactory {
    /// Creates a factory that expects no torrent yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// Makes the next torrent added with `info_hash` keep its files in
    /// `directory`, with `bits` as its resume bitfield: the pieces the files
    /// hold, which the torrent's first check samples.
    pub fn expect(&self, info_hash: Id20, directory: PathBuf, bits: Vec<u8>) -> anyhow::Result<()> {
        self.expected
            .lock()
            .map_err(|_| anyhow::anyhow!("the factory was poisoned"))?
            .insert(info_hash, Expected { directory, bits });
        Ok(())
    }

    /// The factory boxed for librqbit.
    pub fn into_boxed(self) -> BoxStorageFactory {
        self.boxed()
    }
}

impl StorageFactory for DiskStorageFactory {
    type Storage = DiskStorage;

    fn create(
        &self,
        shared: &ManagedTorrentShared,
        _metadata: &TorrentMetadata,
    ) -> anyhow::Result<DiskStorage> {
        let expected = self
            .expected
            .lock()
            .map_err(|_| anyhow::anyhow!("the factory was poisoned"))?
            .remove(&shared.info_hash)
            .context("the torrent was added for disk storage without a directory")?;
        Ok(DiskStorage {
            pending: Some(expected),
            files: Mutex::new(None),
        })
    }

    fn clone_box(&self) -> BoxStorageFactory {
        self.clone().boxed()
    }
}

/// A disk torrent's open files and resume bitfield.
pub struct DiskFiles {
    // One per file in metadata order; `None` for a padding file.
    files: Vec<Option<File>>,
    bitfield: ResumeBitfield,
}

impl DiskFiles {
    /// Creates `directory` and the files `metadata` lists in it, keeping
    /// any data they hold, and replaces the resume bitfield beside it with
    /// `bits`.
    pub fn open(
        directory: &Path,
        metadata: &ReportedMetadata,
        bits: &[u8],
    ) -> anyhow::Result<Self> {
        let paths = paths::map_files(metadata)?;
        let mut files = Vec::with_capacity(paths.len());
        for path in paths {
            let Some(path) = path else {
                files.push(None);
                continue;
            };
            let path = directory.join(path);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("creating {}", parent.display()))?;
            }
            let file = OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .open(&path)
                .with_context(|| format!("opening {}", path.display()))?;
            files.push(Some(file));
        }
        let bitfield = ResumeBitfield::create(&bitfield::path_for(directory), bits)
            .context("writing the resume bitfield")?;
        Ok(Self { files, bitfield })
    }

    fn file(&self, file_id: usize) -> anyhow::Result<Option<&File>> {
        Ok(self
            .files
            .get(file_id)
            .with_context(|| format!("the torrent has no file {file_id}"))?
            .as_ref())
    }

    /// Fills `buf` from `offset` in file `file_id`; a padding file reads as
    /// zeros.
    pub fn read(&self, file_id: usize, offset: u64, buf: &mut [u8]) -> anyhow::Result<()> {
        match self.file(file_id)? {
            Some(file) => Ok(file.read_exact_at(buf, offset)?),
            None => {
                buf.fill(0);
                Ok(())
            }
        }
    }

    /// Writes `buf` at `offset` in file `file_id`; a padding file keeps
    /// nothing.
    pub fn write(&self, file_id: usize, offset: u64, buf: &[u8]) -> anyhow::Result<()> {
        match self.file(file_id)? {
            Some(file) => Ok(file.write_all_at(buf, offset)?),
            None => Ok(()),
        }
    }

    /// Sets file `file_id`'s length without writing data, so it stays sparse
    /// where the filesystem allows.
    pub fn set_length(&self, file_id: usize, length: u64) -> anyhow::Result<()> {
        if let Some(file) = self.file(file_id)? {
            file.set_len(length)?;
        }
        Ok(())
    }

    /// The resume bitfield.
    pub fn bitfield(&self) -> &ResumeBitfield {
        &self.bitfield
    }

    /// Makes the files' data and the resume bitfield durable.
    pub fn flush(&self) -> anyhow::Result<()> {
        for file in self.files.iter().flatten() {
            file.sync_data()?;
        }
        Ok(self.bitfield.flush()?)
    }
}

/// librqbit storage over a torrent's [`DiskFiles`].
pub struct DiskStorage {
    // Taken by `init`.
    pending: Option<Expected>,
    // Absent until `init`, and once a later state of the torrent took them.
    files: Mutex<Option<Arc<DiskFiles>>>,
}

impl DiskStorage {
    fn files(&self) -> anyhow::Result<Arc<DiskFiles>> {
        self.files
            .lock()
            .map_err(|_| anyhow::anyhow!("the storage was poisoned"))?
            .clone()
            .context("the storage is not open, or a later state of the torrent took it")
    }
}

impl TorrentStorage for DiskStorage {
    fn init(
        &mut self,
        _shared: &ManagedTorrentShared,
        metadata: &TorrentMetadata,
    ) -> anyhow::Result<()> {
        let Expected { directory, bits } = self
            .pending
            .take()
            .context("the storage was opened already")?;
        let reported = inspect::reported_metadata(&metadata.info);
        let files = DiskFiles::open(&directory, &reported, &bits)?;
        *self
            .files
            .lock()
            .map_err(|_| anyhow::anyhow!("the storage was poisoned"))? = Some(Arc::new(files));
        Ok(())
    }

    fn pread_exact(&self, file_id: usize, offset: u64, buf: &mut [u8]) -> anyhow::Result<()> {
        self.files()?.read(file_id, offset, buf)
    }

    fn pwrite_all(&self, file_id: usize, offset: u64, buf: &[u8]) -> anyhow::Result<()> {
        self.files()?.write(file_id, offset, buf)
    }

    // TorrPlay deletes a torrent's directory itself, after closing its
    // files.
    fn remove_file(&self, _file_id: usize, _filename: &Path) -> anyhow::Result<()> {
        Ok(())
    }

    fn remove_directory_if_empty(&self, _path: &Path) -> anyhow::Result<()> {
        Ok(())
    }

    fn ensure_file_length(&self, file_id: usize, length: u64) -> anyhow::Result<()> {
        self.files()?.set_length(file_id, length)
    }

    fn take(&self) -> anyhow::Result<Box<dyn TorrentStorage>> {
        let files = self
            .files
            .lock()
            .map_err(|_| anyhow::anyhow!("the storage was poisoned"))?
            .take()
            .context("the storage was already taken")?;
        Ok(Box::new(DiskStorage {
            pending: None,
            files: Mutex::new(Some(files)),
        }))
    }

    fn on_piece_completed(&self, piece: ValidPieceIndex) -> anyhow::Result<()> {
        Ok(self.files()?.bitfield().set(piece.get())?)
    }
}

#[cfg(test)]
mod tests {
    use torrplay_core::metadata::ReportedFile;

    use super::*;

    fn metadata() -> ReportedMetadata {
        let file = |path: &[&str], size_bytes, padding| ReportedFile {
            path: path.iter().map(|part| part.as_bytes().to_vec()).collect(),
            size_bytes,
            padding,
        };
        ReportedMetadata {
            name: b"Example".to_vec(),
            files: vec![
                file(&["Example", "a?.bin"], 10, false),
                file(&["Example", ".pad", "6"], 6, true),
                file(&["Example", "sub", "b.bin"], 8, false),
            ],
        }
    }

    #[test]
    fn files_open_at_their_mapped_paths_and_keep_their_data() {
        let dir = tempfile::tempdir().unwrap();
        let torrent = dir.path().join("trr_x");
        let files = DiskFiles::open(&torrent, &metadata(), &[0]).unwrap();
        files.write(0, 2, b"abc").unwrap();
        files.write(2, 0, b"12345678").unwrap();
        files.write(1, 0, b"ignored").unwrap();
        files.set_length(0, 10).unwrap();
        files.flush().unwrap();
        drop(files);

        assert!(torrent.join("Example").join("a%3F.bin").is_file());
        assert!(
            !torrent.join("Example").join(".pad").exists(),
            "padding files have no file"
        );
        let files = DiskFiles::open(&torrent, &metadata(), &[0]).unwrap();
        let mut read = [0; 5];
        files.read(0, 1, &mut read).unwrap();
        assert_eq!(
            &read, b"\0abc\0",
            "data survives reopening, holes read as zeros"
        );
        let mut padding = [1; 6];
        files.read(1, 0, &mut padding).unwrap();
        assert_eq!(padding, [0; 6]);
        let mut past_end = [0; 4];
        assert!(
            files.read(2, 6, &mut past_end).is_err(),
            "a read past the end of a file fails"
        );
    }

    #[test]
    fn pieces_completed_reach_the_resume_bitfield() {
        let dir = tempfile::tempdir().unwrap();
        let torrent = dir.path().join("trr_x");
        let files = DiskFiles::open(&torrent, &metadata(), &[0b0100_0000]).unwrap();
        files.bitfield().set(0).unwrap();
        files.flush().unwrap();
        assert_eq!(
            bitfield::load(&bitfield::path_for(&torrent), 3).unwrap(),
            Some(vec![0b1100_0000])
        );
    }

    #[test]
    fn files_the_mapping_refuses_are_never_created() {
        let dir = tempfile::tempdir().unwrap();
        let mut colliding = metadata();
        colliding.files[2].path = vec![b"Example".to_vec(), b"A?.bin".to_vec()];
        assert!(DiskFiles::open(&dir.path().join("trr_x"), &colliding, &[0]).is_err());
        assert!(!dir.path().join("trr_x").exists());
    }
}
