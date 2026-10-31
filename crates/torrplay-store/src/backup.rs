// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! Whole copies of the records, for backups: a snapshot of the database
//! that is read while the store goes on, and the replacement of every
//! record a restore writes in one transaction.

use std::path::Path;

use rusqlite::{Connection, OpenFlags};
use torrplay_core::id::{ProfileId, TorrentId};
use torrplay_core::profiles::Profile;

use crate::images::{self, Images};
use crate::{
    StoreError, StoredDismissal, StoredProgress, StoredSettings, StoredTemporaryProgress,
    StoredTitleProgress, StoredTorrent, profiles, schema, worker,
};

/// The owner of a poster or an avatar a [`Replacement`] describes.
#[derive(Clone, Copy, Debug)]
pub enum ImageOwner<'a> {
    /// A torrent's poster.
    Torrent(&'a TorrentId),
    /// A profile's avatar.
    Profile(&'a ProfileId),
}

/// Reads the bytes of each image a [`Replacement`] describes.
pub type ImageReader = Box<dyn FnMut(ImageOwner<'_>) -> std::io::Result<Vec<u8>> + Send>;

/// Every torrent, profile, and setting a restore writes in place of the
/// stored ones.
pub struct Replacement {
    /// The torrents, with their torrent files; their posters' descriptions
    /// name images that [`images`](Self::images) reads.
    pub torrents: Vec<StoredTorrent>,
    /// The explicit settings.
    pub settings: StoredSettings,
    /// The profiles; their avatars' descriptions name images as posters do.
    pub profiles: Vec<Profile>,
    /// File progress, of the torrents and profiles above.
    pub progress: Vec<StoredProgress>,
    /// Title progress.
    pub titles: Vec<StoredTitleProgress>,
    /// Temporary torrents' progress.
    pub temporary_progress: Vec<StoredTemporaryProgress>,
    /// Up next dismissals, of the torrents and profiles above.
    pub dismissals: Vec<StoredDismissal>,
    /// Reads each image while the transaction runs, one at a time, so they
    /// need not all be in memory.
    pub images: ImageReader,
}

/// What a [`Snapshot`] holds: the records [`Store::open`](crate::Store::open)
/// returns, without the credentials.
#[derive(Debug)]
pub struct SnapshotContents {
    /// The torrents in creation order, with their torrent files.
    pub torrents: Vec<StoredTorrent>,
    /// The explicit settings.
    pub settings: StoredSettings,
    /// The profiles.
    pub profiles: Vec<Profile>,
    /// File progress.
    pub progress: Vec<StoredProgress>,
    /// Title progress.
    pub titles: Vec<StoredTitleProgress>,
    /// Temporary torrents' progress.
    pub temporary_progress: Vec<StoredTemporaryProgress>,
    /// Up next dismissals.
    pub dismissals: Vec<StoredDismissal>,
}

/// A copy of the database that [`Store::snapshot`](crate::Store::snapshot)
/// wrote, read without the store.
pub struct Snapshot {
    connection: Connection,
}

impl Snapshot {
    /// Opens the copy at `path` read-only, with its records. Blocks on disk
    /// I/O.
    pub fn open(path: &Path) -> Result<(Self, SnapshotContents), StoreError> {
        let mut connection = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        let contents = SnapshotContents {
            torrents: schema::torrents(&mut connection)?,
            settings: schema::settings(&connection)?,
            profiles: profiles::profiles(&connection)?,
            progress: profiles::progress(&connection)?,
            titles: profiles::titles(&connection)?,
            temporary_progress: profiles::temporary_progress(&connection)?,
            dismissals: profiles::dismissals(&connection)?,
        };
        Ok((Self { connection }, contents))
    }

    /// The bytes of torrent `id`'s poster, if it has one.
    pub fn poster(&self, id: &TorrentId) -> Result<Option<Vec<u8>>, StoreError> {
        Ok(images::one(&self.connection, Images::Posters, id.as_str())?.map(|(_, bytes)| bytes))
    }

    /// The bytes of profile `id`'s avatar, if it has one.
    pub fn avatar(&self, id: &ProfileId) -> Result<Option<Vec<u8>>, StoreError> {
        Ok(images::one(&self.connection, Images::Avatars, id.as_str())?.map(|(_, bytes)| bytes))
    }
}

/// Deletes every torrent and profile, with all that refers to them, and
/// writes `replacement` instead. The credentials, the media server's UDN,
/// and the record of runs stay.
pub(crate) fn replace(
    connection: &Connection,
    mut replacement: Replacement,
) -> Result<(), StoreError> {
    // Torrent files, labels, posters, avatars, progress, and dismissals go
    // with their torrents and profiles.
    connection.execute_batch("DELETE FROM torrents; DELETE FROM profiles;")?;
    schema::set_settings(connection, &replacement.settings)?;
    for torrent in &replacement.torrents {
        worker::insert(connection, torrent)?;
        if let Some(poster) = &torrent.poster {
            let bytes = (replacement.images)(ImageOwner::Torrent(&torrent.id))
                .map_err(StoreError::Image)?;
            images::set(
                connection,
                Images::Posters,
                torrent.id.as_str(),
                poster,
                &bytes,
            )?;
        }
    }
    for profile in &replacement.profiles {
        profiles::insert_profile(connection, profile)?;
        if let Some(avatar) = &profile.avatar {
            let bytes = (replacement.images)(ImageOwner::Profile(&profile.id))
                .map_err(StoreError::Image)?;
            images::set(
                connection,
                Images::Avatars,
                profile.id.as_str(),
                avatar,
                &bytes,
            )?;
        }
    }
    for progress in &replacement.progress {
        profiles::set_progress(connection, progress)?;
    }
    for title in &replacement.titles {
        profiles::set_title_progress(connection, title)?;
    }
    for progress in &replacement.temporary_progress {
        profiles::set_temporary_progress(connection, progress)?;
    }
    for dismissal in &replacement.dismissals {
        profiles::set_dismissal(connection, dismissal)?;
    }
    Ok(())
}
