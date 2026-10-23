// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! Opening the database: connection settings, the file lock, migrations, and
//! reading the stored torrents.
//!
//! `PRAGMA user_version` holds the schema version. Each migration runs in its
//! own transaction, so a failed one leaves the version before it, and a
//! database from a newer schema is refused rather than opened. Names follow
//! ADR 8's schema conventions.

use std::path::Path;
use std::time::{Duration, SystemTime};

use rusqlite::{Connection, ErrorCode, TransactionBehavior};
use torrplay_core::id::{InfoHashV1, TorrentId};
use torrplay_core::lifecycle::{Configuration, DesiredState};

use crate::{StoreError, StoredTorrent};

/// The schema version this build creates and migrates to.
pub(crate) const VERSION: u32 = 1;

/// The schema at each version, applied in order to reach the next one.
const MIGRATIONS: [&str; VERSION as usize] = [
    // Version 1: torrents and their torrent files.
    "CREATE TABLE torrents (
        id TEXT PRIMARY KEY,
        info_hash BLOB NOT NULL CHECK (length(info_hash) = 20),
        created_at_ns INTEGER NOT NULL CHECK (created_at_ns >= 0),
        desired_state TEXT NOT NULL CHECK (desired_state IN ('started', 'paused')),
        configuration_version INTEGER NOT NULL CHECK (configuration_version >= 1),
        storage_mode TEXT NOT NULL DEFAULT 'memory' CHECK (storage_mode IN ('memory', 'disk')),
        magnet_uri TEXT,
        CONSTRAINT torrents_info_hash_key UNIQUE (info_hash)
    ) STRICT;
    CREATE INDEX torrents_created_at_ns_id_idx ON torrents (created_at_ns, id);
    -- Torrent files can be megabytes; reading a torrent's row never reads
    -- them.
    CREATE TABLE torrent_files (
        torrent_id TEXT PRIMARY KEY REFERENCES torrents (id) ON DELETE CASCADE,
        bytes BLOB NOT NULL
    ) STRICT;",
];

/// Opens the database at `path`, takes its lock, and migrates it.
pub(crate) fn open(path: &Path) -> Result<Connection, StoreError> {
    let mut connection = Connection::open(path)?;
    // Nothing else may hold the file, so waiting for it would only delay the
    // failure; rusqlite's default is to wait five seconds.
    connection.busy_timeout(Duration::ZERO)?;
    // One connection holds the file for the store's whole life, so a second
    // process fails at once instead of waiting. WAL's shared memory then lives
    // in the process, which nothing else needs.
    connection.pragma_update(None, "locking_mode", "EXCLUSIVE")?;
    connection
        .pragma_update(None, "journal_mode", "WAL")
        .map_err(|error| in_use(error, path))?;
    // Durable once a commit returns, as ADR 8 decides.
    connection.pragma_update(None, "synchronous", "FULL")?;
    connection.pragma_update(None, "foreign_keys", "ON")?;
    // An exclusive transaction takes the lock now rather than at the first
    // write.
    connection
        .execute_batch("BEGIN EXCLUSIVE; COMMIT;")
        .map_err(|error| in_use(error, path))?;
    migrate(&mut connection)?;
    Ok(connection)
}

fn in_use(error: rusqlite::Error, path: &Path) -> StoreError {
    match error.sqlite_error_code() {
        Some(ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked) => StoreError::InUse {
            path: path.to_owned(),
        },
        _ => error.into(),
    }
}

fn migrate(connection: &mut Connection) -> Result<(), StoreError> {
    let found: u32 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if found > VERSION {
        return Err(StoreError::TooNew {
            found,
            supported: VERSION,
        });
    }
    for (version, migration) in (1..).zip(MIGRATIONS).skip(found as usize) {
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute_batch(migration)?;
        transaction.pragma_update(None, "user_version", version)?;
        transaction.commit()?;
    }
    Ok(())
}

/// Reads every stored torrent in creation order, in one transaction.
pub(crate) fn torrents(connection: &mut Connection) -> Result<Vec<StoredTorrent>, StoreError> {
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Deferred)?;
    let mut statement = transaction.prepare(
        "SELECT t.id, t.info_hash, t.created_at_ns, t.desired_state, t.configuration_version,
                t.magnet_uri, f.bytes
         FROM torrents t LEFT JOIN torrent_files f ON f.torrent_id = t.id
         ORDER BY t.created_at_ns, t.id",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, Vec<u8>>(1)?,
            row.get::<_, i64>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, i64>(4)?,
            row.get::<_, Option<String>>(5)?,
            row.get::<_, Option<Vec<u8>>>(6)?,
        ))
    })?;
    let mut torrents = Vec::new();
    for row in rows {
        let (id, info_hash, created_at_ns, desired_state, version, magnet_uri, torrent_file) = row?;
        let corrupt = |what: &str| StoreError::Corrupt(format!("torrent {id}: {what}"));
        let info_hash: [u8; 20] = info_hash
            .try_into()
            .map_err(|_| corrupt("the info hash is not 20 bytes"))?;
        let created_at = u64::try_from(created_at_ns)
            .ok()
            .and_then(|nanos| SystemTime::UNIX_EPOCH.checked_add(Duration::from_nanos(nanos)))
            .ok_or_else(|| corrupt("the creation time is out of range"))?;
        let desired_state =
            DesiredState::parse(&desired_state).ok_or_else(|| corrupt("unknown desired state"))?;
        let configuration = u64::try_from(version)
            .ok()
            .and_then(|version| Configuration::from_stored(desired_state, version))
            .ok_or_else(|| corrupt("the configuration version is out of range"))?;
        if magnet_uri.is_none() && torrent_file.is_none() {
            return Err(corrupt("neither a magnet URI nor a torrent file"));
        }
        torrents.push(StoredTorrent {
            id: TorrentId::from_stored(&id).ok_or_else(|| corrupt("malformed ID"))?,
            info_hash: InfoHashV1::from_bytes(info_hash),
            created_at,
            configuration,
            magnet_uri,
            torrent_file,
        });
    }
    drop(statement);
    transaction.commit()?;
    Ok(torrents)
}

/// The creation time as SQLite's signed integer of nanoseconds since the
/// Unix epoch, which reaches the year 2262.
pub(crate) fn created_at_ns(created_at: SystemTime) -> Result<i64, StoreError> {
    created_at
        .duration_since(SystemTime::UNIX_EPOCH)
        .ok()
        .and_then(|since| i64::try_from(since.as_nanos()).ok())
        .ok_or_else(|| StoreError::Corrupt("the creation time is out of range".into()))
}

/// A configuration version as SQLite's signed integer.
pub(crate) fn version(configuration: &Configuration) -> Result<i64, StoreError> {
    i64::try_from(configuration.version())
        .map_err(|_| StoreError::Corrupt("the configuration version is out of range".into()))
}
