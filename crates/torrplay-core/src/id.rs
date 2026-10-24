// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! Torrent IDs, file IDs, and v1 info hashes, as
//! `docs/adr/0004-resource-identity.md` defines them.
//!
//! Torrent and file IDs are opaque to clients. The server never validates
//! the shape of a requested ID: lookups take any `&str`, and one that names
//! nothing is simply absent, so the format can change without changing the
//! contract.

use std::borrow::Borrow;
use std::fmt;

use sha2::{Digest, Sha256};

use crate::base32;

/// The number of random bytes in a torrent ID.
pub const TORRENT_ID_RANDOM_BYTES: usize = 16;

/// The domain separation string that starts every file ID digest. A change
/// to the derivation changes this string.
const FILE_ID_DOMAIN: &[u8] = b"torrplay-file-id-v1";

/// The number of digest bytes a file ID keeps.
const FILE_ID_DIGEST_BYTES: usize = 16;

/// The opaque, server-assigned identifier of a torrent resource: `trr_`
/// followed by 128 random bits in lowercase base32.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TorrentId(String);

impl TorrentId {
    /// The prefix of every torrent ID the server issues.
    pub const PREFIX: &str = "trr_";

    /// Builds the ID that encodes `random`. Production IDs take these bytes
    /// from [`random_torrent_id_bytes`].
    pub fn from_random_bytes(random: [u8; TORRENT_ID_RANDOM_BYTES]) -> Self {
        Self(format!("{}{}", Self::PREFIX, base32::encode(&random)))
    }

    /// Wraps an ID that was previously issued, such as one carried in a
    /// cursor, without checking its shape.
    pub(crate) fn from_issued(id: String) -> Self {
        Self(id)
    }

    /// Reads an ID the server issued and stored: `None` unless it has the
    /// shape [`from_random_bytes`](Self::from_random_bytes) produces, `trr_`
    /// and 26 lowercase base32 characters.
    pub fn from_stored(text: &str) -> Option<Self> {
        let encoded = text.strip_prefix(Self::PREFIX)?;
        let shaped = encoded.len() == 26
            && encoded
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || (b'2'..=b'7').contains(&byte));
        shaped.then(|| Self(text.to_owned()))
    }

    /// Returns the ID's text.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for TorrentId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl Borrow<str> for TorrentId {
    fn borrow(&self) -> &str {
        &self.0
    }
}

/// The operating system's random number generator failed.
#[derive(Debug, thiserror::Error)]
#[error("the operating system's random number generator failed")]
pub struct RandomnessError(#[source] getrandom::Error);

/// Draws the random bytes of a new torrent ID from the operating system's
/// cryptographically secure random number generator.
pub fn random_torrent_id_bytes() -> Result<[u8; TORRENT_ID_RANDOM_BYTES], RandomnessError> {
    let mut bytes = [0; TORRENT_ID_RANDOM_BYTES];
    getrandom::fill(&mut bytes).map_err(RandomnessError)?;
    Ok(bytes)
}

/// The opaque, server-assigned identifier of a preload: `prl_` followed by
/// 128 random bits in lowercase base32, drawn like a torrent ID's. Preloads
/// are not stored, so an ID names its preload only while the process runs.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PreloadId(String);

impl PreloadId {
    /// The prefix of every preload ID the server issues.
    pub const PREFIX: &str = "prl_";

    /// Builds the ID that encodes `random`. Production IDs take these bytes
    /// from [`random_torrent_id_bytes`].
    pub fn from_random_bytes(random: [u8; TORRENT_ID_RANDOM_BYTES]) -> Self {
        Self(format!("{}{}", Self::PREFIX, base32::encode(&random)))
    }

    /// Returns the ID's text.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for PreloadId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl Borrow<str> for PreloadId {
    fn borrow(&self) -> &str {
        &self.0
    }
}

/// The identifier of a file within its torrent, derived from the file's raw
/// path components: `fil_` followed by the first 128 bits of a SHA-256
/// digest in lowercase base32.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FileId(String);

impl FileId {
    /// The prefix of every file ID.
    pub const PREFIX: &str = "fil_";

    /// Derives the ID of the file at `path`: the info dictionary's `name`,
    /// followed by the file's `path` list for a multi-file torrent, as raw
    /// bytes from the metadata. The digest covers the domain separation
    /// string and then each component preceded by its length as an unsigned
    /// 64-bit big-endian integer, so component boundaries are unambiguous.
    pub fn derive<C: AsRef<[u8]>>(path: &[C]) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(FILE_ID_DOMAIN);
        for component in path {
            let component = component.as_ref();
            hasher.update((component.len() as u64).to_be_bytes());
            hasher.update(component);
        }
        let digest = hasher.finalize();
        Self(format!(
            "{}{}",
            Self::PREFIX,
            base32::encode(&digest[..FILE_ID_DIGEST_BYTES])
        ))
    }

    /// Returns the ID's text.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for FileId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl Borrow<str> for FileId {
    fn borrow(&self) -> &str {
        &self.0
    }
}

/// A BitTorrent v1 info hash: the SHA-1 hash of the info dictionary.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct InfoHashV1([u8; 20]);

impl InfoHashV1 {
    /// The algorithm name the API pairs with the value.
    pub const ALGORITHM: &str = "sha1";

    /// Wraps the 20 bytes of a SHA-1 info hash.
    pub const fn from_bytes(bytes: [u8; 20]) -> Self {
        Self(bytes)
    }

    /// Parses 40 hexadecimal digits in either case.
    pub fn from_hex(text: &str) -> Option<Self> {
        let text = text.as_bytes();
        if text.len() != 40 {
            return None;
        }
        let mut bytes = [0; 20];
        let (pairs, _) = text.as_chunks::<2>();
        for (byte, &[high, low]) in bytes.iter_mut().zip(pairs) {
            *byte = (hex_value(high)? << 4) | hex_value(low)?;
        }
        Some(Self(bytes))
    }

    /// Returns the hash's bytes.
    pub const fn as_bytes(&self) -> &[u8; 20] {
        &self.0
    }
}

/// Formats the hash as lowercase hexadecimal, the API's `value`.
impl fmt::Display for InfoHashV1 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0 {
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}

fn hex_value(digit: u8) -> Option<u8> {
    match digit {
        b'0'..=b'9' => Some(digit - b'0'),
        b'a'..=b'f' => Some(digit - b'a' + 10),
        b'A'..=b'F' => Some(digit - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    #[test]
    fn torrent_ids_encode_their_random_bytes() {
        let cases: [([u8; 16], &str); 3] = [
            ([0; 16], "trr_aaaaaaaaaaaaaaaaaaaaaaaaaa"),
            (
                core::array::from_fn(|i| i as u8),
                "trr_aaaqeayeaudaocajbifqydiob4",
            ),
            ([0xff; 16], "trr_77777777777777777777777774"),
        ];
        for (bytes, expected) in cases {
            assert_eq!(TorrentId::from_random_bytes(bytes).as_str(), expected);
        }
    }

    #[test]
    fn preload_ids_encode_their_random_bytes_with_their_own_prefix() {
        assert_eq!(
            PreloadId::from_random_bytes([0; 16]).as_str(),
            "prl_aaaaaaaaaaaaaaaaaaaaaaaaaa"
        );
        assert_eq!(
            PreloadId::from_random_bytes([0xff; 16]).to_string(),
            "prl_77777777777777777777777774"
        );
    }

    #[test]
    fn generated_torrent_ids_match_the_documented_pattern() {
        let id = TorrentId::from_random_bytes(random_torrent_id_bytes().unwrap());
        let suffix = id.as_str().strip_prefix("trr_").unwrap();
        assert_eq!(suffix.len(), 26);
        assert!(
            suffix
                .bytes()
                .all(|byte| matches!(byte, b'a'..=b'z' | b'2'..=b'7')),
            "{id} must match ^trr_[a-z2-7]{{26}}$"
        );
    }

    #[test]
    fn stored_ids_are_read_only_in_the_issued_shape() {
        let issued = TorrentId::from_random_bytes(random_torrent_id_bytes().unwrap());
        assert_eq!(TorrentId::from_stored(issued.as_str()), Some(issued));
        for malformed in [
            "trr_",
            "trr_aaaaaaaaaaaaaaaaaaaaaaaaa",
            "trr_aaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "trr_aaaaaaaaaaaaaaaaaaaaaaaaa1",
            "trr_aaaaaaaaaaaaaaaaaaaaaaaaaA",
            "fil_aaaaaaaaaaaaaaaaaaaaaaaaaa",
        ] {
            assert_eq!(TorrentId::from_stored(malformed), None, "{malformed}");
        }
    }

    #[test]
    fn file_ids_match_fixed_vectors() {
        // Computed independently with Python's hashlib and base64; the first
        // is also the contract's example file ID.
        let cases: [(&[&[u8]], &str); 4] = [
            (
                &[b"Example Collection", b"part-1.mkv"],
                "fil_xfuperelxpqq4qhvgmtq62qwv4",
            ),
            (&[b"single-small.bin"], "fil_3hegvix7jyqr637hql2s2saf4q"),
            (&[b"ab", b"c"], "fil_qdi6kw23vj7ybufp6t3oeotkve"),
            (&[b"\xff\xfe", b"x"], "fil_qgd5mgwcu3ilxdylogvfrwmjcu"),
        ];
        for (path, expected) in cases {
            assert_eq!(FileId::derive(path).as_str(), expected, "{path:?}");
        }
    }

    #[test]
    fn length_prefixes_separate_components() {
        assert_ne!(
            FileId::derive(&[&b"ab"[..], b"c"]),
            FileId::derive(&[&b"a"[..], b"bc"]),
        );
        assert_ne!(
            FileId::derive(&[&b"abc"[..]]),
            FileId::derive(&[&b"ab"[..], b"c"])
        );
    }

    #[test]
    fn ids_are_found_by_their_text() {
        let id = TorrentId::from_random_bytes([7; 16]);
        let map = HashMap::from([(id.clone(), ())]);
        assert!(map.contains_key(id.as_str()));
        assert!(!map.contains_key("trr_01"));
    }

    #[test]
    fn info_hashes_round_trip_through_hex() {
        let text = "0123456789abcdef0123456789abcdef01234567";
        let hash = InfoHashV1::from_hex(text).unwrap();
        assert_eq!(hash.to_string(), text);
        assert_eq!(InfoHashV1::from_hex(&text.to_uppercase()), Some(hash));
        for invalid in ["", "0123", &format!("{text}0"), &text.replace('a', "g")] {
            assert_eq!(InfoHashV1::from_hex(invalid), None, "{invalid:?}");
        }
    }
}
