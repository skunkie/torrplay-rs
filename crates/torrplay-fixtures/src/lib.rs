// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! Deterministic synthetic torrents for TorrPlay tests and benchmarks.
//!
//! A [`FixtureSpec`] describes a torrent's files and piece length. Its
//! content comes from the generator in [`generator`], and its BitTorrent v1
//! metainfo is built here directly from that content, with files in spec
//! order, so the same spec yields the same bytes and info hash everywhere.
//! [`catalog`] holds the fixtures `docs/acceptance.md` lists, including the
//! v2-only and hybrid torrents that check the prototype's v1-only boundary.

pub mod catalog;
pub mod generator;

mod bencode;
mod v2;

use std::fs::{self, File};
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};

use sha1::{Digest, Sha1};

use crate::bencode::Value;

/// The size of the buffers content is generated and hashed in.
const CHUNK_BYTES: usize = 64 * 1024;

/// One file of a fixture, in metadata order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileSpec {
    /// The path components below the torrent's name.
    pub path: Vec<String>,
    /// The file's size.
    pub size_bytes: u64,
}

/// How a fixture's files are laid out in its metainfo.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Layout {
    /// A single-file torrent whose file is named after the torrent.
    SingleFile {
        /// The file's size.
        size_bytes: u64,
    },
    /// A multi-file torrent whose files sit in a directory named after the
    /// torrent.
    MultiFile {
        /// The files, in metadata order.
        files: Vec<FileSpec>,
    },
}

/// A synthetic torrent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FixtureSpec {
    /// The fixture's name in `docs/acceptance.md`.
    pub id: String,
    /// The `name` in the info dictionary: the file name of a single-file
    /// torrent, or the directory name of a multi-file torrent.
    pub torrent_name: String,
    /// The fixture's seed. File `i` uses the seed plus `i`.
    pub seed: u64,
    /// The piece length.
    pub piece_length: u32,
    /// The file layout.
    pub layout: Layout,
}

/// A fixture's metainfo.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Metainfo {
    /// The bencoded torrent file.
    pub bytes: Vec<u8>,
    /// The SHA-1 info hash.
    pub info_hash: [u8; 20],
    /// The number of pieces.
    pub piece_count: u64,
}

impl FixtureSpec {
    /// Returns the files in metadata order.
    pub fn files(&self) -> Vec<FileSpec> {
        match &self.layout {
            Layout::SingleFile { size_bytes } => vec![FileSpec {
                path: vec![self.torrent_name.clone()],
                size_bytes: *size_bytes,
            }],
            Layout::MultiFile { files } => files.clone(),
        }
    }

    /// Returns the seed of the file at `index` in metadata order.
    pub fn file_seed(&self, index: usize) -> u64 {
        self.seed.wrapping_add(index as u64)
    }

    /// Returns the total size of all files.
    pub fn total_size_bytes(&self) -> u64 {
        self.files().iter().map(|file| file.size_bytes).sum()
    }

    /// Returns the SHA-1 hash of each piece, computed from the generated
    /// content of the files concatenated in metadata order.
    pub fn piece_hashes(&self) -> Vec<[u8; 20]> {
        let segments = self
            .files()
            .iter()
            .enumerate()
            .map(|(index, file)| Segment::Generated {
                seed: self.file_seed(index),
                size_bytes: file.size_bytes,
            })
            .collect::<Vec<_>>();
        sha1_piece_hashes(&segments, self.piece_length)
    }

    /// Builds the bencoded v1 torrent file and its info hash.
    pub fn metainfo(&self) -> Metainfo {
        self.build_metainfo(None)
    }

    /// Like [`metainfo`](Self::metainfo), announcing to the tracker at
    /// `announce`. The announce URL lies outside the info dictionary, so the
    /// info hash is the same.
    pub fn metainfo_announcing(&self, announce: &str) -> Metainfo {
        self.build_metainfo(Some(announce))
    }

    fn build_metainfo(&self, announce: Option<&str>) -> Metainfo {
        let hashes = self.piece_hashes();
        let mut info = vec![
            ("name", Value::text(&self.torrent_name)),
            ("piece length", Value::Integer(u64::from(self.piece_length))),
            ("pieces", Value::Bytes(hashes.concat())),
        ];
        match &self.layout {
            Layout::SingleFile { size_bytes } => {
                info.push(("length", Value::Integer(*size_bytes)));
            }
            Layout::MultiFile { files } => {
                let files = files
                    .iter()
                    .map(|file| {
                        Value::dictionary([
                            ("length", Value::Integer(file.size_bytes)),
                            (
                                "path",
                                Value::List(
                                    file.path.iter().map(|part| Value::text(part)).collect(),
                                ),
                            ),
                        ])
                    })
                    .collect();
                info.push(("files", Value::List(files)));
            }
        }
        let info = Value::Dictionary(
            info.into_iter()
                .map(|(key, value)| (key.as_bytes().to_vec(), value))
                .collect(),
        );
        let info_hash = Sha1::digest(info.encode()).into();
        let bytes = match announce {
            Some(announce) => {
                Value::dictionary([("announce", Value::text(announce)), ("info", info)])
            }
            None => Value::dictionary([("info", info)]),
        }
        .encode();
        Metainfo {
            bytes,
            info_hash,
            piece_count: hashes.len() as u64,
        }
    }

    /// Writes the fixture's files under `dir` and returns the path of the
    /// torrent's content: the file of a single-file torrent, or the
    /// directory of a multi-file torrent.
    pub fn write_content(&self, dir: &Path) -> io::Result<PathBuf> {
        let root = dir.join(&self.torrent_name);
        let mut chunk = vec![0; CHUNK_BYTES];
        for (index, file) in self.files().iter().enumerate() {
            let path = match self.layout {
                Layout::SingleFile { .. } => root.clone(),
                Layout::MultiFile { .. } => file
                    .path
                    .iter()
                    .fold(root.clone(), |path, part| path.join(part)),
            };
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
            let mut out = BufWriter::new(File::create(&path)?);
            let seed = self.file_seed(index);
            let mut offset = 0;
            while offset < file.size_bytes {
                let len = (file.size_bytes - offset).min(CHUNK_BYTES as u64) as usize;
                generator::fill(seed, offset, &mut chunk[..len]);
                out.write_all(&chunk[..len])?;
                offset += len as u64;
            }
            out.flush()?;
        }
        Ok(root)
    }

    /// Returns a magnet URI for the fixture with its display name and no
    /// trackers; tests add peers explicitly.
    pub fn magnet_uri(&self, info_hash: &[u8; 20]) -> String {
        format!(
            "magnet:?xt=urn:btih:{}&dn={}",
            hex(info_hash),
            percent_encode(&self.torrent_name)
        )
    }

    /// Like [`magnet_uri`](Self::magnet_uri), naming the tracker at
    /// `announce`.
    pub fn magnet_uri_announcing(&self, info_hash: &[u8; 20], announce: &str) -> String {
        format!(
            "{}&tr={}",
            self.magnet_uri(info_hash),
            percent_encode(announce)
        )
    }
}

/// A run of torrent content in v1 piece order: a generated file, or the
/// zeros of a BEP 47 padding file.
#[derive(Clone, Copy, Debug)]
enum Segment {
    Generated { seed: u64, size_bytes: u64 },
    Zeros { size_bytes: u64 },
}

impl Segment {
    fn size_bytes(self) -> u64 {
        match self {
            Self::Generated { size_bytes, .. } | Self::Zeros { size_bytes } => size_bytes,
        }
    }

    fn fill(self, offset: u64, buf: &mut [u8]) {
        match self {
            Self::Generated { seed, .. } => generator::fill(seed, offset, buf),
            Self::Zeros { .. } => buf.fill(0),
        }
    }
}

/// Hashes the concatenated segments into SHA-1 pieces of `piece_length`.
fn sha1_piece_hashes(segments: &[Segment], piece_length: u32) -> Vec<[u8; 20]> {
    let piece_length = u64::from(piece_length);
    let mut hashes = Vec::new();
    let mut hasher = Sha1::new();
    let mut in_piece = 0;
    let mut chunk = vec![0; CHUNK_BYTES];
    for segment in segments {
        let size_bytes = segment.size_bytes();
        let mut offset = 0;
        while offset < size_bytes {
            let len = (size_bytes - offset)
                .min(piece_length - in_piece)
                .min(CHUNK_BYTES as u64) as usize;
            segment.fill(offset, &mut chunk[..len]);
            hasher.update(&chunk[..len]);
            offset += len as u64;
            in_piece += len as u64;
            if in_piece == piece_length {
                hashes.push(hasher.finalize_reset().into());
                in_piece = 0;
            }
        }
    }
    if in_piece > 0 {
        hashes.push(hasher.finalize().into());
    }
    hashes
}

/// Formats bytes as lowercase hexadecimal, as info hashes are written.
pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn percent_encode(text: &str) -> String {
    text.bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                char::from(byte).to_string()
            }
            _ => format!("%{byte:02X}"),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_announce_url_leaves_the_info_hash_alone() {
        let spec = catalog::single_small();
        let plain = spec.metainfo();
        let announcing = spec.metainfo_announcing("http://127.0.0.1:6969/announce");
        assert_eq!(announcing.info_hash, plain.info_hash);
        assert!(
            announcing
                .bytes
                .starts_with(b"d8:announce30:http://127.0.0.1:6969/announce4:info")
        );
        assert_eq!(
            spec.magnet_uri_announcing(&plain.info_hash, "http://t/a"),
            format!(
                "magnet:?xt=urn:btih:{}&dn=single-small.bin&tr=http%3A%2F%2Ft%2Fa",
                hex(&plain.info_hash)
            )
        );
    }

    fn read_concatenated(spec: &FixtureSpec, root: &Path) -> Vec<u8> {
        spec.files()
            .iter()
            .flat_map(|file| {
                let path = match spec.layout {
                    Layout::SingleFile { .. } => root.to_path_buf(),
                    Layout::MultiFile { .. } => file
                        .path
                        .iter()
                        .fold(root.to_path_buf(), |p, part| p.join(part)),
                };
                fs::read(path).unwrap()
            })
            .collect()
    }

    #[test]
    fn written_content_matches_the_piece_hashes() {
        let dir = tempfile::tempdir().unwrap();
        for spec in [
            catalog::single_small(),
            catalog::multi_boundary(),
            catalog::media_names(),
        ] {
            let root = spec.write_content(dir.path()).unwrap();
            let content = read_concatenated(&spec, &root);
            assert_eq!(content.len() as u64, spec.total_size_bytes(), "{}", spec.id);
            let from_disk: Vec<[u8; 20]> = content
                .chunks(spec.piece_length as usize)
                .map(|piece| Sha1::digest(piece).into())
                .collect();
            assert_eq!(from_disk, spec.piece_hashes(), "{}", spec.id);
        }
    }

    #[test]
    fn files_use_consecutive_seeds() {
        let dir = tempfile::tempdir().unwrap();
        let spec = catalog::multi_boundary();
        let root = spec.write_content(dir.path()).unwrap();
        for (index, file) in spec.files().iter().enumerate() {
            let bytes =
                fs::read(file.path.iter().fold(root.clone(), |p, part| p.join(part))).unwrap();
            assert_eq!(
                generator::first_mismatch(spec.seed + index as u64, 0, &bytes),
                None,
                "file {index}"
            );
        }
    }

    #[test]
    fn metainfo_is_deterministic_and_counts_pieces() {
        let spec = catalog::multi_boundary();
        let first = spec.metainfo();
        assert_eq!(
            first,
            spec.metainfo(),
            "building twice must give identical bytes"
        );
        let piece_length = u64::from(spec.piece_length);
        assert_eq!(
            first.piece_count,
            spec.total_size_bytes().div_ceil(piece_length)
        );
        assert_eq!(
            Sha1::digest(&first.bytes[7..first.bytes.len() - 1]).as_slice(),
            first.info_hash
        );
    }

    #[test]
    fn single_file_metainfo_has_a_length_and_no_file_list() {
        let bytes = catalog::single_small().metainfo().bytes;
        let text = String::from_utf8_lossy(&bytes);
        assert!(text.starts_with("d4:infod6:lengthi1048576e4:name16:single-small.bin"));
        assert!(!text.contains("5:files"));
    }

    #[test]
    fn magnet_uri_carries_the_info_hash_and_encoded_name() {
        let spec = catalog::media_names();
        let info_hash = spec.metainfo().info_hash;
        let uri = spec.magnet_uri(&info_hash);
        assert!(uri.starts_with(&format!("magnet:?xt=urn:btih:{}&dn=", hex(&info_hash))));
        assert!(uri.ends_with("dn=media-names"));
        assert_eq!(percent_encode("a b/c"), "a%20b%2Fc");
    }
}
