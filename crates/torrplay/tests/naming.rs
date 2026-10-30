// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! The workspace's names follow `docs/naming.md`.

// Helpers assert as tests do; clippy's test exemption covers only test
// functions.
#![allow(clippy::expect_used)]

use std::fs;
use std::path::{Path, PathBuf};

/// Constants named for what a protocol or the contract names, which keep
/// that name: SSDP's `max-age` and the `min_level` query parameter.
const EXTERNAL_NAMES: &[&str] = &["MAX_AGE_SECONDS", "MIN_LEVEL"];

/// Qualifiers that come last in a constant's name.
const QUALIFIERS: &[&str] = &["MAX_", "MIN_", "DEFAULT_"];

/// Every Rust file of the workspace's crates, generated ones aside.
fn sources(directory: &Path, found: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(directory).expect("a readable directory") {
        let path = entry.expect("a directory entry").path();
        if path.is_dir() {
            if !path.ends_with("generated") && !path.ends_with("target") {
                sources(&path, found);
            }
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            found.push(path);
        }
    }
}

/// The name a line declares as a constant, if it declares one.
fn constant(line: &str) -> Option<&str> {
    let declaration = line.trim_start();
    let rest = [
        "pub const ",
        "pub(crate) const ",
        "pub(super) const ",
        "const ",
    ]
    .iter()
    .find_map(|prefix| declaration.strip_prefix(prefix))?;
    rest.split(':').next().map(str::trim)
}

#[test]
fn constants_put_their_qualifier_last() {
    let crates = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("the crates directory");
    let mut files = Vec::new();
    sources(crates, &mut files);
    assert!(files.len() > 100, "the workspace's sources are found");
    let mut misnamed = Vec::new();
    for file in files {
        let text = fs::read_to_string(&file).expect("a readable source");
        for (number, line) in text.lines().enumerate() {
            let Some(name) = constant(line) else {
                continue;
            };
            if QUALIFIERS
                .iter()
                .any(|qualifier| name.starts_with(qualifier))
                && !EXTERNAL_NAMES.contains(&name)
            {
                misnamed.push(format!("{}:{}: {name}", file.display(), number + 1));
            }
        }
    }
    assert!(
        misnamed.is_empty(),
        "these constants put a qualifier first; docs/naming.md puts it last:\n{}",
        misnamed.join("\n")
    );
}

#[test]
fn a_misnamed_constant_is_found() {
    assert_eq!(
        constant("    pub const MAX_BODY_BYTES: usize = 1;"),
        Some("MAX_BODY_BYTES")
    );
    assert_eq!(
        constant("const BODY_MAX_BYTES: usize = 1;"),
        Some("BODY_MAX_BYTES")
    );
    assert_eq!(constant("let x = 1;"), None);
}
