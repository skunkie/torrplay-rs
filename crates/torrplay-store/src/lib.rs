// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! TorrPlay's persistent store of torrents, in a SQLite database as
//! [ADR 8](../../../docs/adr/0008-database.md) decides.
//!
//! The application reads its own in-memory records and writes through to
//! the store: every mutation is one durable transaction, and the caller
//! changes its records only after the store committed. The database is read
//! once, at startup, by [`Store::open`], which returns the stored torrents,
//! except for poster and avatar images, which [`Store::poster`] and
//! [`Store::avatar`] read when they are served. Access tokens are stored by
//! their secrets' digests, never the secrets.
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
use torrplay_core::credentials::{AccessToken, PasswordHash, SigningKey};
use torrplay_core::id::{AccessTokenId, FileId, InfoHashV1, ProfileId, TorrentId, UpNextEntryId};
use torrplay_core::image::ImageInfo;
use torrplay_core::lifecycle::{Configuration, StorageMode};
use torrplay_core::profiles::{Profile, Progress, TitleKey, TitleProgress};
pub use torrplay_core::settings::Stored as StoredSettings;

mod credentials;
mod images;
mod profiles;
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
    /// The storage mode in effect, which differs from the configuration's
    /// while a transition is unfinished.
    pub storage_mode: StorageMode,
    /// The magnet URI the torrent was created from, if it was.
    pub magnet_uri: Option<String>,
    /// The torrent file: an uploaded one's original bytes, or the one built
    /// from retrieved metadata and the magnet URI's trackers. At least one of
    /// this and `magnet_uri` is present.
    pub torrent_file: Option<Vec<u8>>,
    /// The poster's description, without its image; a new torrent has
    /// none, and [`Store::insert`] ignores it.
    pub poster: Option<ImageInfo>,
}

/// A profile's stored progress of one file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredProgress {
    /// The profile.
    pub profile: ProfileId,
    /// The torrent.
    pub torrent: TorrentId,
    /// The file.
    pub file: FileId,
    /// The progress.
    pub progress: Progress,
}

/// A profile's stored progress of one title.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredTitleProgress {
    /// The profile.
    pub profile: ProfileId,
    /// The title.
    pub key: TitleKey,
    /// The progress, with the file that last played it.
    pub progress: TitleProgress,
}

/// A profile's stored progress of a temporary torrent's file, by the
/// torrent's info hash.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredTemporaryProgress {
    /// The profile.
    pub profile: ProfileId,
    /// The temporary torrent's info hash.
    pub info_hash: InfoHashV1,
    /// The file.
    pub file: FileId,
    /// The progress.
    pub progress: Progress,
}

/// A profile's stored dismissal of an up next entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredDismissal {
    /// The profile.
    pub profile: ProfileId,
    /// The entry.
    pub entry: UpNextEntryId,
    /// The entry's torrent, with which the dismissal is deleted.
    pub torrent: TorrentId,
    /// When it was dismissed.
    pub at: SystemTime,
}

/// The stored credentials.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredCredentials {
    /// The password's hash and when it was set, while authentication is on.
    pub password: Option<(PasswordHash, SystemTime)>,
    /// The key that signs playback tokens, drawn when the store first opens.
    pub key: SigningKey,
    /// The access tokens that had not expired when the store was opened, in
    /// no order.
    pub tokens: Vec<AccessToken>,
}

/// What the store held when it was opened.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Contents {
    /// The stored torrents, in creation order.
    pub torrents: Vec<StoredTorrent>,
    /// The stored settings: the explicit values and their version, or every
    /// setting unset at version 1 if none were stored.
    pub settings: StoredSettings,
    /// The stored profiles, with their avatars' descriptions, in no order.
    pub profiles: Vec<Profile>,
    /// Every profile's stored progress.
    pub progress: Vec<StoredProgress>,
    /// Every profile's stored title progress.
    pub titles: Vec<StoredTitleProgress>,
    /// Every profile's stored progress of temporary torrents' files.
    pub temporary_progress: Vec<StoredTemporaryProgress>,
    /// Every profile's stored dismissals.
    pub dismissals: Vec<StoredDismissal>,
    /// The password, the signing key, and the access tokens.
    pub credentials: StoredCredentials,
    /// The media server's device UUID, drawn when the store first opens.
    pub media_server_udn: String,
    /// Whether the last run recorded a clean shutdown. After a crash or a
    /// power loss, disk torrents' resume bitfields may be newer than their
    /// data.
    pub clean_shutdown: bool,
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
    /// No stored profile has the ID.
    #[error("no stored profile has this ID")]
    UnknownProfile,
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
    SetStorageMode(TorrentId, StorageMode, Reply<()>),
    RecordCleanShutdown(Reply<()>),
    SetSettings(StoredSettings, Reply<()>),
    SetTorrentFile(TorrentId, Vec<u8>, Reply<()>),
    SetPoster(TorrentId, ImageInfo, Vec<u8>, Reply<()>),
    DeletePoster(TorrentId, Reply<()>),
    Poster(TorrentId, Reply<Option<(ImageInfo, Vec<u8>)>>),
    TorrentFile(TorrentId, Reply<Option<Vec<u8>>>),
    InsertProfile(Profile, Reply<()>),
    RenameProfile(ProfileId, String, Reply<()>),
    DeleteProfile(ProfileId, Reply<()>),
    SetAvatar(ProfileId, ImageInfo, Vec<u8>, Reply<()>),
    DeleteAvatar(ProfileId, Reply<()>),
    Avatar(ProfileId, Reply<Option<(ImageInfo, Vec<u8>)>>),
    SetProgress(StoredProgress, Reply<()>),
    DeleteProgress(ProfileId, TorrentId, FileId, Reply<()>),
    SetTitleProgress(StoredTitleProgress, Option<TitleKey>, Reply<()>),
    SetTemporaryProgress(StoredTemporaryProgress, Reply<()>),
    DeleteTemporaryProgress(ProfileId, InfoHashV1, FileId, Reply<()>),
    ExpireTemporaryProgress(SystemTime, Reply<()>),
    Promote(StoredTorrent, InfoHashV1, Reply<()>),
    DeleteTitleProgress(ProfileId, TitleKey, Reply<()>),
    SetDismissal(StoredDismissal, Reply<()>),
    SetPassword(PasswordHash, SystemTime, bool, Reply<()>),
    RemovePassword(SigningKey, Reply<()>),
    InsertAccessToken(AccessToken, Vec<AccessTokenId>, Reply<()>),
    DeleteAccessToken(AccessTokenId, Reply<()>),
    TouchAccessToken(AccessTokenId, SystemTime, Reply<()>),
    Delete(TorrentId, Reply<()>),
}

/// An open store.
pub struct Store {
    path: PathBuf,
    commands: Mutex<Option<mpsc::Sender<Command>>>,
    worker: Mutex<Option<JoinHandle<Result<(), StoreError>>>>,
    #[cfg(feature = "test-support")]
    failing: Arc<AtomicBool>,
    #[cfg(feature = "test-support")]
    failing_reads: Arc<AtomicBool>,
}

impl Store {
    /// Opens or creates the database in `directory`, migrating it to the
    /// current schema after copying a database an earlier release wrote, and
    /// returns it with what it holds. Blocks on disk I/O, so async callers run it on a blocking
    /// thread.
    pub fn open(directory: &Path) -> Result<(Self, Contents), StoreError> {
        std::fs::create_dir_all(directory).map_err(|source| StoreError::DataDirectory {
            path: directory.to_owned(),
            source,
        })?;
        let path = directory.join(FILE_NAME);
        let mut connection = schema::open(&path)?;
        let torrents = schema::torrents(&mut connection)?;
        let settings = schema::settings(&connection)?;
        let profiles = profiles::profiles(&connection)?;
        let progress = profiles::progress(&connection)?;
        let titles = profiles::titles(&connection)?;
        let temporary_progress = profiles::temporary_progress(&connection)?;
        let dismissals = profiles::dismissals(&connection)?;
        let credentials = credentials::credentials(&mut connection, SystemTime::now())?;
        let media_server_udn = schema::media_server_udn(&mut connection)?;
        let clean_shutdown = schema::begin_run(&mut connection)?;
        let (sender, receiver) = mpsc::channel(QUEUE);
        #[cfg(feature = "test-support")]
        let (failing, failing_reads) = (
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicBool::new(false)),
        );
        let worker = worker::Worker {
            connection,
            #[cfg(feature = "test-support")]
            failing: Arc::clone(&failing),
            #[cfg(feature = "test-support")]
            failing_reads: Arc::clone(&failing_reads),
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
            #[cfg(feature = "test-support")]
            failing_reads,
        };
        Ok((
            store,
            Contents {
                torrents,
                settings,
                profiles,
                progress,
                titles,
                temporary_progress,
                dismissals,
                credentials,
                media_server_udn,
                clean_shutdown,
            },
        ))
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

    /// Records the storage mode in effect for a torrent, once a transition
    /// to it completed.
    pub async fn set_storage_mode(
        &self,
        id: &TorrentId,
        storage_mode: StorageMode,
    ) -> Result<(), StoreError> {
        let id = id.clone();
        self.send(|reply| Command::SetStorageMode(id, storage_mode, reply))
            .await
    }

    /// Records that this run shut down cleanly: disk torrents' data and
    /// resume bitfields are flushed, so the next run may trust them.
    pub async fn record_clean_shutdown(&self) -> Result<(), StoreError> {
        self.send(Command::RecordCleanShutdown).await
    }

    /// Replaces the stored settings.
    pub async fn set_settings(&self, settings: StoredSettings) -> Result<(), StoreError> {
        self.send(|reply| Command::SetSettings(settings, reply))
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

    /// Stores `bytes`, which `poster` describes, as a stored torrent's
    /// poster, replacing any it had.
    pub async fn set_poster(
        &self,
        id: &TorrentId,
        poster: &ImageInfo,
        bytes: Vec<u8>,
    ) -> Result<(), StoreError> {
        let (id, poster) = (id.clone(), poster.clone());
        self.send(|reply| Command::SetPoster(id, poster, bytes, reply))
            .await
    }

    /// Deletes a stored torrent's poster, if it has one.
    pub async fn delete_poster(&self, id: &TorrentId) -> Result<(), StoreError> {
        let id = id.clone();
        self.send(|reply| Command::DeletePoster(id, reply)).await
    }

    /// Reads a stored torrent's poster with its image, in a read-only
    /// transaction, or `None` if it has none.
    pub async fn poster(&self, id: &TorrentId) -> Result<Option<(ImageInfo, Vec<u8>)>, StoreError> {
        let id = id.clone();
        self.send(|reply| Command::Poster(id, reply)).await
    }

    /// Reads a stored torrent's torrent file, in a read-only transaction, or
    /// `None` if it has none yet.
    pub async fn torrent_file(&self, id: &TorrentId) -> Result<Option<Vec<u8>>, StoreError> {
        let id = id.clone();
        self.send(|reply| Command::TorrentFile(id, reply)).await
    }

    /// Stores a new profile. Its ID and name must be free.
    pub async fn insert_profile(&self, profile: &Profile) -> Result<(), StoreError> {
        let profile = profile.clone();
        self.send(|reply| Command::InsertProfile(profile, reply))
            .await
    }

    /// Renames a stored profile; the name must be free.
    pub async fn rename_profile(&self, id: &ProfileId, name: &str) -> Result<(), StoreError> {
        let (id, name) = (id.clone(), name.to_owned());
        self.send(|reply| Command::RenameProfile(id, name, reply))
            .await
    }

    /// Deletes a stored profile with its avatar, progress, and dismissals.
    pub async fn delete_profile(&self, id: &ProfileId) -> Result<(), StoreError> {
        let id = id.clone();
        self.send(|reply| Command::DeleteProfile(id, reply)).await
    }

    /// Stores `bytes`, which `avatar` describes, as a stored profile's
    /// avatar, replacing any it had.
    pub async fn set_avatar(
        &self,
        id: &ProfileId,
        avatar: &ImageInfo,
        bytes: Vec<u8>,
    ) -> Result<(), StoreError> {
        let (id, avatar) = (id.clone(), avatar.clone());
        self.send(|reply| Command::SetAvatar(id, avatar, bytes, reply))
            .await
    }

    /// Deletes a stored profile's avatar, if it has one.
    pub async fn delete_avatar(&self, id: &ProfileId) -> Result<(), StoreError> {
        let id = id.clone();
        self.send(|reply| Command::DeleteAvatar(id, reply)).await
    }

    /// Reads a stored profile's avatar with its image, in a read-only
    /// transaction, or `None` if it has none.
    pub async fn avatar(&self, id: &ProfileId) -> Result<Option<(ImageInfo, Vec<u8>)>, StoreError> {
        let id = id.clone();
        self.send(|reply| Command::Avatar(id, reply)).await
    }

    /// Stores a profile's progress of a file of a stored torrent.
    pub async fn set_progress(&self, progress: &StoredProgress) -> Result<(), StoreError> {
        let progress = progress.clone();
        self.send(|reply| Command::SetProgress(progress, reply))
            .await
    }

    /// Deletes a profile's progress of a file, if it has any.
    pub async fn delete_progress(
        &self,
        profile: &ProfileId,
        torrent: &TorrentId,
        file: &FileId,
    ) -> Result<(), StoreError> {
        let (profile, torrent, file) = (profile.clone(), torrent.clone(), file.clone());
        self.send(|reply| Command::DeleteProgress(profile, torrent, file, reply))
            .await
    }

    /// Stores a profile's progress of a temporary torrent's file.
    pub async fn set_temporary_progress(
        &self,
        progress: &StoredTemporaryProgress,
    ) -> Result<(), StoreError> {
        let progress = progress.clone();
        self.send(|reply| Command::SetTemporaryProgress(progress, reply))
            .await
    }

    /// Deletes a profile's progress of a temporary torrent's file, if it has
    /// any.
    pub async fn delete_temporary_progress(
        &self,
        profile: &ProfileId,
        info_hash: &InfoHashV1,
        file: &FileId,
    ) -> Result<(), StoreError> {
        let (profile, info_hash, file) = (profile.clone(), *info_hash, file.clone());
        self.send(|reply| Command::DeleteTemporaryProgress(profile, info_hash, file, reply))
            .await
    }

    /// Deletes the progress of temporary torrents' files last saved before
    /// `cutoff`.
    pub async fn expire_temporary_progress(&self, cutoff: SystemTime) -> Result<(), StoreError> {
        self.send(|reply| Command::ExpireTemporaryProgress(cutoff, reply))
            .await
    }

    /// Stores a temporary torrent that becomes persisted, as
    /// [`insert`](Self::insert) does, and in the same transaction turns the
    /// progress of the temporary torrent with the info hash into its
    /// progress.
    pub async fn promote(
        &self,
        torrent: StoredTorrent,
        info_hash: InfoHashV1,
    ) -> Result<(), StoreError> {
        self.send(|reply| Command::Promote(torrent, info_hash, reply))
            .await
    }

    /// Stores a profile's progress of a title, deleting the progress of the
    /// title `replaced`, which makes room for it, in the same transaction.
    pub async fn set_title_progress(
        &self,
        progress: &StoredTitleProgress,
        replaced: Option<&TitleKey>,
    ) -> Result<(), StoreError> {
        let progress = progress.clone();
        let replaced = replaced.cloned();
        self.send(|reply| Command::SetTitleProgress(progress, replaced, reply))
            .await
    }

    /// Deletes a profile's progress of a title, if it has any.
    pub async fn delete_title_progress(
        &self,
        profile: &ProfileId,
        key: &TitleKey,
    ) -> Result<(), StoreError> {
        let (profile, key) = (profile.clone(), key.clone());
        self.send(|reply| Command::DeleteTitleProgress(profile, key, reply))
            .await
    }

    /// Stores a profile's dismissal of an up next entry, replacing an
    /// earlier one.
    pub async fn set_dismissal(&self, dismissal: &StoredDismissal) -> Result<(), StoreError> {
        let dismissal = dismissal.clone();
        self.send(|reply| Command::SetDismissal(dismissal, reply))
            .await
    }

    /// Stores the password's hash, set at `at`, deleting every access token
    /// if `revoke`.
    pub async fn set_password(
        &self,
        hash: &PasswordHash,
        at: SystemTime,
        revoke: bool,
    ) -> Result<(), StoreError> {
        let hash = hash.clone();
        self.send(|reply| Command::SetPassword(hash, at, revoke, reply))
            .await
    }

    /// Removes the password, deletes every access token, and stores `key`
    /// as the new signing key.
    pub async fn remove_password(&self, key: &SigningKey) -> Result<(), StoreError> {
        let key = key.clone();
        self.send(|reply| Command::RemovePassword(key, reply)).await
    }

    /// Stores a new access token, deleting the `expired` ones in the same
    /// transaction.
    pub async fn insert_access_token(
        &self,
        token: &AccessToken,
        expired: &[AccessTokenId],
    ) -> Result<(), StoreError> {
        let (token, expired) = (token.clone(), expired.to_vec());
        self.send(|reply| Command::InsertAccessToken(token, expired, reply))
            .await
    }

    /// Deletes an access token, if it is stored.
    pub async fn delete_access_token(&self, id: &AccessTokenId) -> Result<(), StoreError> {
        let id = id.clone();
        self.send(|reply| Command::DeleteAccessToken(id, reply))
            .await
    }

    /// Records when a request last carried an access token, if it is still
    /// stored.
    pub async fn touch_access_token(
        &self,
        id: &AccessTokenId,
        at: SystemTime,
    ) -> Result<(), StoreError> {
        let id = id.clone();
        self.send(|reply| Command::TouchAccessToken(id, at, reply))
            .await
    }

    /// Deletes a stored torrent with its torrent file, library lists, and
    /// poster. Deleting a torrent that is not stored does nothing.
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

    /// Makes every later read of a poster, an avatar, or a torrent file
    /// fail with [`StoreError::Injected`], or succeed again.
    #[cfg(feature = "test-support")]
    pub fn fail_reads(&self, failing: bool) {
        self.failing_reads
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
