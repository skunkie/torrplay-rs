// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! The SQLite backend.
//!
//! The database runs in WAL mode with `synchronous=FULL`, so a returned
//! commit survives power loss and readers never block the writer. One
//! connection writes, behind a mutex; every [`Snapshot`] opens its own
//! read-only connection holding a deferred transaction, which SQLite pins to
//! one state at its first read. `PRAGMA user_version` records the schema
//! version, and each migration runs in its own transaction.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use rusqlite::{Connection, OpenFlags, OptionalExtension, TransactionBehavior, params};
use serde_json::{Map, Value};

use crate::{
    Cursor, Inserted, Invalid, SCHEMA_VERSION, Snapshot, Source, StorageMode, Store, StoreError,
    Summary, Torrent, Updated, Validate, parse_settings, patched_settings,
};

/// How long a connection waits for another's lock before failing.
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// Schema version 1: everything but the storage mode.
const SCHEMA_V1: &str = "
CREATE TABLE torrents (
    id TEXT PRIMARY KEY,
    info_hash BLOB NOT NULL UNIQUE,
    created_ms INTEGER NOT NULL,
    magnet TEXT,
    configuration TEXT NOT NULL,
    version INTEGER NOT NULL
) STRICT;
CREATE INDEX torrents_by_creation ON torrents (created_ms, id);
-- Metainfo can be megabytes; listing never reads this table.
CREATE TABLE metainfo (
    id TEXT PRIMARY KEY REFERENCES torrents (id) ON DELETE CASCADE,
    bytes BLOB NOT NULL
) STRICT;
CREATE TABLE settings (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    document TEXT NOT NULL
) STRICT;
";

/// Schema version 2 adds the storage mode, memory for existing torrents.
const MIGRATE_V1_TO_V2: &str =
    "ALTER TABLE torrents ADD COLUMN storage_mode INTEGER NOT NULL DEFAULT 0;";

/// A store in a SQLite database file.
pub struct SqliteStore {
    path: PathBuf,
    writer: Mutex<Connection>,
}

impl SqliteStore {
    /// Opens or creates the database at `path` and migrates it.
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        let mut writer = connect(path, OpenFlags::default())?;
        writer.pragma_update(None, "journal_mode", "WAL")?;
        migrate(&mut writer, SCHEMA_VERSION)?;
        Ok(Self {
            path: path.to_owned(),
            writer: Mutex::new(writer),
        })
    }

    /// Opens a [`Store`] for tests that run against every backend.
    pub fn opener(path: &Path) -> Result<Box<dyn Store>, StoreError> {
        Ok(Box::new(Self::open(path)?))
    }

    /// Creates a database at schema version 1, for migration tests.
    #[doc(hidden)]
    pub fn create_v1(path: &Path, torrents: &[(String, [u8; 20], u64)]) -> Result<(), StoreError> {
        let mut connection = connect(path, OpenFlags::default())?;
        connection.pragma_update(None, "journal_mode", "WAL")?;
        migrate(&mut connection, 1)?;
        for (id, info_hash, created_ms) in torrents {
            connection.execute(
                "INSERT INTO torrents (id, info_hash, created_ms, magnet, configuration, version)
                 VALUES (?1, ?2, ?3, 'magnet:?xt=urn:btih:v1', '{}', 1)",
                params![id, info_hash.as_slice(), int(*created_ms)?],
            )?;
        }
        Ok(())
    }

    fn writer(&self) -> MutexGuard<'_, Connection> {
        // A panic while writing rolled its transaction back, so the
        // connection is still consistent.
        self.writer
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

fn connect(path: &Path, flags: OpenFlags) -> Result<Connection, StoreError> {
    let connection = Connection::open_with_flags(path, flags)?;
    connection.busy_timeout(BUSY_TIMEOUT)?;
    connection.pragma_update(None, "synchronous", "FULL")?;
    connection.pragma_update(None, "foreign_keys", "ON")?;
    Ok(connection)
}

/// Migrates the schema up to `target`, one transaction per version.
fn migrate(connection: &mut Connection, target: u32) -> Result<(), StoreError> {
    let found: u32 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if found > SCHEMA_VERSION {
        return Err(StoreError::TooNew { found });
    }
    for version in found + 1..=target {
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute_batch(match version {
            1 => SCHEMA_V1,
            2 => MIGRATE_V1_TO_V2,
            _ => return Err(StoreError::TooNew { found: version }),
        })?;
        transaction.pragma_update(None, "user_version", version)?;
        transaction.commit()?;
    }
    Ok(())
}

impl Store for SqliteStore {
    fn insert(&self, torrent: &Torrent) -> Result<Inserted, StoreError> {
        let mut writer = self.writer();
        let transaction = writer.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(existing) = transaction
            .query_row(
                "SELECT id FROM torrents WHERE info_hash = ?1",
                [torrent.info_hash.as_slice()],
                |row| row.get(0),
            )
            .optional()?
        {
            return Ok(Inserted::Duplicate { existing });
        }
        insert_row(&transaction, torrent)?;
        transaction.commit()?;
        Ok(Inserted::Created)
    }

    fn insert_batch(
        &self,
        torrents: &[Torrent],
        before_commit: &mut dyn FnMut(),
    ) -> Result<(), StoreError> {
        let mut writer = self.writer();
        let transaction = writer.transaction_with_behavior(TransactionBehavior::Immediate)?;
        for torrent in torrents {
            insert_row(&transaction, torrent)?;
        }
        before_commit();
        transaction.commit()?;
        Ok(())
    }

    fn update_configuration(
        &self,
        id: &str,
        expected: u64,
        configuration: &str,
    ) -> Result<Updated, StoreError> {
        let mut writer = self.writer();
        let transaction = writer.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current: Option<i64> = transaction
            .query_row("SELECT version FROM torrents WHERE id = ?1", [id], |row| {
                row.get(0)
            })
            .optional()?;
        let Some(current) = current.map(uint).transpose()? else {
            return Ok(Updated::NotFound);
        };
        if current != expected {
            return Ok(Updated::Conflict { current });
        }
        transaction.execute(
            "UPDATE torrents SET configuration = ?2, version = ?3 WHERE id = ?1",
            params![id, configuration, int(current + 1)?],
        )?;
        transaction.commit()?;
        Ok(Updated::Replaced(current + 1))
    }

    fn delete(&self, id: &str) -> Result<bool, StoreError> {
        // Deleting the row deletes its metainfo through the foreign key.
        let deleted = self
            .writer()
            .execute("DELETE FROM torrents WHERE id = ?1", [id])?;
        Ok(deleted > 0)
    }

    fn snapshot(&self) -> Result<Box<dyn Snapshot + '_>, StoreError> {
        let connection = connect(
            &self.path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        connection.execute_batch("BEGIN DEFERRED")?;
        // The first read pins the transaction to the current state.
        connection.query_row("SELECT count(*) FROM torrents", [], |_| Ok(()))?;
        Ok(Box::new(SqliteSnapshot { connection }))
    }

    fn patch_settings(
        &self,
        patch: &Value,
        validate: &Validate,
    ) -> Result<Result<Map<String, Value>, Invalid>, StoreError> {
        let mut writer = self.writer();
        let transaction = writer.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let stored = read_settings(&transaction)?;
        let document = match patched_settings(stored, patch, validate) {
            Ok(document) => document,
            // Dropping the transaction rolls it back.
            Err(invalid) => return Ok(Err(invalid)),
        };
        transaction.execute(
            "INSERT INTO settings (singleton, document) VALUES (1, ?1)
             ON CONFLICT (singleton) DO UPDATE SET document = excluded.document",
            [Value::Object(document.clone()).to_string()],
        )?;
        transaction.commit()?;
        Ok(Ok(document))
    }
}

fn insert_row(connection: &Connection, torrent: &Torrent) -> Result<(), StoreError> {
    let magnet = match &torrent.source {
        Source::Magnet(uri) => Some(uri.as_str()),
        Source::Metainfo(_) => None,
    };
    connection.execute(
        "INSERT INTO torrents
             (id, info_hash, created_ms, magnet, configuration, version, storage_mode)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            torrent.id,
            torrent.info_hash.as_slice(),
            int(torrent.created_ms)?,
            magnet,
            torrent.configuration,
            int(torrent.version)?,
            torrent.storage_mode.code(),
        ],
    )?;
    if let Source::Metainfo(bytes) = &torrent.source {
        connection.execute(
            "INSERT INTO metainfo (id, bytes) VALUES (?1, ?2)",
            params![torrent.id, bytes],
        )?;
    }
    Ok(())
}

fn read_settings(connection: &Connection) -> Result<Map<String, Value>, StoreError> {
    let document: Option<String> = connection
        .query_row(
            "SELECT document FROM settings WHERE singleton = 1",
            [],
            |row| row.get(0),
        )
        .optional()?;
    document.map_or_else(|| Ok(Map::new()), |text| parse_settings(&text))
}

/// SQLite integers are signed 64-bit; stored counters and times are not
/// negative.
fn int(value: u64) -> Result<i64, StoreError> {
    i64::try_from(value)
        .map_err(|_| StoreError::Corrupt(format!("{value} exceeds SQLite integers")))
}

fn uint(value: i64) -> Result<u64, StoreError> {
    u64::try_from(value).map_err(|_| StoreError::Corrupt(format!("negative value {value}")))
}

fn info_hash(bytes: Vec<u8>) -> Result<[u8; 20], StoreError> {
    bytes
        .try_into()
        .map_err(|bytes: Vec<u8>| StoreError::Corrupt(format!("{}-byte info hash", bytes.len())))
}

struct SqliteSnapshot {
    connection: Connection,
}

impl Drop for SqliteSnapshot {
    fn drop(&mut self) {
        // A read-only transaction has nothing to roll back; ending it only
        // releases its snapshot, which closing the connection also does.
        let _ = self.connection.execute_batch("ROLLBACK");
    }
}

impl Snapshot for SqliteSnapshot {
    fn get(&self, id: &str) -> Result<Option<Torrent>, StoreError> {
        let row = self
            .connection
            .query_row(
                "SELECT t.info_hash, t.created_ms, t.magnet, t.configuration, t.version,
                        t.storage_mode, m.bytes
                 FROM torrents t LEFT JOIN metainfo m ON m.id = t.id
                 WHERE t.id = ?1",
                [id],
                |row| {
                    Ok((
                        row.get::<_, Vec<u8>>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, i64>(4)?,
                        row.get::<_, i64>(5)?,
                        row.get::<_, Option<Vec<u8>>>(6)?,
                    ))
                },
            )
            .optional()?;
        let Some((hash, created_ms, magnet, configuration, version, mode, bytes)) = row else {
            return Ok(None);
        };
        let source = match (bytes, magnet) {
            (Some(bytes), _) => Source::Metainfo(bytes),
            (None, Some(uri)) => Source::Magnet(uri),
            (None, None) => return Err(StoreError::Corrupt(format!("{id} has no source"))),
        };
        Ok(Some(Torrent {
            id: id.to_owned(),
            info_hash: info_hash(hash)?,
            created_ms: uint(created_ms)?,
            source,
            configuration,
            version: uint(version)?,
            storage_mode: StorageMode::from_code(mode)?,
        }))
    }

    fn list(&self, after: Option<&Cursor>, limit: usize) -> Result<Vec<Summary>, StoreError> {
        let (created_ms, id) = after.map_or((-1, ""), |cursor| {
            (
                i64::try_from(cursor.created_ms).unwrap_or(i64::MAX),
                cursor.id.as_str(),
            )
        });
        let mut statement = self.connection.prepare_cached(
            "SELECT id, info_hash, created_ms, version FROM torrents
             WHERE (created_ms, id) > (?1, ?2)
             ORDER BY created_ms, id
             LIMIT ?3",
        )?;
        let rows = statement.query_map(
            params![created_ms, id, i64::try_from(limit).unwrap_or(i64::MAX)],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Vec<u8>>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                ))
            },
        )?;
        rows.map(|row| {
            let (id, hash, created_ms, version) = row?;
            Ok(Summary {
                id,
                info_hash: info_hash(hash)?,
                created_ms: uint(created_ms)?,
                version: uint(version)?,
            })
        })
        .collect()
    }

    fn count(&self) -> Result<u64, StoreError> {
        uint(
            self.connection
                .query_row("SELECT count(*) FROM torrents", [], |row| row.get(0))?,
        )
    }

    fn settings(&self) -> Result<Map<String, Value>, StoreError> {
        read_settings(&self.connection)
    }
}
