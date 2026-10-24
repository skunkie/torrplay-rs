// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! The worker thread that owns the database connection and applies each
//! command in its own transaction.

#[cfg(feature = "test-support")]
use std::sync::Arc;
#[cfg(feature = "test-support")]
use std::sync::atomic::{AtomicBool, Ordering};

use rusqlite::{Connection, TransactionBehavior, params};
use tokio::sync::mpsc;
use torrplay_core::id::TorrentId;
use torrplay_core::library::Library;
use torrplay_core::lifecycle::Configuration;

use crate::images::{self, Images};
use crate::profiles;
use crate::schema::{self, to_ns, version};
use crate::{Command, StoreError, StoredTorrent};

pub(crate) struct Worker {
    pub(crate) connection: Connection,
    #[cfg(feature = "test-support")]
    pub(crate) failing: Arc<AtomicBool>,
}

impl Worker {
    /// Applies commands until every sender is gone, then checkpoints the
    /// write-ahead log into the database file.
    pub(crate) fn run(mut self, mut commands: mpsc::Receiver<Command>) -> Result<(), StoreError> {
        while let Some(command) = commands.blocking_recv() {
            // A caller that stopped waiting no longer needs the result.
            match command {
                Command::Insert(torrent, reply) => {
                    let _ = reply.send(self.write(|connection| insert(connection, &torrent)));
                }
                Command::SetConfiguration(id, configuration, reply) => {
                    let _ =
                        reply.send(self.write(|connection| {
                            set_configuration(connection, &id, &configuration)
                        }));
                }
                Command::SetStorageMode(id, storage_mode, reply) => {
                    let _ = reply.send(self.write(|connection| {
                        let updated = connection.execute(
                            "UPDATE torrents SET storage_mode = ?2 WHERE id = ?1",
                            params![id.as_str(), storage_mode.as_str()],
                        )?;
                        if updated == 0 {
                            return Err(StoreError::UnknownTorrent);
                        }
                        Ok(())
                    }));
                }
                Command::RecordCleanShutdown(reply) => {
                    let _ = reply.send(self.write(|connection| {
                        connection
                            .execute("UPDATE runs SET clean_shutdown = 1 WHERE id = 1", [])?;
                        Ok(())
                    }));
                }
                Command::SetSettings(settings, reply) => {
                    let _ = reply
                        .send(self.write(|connection| schema::set_settings(connection, &settings)));
                }
                Command::SetTorrentFile(id, bytes, reply) => {
                    let _ = reply.send(self.write(|connection| {
                        require(connection, &id)?;
                        connection.execute(
                            "INSERT INTO torrent_files (torrent_id, bytes) VALUES (?1, ?2)
                             ON CONFLICT (torrent_id) DO NOTHING",
                            params![id.as_str(), bytes],
                        )?;
                        Ok(())
                    }));
                }
                Command::SetPoster(id, poster, bytes, reply) => {
                    let _ = reply.send(self.write(|connection| {
                        images::set(connection, Images::Posters, id.as_str(), &poster, &bytes)
                    }));
                }
                Command::DeletePoster(id, reply) => {
                    let _ = reply.send(self.write(|connection| {
                        images::delete(connection, Images::Posters, id.as_str())
                    }));
                }
                Command::Poster(id, reply) => {
                    let _ =
                        reply.send(self.read(|connection| {
                            images::one(connection, Images::Posters, id.as_str())
                        }));
                }
                Command::InsertProfile(profile, reply) => {
                    let _ = reply.send(
                        self.write(|connection| profiles::insert_profile(connection, &profile)),
                    );
                }
                Command::RenameProfile(id, name, reply) => {
                    let _ = reply.send(
                        self.write(|connection| profiles::rename_profile(connection, &id, &name)),
                    );
                }
                Command::DeleteProfile(id, reply) => {
                    let _ = reply
                        .send(self.write(|connection| profiles::delete_profile(connection, &id)));
                }
                Command::SetAvatar(id, avatar, bytes, reply) => {
                    let _ = reply.send(self.write(|connection| {
                        images::set(connection, Images::Avatars, id.as_str(), &avatar, &bytes)
                    }));
                }
                Command::DeleteAvatar(id, reply) => {
                    let _ = reply.send(self.write(|connection| {
                        images::delete(connection, Images::Avatars, id.as_str())
                    }));
                }
                Command::Avatar(id, reply) => {
                    let _ =
                        reply.send(self.read(|connection| {
                            images::one(connection, Images::Avatars, id.as_str())
                        }));
                }
                Command::SetProgress(progress, reply) => {
                    let _ = reply.send(
                        self.write(|connection| profiles::set_progress(connection, &progress)),
                    );
                }
                Command::DeleteProgress(profile, torrent, file, reply) => {
                    let _ = reply.send(self.write(|connection| {
                        profiles::delete_progress(connection, &profile, &torrent, &file)
                    }));
                }
                Command::SetDismissal(dismissal, reply) => {
                    let _ = reply.send(
                        self.write(|connection| profiles::set_dismissal(connection, &dismissal)),
                    );
                }
                Command::Delete(id, reply) => {
                    let _ = reply.send(self.write(|connection| {
                        // The torrent file, library lists, poster, progress,
                        // and dismissals go through the foreign keys.
                        connection.execute("DELETE FROM torrents WHERE id = ?1", [id.as_str()])?;
                        Ok(())
                    }));
                }
            }
        }
        self.connection
            .execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")?;
        Ok(())
    }

    /// Runs `apply` in one immediate transaction, committed only if it
    /// succeeds.
    fn write(
        &mut self,
        apply: impl FnOnce(&Connection) -> Result<(), StoreError>,
    ) -> Result<(), StoreError> {
        #[cfg(feature = "test-support")]
        if self.failing.load(Ordering::SeqCst) {
            return Err(StoreError::Injected);
        }
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        apply(&transaction)?;
        transaction.commit()?;
        Ok(())
    }

    /// Runs `read` in one deferred transaction, which takes no write lock
    /// because it only reads.
    fn read<T>(
        &mut self,
        read: impl FnOnce(&Connection) -> Result<T, StoreError>,
    ) -> Result<T, StoreError> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Deferred)?;
        let value = read(&transaction)?;
        transaction.commit()?;
        Ok(value)
    }
}

/// Fails unless torrent `id` is stored.
fn require(connection: &Connection, id: &TorrentId) -> Result<(), StoreError> {
    let stored: bool = connection.query_row(
        "SELECT EXISTS (SELECT 1 FROM torrents WHERE id = ?1)",
        [id.as_str()],
        |row| row.get(0),
    )?;
    if stored {
        Ok(())
    } else {
        Err(StoreError::UnknownTorrent)
    }
}

fn set_configuration(
    connection: &Connection,
    id: &TorrentId,
    configuration: &Configuration,
) -> Result<(), StoreError> {
    let library = configuration.library();
    let updated = connection.execute(
        "UPDATE torrents
         SET desired_state = ?2, desired_storage_mode = ?3, configuration_version = ?4,
             title = ?5, kind = ?6, year = ?7, description = ?8
         WHERE id = ?1",
        params![
            id.as_str(),
            configuration.desired_state().as_str(),
            configuration.storage_mode().as_str(),
            version(configuration)?,
            library.title,
            library.kind.map(|kind| kind.as_str()),
            library.year,
            library.description
        ],
    )?;
    if updated == 0 {
        return Err(StoreError::UnknownTorrent);
    }
    set_labels(connection, id, library)
}

/// Replaces a torrent's categories and genres.
fn set_labels(
    connection: &Connection,
    id: &TorrentId,
    library: &Library,
) -> Result<(), StoreError> {
    for (table, labels) in [
        ("torrent_categories", &library.categories),
        ("torrent_genres", &library.genres),
    ] {
        connection.execute(
            &format!("DELETE FROM {table} WHERE torrent_id = ?1"),
            [id.as_str()],
        )?;
        let mut insert = connection.prepare_cached(&format!(
            "INSERT INTO {table} (torrent_id, position, name) VALUES (?1, ?2, ?3)"
        ))?;
        for (position, name) in (0i64..).zip(labels) {
            insert.execute(params![id.as_str(), position, name])?;
        }
    }
    Ok(())
}

fn insert(connection: &Connection, torrent: &StoredTorrent) -> Result<(), StoreError> {
    let library = torrent.configuration.library();
    connection.execute(
        "INSERT INTO torrents
             (id, info_hash, created_at_ns, desired_state, desired_storage_mode,
              configuration_version, storage_mode, magnet_uri, title, kind, year,
              description)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
        params![
            torrent.id.as_str(),
            torrent.info_hash.as_bytes().as_slice(),
            to_ns(torrent.created_at)?,
            torrent.configuration.desired_state().as_str(),
            torrent.configuration.storage_mode().as_str(),
            version(&torrent.configuration)?,
            torrent.storage_mode.as_str(),
            torrent.magnet_uri,
            library.title,
            library.kind.map(|kind| kind.as_str()),
            library.year,
            library.description,
        ],
    )?;
    set_labels(connection, &torrent.id, library)?;
    if let Some(bytes) = &torrent.torrent_file {
        connection.execute(
            "INSERT INTO torrent_files (torrent_id, bytes) VALUES (?1, ?2)",
            params![torrent.id.as_str(), bytes],
        )?;
    }
    Ok(())
}
