// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! Where a disk torrent's files go below its directory, as ADR 9 decides.
//!
//! Paths come from metadata, so they are untrusted. Each path component maps
//! to a file name that every supported platform can hold: the bytes and
//! names some platform cannot hold are percent-encoded, and `%` itself is
//! too, so the mapping is injective and two components never share a name.
//! The same mapping applies on every platform.
//!
//! Files whose mapped paths differ only in case or Unicode normalization
//! would be one file on macOS and Windows, and a mapped component longer
//! than [`MAX_COMPONENT_BYTES`] fits no common filesystem, so a torrent with
//! either cannot use disk storage anywhere.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::PathBuf;

use icu_normalizer::ComposingNormalizerBorrowed;
use torrplay_core::metadata::ReportedMetadata;

/// The longest mapped path component, in bytes, that common filesystems
/// hold.
pub const MAX_COMPONENT_BYTES: usize = 255;

/// The characters Windows forbids in file names, beside control characters.
const FORBIDDEN: &[u8] = b"/\\:*?\"<>|";

/// The names Windows reserves for devices, with or without an extension.
const RESERVED: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "COM0", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7",
    "COM8", "COM9", "COM¹", "COM²", "COM³", "LPT0", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6",
    "LPT7", "LPT8", "LPT9", "LPT¹", "LPT²", "LPT³",
];

/// Why a torrent's files cannot use disk storage.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum UnmappablePaths {
    /// Two files would be one on a case-insensitive filesystem.
    #[error(
        "Disk storage cannot hold the files {first} and {second}, which differ only in case or Unicode normalization."
    )]
    Collision {
        /// One file's path, as the API reports it.
        first: String,
        /// The other's.
        second: String,
    },
    /// A file sits where another needs a directory.
    #[error("Disk storage cannot hold the file {file}, which another file needs as a directory.")]
    FileAsDirectory {
        /// The file's path, as the API reports it.
        file: String,
    },
    /// A component maps to a name longer than filesystems hold.
    #[error(
        "Disk storage cannot hold the file {file}, whose names exceed {MAX_COMPONENT_BYTES} bytes."
    )]
    TooLong {
        /// The file's path, as the API reports it.
        file: String,
    },
    /// A path has an empty component.
    #[error("Disk storage cannot hold the file {file}, whose path has an empty name.")]
    Empty {
        /// The file's path, as the API reports it.
        file: String,
    },
}

/// Maps one path component to its file name.
pub fn map_component(component: &[u8]) -> String {
    let mut name = String::with_capacity(component.len());
    for chunk in component.utf8_chunks() {
        for (index, character) in chunk.valid().char_indices() {
            let at_start = name.is_empty() && index == 0;
            if escapes(character, at_start) {
                let mut bytes = [0; 4];
                for byte in character.encode_utf8(&mut bytes).bytes() {
                    let _ = write!(name, "%{byte:02X}");
                }
            } else {
                name.push(character);
            }
        }
        for byte in chunk.invalid() {
            let _ = write!(name, "%{byte:02X}");
        }
    }
    // Windows strips a trailing space or dot, which also covers `.` and
    // `..`.
    if let Some(last) = name.chars().last()
        && (last == ' ' || last == '.')
    {
        name.pop();
        let _ = write!(name, "%{:02X}", last as u8);
    }
    if is_reserved(&name) {
        let first = name.remove(0);
        name.insert_str(0, &format!("%{:02X}", first as u8));
    }
    name
}

fn escapes(character: char, at_start: bool) -> bool {
    character == '%'
        || character.is_control()
        || (character.is_ascii() && FORBIDDEN.contains(&(character as u8)))
        || (at_start && character == ' ')
}

/// Whether Windows reserves the name, ignoring case and any extension.
fn is_reserved(name: &str) -> bool {
    let stem = name.split('.').next().unwrap_or(name);
    RESERVED
        .iter()
        .any(|reserved| reserved.eq_ignore_ascii_case(stem))
}

/// The path below the torrent's directory of each file in `metadata`, in its
/// order, or `None` for a padding file, which has no file on disk.
pub fn map_files(metadata: &ReportedMetadata) -> Result<Vec<Option<PathBuf>>, UnmappablePaths> {
    let nfc = ComposingNormalizerBorrowed::new_nfc();
    // Each file's and each directory's folded path, with the file it came
    // from.
    let mut files: HashMap<String, usize> = HashMap::new();
    let mut directories: HashMap<String, usize> = HashMap::new();
    let mut mapped = Vec::with_capacity(metadata.files.len());
    for (index, file) in metadata.files.iter().enumerate() {
        if file.padding {
            mapped.push(None);
            continue;
        }
        let shown = shown(&file.path);
        let mut path = PathBuf::new();
        let mut folded = String::new();
        for (depth, component) in file.path.iter().enumerate() {
            if component.is_empty() {
                return Err(UnmappablePaths::Empty { file: shown });
            }
            let name = map_component(component);
            if name.len() > MAX_COMPONENT_BYTES {
                return Err(UnmappablePaths::TooLong { file: shown });
            }
            if depth > 0 {
                folded.push('/');
            }
            folded.push_str(&nfc.normalize(&name).to_lowercase());
            path.push(name);
            let last = depth + 1 == file.path.len();
            if !last {
                if let Some(&other) = files.get(&folded) {
                    return Err(UnmappablePaths::FileAsDirectory {
                        file: self::shown(&metadata.files[other].path),
                    });
                }
                directories.entry(folded.clone()).or_insert(index);
            }
        }
        if directories.contains_key(&folded) {
            return Err(UnmappablePaths::FileAsDirectory { file: shown });
        }
        if let Some(&other) = files.get(&folded) {
            return Err(UnmappablePaths::Collision {
                first: self::shown(&metadata.files[other].path),
                second: shown,
            });
        }
        files.insert(folded, index);
        mapped.push(Some(path));
    }
    Ok(mapped)
}

/// A path as the API reports it, for details.
fn shown(path: &[Vec<u8>]) -> String {
    path.iter()
        .map(|component| String::from_utf8_lossy(component))
        .collect::<Vec<_>>()
        .join("/")
}

#[cfg(test)]
mod tests {
    use torrplay_core::metadata::ReportedFile;

    use super::*;

    fn metadata(paths: &[&[&str]]) -> ReportedMetadata {
        ReportedMetadata {
            name: b"Example".to_vec(),
            files: paths
                .iter()
                .map(|path| ReportedFile {
                    path: path.iter().map(|part| part.as_bytes().to_vec()).collect(),
                    size_bytes: 1,
                    padding: false,
                })
                .collect(),
        }
    }

    #[test]
    fn ordinary_names_stay_as_they_are() {
        for name in [
            "Movie (2026).mkv",
            "Фильм.mkv",
            "映画.mp4",
            ".hidden",
            "a b",
        ] {
            assert_eq!(map_component(name.as_bytes()), name);
        }
    }

    #[test]
    fn names_some_platform_cannot_hold_are_escaped() {
        for (name, mapped) in [
            ("100%", "100%25"),
            ("a:b", "a%3Ab"),
            ("why?", "why%3F"),
            ("tab\there", "tab%09here"),
            (" lead", "%20lead"),
            ("trail ", "trail%20"),
            ("end.", "end%2E"),
            (".", "%2E"),
            ("..", ".%2E"),
            ("CON", "%43ON"),
            ("con.txt", "%63on.txt"),
            ("COM1.log", "%43OM1.log"),
            ("CONSOLE", "CONSOLE"),
            ("a\\b", "a%5Cb"),
        ] {
            assert_eq!(map_component(name.as_bytes()), mapped, "{name:?}");
        }
        assert_eq!(map_component(b"bad\xffbyte"), "bad%FFbyte");
    }

    #[test]
    fn the_mapping_is_injective() {
        let names: [&[u8]; 8] = [b"%41", b"A", b"a:b", b"a%3Ab", b".", b"%2E", b"..", b".%2E"];
        let mapped: std::collections::HashSet<String> =
            names.iter().map(|name| map_component(name)).collect();
        assert_eq!(mapped.len(), names.len());
    }

    #[test]
    fn files_map_below_the_torrent_directory_in_order() {
        let mut metadata = metadata(&[&["Example", "a.mkv"], &["Example", "sub", "b?.srt"]]);
        metadata.files.insert(
            1,
            ReportedFile {
                path: vec![b"Example".to_vec(), b".pad".to_vec(), b"0".to_vec()],
                size_bytes: 1,
                padding: true,
            },
        );
        assert_eq!(
            map_files(&metadata).unwrap(),
            [
                Some(PathBuf::from("Example").join("a.mkv")),
                None,
                Some(PathBuf::from("Example").join("sub").join("b%3F.srt")),
            ]
        );
    }

    #[test]
    fn files_differing_only_in_case_or_normalization_collide() {
        let case = metadata(&[&["Example", "a.mkv"], &["Example", "A.mkv"]]);
        assert_eq!(
            map_files(&case),
            Err(UnmappablePaths::Collision {
                first: "Example/a.mkv".into(),
                second: "Example/A.mkv".into(),
            })
        );
        let normalization = metadata(&[&["Example", "caf\u{e9}"], &["Example", "cafe\u{301}"]]);
        assert!(matches!(
            map_files(&normalization),
            Err(UnmappablePaths::Collision { .. })
        ));
        let directories = metadata(&[&["Example", "Sub", "a"], &["Example", "sub", "b"]]);
        assert!(
            map_files(&directories).is_ok(),
            "directories that differ in case merge without losing a file"
        );
    }

    #[test]
    fn a_file_where_a_directory_must_go_is_refused() {
        for paths in [
            &[&["Example", "a"][..], &["Example", "A", "b"]][..],
            &[&["Example", "a", "b"], &["Example", "a"]],
        ] {
            assert!(
                matches!(
                    map_files(&metadata(paths)),
                    Err(UnmappablePaths::FileAsDirectory { .. })
                ),
                "{paths:?}"
            );
        }
    }

    #[test]
    fn overlong_and_empty_names_are_refused() {
        let long = "x".repeat(MAX_COMPONENT_BYTES + 1);
        assert!(matches!(
            map_files(&metadata(&[&["Example", &long]])),
            Err(UnmappablePaths::TooLong { .. })
        ));
        let escaped = "?".repeat(MAX_COMPONENT_BYTES / 3 + 1);
        assert!(
            matches!(
                map_files(&metadata(&[&["Example", &escaped]])),
                Err(UnmappablePaths::TooLong { .. })
            ),
            "the limit applies to the mapped name"
        );
        assert!(matches!(
            map_files(&metadata(&[&["Example", ""]])),
            Err(UnmappablePaths::Empty { .. })
        ));
    }
}
