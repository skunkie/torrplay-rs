// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! BitTorrent v2 (BEP 52) and hybrid metainfo for fixtures that exercise the
//! prototype's v1-only boundary: v2-only torrents must be rejected, and
//! hybrid torrents must load through their v1 part. The builders follow
//! BEP 52 and BEP 47 but have not been checked against a v2 implementation.

use std::collections::BTreeMap;

use sha1::Sha1;
use sha2::{Digest, Sha256};

use crate::bencode::Value;
use crate::{FixtureSpec, Metainfo, Segment, generator, sha1_piece_hashes};

/// BEP 52 hashes files in 16 KiB blocks.
const BLOCK_BYTES: u64 = 16 * 1024;

/// The hash of a leaf beyond the end of a file.
const ZERO_LEAF: [u8; 32] = [0; 32];

/// A file's BEP 52 Merkle tree: its pieces root and, for files longer than
/// one piece, the piece layer the torrent's `piece layers` carries.
struct FileTree {
    pieces_root: [u8; 32],
    piece_layer: Vec<[u8; 32]>,
}

fn file_tree(seed: u64, size_bytes: u64, piece_length: u64) -> FileTree {
    let mut leaves = Vec::new();
    let mut block = vec![0; BLOCK_BYTES as usize];
    let mut offset = 0;
    while offset < size_bytes {
        let len = (size_bytes - offset).min(BLOCK_BYTES) as usize;
        generator::fill(seed, offset, &mut block[..len]);
        leaves.push(Sha256::digest(&block[..len]).into());
        offset += len as u64;
    }
    // Leaves are padded with zero hashes to a power of two, so every piece
    // of a file longer than one piece covers a whole subtree.
    let width = leaves.len().next_power_of_two();
    leaves.resize(width, ZERO_LEAF);

    let blocks_per_piece = (piece_length / BLOCK_BYTES) as usize;
    let piece_count = size_bytes.div_ceil(piece_length) as usize;
    let mut level = leaves;
    let mut level_width = 1;
    let mut piece_layer = Vec::new();
    loop {
        if size_bytes > piece_length && level_width == blocks_per_piece {
            piece_layer = level[..piece_count].to_vec();
        }
        if level.len() == 1 {
            break;
        }
        level = level
            .chunks(2)
            .map(|pair| hash_pair(&pair[0], &pair[1]))
            .collect();
        level_width *= 2;
    }
    FileTree {
        pieces_root: level[0],
        piece_layer,
    }
}

fn hash_pair(left: &[u8; 32], right: &[u8; 32]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(left);
    hasher.update(right);
    hasher.finalize().into()
}

impl FixtureSpec {
    /// Builds the v2 `file tree`, and the `piece layers` entries for files
    /// longer than one piece.
    fn v2_trees(&self) -> (Value, BTreeMap<Vec<u8>, Value>) {
        let piece_length = u64::from(self.piece_length);
        let mut tree = BTreeMap::new();
        let mut layers = BTreeMap::new();
        for (index, file) in self.files().iter().enumerate() {
            let FileTree {
                pieces_root,
                piece_layer,
            } = file_tree(self.file_seed(index), file.size_bytes, piece_length);
            if !piece_layer.is_empty() {
                layers.insert(pieces_root.to_vec(), Value::Bytes(piece_layer.concat()));
            }
            let leaf = Value::dictionary([(
                "",
                Value::dictionary([
                    ("length", Value::Integer(file.size_bytes)),
                    ("pieces root", Value::Bytes(pieces_root.to_vec())),
                ]),
            )]);
            insert_path(&mut tree, &file.path, leaf);
        }
        (Value::Dictionary(tree), layers)
    }

    fn v2_info_entries(&self, file_tree: Value) -> Vec<(&'static str, Value)> {
        vec![
            ("file tree", file_tree),
            ("meta version", Value::Integer(2)),
            ("name", Value::text(&self.torrent_name)),
            ("piece length", Value::Integer(u64::from(self.piece_length))),
        ]
    }

    /// Builds a v2-only torrent file: a `file tree` and `piece layers`, and
    /// no v1 `pieces`.
    pub fn v2_only_metainfo(&self) -> Vec<u8> {
        let (file_tree, layers) = self.v2_trees();
        let info = dictionary(self.v2_info_entries(file_tree));
        Value::dictionary([("info", info), ("piece layers", Value::Dictionary(layers))]).encode()
    }

    /// Builds a hybrid torrent file: v2 fields plus a v1 file list in which
    /// BEP 47 padding files align every file but the last to a piece
    /// boundary. The returned info hash is the v1 SHA-1 hash.
    pub fn hybrid_metainfo(&self) -> Metainfo {
        let piece_length = u64::from(self.piece_length);
        let files = self.files();
        let mut segments = Vec::new();
        let mut v1_files = Vec::new();
        for (index, file) in files.iter().enumerate() {
            segments.push(Segment::Generated {
                seed: self.file_seed(index),
                size_bytes: file.size_bytes,
            });
            v1_files.push(Value::dictionary([
                ("length", Value::Integer(file.size_bytes)),
                (
                    "path",
                    Value::List(file.path.iter().map(|part| Value::text(part)).collect()),
                ),
            ]));
            let remainder = file.size_bytes % piece_length;
            if index + 1 < files.len() && remainder != 0 {
                let padding = piece_length - remainder;
                segments.push(Segment::Zeros {
                    size_bytes: padding,
                });
                v1_files.push(Value::dictionary([
                    ("attr", Value::text("p")),
                    ("length", Value::Integer(padding)),
                    (
                        "path",
                        Value::List(vec![Value::text(".pad"), Value::text(&padding.to_string())]),
                    ),
                ]));
            }
        }
        let hashes = sha1_piece_hashes(&segments, self.piece_length);
        let (file_tree, layers) = self.v2_trees();
        let mut entries = self.v2_info_entries(file_tree);
        entries.push(("files", Value::List(v1_files)));
        entries.push(("pieces", Value::Bytes(hashes.concat())));
        let info = dictionary(entries);
        let info_hash = <Sha1 as Digest>::digest(info.encode()).into();
        let bytes =
            Value::dictionary([("info", info), ("piece layers", Value::Dictionary(layers))])
                .encode();
        Metainfo {
            bytes,
            info_hash,
            piece_count: hashes.len() as u64,
        }
    }
}

fn dictionary(entries: Vec<(&str, Value)>) -> Value {
    Value::Dictionary(
        entries
            .into_iter()
            .map(|(key, value)| (key.as_bytes().to_vec(), value))
            .collect(),
    )
}

/// Inserts `leaf` into a v2 file tree at `path`, creating directories.
fn insert_path(tree: &mut BTreeMap<Vec<u8>, Value>, path: &[String], leaf: Value) {
    let Some((first, rest)) = path.split_first() else {
        return;
    };
    let key = first.as_bytes().to_vec();
    if rest.is_empty() {
        tree.insert(key, leaf);
        return;
    }
    let entry = tree
        .entry(key)
        .or_insert_with(|| Value::Dictionary(BTreeMap::new()));
    if let Value::Dictionary(children) = entry {
        insert_path(children, rest, leaf);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog;

    #[test]
    fn a_single_block_file_has_its_block_hash_as_root() {
        let mut block = vec![0; 1000];
        generator::fill(9, 0, &mut block);
        let tree = file_tree(9, 1000, 16 * 1024);
        assert_eq!(tree.pieces_root, <[u8; 32]>::from(Sha256::digest(&block)));
        assert!(
            tree.piece_layer.is_empty(),
            "a file within one piece has no piece layer"
        );
    }

    #[test]
    fn the_piece_layer_reduces_to_the_pieces_root() {
        let piece_length = 32 * 1024;
        let tree = file_tree(3, 5 * piece_length + 100, piece_length);
        assert_eq!(tree.piece_layer.len(), 6);
        // Above the piece layer, missing pieces are subtrees of zero leaves.
        let mut zero_subtree = ZERO_LEAF;
        for _ in 0..(piece_length / BLOCK_BYTES).trailing_zeros() {
            zero_subtree = hash_pair(&zero_subtree, &zero_subtree);
        }
        let mut level = tree.piece_layer.clone();
        level.resize(level.len().next_power_of_two(), zero_subtree);
        while level.len() > 1 {
            level = level
                .chunks(2)
                .map(|pair| hash_pair(&pair[0], &pair[1]))
                .collect();
        }
        assert_eq!(level[0], tree.pieces_root);
    }

    #[test]
    fn v2_only_metainfo_has_no_v1_pieces() {
        let bytes = catalog::v2_only().v2_only_metainfo();
        assert!(contains(&bytes, b"12:meta versioni2e"));
        assert!(contains(&bytes, b"12:piece layers"));
        assert!(
            !contains(&bytes, b"6:pieces"),
            "v2-only metainfo must not carry v1 piece hashes"
        );
    }

    #[test]
    fn hybrid_padding_aligns_files_to_pieces() {
        let spec = catalog::hybrid();
        let metainfo = spec.hybrid_metainfo();
        let piece_length = u64::from(spec.piece_length);
        let padded: u64 = spec
            .files()
            .iter()
            .enumerate()
            .map(|(index, file)| {
                if index + 1 < spec.files().len() {
                    file.size_bytes.div_ceil(piece_length) * piece_length
                } else {
                    file.size_bytes
                }
            })
            .sum();
        assert_eq!(metainfo.piece_count, padded.div_ceil(piece_length));
        assert!(contains(&metainfo.bytes, b"4:attr1:p"));
        assert!(contains(&metainfo.bytes, b"12:meta versioni2e"));
    }

    #[test]
    fn equal_padding_fixture_repeats_a_padding_file_name() {
        let bytes = catalog::hybrid_equal_padding().hybrid_metainfo().bytes;
        let name = b"4:.pad4:6384";
        let count = bytes
            .windows(name.len())
            .filter(|window| window == name)
            .count();
        assert_eq!(
            count, 2,
            "two padding files of the same length must share a name"
        );
    }

    fn contains(haystack: &[u8], needle: &[u8]) -> bool {
        haystack
            .windows(needle.len())
            .any(|window| window == needle)
    }
}
