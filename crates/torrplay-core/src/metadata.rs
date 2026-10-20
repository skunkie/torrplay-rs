// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! A torrent's metadata as the API sees it: its name, size, and files with
//! their file IDs.
//!
//! The engine reports metadata as [`ReportedMetadata`], with every file of
//! its own list, including BEP 47 padding files. [`Metadata::identify`]
//! drops the padding files, derives file IDs, and rejects metadata with two
//! files of the same path or file ID, as ADR 4 requires. An uploaded torrent
//! file with such metadata is `validation-failed`; retrieved metadata with it
//! fails the torrent with the error's text as the failure detail.

use std::collections::{HashMap, HashSet};

use crate::id::FileId;

/// Metadata as the engine reports it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReportedMetadata {
    /// The info dictionary's `name`, as raw bytes.
    pub name: Vec<u8>,
    /// Every file in the engine's list, in metadata order, including
    /// padding files.
    pub files: Vec<ReportedFile>,
}

/// One file of [`ReportedMetadata`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReportedFile {
    /// The file's path as raw bytes: the torrent's name, then the file's
    /// `path` list for a multi-file torrent, or the name alone for a
    /// single-file torrent.
    pub path: Vec<Vec<u8>>,
    /// The file's size.
    pub size_bytes: u64,
    /// Whether the file is a BEP 47 padding file, which the API omits.
    pub padding: bool,
}

/// A file's position in the engine's file list, padding files included. It
/// addresses files in [`Engine`](crate::engine::Engine) calls and never
/// appears in the API.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FileIndex(pub usize);

/// Metadata that cannot be served.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum MetadataError {
    /// A file's path has no components.
    #[error("The metadata lists a file without a path.")]
    EmptyPath,
    /// Two files that are not padding files share a path.
    #[error("The metadata lists two files with the path {path}.")]
    DuplicatePath {
        /// The shared path, decoded for display.
        path: String,
    },
    /// Two files with different paths share a file ID.
    #[error("The metadata lists two files with the file ID {id}.")]
    DuplicateFileId {
        /// The shared file ID.
        id: FileId,
    },
    /// The files' sizes add up to more than 2^63 - 1 bytes, the largest
    /// size the API can report.
    #[error("The metadata's total size is too large.")]
    TooLarge,
}

/// Validated metadata with file IDs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Metadata {
    name: Vec<u8>,
    files: Vec<MetadataFile>,
    size_bytes: u64,
    positions: HashMap<FileId, usize>,
}

/// A file the API lists.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MetadataFile {
    id: FileId,
    index: FileIndex,
    path: Vec<Vec<u8>>,
    size_bytes: u64,
}

impl Metadata {
    /// Validates reported metadata and derives its file IDs.
    pub fn identify(reported: &ReportedMetadata) -> Result<Self, MetadataError> {
        Self::identify_with(reported, FileId::derive)
    }

    /// Identifies files with `derive`, so tests can force file ID
    /// collisions that SHA-256 makes impossible to find.
    fn identify_with(
        reported: &ReportedMetadata,
        derive: impl Fn(&[Vec<u8>]) -> FileId,
    ) -> Result<Self, MetadataError> {
        let mut files = Vec::new();
        let mut paths = HashSet::new();
        let mut positions = HashMap::new();
        let mut size_bytes: u64 = 0;
        for (index, file) in reported.files.iter().enumerate() {
            if file.path.is_empty() {
                return Err(MetadataError::EmptyPath);
            }
            if file.padding {
                continue;
            }
            if !paths.insert(file.path.as_slice()) {
                return Err(MetadataError::DuplicatePath {
                    path: display_path(&file.path),
                });
            }
            let id = derive(&file.path);
            if positions.insert(id.clone(), files.len()).is_some() {
                return Err(MetadataError::DuplicateFileId { id });
            }
            size_bytes = size_bytes
                .checked_add(file.size_bytes)
                .filter(|&total| i64::try_from(total).is_ok())
                .ok_or(MetadataError::TooLarge)?;
            files.push(MetadataFile {
                id,
                index: FileIndex(index),
                path: file.path.clone(),
                size_bytes: file.size_bytes,
            });
        }
        Ok(Self {
            name: reported.name.clone(),
            files,
            size_bytes,
            positions,
        })
    }

    /// Returns the info dictionary's `name` as raw bytes.
    pub fn name(&self) -> &[u8] {
        &self.name
    }

    /// Returns the name decoded for display, replacing invalid UTF-8.
    pub fn display_name(&self) -> String {
        String::from_utf8_lossy(&self.name).into_owned()
    }

    /// Returns the total size of the files the API lists.
    pub fn size_bytes(&self) -> u64 {
        self.size_bytes
    }

    /// Returns the files the API lists, in metadata order, without padding
    /// files.
    pub fn files(&self) -> &[MetadataFile] {
        &self.files
    }

    /// Returns the file with the given ID and its position in
    /// [`files`](Self::files).
    pub fn file(&self, id: &str) -> Option<(usize, &MetadataFile)> {
        let position = *self.positions.get(id)?;
        self.files.get(position).map(|file| (position, file))
    }
}

impl MetadataFile {
    /// Returns the file's ID.
    pub fn id(&self) -> &FileId {
        &self.id
    }

    /// Returns the file's position in the engine's list.
    pub fn index(&self) -> FileIndex {
        self.index
    }

    /// Returns the file's raw path components, the torrent's name first.
    pub fn path(&self) -> &[Vec<u8>] {
        &self.path
    }

    /// Returns the path components decoded for display, replacing invalid
    /// UTF-8.
    pub fn display_path(&self) -> Vec<String> {
        self.path
            .iter()
            .map(|component| String::from_utf8_lossy(component).into_owned())
            .collect()
    }

    /// Returns the file's size.
    pub fn size_bytes(&self) -> u64 {
        self.size_bytes
    }
}

fn display_path(path: &[Vec<u8>]) -> String {
    path.iter()
        .map(|component| String::from_utf8_lossy(component))
        .collect::<Vec<_>>()
        .join("/")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(path: &[&str], size_bytes: u64) -> ReportedFile {
        ReportedFile {
            path: path.iter().map(|part| part.as_bytes().to_vec()).collect(),
            size_bytes,
            padding: false,
        }
    }

    fn padding(path: &[&str], size_bytes: u64) -> ReportedFile {
        ReportedFile {
            padding: true,
            ..file(path, size_bytes)
        }
    }

    fn metadata(files: Vec<ReportedFile>) -> ReportedMetadata {
        ReportedMetadata {
            name: b"Example".to_vec(),
            files,
        }
    }

    #[test]
    fn omits_padding_files_and_keeps_engine_indexes() {
        let reported = metadata(vec![
            file(&["Example", "a.mkv"], 10),
            padding(&["Example", ".pad", "6"], 6),
            file(&["Example", "b.mkv"], 20),
            padding(&["Example", ".pad", "6"], 6),
            file(&["Example", "c.srt"], 5),
        ]);
        let metadata = Metadata::identify(&reported).unwrap();
        let indexes: Vec<_> = metadata.files().iter().map(MetadataFile::index).collect();
        assert_eq!(indexes, [FileIndex(0), FileIndex(2), FileIndex(4)]);
        assert_eq!(
            metadata.size_bytes(),
            35,
            "padding files do not count toward the size"
        );
        let last = &metadata.files()[2];
        assert_eq!(last.id(), &FileId::derive(&[&b"Example"[..], b"c.srt"]));
        assert_eq!(
            metadata
                .file(last.id().as_str())
                .map(|(position, _)| position),
            Some(2)
        );
        assert_eq!(metadata.file("fil_01"), None);
    }

    #[test]
    fn rejects_duplicate_paths_of_non_padding_files() {
        let reported = metadata(vec![
            file(&["Example", "a.mkv"], 10),
            file(&["Example", "a.mkv"], 20),
        ]);
        assert_eq!(
            Metadata::identify(&reported),
            Err(MetadataError::DuplicatePath {
                path: "Example/a.mkv".into()
            })
        );
    }

    #[test]
    fn rejects_duplicate_file_ids() {
        let reported = metadata(vec![
            file(&["Example", "a.mkv"], 10),
            file(&["Example", "b.mkv"], 20),
        ]);
        let colliding = FileId::derive(&[b"collision"]);
        assert_eq!(
            Metadata::identify_with(&reported, |_| colliding.clone()),
            Err(MetadataError::DuplicateFileId { id: colliding })
        );
    }

    #[test]
    fn rejects_empty_paths_and_oversized_totals() {
        assert_eq!(
            Metadata::identify(&metadata(vec![file(&[], 1)])),
            Err(MetadataError::EmptyPath)
        );
        let reported = metadata(vec![
            file(&["Example", "a"], i64::MAX as u64),
            file(&["Example", "b"], 1),
        ]);
        assert_eq!(Metadata::identify(&reported), Err(MetadataError::TooLarge));
    }

    #[test]
    fn decodes_names_for_display_without_changing_ids() {
        let reported = ReportedMetadata {
            name: b"caf\xe9".to_vec(),
            files: vec![ReportedFile {
                path: vec![b"caf\xe9".to_vec()],
                size_bytes: 1,
                padding: false,
            }],
        };
        let metadata = Metadata::identify(&reported).unwrap();
        assert_eq!(metadata.display_name(), "caf\u{fffd}");
        assert_eq!(metadata.files()[0].display_path(), ["caf\u{fffd}"]);
        assert_eq!(
            metadata.files()[0].id(),
            &FileId::derive(&[&b"caf\xe9"[..]]),
            "the ID hashes the raw bytes"
        );
    }
}
