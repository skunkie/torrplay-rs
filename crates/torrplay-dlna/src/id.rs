// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! Object IDs, as the contract's Object IDs section structures them. Every
//! ID parses into exactly one [`ObjectId`] and formats back to the same
//! text, so category and genre names holding `/` or `:` round-trip.

use std::fmt;

/// A container or item in the content tree.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum ObjectId {
    /// `0`, the root container.
    Root,
    /// `all`, every torrent.
    All,
    /// `recent`, the newest torrents.
    Recent,
    /// `movies`.
    Movies,
    /// `series`.
    Series,
    /// `categories`, one container per category.
    Categories,
    /// `category:<name>`.
    Category(String),
    /// `genres`, one container per genre.
    Genres,
    /// `genre:<name>`.
    Genre(String),
    /// `t:<torrent_id>`, a torrent's playable files.
    Torrent(String),
    /// `f:<torrent_id>:<file_id>`, one file.
    File(String, String),
}

/// The parent ID of the root container.
pub const ROOT_PARENT: &str = "-1";

impl ObjectId {
    /// Parses an ID: `None` unless it is the canonical form of one.
    pub fn parse(text: &str) -> Option<Self> {
        Some(match text {
            "0" => Self::Root,
            "all" => Self::All,
            "recent" => Self::Recent,
            "movies" => Self::Movies,
            "series" => Self::Series,
            "categories" => Self::Categories,
            "genres" => Self::Genres,
            _ => {
                let (kind, rest) = text.split_once(':')?;
                match kind {
                    "category" => Self::Category(decode(rest)?),
                    "genre" => Self::Genre(decode(rest)?),
                    "t" if is_resource_id(rest) => Self::Torrent(rest.to_owned()),
                    "f" => {
                        let (torrent, file) = rest.split_once(':')?;
                        if !is_resource_id(torrent) || !is_resource_id(file) {
                            return None;
                        }
                        Self::File(torrent.to_owned(), file.to_owned())
                    }
                    _ => return None,
                }
            }
        })
    }
}

impl fmt::Display for ObjectId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Root => f.write_str("0"),
            Self::All => f.write_str("all"),
            Self::Recent => f.write_str("recent"),
            Self::Movies => f.write_str("movies"),
            Self::Series => f.write_str("series"),
            Self::Categories => f.write_str("categories"),
            Self::Category(name) => write!(f, "category:{}", encode(name)),
            Self::Genres => f.write_str("genres"),
            Self::Genre(name) => write!(f, "genre:{}", encode(name)),
            Self::Torrent(id) => write!(f, "t:{id}"),
            Self::File(torrent, file) => write!(f, "f:{torrent}:{file}"),
        }
    }
}

/// Whether `text` could be a torrent or file ID: non-empty, of the
/// characters the native API's IDs use. A shape that names nothing is
/// still unknown; this only keeps separators out.
fn is_resource_id(text: &str) -> bool {
    !text.is_empty()
        && text
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
}

fn is_unreserved(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~')
}

/// Percent-encodes UTF-8 text but for unreserved characters, with uppercase
/// hexadecimal.
pub fn encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        if is_unreserved(byte) {
            out.push(char::from(byte));
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// Decodes text [`encode`] produced: `None` for any other encoding of it,
/// for invalid UTF-8, and for empty text.
fn decode(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        if byte == b'%' {
            let hex = text.get(index + 1..index + 3)?;
            if hex.bytes().any(|digit| digit.is_ascii_lowercase()) {
                return None;
            }
            let value = u8::from_str_radix(hex, 16).ok()?;
            if is_unreserved(value) {
                return None;
            }
            out.push(value);
            index += 3;
        } else if is_unreserved(byte) {
            out.push(byte);
            index += 1;
        } else {
            return None;
        }
    }
    let decoded = String::from_utf8(out).ok()?;
    (!decoded.is_empty()).then_some(decoded)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_round_trip() {
        let ids = [
            ObjectId::Root,
            ObjectId::All,
            ObjectId::Recent,
            ObjectId::Movies,
            ObjectId::Series,
            ObjectId::Categories,
            ObjectId::Genres,
            ObjectId::Category("Films/Drama: 1970s".into()),
            ObjectId::Category("Кино".into()),
            ObjectId::Genre("Sci-Fi".into()),
            ObjectId::Torrent("trr_7ndq4m2kzb6hxw3rjpt5yc4ula".into()),
            ObjectId::File(
                "trr_7ndq4m2kzb6hxw3rjpt5yc4ula".into(),
                "fil_xfuperelxpqq4qhvgmtq62qwv4".into(),
            ),
        ];
        for id in ids {
            let text = id.to_string();
            assert_eq!(ObjectId::parse(&text), Some(id), "{text}");
        }
        assert_eq!(
            ObjectId::Category("Films/Drama: 1970s".into()).to_string(),
            "category:Films%2FDrama%3A%201970s"
        );
    }

    #[test]
    fn only_canonical_ids_parse() {
        for text in [
            "",
            "1",
            "ALL",
            "category:",
            "category:Films/Drama",
            "category:Films%2fDrama",
            "category:%41",
            "category:%FF",
            "category:%2",
            "genre",
            "t:",
            "t:trr_x:extra",
            "f:trr_x",
            "f:trr_x:",
            "f::fil_y",
            "x:anything",
        ] {
            assert_eq!(ObjectId::parse(text), None, "{text:?}");
        }
    }
}
