// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! The redb backend.
//!
//! Commits use redb's default durability, persistent once `commit` returns.
//! redb serializes write transactions itself and gives every read
//! transaction a consistent snapshot. Records are JSON values, so a field a
//! later schema adds reads as its default instead of needing a copy of the
//! table; a key or table that changes shape is migrated by copying it into a
//! new table. The `meta` table records the schema version.

use std::ops::Bound;
use std::path::Path;

use ::redb::{
    Database, ReadTransaction, ReadableDatabase, ReadableTable, ReadableTableMetadata,
    TableDefinition, WriteTransaction,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::{
    Cursor, Inserted, Invalid, SCHEMA_VERSION, Snapshot, Source, StorageMode, Store, StoreError,
    Summary, Torrent, Updated, Validate, parse_settings, patched_settings,
};

const META: TableDefinition<&str, u32> = TableDefinition::new("meta");
/// Each torrent's record, without its metainfo.
const TORRENTS: TableDefinition<&str, &[u8]> = TableDefinition::new("torrents");
/// Metainfo can be megabytes; listing never reads this table.
const METAINFO: TableDefinition<&str, &[u8]> = TableDefinition::new("metainfo");
/// The listing order: creation time, then ID.
const BY_CREATION: TableDefinition<(u64, &str), ()> = TableDefinition::new("by_creation");
const BY_INFO_HASH: TableDefinition<&[u8], &str> = TableDefinition::new("by_info_hash");
const SETTINGS: TableDefinition<&str, &str> = TableDefinition::new("settings");

const SCHEMA_KEY: &str = "schema_version";
const SETTINGS_KEY: &str = "document";

/// A torrent's stored record.
#[derive(Serialize, Deserialize)]
struct Record {
    info_hash: [u8; 20],
    created_ms: u64,
    magnet: Option<String>,
    configuration: String,
    version: u64,
    /// Added in schema version 2; version 1 records read as memory.
    #[serde(default)]
    storage_mode: i64,
}

/// A store in a redb database file.
pub struct RedbStore {
    database: Database,
}

impl RedbStore {
    /// Opens or creates the database at `path` and migrates it.
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        let database = Database::create(path).map_err(::redb::Error::from)?;
        migrate(&database, SCHEMA_VERSION)?;
        Ok(Self { database })
    }

    /// Opens a [`Store`] for tests that run against every backend.
    pub fn opener(path: &Path) -> Result<Box<dyn Store>, StoreError> {
        Ok(Box::new(Self::open(path)?))
    }

    /// Creates a database at schema version 1, for migration tests.
    #[doc(hidden)]
    pub fn create_v1(path: &Path, torrents: &[(String, [u8; 20], u64)]) -> Result<(), StoreError> {
        #[derive(Serialize)]
        struct RecordV1<'a> {
            info_hash: [u8; 20],
            created_ms: u64,
            magnet: Option<&'a str>,
            configuration: &'a str,
            version: u64,
        }
        let database = Database::create(path).map_err(::redb::Error::from)?;
        migrate(&database, 1)?;
        let transaction = begin_write(&database)?;
        {
            let mut records = transaction.open_table(TORRENTS).map_err(redb_error)?;
            let mut by_creation = transaction.open_table(BY_CREATION).map_err(redb_error)?;
            let mut by_info_hash = transaction.open_table(BY_INFO_HASH).map_err(redb_error)?;
            for (id, info_hash, created_ms) in torrents {
                let record = RecordV1 {
                    info_hash: *info_hash,
                    created_ms: *created_ms,
                    magnet: Some("magnet:?xt=urn:btih:v1"),
                    configuration: "{}",
                    version: 1,
                };
                let bytes = serde_json::to_vec(&record)
                    .map_err(|error| StoreError::Corrupt(error.to_string()))?;
                records
                    .insert(id.as_str(), bytes.as_slice())
                    .map_err(redb_error)?;
                by_creation
                    .insert((*created_ms, id.as_str()), ())
                    .map_err(redb_error)?;
                by_info_hash
                    .insert(info_hash.as_slice(), id.as_str())
                    .map_err(redb_error)?;
            }
        }
        transaction.commit().map_err(redb_error)?;
        Ok(())
    }
}

fn redb_error(error: impl Into<::redb::Error>) -> StoreError {
    StoreError::Redb(error.into())
}

fn begin_write(database: &Database) -> Result<WriteTransaction, StoreError> {
    database.begin_write().map_err(redb_error)
}

/// Migrates the schema up to `target`, one transaction per version.
fn migrate(database: &Database, target: u32) -> Result<(), StoreError> {
    let found = {
        let transaction = database.begin_read().map_err(redb_error)?;
        match transaction.open_table(META) {
            Ok(meta) => meta
                .get(SCHEMA_KEY)
                .map_err(redb_error)?
                .map_or(0, |version| version.value()),
            Err(::redb::TableError::TableDoesNotExist(_)) => 0,
            Err(error) => return Err(redb_error(error)),
        }
    };
    if found > SCHEMA_VERSION {
        return Err(StoreError::TooNew { found });
    }
    for version in found + 1..=target {
        let transaction = begin_write(database)?;
        match version {
            // Opening a table in a write transaction creates it.
            1 => {
                transaction.open_table(TORRENTS).map_err(redb_error)?;
                transaction.open_table(METAINFO).map_err(redb_error)?;
                transaction.open_table(BY_CREATION).map_err(redb_error)?;
                transaction.open_table(BY_INFO_HASH).map_err(redb_error)?;
                transaction.open_table(SETTINGS).map_err(redb_error)?;
            }
            // The storage mode is a new record field with a default.
            2 => {}
            _ => return Err(StoreError::TooNew { found: version }),
        }
        transaction
            .open_table(META)
            .map_err(redb_error)?
            .insert(SCHEMA_KEY, version)
            .map_err(redb_error)?;
        transaction.commit().map_err(redb_error)?;
    }
    Ok(())
}

fn encode(torrent: &Torrent) -> Result<Vec<u8>, StoreError> {
    let record = Record {
        info_hash: torrent.info_hash,
        created_ms: torrent.created_ms,
        magnet: match &torrent.source {
            Source::Magnet(uri) => Some(uri.clone()),
            Source::Metainfo(_) => None,
        },
        configuration: torrent.configuration.clone(),
        version: torrent.version,
        storage_mode: torrent.storage_mode.code(),
    };
    serde_json::to_vec(&record).map_err(|error| StoreError::Corrupt(error.to_string()))
}

fn decode(bytes: &[u8]) -> Result<Record, StoreError> {
    serde_json::from_slice(bytes).map_err(|error| StoreError::Corrupt(error.to_string()))
}

fn insert_row(transaction: &WriteTransaction, torrent: &Torrent) -> Result<(), StoreError> {
    let bytes = encode(torrent)?;
    transaction
        .open_table(TORRENTS)
        .map_err(redb_error)?
        .insert(torrent.id.as_str(), bytes.as_slice())
        .map_err(redb_error)?;
    transaction
        .open_table(BY_CREATION)
        .map_err(redb_error)?
        .insert((torrent.created_ms, torrent.id.as_str()), ())
        .map_err(redb_error)?;
    transaction
        .open_table(BY_INFO_HASH)
        .map_err(redb_error)?
        .insert(torrent.info_hash.as_slice(), torrent.id.as_str())
        .map_err(redb_error)?;
    if let Source::Metainfo(bytes) = &torrent.source {
        transaction
            .open_table(METAINFO)
            .map_err(redb_error)?
            .insert(torrent.id.as_str(), bytes.as_slice())
            .map_err(redb_error)?;
    }
    Ok(())
}

impl Store for RedbStore {
    fn insert(&self, torrent: &Torrent) -> Result<Inserted, StoreError> {
        let transaction = begin_write(&self.database)?;
        let existing = transaction
            .open_table(BY_INFO_HASH)
            .map_err(redb_error)?
            .get(torrent.info_hash.as_slice())
            .map_err(redb_error)?
            .map(|id| id.value().to_owned());
        if let Some(existing) = existing {
            // Dropping the transaction aborts it.
            return Ok(Inserted::Duplicate { existing });
        }
        insert_row(&transaction, torrent)?;
        transaction.commit().map_err(redb_error)?;
        Ok(Inserted::Created)
    }

    fn insert_batch(
        &self,
        torrents: &[Torrent],
        before_commit: &mut dyn FnMut(),
    ) -> Result<(), StoreError> {
        let transaction = begin_write(&self.database)?;
        for torrent in torrents {
            insert_row(&transaction, torrent)?;
        }
        before_commit();
        transaction.commit().map_err(redb_error)?;
        Ok(())
    }

    fn update_configuration(
        &self,
        id: &str,
        expected: u64,
        configuration: &str,
    ) -> Result<Updated, StoreError> {
        let transaction = begin_write(&self.database)?;
        let updated = {
            let mut records = transaction.open_table(TORRENTS).map_err(redb_error)?;
            let record = records
                .get(id)
                .map_err(redb_error)?
                .map(|bytes| decode(bytes.value()))
                .transpose()?;
            let Some(mut record) = record else {
                return Ok(Updated::NotFound);
            };
            if record.version != expected {
                return Ok(Updated::Conflict {
                    current: record.version,
                });
            }
            record.version += 1;
            record.configuration = configuration.to_owned();
            let bytes = serde_json::to_vec(&record)
                .map_err(|error| StoreError::Corrupt(error.to_string()))?;
            records.insert(id, bytes.as_slice()).map_err(redb_error)?;
            Updated::Replaced(record.version)
        };
        transaction.commit().map_err(redb_error)?;
        Ok(updated)
    }

    fn delete(&self, id: &str) -> Result<bool, StoreError> {
        let transaction = begin_write(&self.database)?;
        let removed = transaction
            .open_table(TORRENTS)
            .map_err(redb_error)?
            .remove(id)
            .map_err(redb_error)?
            .map(|bytes| decode(bytes.value()))
            .transpose()?;
        let Some(record) = removed else {
            return Ok(false);
        };
        transaction
            .open_table(BY_CREATION)
            .map_err(redb_error)?
            .remove((record.created_ms, id))
            .map_err(redb_error)?;
        transaction
            .open_table(BY_INFO_HASH)
            .map_err(redb_error)?
            .remove(record.info_hash.as_slice())
            .map_err(redb_error)?;
        transaction
            .open_table(METAINFO)
            .map_err(redb_error)?
            .remove(id)
            .map_err(redb_error)?;
        transaction.commit().map_err(redb_error)?;
        Ok(true)
    }

    fn snapshot(&self) -> Result<Box<dyn Snapshot + '_>, StoreError> {
        let transaction = self.database.begin_read().map_err(redb_error)?;
        Ok(Box::new(RedbSnapshot { transaction }))
    }

    fn patch_settings(
        &self,
        patch: &Value,
        validate: &Validate,
    ) -> Result<Result<Map<String, Value>, Invalid>, StoreError> {
        let transaction = begin_write(&self.database)?;
        let document = {
            let mut table = transaction.open_table(SETTINGS).map_err(redb_error)?;
            let stored = table
                .get(SETTINGS_KEY)
                .map_err(redb_error)?
                .map(|text| parse_settings(text.value()))
                .transpose()?
                .unwrap_or_default();
            let document = match patched_settings(stored, patch, validate) {
                Ok(document) => document,
                // Dropping the transaction aborts it.
                Err(invalid) => return Ok(Err(invalid)),
            };
            table
                .insert(
                    SETTINGS_KEY,
                    Value::Object(document.clone()).to_string().as_str(),
                )
                .map_err(redb_error)?;
            document
        };
        transaction.commit().map_err(redb_error)?;
        Ok(Ok(document))
    }
}

struct RedbSnapshot {
    transaction: ReadTransaction,
}

impl Snapshot for RedbSnapshot {
    fn get(&self, id: &str) -> Result<Option<Torrent>, StoreError> {
        let records = self.transaction.open_table(TORRENTS).map_err(redb_error)?;
        let Some(record) = records
            .get(id)
            .map_err(redb_error)?
            .map(|bytes| decode(bytes.value()))
            .transpose()?
        else {
            return Ok(None);
        };
        let metainfo = self
            .transaction
            .open_table(METAINFO)
            .map_err(redb_error)?
            .get(id)
            .map_err(redb_error)?
            .map(|bytes| bytes.value().to_vec());
        let source = match (metainfo, record.magnet) {
            (Some(bytes), _) => Source::Metainfo(bytes),
            (None, Some(uri)) => Source::Magnet(uri),
            (None, None) => return Err(StoreError::Corrupt(format!("{id} has no source"))),
        };
        Ok(Some(Torrent {
            id: id.to_owned(),
            info_hash: record.info_hash,
            created_ms: record.created_ms,
            source,
            configuration: record.configuration,
            version: record.version,
            storage_mode: StorageMode::from_code(record.storage_mode)?,
        }))
    }

    fn list(&self, after: Option<&Cursor>, limit: usize) -> Result<Vec<Summary>, StoreError> {
        let order = self
            .transaction
            .open_table(BY_CREATION)
            .map_err(redb_error)?;
        let records = self.transaction.open_table(TORRENTS).map_err(redb_error)?;
        let start = match after {
            Some(cursor) => Bound::Excluded((cursor.created_ms, cursor.id.as_str())),
            None => Bound::Unbounded,
        };
        let mut page = Vec::new();
        for entry in order
            .range::<(u64, &str)>((start, Bound::Unbounded))
            .map_err(redb_error)?
            .take(limit)
        {
            let (key, _) = entry.map_err(redb_error)?;
            let (created_ms, id) = key.value();
            let record = records
                .get(id)
                .map_err(redb_error)?
                .map(|bytes| decode(bytes.value()))
                .transpose()?
                .ok_or_else(|| StoreError::Corrupt(format!("{id} is listed but missing")))?;
            page.push(Summary {
                id: id.to_owned(),
                info_hash: record.info_hash,
                created_ms,
                version: record.version,
            });
        }
        Ok(page)
    }

    fn count(&self) -> Result<u64, StoreError> {
        self.transaction
            .open_table(TORRENTS)
            .map_err(redb_error)?
            .len()
            .map_err(redb_error)
    }

    fn settings(&self) -> Result<Map<String, Value>, StoreError> {
        let table = self.transaction.open_table(SETTINGS).map_err(redb_error)?;
        let stored = table
            .get(SETTINGS_KEY)
            .map_err(redb_error)?
            .map(|text| parse_settings(text.value()))
            .transpose()?;
        Ok(stored.unwrap_or_default())
    }
}
