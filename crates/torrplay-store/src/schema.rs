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

use rusqlite::{Connection, ErrorCode, OptionalExtension, TransactionBehavior};
use std::collections::HashMap;

use torrplay_core::id::{InfoHashV1, TorrentId};
use torrplay_core::library::{self, Kind, Library};
use torrplay_core::lifecycle::{Configuration, DesiredState, StorageMode};
use torrplay_core::settings::{self, Explicit, Stored};

use crate::images::{self, Images};
use crate::{StoreError, StoredTorrent};

/// The schema version this build creates and migrates to.
pub(crate) const VERSION: u32 = 18;

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
    // Version 2: the settings, one nullable column per setting, so NULL is
    // unset and takes the running release's default. A later setting is a
    // new nullable column, unset in every existing database.
    "CREATE TABLE settings (
        id INTEGER PRIMARY KEY CHECK (id = 1),
        version INTEGER NOT NULL CHECK (version >= 1),
        piece_wait_seconds INTEGER CHECK (piece_wait_seconds >= 0),
        idle_timeout_seconds INTEGER CHECK (idle_timeout_seconds >= 0),
        idle_readers_per_torrent INTEGER CHECK (idle_readers_per_torrent >= 0),
        idle_readers_total INTEGER CHECK (idle_readers_total >= 0),
        drain_period_seconds INTEGER CHECK (drain_period_seconds >= 0),
        peer_limit INTEGER CHECK (peer_limit >= 0)
    ) STRICT;",
    // Version 3: the cache capacity.
    "ALTER TABLE settings
        ADD COLUMN cache_capacity_bytes INTEGER CHECK (cache_capacity_bytes >= 0);",
    // Version 4: the storage mode the configuration asks for, beside
    // storage_mode, the one in effect, which a transition changes.
    "ALTER TABLE torrents
        ADD COLUMN desired_storage_mode TEXT NOT NULL DEFAULT 'memory'
            CHECK (desired_storage_mode IN ('memory', 'disk'));",
    // Version 5: whether the last run shut down cleanly, one row, which a
    // run clears when it opens the store and sets when it shuts down.
    "CREATE TABLE runs (
        id INTEGER PRIMARY KEY CHECK (id = 1),
        clean_shutdown INTEGER NOT NULL CHECK (clean_shutdown IN (0, 1))
    ) STRICT;",
    // Version 6: the background downloads switch, NULL while unset.
    "ALTER TABLE settings
        ADD COLUMN background_downloads INTEGER CHECK (background_downloads IN (0, 1));",
    // Version 7: the library members, NULL or no rows while unset, and
    // posters. A list keeps its order by position.
    "ALTER TABLE torrents ADD COLUMN title TEXT;
    ALTER TABLE torrents
        ADD COLUMN kind TEXT CHECK (kind IN ('movie', 'series', 'other'));
    ALTER TABLE torrents
        ADD COLUMN year INTEGER CHECK (year BETWEEN 1800 AND 2200);
    ALTER TABLE torrents ADD COLUMN description TEXT;
    CREATE TABLE torrent_categories (
        torrent_id TEXT NOT NULL REFERENCES torrents (id) ON DELETE CASCADE,
        position INTEGER NOT NULL CHECK (position >= 0),
        name TEXT NOT NULL,
        PRIMARY KEY (torrent_id, position),
        CONSTRAINT torrent_categories_torrent_id_name_key UNIQUE (torrent_id, name)
    ) STRICT;
    CREATE TABLE torrent_genres (
        torrent_id TEXT NOT NULL REFERENCES torrents (id) ON DELETE CASCADE,
        position INTEGER NOT NULL CHECK (position >= 0),
        name TEXT NOT NULL,
        PRIMARY KEY (torrent_id, position),
        CONSTRAINT torrent_genres_torrent_id_name_key UNIQUE (torrent_id, name)
    ) STRICT;
    -- Posters can be a megabyte; startup reads every column but the image.
    CREATE TABLE torrent_posters (
        torrent_id TEXT PRIMARY KEY REFERENCES torrents (id) ON DELETE CASCADE,
        media_type TEXT NOT NULL CHECK (media_type IN
            ('image/avif', 'image/gif', 'image/jpeg', 'image/png', 'image/webp')),
        digest BLOB NOT NULL CHECK (length(digest) = 32),
        updated_at_ns INTEGER NOT NULL CHECK (updated_at_ns >= 0),
        bytes BLOB NOT NULL
    ) STRICT;",
    // Version 8: profiles, their avatars, their progress of torrents'
    // files, and their dismissals of up next entries. Progress and
    // dismissals go with their profile and their torrent.
    "CREATE TABLE profiles (
        id TEXT PRIMARY KEY,
        name TEXT NOT NULL,
        created_at_ns INTEGER NOT NULL CHECK (created_at_ns >= 0),
        CONSTRAINT profiles_name_key UNIQUE (name)
    ) STRICT;
    CREATE TABLE profile_avatars (
        profile_id TEXT PRIMARY KEY REFERENCES profiles (id) ON DELETE CASCADE,
        media_type TEXT NOT NULL CHECK (media_type IN
            ('image/avif', 'image/gif', 'image/jpeg', 'image/png', 'image/webp')),
        digest BLOB NOT NULL CHECK (length(digest) = 32),
        updated_at_ns INTEGER NOT NULL CHECK (updated_at_ns >= 0),
        bytes BLOB NOT NULL
    ) STRICT;
    CREATE TABLE progress (
        profile_id TEXT NOT NULL REFERENCES profiles (id) ON DELETE CASCADE,
        torrent_id TEXT NOT NULL REFERENCES torrents (id) ON DELETE CASCADE,
        file_id TEXT NOT NULL,
        position_ms INTEGER NOT NULL CHECK (position_ms >= 0),
        duration_ms INTEGER NOT NULL CHECK (duration_ms > 0 AND position_ms <= duration_ms),
        viewed_at_ns INTEGER CHECK (viewed_at_ns >= 0),
        updated_at_ns INTEGER NOT NULL CHECK (updated_at_ns >= 0),
        PRIMARY KEY (profile_id, torrent_id, file_id)
    ) STRICT;
    CREATE INDEX progress_torrent_id_idx ON progress (torrent_id);
    CREATE TABLE up_next_dismissals (
        profile_id TEXT NOT NULL REFERENCES profiles (id) ON DELETE CASCADE,
        entry_id TEXT NOT NULL,
        torrent_id TEXT NOT NULL REFERENCES torrents (id) ON DELETE CASCADE,
        dismissed_at_ns INTEGER NOT NULL CHECK (dismissed_at_ns >= 0),
        PRIMARY KEY (profile_id, entry_id)
    ) STRICT;
    CREATE INDEX up_next_dismissals_torrent_id_idx ON up_next_dismissals (torrent_id);",
    // Version 9: the password's hash, NULL while authentication is off, the
    // key that signs playback tokens, one row, and access tokens by their
    // secrets' digests.
    "CREATE TABLE credentials (
        id INTEGER PRIMARY KEY CHECK (id = 1),
        password_hash TEXT,
        password_updated_at_ns INTEGER CHECK (password_updated_at_ns >= 0),
        signing_key BLOB NOT NULL CHECK (length(signing_key) = 32),
        CHECK ((password_hash IS NULL) = (password_updated_at_ns IS NULL))
    ) STRICT;
    CREATE TABLE access_tokens (
        id TEXT PRIMARY KEY,
        secret_digest BLOB NOT NULL CHECK (length(secret_digest) = 32),
        name TEXT NOT NULL,
        scope TEXT NOT NULL CHECK (scope IN ('api', 'playback')),
        created_at_ns INTEGER NOT NULL CHECK (created_at_ns >= 0),
        expires_at_ns INTEGER CHECK (expires_at_ns >= 0),
        last_used_at_ns INTEGER CHECK (last_used_at_ns >= 0),
        CONSTRAINT access_tokens_secret_digest_key UNIQUE (secret_digest)
    ) STRICT;",
    // Version 10: the DLNA media server switch, NULL while unset, and the
    // media server's device UUID, one row, drawn when the store first opens.
    "ALTER TABLE settings
        ADD COLUMN dlna_enabled INTEGER CHECK (dlna_enabled IN (0, 1));
    CREATE TABLE media_server (
        id INTEGER PRIMARY KEY CHECK (id = 1),
        udn TEXT NOT NULL
    ) STRICT;",
    // Version 11: the media server's friendly name, the DHT switch, and the
    // log level, each NULL while unset.
    "ALTER TABLE settings ADD COLUMN dlna_name TEXT;
    ALTER TABLE settings
        ADD COLUMN dht_enabled INTEGER CHECK (dht_enabled IN (0, 1));
    ALTER TABLE settings
        ADD COLUMN log_level TEXT
        CHECK (log_level IN ('error', 'warn', 'info', 'debug', 'trace'));",
    // Version 12: the temporary torrents' idle limit and count limit, each
    // NULL while unset.
    "ALTER TABLE settings
        ADD COLUMN temporary_idle_seconds INTEGER CHECK (temporary_idle_seconds >= 0);
    ALTER TABLE settings
        ADD COLUMN temporary_torrents_max INTEGER CHECK (temporary_torrents_max >= 0);",
    // Version 13: each profile's kind, existing ones being people's, and at
    // most one device profile.
    "ALTER TABLE profiles
        ADD COLUMN kind TEXT NOT NULL DEFAULT 'person' CHECK (kind IN ('person', 'device'));
    CREATE UNIQUE INDEX profiles_kind_device_key ON profiles (kind) WHERE kind = 'device';",
    // Version 14: each profile's progress of titles in external catalogs,
    // which outlives torrents, so the file last played has no foreign key.
    "CREATE TABLE title_progress (
        profile_id TEXT NOT NULL REFERENCES profiles (id) ON DELETE CASCADE,
        title_key TEXT NOT NULL,
        position_ms INTEGER NOT NULL CHECK (position_ms >= 0),
        duration_ms INTEGER NOT NULL CHECK (duration_ms > 0 AND position_ms <= duration_ms),
        viewed_at_ns INTEGER CHECK (viewed_at_ns >= 0),
        updated_at_ns INTEGER NOT NULL CHECK (updated_at_ns >= 0),
        torrent_id TEXT,
        file_id TEXT,
        CHECK ((torrent_id IS NULL) = (file_id IS NULL)),
        PRIMARY KEY (profile_id, title_key)
    ) STRICT;",
    // Version 15: temporary torrents' progress, by info hash because the
    // torrents are not stored, and how long it is kept.
    "ALTER TABLE settings
        ADD COLUMN temporary_progress_seconds INTEGER CHECK (temporary_progress_seconds >= 0);
    CREATE TABLE temporary_progress (
        profile_id TEXT NOT NULL REFERENCES profiles (id) ON DELETE CASCADE,
        info_hash TEXT NOT NULL,
        file_id TEXT NOT NULL,
        position_ms INTEGER NOT NULL CHECK (position_ms >= 0),
        duration_ms INTEGER NOT NULL CHECK (duration_ms > 0 AND position_ms <= duration_ms),
        viewed_at_ns INTEGER CHECK (viewed_at_ns >= 0),
        updated_at_ns INTEGER NOT NULL CHECK (updated_at_ns >= 0),
        PRIMARY KEY (profile_id, info_hash, file_id)
    ) STRICT;",
    // Version 16: the metrics scope. SQLite cannot change a CHECK
    // constraint, so the table is rebuilt; nothing references it.
    "CREATE TABLE access_tokens_v16 (
        id TEXT PRIMARY KEY,
        secret_digest BLOB NOT NULL CHECK (length(secret_digest) = 32),
        name TEXT NOT NULL,
        scope TEXT NOT NULL CHECK (scope IN ('api', 'playback', 'metrics')),
        created_at_ns INTEGER NOT NULL CHECK (created_at_ns >= 0),
        expires_at_ns INTEGER CHECK (expires_at_ns >= 0),
        last_used_at_ns INTEGER CHECK (last_used_at_ns >= 0),
        CONSTRAINT access_tokens_v16_secret_digest_key UNIQUE (secret_digest)
    ) STRICT;
    INSERT INTO access_tokens_v16 SELECT id, secret_digest, name, scope, created_at_ns,
        expires_at_ns, last_used_at_ns FROM access_tokens;
    DROP TABLE access_tokens;
    ALTER TABLE access_tokens_v16 RENAME TO access_tokens;",
    // Version 17: each torrent's IMDb title ID.
    "ALTER TABLE torrents ADD COLUMN imdb_id TEXT CHECK (imdb_id GLOB 'tt[0-9]*');",
    // Version 18: names aligned with the contract and the glossary. The
    // settings' switch is named as the contract's other switches are, and
    // their limits and the idle reader timeout name what they bound; the
    // metainfo table is named for the glossary's term; and an up next entry's
    // ID is named as the contract names it. Temporary progress stores info
    // hashes as torrents do, and the access tokens' constraint loses the
    // name version 16 gave it; SQLite cannot change a column's type or a
    // constraint's name, so those tables are rebuilt.
    "ALTER TABLE settings RENAME COLUMN background_downloads TO background_downloads_enabled;
    ALTER TABLE settings RENAME COLUMN temporary_torrents_max TO temporary_torrent_limit;
    ALTER TABLE settings RENAME COLUMN idle_readers_total TO idle_reader_limit;
    ALTER TABLE settings
        RENAME COLUMN idle_readers_per_torrent TO idle_reader_limit_per_torrent;
    ALTER TABLE settings RENAME COLUMN idle_timeout_seconds TO idle_reader_timeout_seconds;
    ALTER TABLE torrent_files RENAME TO torrent_metainfo;
    ALTER TABLE up_next_dismissals RENAME COLUMN entry_id TO up_next_entry_id;
    ALTER TABLE temporary_progress RENAME TO temporary_progress_v17;
    CREATE TABLE temporary_progress (
        profile_id TEXT NOT NULL REFERENCES profiles (id) ON DELETE CASCADE,
        info_hash BLOB NOT NULL CHECK (length(info_hash) = 20),
        file_id TEXT NOT NULL,
        position_ms INTEGER NOT NULL CHECK (position_ms >= 0),
        duration_ms INTEGER NOT NULL CHECK (duration_ms > 0 AND position_ms <= duration_ms),
        viewed_at_ns INTEGER CHECK (viewed_at_ns >= 0),
        updated_at_ns INTEGER NOT NULL CHECK (updated_at_ns >= 0),
        PRIMARY KEY (profile_id, info_hash, file_id)
    ) STRICT;
    -- unhex() is NULL for text that is not hexadecimal, which NOT NULL
    -- refuses, so a damaged row fails the migration rather than vanishing.
    INSERT INTO temporary_progress SELECT profile_id, unhex(info_hash), file_id, position_ms,
        duration_ms, viewed_at_ns, updated_at_ns FROM temporary_progress_v17;
    DROP TABLE temporary_progress_v17;
    ALTER TABLE access_tokens RENAME TO access_tokens_v17;
    CREATE TABLE access_tokens (
        id TEXT PRIMARY KEY,
        secret_digest BLOB NOT NULL CHECK (length(secret_digest) = 32),
        name TEXT NOT NULL,
        scope TEXT NOT NULL CHECK (scope IN ('api', 'playback', 'metrics')),
        created_at_ns INTEGER NOT NULL CHECK (created_at_ns >= 0),
        expires_at_ns INTEGER CHECK (expires_at_ns >= 0),
        last_used_at_ns INTEGER CHECK (last_used_at_ns >= 0),
        CONSTRAINT access_tokens_secret_digest_key UNIQUE (secret_digest)
    ) STRICT;
    INSERT INTO access_tokens SELECT id, secret_digest, name, scope, created_at_ns,
        expires_at_ns, last_used_at_ns FROM access_tokens_v17;
    DROP TABLE access_tokens_v17;",
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
    migrate(&mut connection, path)?;
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

/// The copy of a database taken before migrating it from `version`.
pub(crate) fn backup_path(path: &Path, version: u32) -> std::path::PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_owned();
    name.push(format!(".v{version}.bak"));
    path.with_file_name(name)
}

fn migrate(connection: &mut Connection, path: &Path) -> Result<(), StoreError> {
    let found: u32 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if found > VERSION {
        return Err(StoreError::TooNew {
            found,
            supported: VERSION,
        });
    }
    // A database a release wrote is copied before it changes, so the user can
    // go back to that release. A copy already there is the older one, from
    // a migration that did not finish, and is kept.
    if (1..VERSION).contains(&found) {
        let backup = backup_path(path, found);
        if !backup.exists() {
            let target = backup
                .to_str()
                .ok_or_else(|| StoreError::Corrupt("the backup path is not UTF-8".into()))?;
            connection.execute("VACUUM INTO ?1", [target])?;
        }
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
    let mut categories = labels(&transaction, "torrent_categories")?;
    let mut genres = labels(&transaction, "torrent_genres")?;
    let mut posters = images::all(&transaction, Images::Posters)?;
    let mut statement = transaction.prepare(
        "SELECT t.id, t.info_hash, t.created_at_ns, t.desired_state, t.configuration_version,
                t.magnet_uri, f.bytes, t.desired_storage_mode, t.storage_mode,
                t.title, t.kind, t.year, t.description, t.imdb_id
         FROM torrents t LEFT JOIN torrent_metainfo f ON f.torrent_id = t.id
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
            row.get::<_, String>(7)?,
            row.get::<_, String>(8)?,
            (
                row.get::<_, Option<String>>(9)?,
                row.get::<_, Option<String>>(10)?,
                row.get::<_, Option<i64>>(11)?,
                row.get::<_, Option<String>>(12)?,
                row.get::<_, Option<String>>(13)?,
            ),
        ))
    })?;
    let mut torrents = Vec::new();
    for row in rows {
        let (
            id,
            info_hash,
            created_at_ns,
            desired_state,
            version,
            magnet_uri,
            metainfo,
            desired_storage_mode,
            storage_mode,
            (title, kind, year, description, imdb_id),
        ) = row?;
        let corrupt = |what: &str| StoreError::Corrupt(format!("torrent {id}: {what}"));
        let info_hash: [u8; 20] = info_hash
            .try_into()
            .map_err(|_| corrupt("the info hash is not 20 bytes"))?;
        let created_at =
            from_ns(created_at_ns).ok_or_else(|| corrupt("the creation time is out of range"))?;
        let desired_state =
            DesiredState::parse(&desired_state).ok_or_else(|| corrupt("unknown desired state"))?;
        let parse_mode =
            |mode: &str| StorageMode::parse(mode).ok_or_else(|| corrupt("unknown storage mode"));
        let desired_storage_mode = parse_mode(&desired_storage_mode)?;
        let storage_mode = parse_mode(&storage_mode)?;
        let library = Library {
            title,
            kind: kind
                .map(|kind| Kind::parse(&kind).ok_or_else(|| corrupt("unknown kind")))
                .transpose()?,
            year: year
                .map(|year| u16::try_from(year).map_err(|_| corrupt("the year is out of range")))
                .transpose()?,
            imdb_id: imdb_id
                .map(|imdb_id| {
                    library::is_imdb_id(&imdb_id)
                        .then_some(imdb_id)
                        .ok_or_else(|| corrupt("the IMDb ID is malformed"))
                })
                .transpose()?,
            description,
            categories: categories.remove(&id).unwrap_or_default(),
            genres: genres.remove(&id).unwrap_or_default(),
        };
        let configuration = u64::try_from(version)
            .ok()
            .and_then(|version| {
                Configuration::from_stored(desired_state, desired_storage_mode, library, version)
            })
            .ok_or_else(|| corrupt("the configuration version is out of range"))?;
        if magnet_uri.is_none() && metainfo.is_none() {
            return Err(corrupt("neither a magnet URI nor a torrent file"));
        }
        torrents.push(StoredTorrent {
            id: TorrentId::from_stored(&id).ok_or_else(|| corrupt("malformed ID"))?,
            info_hash: InfoHashV1::from_bytes(info_hash),
            created_at,
            configuration,
            storage_mode,
            poster: posters.remove(&id),
            magnet_uri,
            metainfo,
        });
    }
    drop(statement);
    transaction.commit()?;
    Ok(torrents)
}

/// Reads every torrent's categories or genres from `table`, in order.
fn labels(
    connection: &Connection,
    table: &str,
) -> Result<HashMap<String, Vec<String>>, StoreError> {
    let mut statement = connection.prepare(&format!(
        "SELECT torrent_id, name FROM {table} ORDER BY torrent_id, position"
    ))?;
    let mut labels: HashMap<String, Vec<String>> = HashMap::new();
    for row in statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))? {
        let (id, name): (String, String) = row?;
        labels.entry(id).or_default().push(name);
    }
    Ok(labels)
}

/// A time from SQLite's signed integer of nanoseconds since the Unix epoch.
pub(crate) fn from_ns(nanos: i64) -> Option<SystemTime> {
    u64::try_from(nanos)
        .ok()
        .and_then(|nanos| SystemTime::UNIX_EPOCH.checked_add(Duration::from_nanos(nanos)))
}

/// A time as SQLite's signed integer of nanoseconds since the Unix epoch,
/// which reaches the year 2262.
pub(crate) fn to_ns(time: SystemTime) -> Result<i64, StoreError> {
    time.duration_since(SystemTime::UNIX_EPOCH)
        .ok()
        .and_then(|since| i64::try_from(since.as_nanos()).ok())
        .ok_or_else(|| StoreError::Corrupt("a time is out of range".into()))
}

/// A configuration version as SQLite's signed integer.
pub(crate) fn version(configuration: &Configuration) -> Result<i64, StoreError> {
    i64::try_from(configuration.version())
        .map_err(|_| StoreError::Corrupt("the configuration version is out of range".into()))
}

/// Returns the media server's device UUID, drawing and storing one the first
/// time.
pub(crate) fn media_server_udn(connection: &mut Connection) -> Result<String, StoreError> {
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let stored = transaction
        .query_row("SELECT udn FROM media_server WHERE id = 1", [], |row| {
            row.get::<_, String>(0)
        })
        .optional()?;
    let udn = match stored {
        Some(udn) => udn,
        None => {
            let udn = torrplay_core::id::random_uuid()
                .map_err(|error| StoreError::Corrupt(error.to_string()))?;
            transaction.execute("INSERT INTO media_server (id, udn) VALUES (1, ?1)", [&udn])?;
            udn
        }
    };
    transaction.commit()?;
    Ok(udn)
}

/// Returns whether the last run shut down cleanly, and records this run as
/// not yet shut down. A database no run recorded counts as clean.
pub(crate) fn begin_run(connection: &mut Connection) -> Result<bool, StoreError> {
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let clean = transaction
        .query_row("SELECT clean_shutdown FROM runs WHERE id = 1", [], |row| {
            row.get::<_, bool>(0)
        })
        .optional()?
        .unwrap_or(true);
    transaction.execute(
        "INSERT INTO runs (id, clean_shutdown) VALUES (1, 0)
         ON CONFLICT (id) DO UPDATE SET clean_shutdown = 0",
        [],
    )?;
    transaction.commit()?;
    Ok(clean)
}

/// Reads the stored settings, or the defaults if none were stored.
pub(crate) fn settings(connection: &Connection) -> Result<Stored, StoreError> {
    let row = connection
        .query_row(
            "SELECT version, piece_wait_seconds, idle_reader_timeout_seconds, idle_reader_limit_per_torrent,
                    idle_reader_limit, drain_period_seconds, peer_limit, cache_capacity_bytes,
                    temporary_idle_seconds, temporary_torrent_limit, temporary_progress_seconds,
                    background_downloads_enabled, dlna_enabled, dlna_name, dht_enabled, log_level
             FROM settings WHERE id = 1",
            [],
            |row| {
                let mut values = [None; settings::ALL.len()];
                for (index, value) in values.iter_mut().enumerate() {
                    *value = row.get::<_, Option<i64>>(index + 1)?;
                }
                let others = settings::ALL.len() + 1;
                let switches = (
                    row.get::<_, Option<bool>>(others)?,
                    row.get::<_, Option<bool>>(others + 1)?,
                    row.get::<_, Option<String>>(others + 2)?,
                    row.get::<_, Option<bool>>(others + 3)?,
                    row.get::<_, Option<String>>(others + 4)?,
                );
                Ok((row.get::<_, i64>(0)?, values, switches))
            },
        )
        .optional()?;
    let Some((
        version,
        values,
        (background_downloads_enabled, dlna_enabled, dlna_name, dht_enabled, log_level),
    )) = row
    else {
        return Ok(Stored::default());
    };
    let corrupt = |what: &str| StoreError::Corrupt(format!("settings: {what}"));
    let version = u64::try_from(version)
        .ok()
        .filter(|&version| version >= 1)
        .ok_or_else(|| corrupt("the version is out of range"))?;
    let log_level = log_level
        .map(|name| settings::LogLevel::parse(&name).ok_or_else(|| corrupt("the log level")))
        .transpose()?;
    let mut explicit = Explicit {
        background_downloads_enabled,
        dlna_enabled,
        dlna_name,
        dht_enabled,
        log_level,
        ..Explicit::default()
    };
    for (bounds, value) in settings::ALL.iter().zip(values) {
        let value = value
            .map(|value| u64::try_from(value).map_err(|_| corrupt(bounds.name)))
            .transpose()?;
        if let Some(slot) = explicit.slot(bounds.name) {
            *slot = value;
        }
    }
    Ok(Stored { explicit, version })
}

/// Replaces the stored settings.
pub(crate) fn set_settings(connection: &Connection, stored: &Stored) -> Result<(), StoreError> {
    let version = i64::try_from(stored.version)
        .map_err(|_| StoreError::Corrupt("the settings version is out of range".into()))?;
    let explicit = &stored.explicit;
    let mut values = [None; settings::ALL.len()];
    for (value, explicit) in values.iter_mut().zip([
        explicit.piece_wait_seconds,
        explicit.idle_reader_timeout_seconds,
        explicit.idle_reader_limit_per_torrent,
        explicit.idle_reader_limit,
        explicit.drain_period_seconds,
        explicit.peer_limit,
        explicit.cache_capacity_bytes,
        explicit.temporary_idle_seconds,
        explicit.temporary_torrent_limit,
        explicit.temporary_progress_seconds,
    ]) {
        *value = explicit
            .map(i64::try_from)
            .transpose()
            .map_err(|_| StoreError::Corrupt("a setting is out of range".into()))?;
    }
    let [
        piece_wait,
        idle_reader_timeout,
        per_torrent,
        total,
        drain,
        peers,
        capacity,
        temporary_idle,
        temporary_limit,
        temporary_progress,
    ] = values;
    connection.execute(
        "INSERT INTO settings (id, version, piece_wait_seconds, idle_reader_timeout_seconds,
                               idle_reader_limit_per_torrent, idle_reader_limit,
                               drain_period_seconds, peer_limit, cache_capacity_bytes,
                               temporary_idle_seconds, temporary_torrent_limit,
                               temporary_progress_seconds,
                               background_downloads_enabled, dlna_enabled, dlna_name,
                               dht_enabled, log_level)
         VALUES (1, ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)
         ON CONFLICT (id) DO UPDATE SET
             version = excluded.version,
             piece_wait_seconds = excluded.piece_wait_seconds,
             idle_reader_timeout_seconds = excluded.idle_reader_timeout_seconds,
             idle_reader_limit_per_torrent = excluded.idle_reader_limit_per_torrent,
             idle_reader_limit = excluded.idle_reader_limit,
             drain_period_seconds = excluded.drain_period_seconds,
             peer_limit = excluded.peer_limit,
             cache_capacity_bytes = excluded.cache_capacity_bytes,
             temporary_idle_seconds = excluded.temporary_idle_seconds,
             temporary_torrent_limit = excluded.temporary_torrent_limit,
             temporary_progress_seconds = excluded.temporary_progress_seconds,
             background_downloads_enabled = excluded.background_downloads_enabled,
             dlna_enabled = excluded.dlna_enabled,
             dlna_name = excluded.dlna_name,
             dht_enabled = excluded.dht_enabled,
             log_level = excluded.log_level",
        rusqlite::params![
            version,
            piece_wait,
            idle_reader_timeout,
            per_torrent,
            total,
            drain,
            peers,
            capacity,
            temporary_idle,
            temporary_limit,
            temporary_progress,
            explicit.background_downloads_enabled,
            explicit.dlna_enabled,
            explicit.dlna_name,
            explicit.dht_enabled,
            explicit.log_level.map(settings::LogLevel::as_str)
        ],
    )?;
    Ok(())
}
