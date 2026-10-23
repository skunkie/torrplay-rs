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

use crate::schema::{created_at_ns, version};
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
                    let _ = reply.send(self.write(|connection| {
                        let updated = connection.execute(
                            "UPDATE torrents SET desired_state = ?2, configuration_version = ?3
                             WHERE id = ?1",
                            params![
                                id.as_str(),
                                configuration.desired_state().as_str(),
                                version(&configuration)?
                            ],
                        )?;
                        if updated == 0 {
                            return Err(StoreError::UnknownTorrent);
                        }
                        Ok(())
                    }));
                }
                Command::SetTorrentFile(id, bytes, reply) => {
                    let _ = reply.send(self.write(|connection| {
                        let stored: bool = connection.query_row(
                            "SELECT EXISTS (SELECT 1 FROM torrents WHERE id = ?1)",
                            [id.as_str()],
                            |row| row.get(0),
                        )?;
                        if !stored {
                            return Err(StoreError::UnknownTorrent);
                        }
                        connection.execute(
                            "INSERT INTO torrent_files (torrent_id, bytes) VALUES (?1, ?2)
                             ON CONFLICT (torrent_id) DO NOTHING",
                            params![id.as_str(), bytes],
                        )?;
                        Ok(())
                    }));
                }
                Command::Delete(id, reply) => {
                    let _ = reply.send(self.write(|connection| {
                        // The torrent file goes through the foreign key.
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
}

fn insert(connection: &Connection, torrent: &StoredTorrent) -> Result<(), StoreError> {
    connection.execute(
        "INSERT INTO torrents
             (id, info_hash, created_at_ns, desired_state, configuration_version, magnet_uri)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            torrent.id.as_str(),
            torrent.info_hash.as_bytes().as_slice(),
            created_at_ns(torrent.created_at)?,
            torrent.configuration.desired_state().as_str(),
            version(&torrent.configuration)?,
            torrent.magnet_uri,
        ],
    )?;
    if let Some(bytes) = &torrent.torrent_file {
        connection.execute(
            "INSERT INTO torrent_files (torrent_id, bytes) VALUES (?1, ?2)",
            params![torrent.id.as_str(), bytes],
        )?;
    }
    Ok(())
}
