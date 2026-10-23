// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! TorrPlay's persistent store of torrents and settings.
//!
//! During the ADR 8 spike this crate holds two candidate backends behind one
//! interface, [`Store`]: [`sqlite::SqliteStore`] over SQLite through
//! rusqlite, and [`redb::RedbStore`] over redb. The same cases in `tests/`
//! run against both, and the decision keeps one of them.
//!
//! The interface is synchronous: both databases block on disk I/O, so the
//! application calls the store from a worker thread rather than from async
//! tasks. Reads go through a [`Snapshot`], a read-only transaction that sees
//! one consistent state while writes commit; every mutation is its own write
//! transaction.
//!
//! Settings are stored as the document of explicitly set values only, so a
//! value set to `false` or zero stays distinct from one never set, and
//! defaults added by a later version apply to every value left unset.
//! [`merge_patch`] applies an RFC 7396 merge patch to that document.

use std::path::Path;

use serde_json::{Map, Value};

#[cfg(feature = "redb")]
pub mod redb;
#[cfg(feature = "sqlite")]
pub mod sqlite;

/// The schema version the backends create and migrate to.
pub const SCHEMA_VERSION: u32 = 2;

/// A persisted torrent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Torrent {
    /// The public ID.
    pub id: String,
    /// The v1 info hash, unique among stored torrents.
    pub info_hash: [u8; 20],
    /// The creation time in milliseconds since the Unix epoch.
    pub created_ms: u64,
    /// Where the metadata comes from.
    pub source: Source,
    /// The writable configuration as a JSON document.
    pub configuration: String,
    /// The configuration's version, which its entity tag is derived from.
    pub version: u64,
    /// Where the torrent's data lives. Added in schema version 2.
    pub storage_mode: StorageMode,
}

/// Where a torrent's metadata comes from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    /// The original metainfo bytes, from an uploaded file or retrieved.
    Metainfo(Vec<u8>),
    /// A magnet URI whose metadata has not been retrieved yet.
    Magnet(String),
}

/// Where a torrent's data lives.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum StorageMode {
    /// In the piece cache only.
    #[default]
    Memory,
    /// In files on disk.
    Disk,
}

impl StorageMode {
    fn code(self) -> i64 {
        match self {
            Self::Memory => 0,
            Self::Disk => 1,
        }
    }

    fn from_code(code: i64) -> Result<Self, StoreError> {
        match code {
            0 => Ok(Self::Memory),
            1 => Ok(Self::Disk),
            _ => Err(StoreError::Corrupt(format!("storage mode {code}"))),
        }
    }
}

/// A torrent as a collection lists it, without its metainfo bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Summary {
    /// The public ID.
    pub id: String,
    /// The v1 info hash.
    pub info_hash: [u8; 20],
    /// The creation time in milliseconds since the Unix epoch.
    pub created_ms: u64,
    /// The configuration's version.
    pub version: u64,
}

/// The position after which a page continues: the last listed torrent's
/// creation time and ID, the collection's sort key.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Cursor {
    /// The creation time in milliseconds.
    pub created_ms: u64,
    /// The ID, the tie-breaker.
    pub id: String,
}

impl From<&Summary> for Cursor {
    fn from(summary: &Summary) -> Self {
        Self {
            created_ms: summary.created_ms,
            id: summary.id.clone(),
        }
    }
}

/// The result of [`Store::insert`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Inserted {
    /// The torrent was stored.
    Created,
    /// A torrent with the same info hash exists; nothing changed.
    Duplicate {
        /// The existing torrent's ID.
        existing: String,
    },
}

/// The result of [`Store::update_configuration`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Updated {
    /// The configuration was replaced; this is its new version.
    Replaced(u64),
    /// The stored version differs from the expected one; nothing changed.
    Conflict {
        /// The stored version.
        current: u64,
    },
    /// No torrent has this ID.
    NotFound,
}

/// A failure of the store.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// SQLite failed.
    #[cfg(feature = "sqlite")]
    #[error("SQLite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    /// redb failed.
    #[cfg(feature = "redb")]
    #[error("redb: {0}")]
    Redb(#[from] ::redb::Error),
    /// Stored data cannot be decoded.
    #[error("corrupt store: {0}")]
    Corrupt(String),
    /// The database was written by a newer schema than this build knows.
    #[error("schema version {found} is newer than {SCHEMA_VERSION}")]
    TooNew {
        /// The version found.
        found: u32,
    },
}

/// A settings change the validator refused; nothing was stored.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("invalid settings: {0}")]
pub struct Invalid(pub String);

/// Accepts or refuses a settings document before it is stored.
pub type Validate = dyn Fn(&Map<String, Value>) -> Result<(), Invalid>;

/// A persistent store of torrents and settings.
pub trait Store: Send + Sync {
    /// Stores a new torrent unless one with its info hash exists.
    fn insert(&self, torrent: &Torrent) -> Result<Inserted, StoreError>;

    /// Stores torrents in one transaction, calling `before_commit` right
    /// before committing it, which tests use to interrupt a write.
    fn insert_batch(
        &self,
        torrents: &[Torrent],
        before_commit: &mut dyn FnMut(),
    ) -> Result<(), StoreError>;

    /// Replaces a torrent's configuration if its version is `expected`, as
    /// a request with `If-Match` does.
    fn update_configuration(
        &self,
        id: &str,
        expected: u64,
        configuration: &str,
    ) -> Result<Updated, StoreError>;

    /// Deletes a torrent and its metainfo. Returns whether it existed.
    fn delete(&self, id: &str) -> Result<bool, StoreError>;

    /// Opens a read-only transaction.
    fn snapshot(&self) -> Result<Box<dyn Snapshot + '_>, StoreError>;

    /// Applies a merge patch to the stored settings document, if `validate`
    /// accepts the result, in one write transaction. Returns the new
    /// document, or the validator's refusal with nothing stored.
    fn patch_settings(
        &self,
        patch: &Value,
        validate: &Validate,
    ) -> Result<Result<Map<String, Value>, Invalid>, StoreError>;
}

/// A read-only transaction: every read sees the state of when it began.
pub trait Snapshot {
    /// The torrent with this ID, with its metainfo.
    fn get(&self, id: &str) -> Result<Option<Torrent>, StoreError>;

    /// Up to `limit` torrents after `after`, in ascending creation time
    /// and then ID.
    fn list(&self, after: Option<&Cursor>, limit: usize) -> Result<Vec<Summary>, StoreError>;

    /// The number of stored torrents.
    fn count(&self) -> Result<u64, StoreError>;

    /// The stored settings document, explicit values only.
    fn settings(&self) -> Result<Map<String, Value>, StoreError>;
}

/// Opens or creates a store at `path`, migrating it to [`SCHEMA_VERSION`].
pub type Opener = fn(&Path) -> Result<Box<dyn Store>, StoreError>;

/// Applies an RFC 7396 merge patch: members set to `null` are removed,
/// objects merge recursively, and anything else replaces the target.
pub fn merge_patch(target: &mut Value, patch: &Value) {
    let Value::Object(patch) = patch else {
        *target = patch.clone();
        return;
    };
    if !target.is_object() {
        *target = Value::Object(Map::new());
    }
    let Value::Object(target) = target else {
        return;
    };
    for (key, value) in patch {
        if value.is_null() {
            target.remove(key);
        } else {
            merge_patch(target.entry(key).or_insert(Value::Null), value);
        }
    }
}

/// The effective settings: `defaults` overridden by the explicit values.
pub fn effective_settings(
    defaults: &Map<String, Value>,
    explicit: &Map<String, Value>,
) -> Map<String, Value> {
    let mut settings = Value::Object(defaults.clone());
    merge_patch(&mut settings, &Value::Object(explicit.clone()));
    match settings {
        Value::Object(settings) => settings,
        _ => Map::new(),
    }
}

/// Parses a stored settings document.
fn parse_settings(text: &str) -> Result<Map<String, Value>, StoreError> {
    match serde_json::from_str(text) {
        Ok(Value::Object(settings)) => Ok(settings),
        Ok(_) => Err(StoreError::Corrupt("settings are not an object".into())),
        Err(error) => Err(StoreError::Corrupt(format!("settings: {error}"))),
    }
}

/// Applies `patch` to `stored` and validates the result.
fn patched_settings(
    stored: Map<String, Value>,
    patch: &Value,
    validate: &Validate,
) -> Result<Map<String, Value>, Invalid> {
    let mut document = Value::Object(stored);
    merge_patch(&mut document, patch);
    let Value::Object(document) = document else {
        return Err(Invalid("settings must be an object".into()));
    };
    validate(&document)?;
    Ok(document)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn a_merge_patch_removes_nulls_and_keeps_false_and_zero() {
        let mut target = json!({"a": 1, "b": {"c": true, "d": 2}});
        merge_patch(&mut target, &json!({"a": null, "b": {"c": false, "e": 0}}));
        assert_eq!(target, json!({"b": {"c": false, "d": 2, "e": 0}}));
    }

    #[test]
    fn explicit_settings_override_defaults_and_unset_ones_take_them() {
        let defaults = json!({"cache": 256, "dht": true, "new_limit": 50});
        let explicit = json!({"cache": 0, "dht": false});
        let (Value::Object(defaults), Value::Object(explicit)) = (defaults, explicit) else {
            unreachable!("literal objects");
        };
        assert_eq!(
            Value::Object(effective_settings(&defaults, &explicit)),
            json!({"cache": 0, "dht": false, "new_limit": 50})
        );
    }
}
