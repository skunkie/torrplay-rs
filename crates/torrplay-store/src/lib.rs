// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! TorrPlay's persistent store of torrents, in a SQLite database as
//! [ADR 8](../../../docs/adr/0008-database.md) decides.
//!
//! The application reads its own in-memory records and writes through to
//! the store: every mutation is one durable transaction, and the caller
//! changes its records only after the store committed. The database is read
//! once, at startup, by [`Store::open`], which returns the stored torrents.
//!
//! One worker thread owns the database connection. The async methods send it
//! commands over a bounded channel and await its reply, so async tasks never
//! block on disk I/O and writes are serialized in one place. The connection
//! holds the database file's lock for as long as the store is open, so a
//! second application on the same data directory fails to open it instead of
//! interleaving its writes.

use std::path::{Path, PathBuf};
#[cfg(feature = "test-support")]
use std::sync::Arc;
#[cfg(feature = "test-support")]
use std::sync::atomic::AtomicBool;
use std::sync::{Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::SystemTime;

use tokio::sync::{mpsc, oneshot};
use torrplay_core::id::{InfoHashV1, TorrentId};
use torrplay_core::lifecycle::Configuration;

mod schema;
mod worker;

/// The database's file name in the data directory.
pub const FILE_NAME: &str = "torrplay.db";

/// How many commands may wait for the worker before senders wait too.
const QUEUE: usize = 64;

/// A stored torrent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredTorrent {
    /// The torrent's ID.
    pub id: TorrentId,
    /// The v1 info hash, unique among stored torrents.
    pub info_hash: InfoHashV1,
    /// The creation time, kept to the nanosecond, since listings sort by it.
    pub created_at: SystemTime,
    /// The configuration, whose version gives its entity tag.
    pub configuration: Configuration,
    /// The magnet URI the torrent was created from, if it was.
    pub magnet_uri: Option<String>,
    /// The torrent file: an uploaded one's original bytes, or the one built
    /// from retrieved metadata and the magnet URI's trackers. At least one of
    /// this and `magnet_uri` is present.
    pub torrent_file: Option<Vec<u8>>,
}

/// A failure of the store.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// The data directory could not be created.
    #[error("creating the data directory {path}: {source}")]
    DataDirectory {
        /// The directory.
        path: PathBuf,
        /// Why it failed.
        source: std::io::Error,
    },
    /// SQLite failed.
    #[error("SQLite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    /// Another process has the database open.
    #[error("{path} is in use by another process")]
    InUse {
        /// The database file.
        path: PathBuf,
    },
    /// The database was written by a newer schema than this build knows.
    #[error("the database's schema version {found} is newer than this build's {supported}")]
    TooNew {
        /// The version found.
        found: u32,
        /// The newest version this build knows.
        supported: u32,
    },
    /// Stored data cannot be read back.
    #[error("the database is inconsistent: {0}")]
    Corrupt(String),
    /// No stored torrent has the ID.
    #[error("no stored torrent has this ID")]
    UnknownTorrent,
    /// The store was closed, or its worker ended.
    #[error("the store is closed")]
    Closed,
    /// The worker thread panicked.
    #[error("the store's worker panicked")]
    WorkerPanicked,
    /// A write failed on purpose, through [`Store::fail_writes`].
    #[cfg(feature = "test-support")]
    #[error("writes fail on purpose")]
    Injected,
}

type Reply<T> = oneshot::Sender<Result<T, StoreError>>;

/// A mutation the worker applies in one transaction.
enum Command {
    Insert(StoredTorrent, Reply<()>),
    SetConfiguration(TorrentId, Configuration, Reply<()>),
    SetTorrentFile(TorrentId, Vec<u8>, Reply<()>),
    Delete(TorrentId, Reply<()>),
}

/// An open store.
pub struct Store {
    path: PathBuf,
    commands: Mutex<Option<mpsc::Sender<Command>>>,
    worker: Mutex<Option<JoinHandle<Result<(), StoreError>>>>,
    #[cfg(feature = "test-support")]
    failing: Arc<AtomicBool>,
}

impl Store {
    /// Opens or creates the database in `directory`, migrating it to the
    /// current schema, and returns it with the stored torrents in creation
    /// order. Blocks on disk I/O, so async callers run it on a blocking
    /// thread.
    pub fn open(directory: &Path) -> Result<(Self, Vec<StoredTorrent>), StoreError> {
        std::fs::create_dir_all(directory).map_err(|source| StoreError::DataDirectory {
            path: directory.to_owned(),
            source,
        })?;
        let path = directory.join(FILE_NAME);
        let mut connection = schema::open(&path)?;
        let torrents = schema::torrents(&mut connection)?;
        let (sender, receiver) = mpsc::channel(QUEUE);
        #[cfg(feature = "test-support")]
        let failing = Arc::new(AtomicBool::new(false));
        let worker = worker::Worker {
            connection,
            #[cfg(feature = "test-support")]
            failing: Arc::clone(&failing),
        };
        let worker = std::thread::Builder::new()
            .name("torrplay-store".into())
            .spawn(move || worker.run(receiver))
            .map_err(|source| StoreError::DataDirectory {
                path: directory.to_owned(),
                source,
            })?;
        let store = Self {
            path,
            commands: Mutex::new(Some(sender)),
            worker: Mutex::new(Some(worker)),
            #[cfg(feature = "test-support")]
            failing,
        };
        Ok((store, torrents))
    }

    /// The database file.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Stores a new torrent. Its ID and info hash must be free.
    pub async fn insert(&self, torrent: StoredTorrent) -> Result<(), StoreError> {
        self.send(|reply| Command::Insert(torrent, reply)).await
    }

    /// Replaces a stored torrent's configuration.
    pub async fn set_configuration(
        &self,
        id: &TorrentId,
        configuration: &Configuration,
    ) -> Result<(), StoreError> {
        let (id, configuration) = (id.clone(), configuration.clone());
        self.send(|reply| Command::SetConfiguration(id, configuration, reply))
            .await
    }

    /// Stores the torrent file of a torrent that has none; a torrent file
    /// already stored is kept.
    pub async fn set_torrent_file(
        &self,
        id: &TorrentId,
        torrent_file: Vec<u8>,
    ) -> Result<(), StoreError> {
        let id = id.clone();
        self.send(|reply| Command::SetTorrentFile(id, torrent_file, reply))
            .await
    }

    /// Deletes a stored torrent and its torrent file. Deleting a torrent
    /// that is not stored does nothing.
    pub async fn delete(&self, id: &TorrentId) -> Result<(), StoreError> {
        let id = id.clone();
        self.send(|reply| Command::Delete(id, reply)).await
    }

    /// Closes the store: the worker finishes the commands sent before,
    /// checkpoints the database, and ends. Later calls fail with
    /// [`StoreError::Closed`].
    pub async fn close(&self) -> Result<(), StoreError> {
        drop(
            self.commands
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .take(),
        );
        let worker = self
            .worker
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        let Some(worker) = worker else {
            return Ok(());
        };
        tokio::task::spawn_blocking(move || worker.join())
            .await
            .map_err(|_| StoreError::WorkerPanicked)?
            .map_err(|_| StoreError::WorkerPanicked)?
    }

    /// Makes every later write fail with [`StoreError::Injected`] without
    /// touching the database, or succeed again.
    #[cfg(feature = "test-support")]
    pub fn fail_writes(&self, failing: bool) {
        self.failing
            .store(failing, std::sync::atomic::Ordering::SeqCst);
    }

    async fn send<T>(&self, command: impl FnOnce(Reply<T>) -> Command) -> Result<T, StoreError> {
        let sender = self
            .commands
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
            .ok_or(StoreError::Closed)?;
        let (reply, result) = oneshot::channel();
        sender
            .send(command(reply))
            .await
            .map_err(|_| StoreError::Closed)?;
        result.await.map_err(|_| StoreError::Closed)?
    }
}

/// Closes a store that was not closed, waiting on the calling thread for the
/// worker to finish its commands and release the database file; async
/// callers [`close`](Store::close) it instead.
impl Drop for Store {
    fn drop(&mut self) {
        drop(
            self.commands
                .get_mut()
                .unwrap_or_else(PoisonError::into_inner)
                .take(),
        );
        if let Some(worker) = self
            .worker
            .get_mut()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
        {
            // The worker's failure has no one left to report to.
            let _ = worker.join();
        }
    }
}
