// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! Profiles, their progress, and their dismissals: reading them at startup
//! and writing each change.

use rusqlite::{Connection, params};
use torrplay_core::id::{FileId, ProfileId, TorrentId, UpNextEntryId};
use torrplay_core::profiles::{Profile, Progress};

use crate::images::{self, Images};
use crate::schema::{from_ns, to_ns};
use crate::{StoreError, StoredDismissal, StoredProgress};

fn corrupt(what: String) -> StoreError {
    StoreError::Corrupt(what)
}

/// Reads every profile with its avatar's description.
pub(crate) fn profiles(connection: &Connection) -> Result<Vec<Profile>, StoreError> {
    let mut avatars = images::all(connection, Images::Avatars)?;
    let mut statement = connection.prepare("SELECT id, name, created_at_ns FROM profiles")?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, i64>(2)?,
        ))
    })?;
    let mut profiles = Vec::new();
    for row in rows {
        let (id, name, created_at_ns) = row?;
        profiles.push(Profile {
            id: ProfileId::from_stored(&id)
                .ok_or_else(|| corrupt(format!("profile {id}: malformed ID")))?,
            name,
            created_at: from_ns(created_at_ns)
                .ok_or_else(|| corrupt(format!("profile {id}: the time is out of range")))?,
            avatar: avatars.remove(&id),
        });
    }
    Ok(profiles)
}

/// Reads every profile's progress.
pub(crate) fn progress(connection: &Connection) -> Result<Vec<StoredProgress>, StoreError> {
    let mut statement = connection.prepare(
        "SELECT profile_id, torrent_id, file_id, position_ms, duration_ms, viewed_at_ns,
                updated_at_ns
         FROM progress",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, i64>(3)?,
            row.get::<_, i64>(4)?,
            row.get::<_, Option<i64>>(5)?,
            row.get::<_, i64>(6)?,
        ))
    })?;
    let mut all = Vec::new();
    for row in rows {
        let (profile, torrent, file, position, duration, viewed_at, updated_at) = row?;
        let bad = |what: &str| corrupt(format!("progress of {profile}, {torrent}, {file}: {what}"));
        let time = |nanos| from_ns(nanos).ok_or_else(|| bad("a time is out of range"));
        all.push(StoredProgress {
            profile: ProfileId::from_stored(&profile).ok_or_else(|| bad("malformed profile ID"))?,
            torrent: TorrentId::from_stored(&torrent).ok_or_else(|| bad("malformed torrent ID"))?,
            file: FileId::from_stored(&file).ok_or_else(|| bad("malformed file ID"))?,
            progress: Progress {
                position_ms: u64::try_from(position).map_err(|_| bad("negative position"))?,
                duration_ms: u64::try_from(duration).map_err(|_| bad("negative duration"))?,
                viewed_at: viewed_at.map(time).transpose()?,
                updated_at: time(updated_at)?,
            },
        });
    }
    Ok(all)
}

/// Reads every profile's dismissals.
pub(crate) fn dismissals(connection: &Connection) -> Result<Vec<StoredDismissal>, StoreError> {
    let mut statement = connection.prepare(
        "SELECT profile_id, entry_id, torrent_id, dismissed_at_ns FROM up_next_dismissals",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, i64>(3)?,
        ))
    })?;
    let mut all = Vec::new();
    for row in rows {
        let (profile, entry, torrent, at) = row?;
        let bad = |what: &str| corrupt(format!("dismissal of {profile}, {entry}: {what}"));
        all.push(StoredDismissal {
            profile: ProfileId::from_stored(&profile).ok_or_else(|| bad("malformed profile ID"))?,
            entry: UpNextEntryId::from_stored(&entry).ok_or_else(|| bad("malformed entry ID"))?,
            torrent: TorrentId::from_stored(&torrent).ok_or_else(|| bad("malformed torrent ID"))?,
            at: from_ns(at).ok_or_else(|| bad("the time is out of range"))?,
        });
    }
    Ok(all)
}

/// Fails unless the profile and the torrent are stored.
fn require(
    connection: &Connection,
    profile: &ProfileId,
    torrent: &TorrentId,
) -> Result<(), StoreError> {
    let (profile_stored, torrent_stored): (bool, bool) = connection.query_row(
        "SELECT EXISTS (SELECT 1 FROM profiles WHERE id = ?1),
                EXISTS (SELECT 1 FROM torrents WHERE id = ?2)",
        [profile.as_str(), torrent.as_str()],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    match (profile_stored, torrent_stored) {
        (false, _) => Err(StoreError::UnknownProfile),
        (true, false) => Err(StoreError::UnknownTorrent),
        (true, true) => Ok(()),
    }
}

pub(crate) fn insert_profile(connection: &Connection, profile: &Profile) -> Result<(), StoreError> {
    connection.execute(
        "INSERT INTO profiles (id, name, created_at_ns) VALUES (?1, ?2, ?3)",
        params![
            profile.id.as_str(),
            profile.name,
            to_ns(profile.created_at)?
        ],
    )?;
    Ok(())
}

pub(crate) fn rename_profile(
    connection: &Connection,
    id: &ProfileId,
    name: &str,
) -> Result<(), StoreError> {
    let updated = connection.execute(
        "UPDATE profiles SET name = ?2 WHERE id = ?1",
        params![id.as_str(), name],
    )?;
    if updated == 0 {
        return Err(StoreError::UnknownProfile);
    }
    Ok(())
}

pub(crate) fn delete_profile(connection: &Connection, id: &ProfileId) -> Result<(), StoreError> {
    // Its avatar, progress, and dismissals go through the foreign keys.
    connection.execute("DELETE FROM profiles WHERE id = ?1", [id.as_str()])?;
    Ok(())
}

pub(crate) fn set_progress(
    connection: &Connection,
    stored: &StoredProgress,
) -> Result<(), StoreError> {
    require(connection, &stored.profile, &stored.torrent)?;
    let progress = &stored.progress;
    let count = |value: u64| {
        i64::try_from(value).map_err(|_| corrupt("a progress value is out of range".into()))
    };
    connection.execute(
        "INSERT INTO progress
             (profile_id, torrent_id, file_id, position_ms, duration_ms, viewed_at_ns,
              updated_at_ns)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT (profile_id, torrent_id, file_id) DO UPDATE SET
             position_ms = excluded.position_ms,
             duration_ms = excluded.duration_ms,
             viewed_at_ns = excluded.viewed_at_ns,
             updated_at_ns = excluded.updated_at_ns",
        params![
            stored.profile.as_str(),
            stored.torrent.as_str(),
            stored.file.as_str(),
            count(progress.position_ms)?,
            count(progress.duration_ms)?,
            progress.viewed_at.map(to_ns).transpose()?,
            to_ns(progress.updated_at)?,
        ],
    )?;
    Ok(())
}

pub(crate) fn delete_progress(
    connection: &Connection,
    profile: &ProfileId,
    torrent: &TorrentId,
    file: &FileId,
) -> Result<(), StoreError> {
    require(connection, profile, torrent)?;
    connection.execute(
        "DELETE FROM progress WHERE profile_id = ?1 AND torrent_id = ?2 AND file_id = ?3",
        [profile.as_str(), torrent.as_str(), file.as_str()],
    )?;
    Ok(())
}

pub(crate) fn set_dismissal(
    connection: &Connection,
    stored: &StoredDismissal,
) -> Result<(), StoreError> {
    require(connection, &stored.profile, &stored.torrent)?;
    connection.execute(
        "INSERT INTO up_next_dismissals (profile_id, entry_id, torrent_id, dismissed_at_ns)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT (profile_id, entry_id) DO UPDATE SET
             torrent_id = excluded.torrent_id,
             dismissed_at_ns = excluded.dismissed_at_ns",
        params![
            stored.profile.as_str(),
            stored.entry.as_str(),
            stored.torrent.as_str(),
            to_ns(stored.at)?,
        ],
    )?;
    Ok(())
}
