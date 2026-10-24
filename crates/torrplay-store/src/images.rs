// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! Uploaded images, torrents' posters and profiles' avatars, which share one
//! table layout: the owner's ID, the description, and the bytes.

use std::collections::HashMap;

use rusqlite::{Connection, OptionalExtension, params};
use torrplay_core::image::{ImageInfo, ImageType};

use crate::StoreError;
use crate::schema::{from_ns, to_ns};

/// A table of images.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Images {
    /// Torrents' posters.
    Posters,
    /// Profiles' avatars.
    Avatars,
}

impl Images {
    /// The table, its owner column, and the owners' table.
    fn names(self) -> (&'static str, &'static str, &'static str) {
        match self {
            Self::Posters => ("torrent_posters", "torrent_id", "torrents"),
            Self::Avatars => ("profile_avatars", "profile_id", "profiles"),
        }
    }
}

/// An image's description columns, then its length.
type Row = (String, Vec<u8>, i64, i64);

fn row(row: &rusqlite::Row<'_>, from: usize) -> rusqlite::Result<Row> {
    Ok((
        row.get(from)?,
        row.get(from + 1)?,
        row.get(from + 2)?,
        row.get(from + 3)?,
    ))
}

fn info(owner: &str, row: Row) -> Result<ImageInfo, StoreError> {
    let (media_type, digest, updated_at_ns, size) = row;
    let corrupt = |what: &str| StoreError::Corrupt(format!("image of {owner}: {what}"));
    Ok(ImageInfo {
        image_type: ImageType::parse(&media_type).ok_or_else(|| corrupt("unknown media type"))?,
        size_bytes: u64::try_from(size).map_err(|_| corrupt("negative size"))?,
        digest: digest
            .try_into()
            .map_err(|_| corrupt("the digest is not 32 bytes"))?,
        updated_at: from_ns(updated_at_ns).ok_or_else(|| corrupt("the time is out of range"))?,
    })
}

/// Reads every image's description by its owner, without the images.
pub(crate) fn all(
    connection: &Connection,
    images: Images,
) -> Result<HashMap<String, ImageInfo>, StoreError> {
    let (table, owner, _) = images.names();
    let mut statement = connection.prepare(&format!(
        "SELECT {owner}, media_type, digest, updated_at_ns, length(bytes) FROM {table}"
    ))?;
    let rows = statement.query_map([], |found| Ok((found.get::<_, String>(0)?, row(found, 1)?)))?;
    let mut all = HashMap::new();
    for found in rows {
        let (id, found) = found?;
        let image = info(&id, found)?;
        all.insert(id, image);
    }
    Ok(all)
}

/// Reads the image of `owner` with its bytes.
pub(crate) fn one(
    connection: &Connection,
    images: Images,
    owner_id: &str,
) -> Result<Option<(ImageInfo, Vec<u8>)>, StoreError> {
    let (table, owner, _) = images.names();
    let found = connection
        .query_row(
            &format!(
                "SELECT media_type, digest, updated_at_ns, length(bytes), bytes
                 FROM {table} WHERE {owner} = ?1"
            ),
            [owner_id],
            |found| Ok((row(found, 0)?, found.get::<_, Vec<u8>>(4)?)),
        )
        .optional()?;
    found
        .map(|(found, bytes)| Ok((info(owner_id, found)?, bytes)))
        .transpose()
}

/// Fails unless the owner `owner_id` is stored.
fn require(connection: &Connection, images: Images, owner_id: &str) -> Result<(), StoreError> {
    let (_, _, owners) = images.names();
    let stored: bool = connection.query_row(
        &format!("SELECT EXISTS (SELECT 1 FROM {owners} WHERE id = ?1)"),
        [owner_id],
        |found| found.get(0),
    )?;
    match (stored, images) {
        (true, _) => Ok(()),
        (false, Images::Posters) => Err(StoreError::UnknownTorrent),
        (false, Images::Avatars) => Err(StoreError::UnknownProfile),
    }
}

/// Stores the image of a stored owner, replacing any it had.
pub(crate) fn set(
    connection: &Connection,
    images: Images,
    owner_id: &str,
    image: &ImageInfo,
    bytes: &[u8],
) -> Result<(), StoreError> {
    require(connection, images, owner_id)?;
    let (table, owner, _) = images.names();
    connection.execute(
        &format!(
            "INSERT INTO {table} ({owner}, media_type, digest, updated_at_ns, bytes)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT ({owner}) DO UPDATE SET
                 media_type = excluded.media_type,
                 digest = excluded.digest,
                 updated_at_ns = excluded.updated_at_ns,
                 bytes = excluded.bytes"
        ),
        params![
            owner_id,
            image.image_type.media_type(),
            image.digest.as_slice(),
            to_ns(image.updated_at)?,
            bytes
        ],
    )?;
    Ok(())
}

/// Deletes the image of a stored owner, if it has one.
pub(crate) fn delete(
    connection: &Connection,
    images: Images,
    owner_id: &str,
) -> Result<(), StoreError> {
    require(connection, images, owner_id)?;
    let (table, owner, _) = images.names();
    connection.execute(
        &format!("DELETE FROM {table} WHERE {owner} = ?1"),
        [owner_id],
    )?;
    Ok(())
}
