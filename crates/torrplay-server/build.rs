// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! Records the commit being built, for `GET /api/v1/system` to report, marked if the
//! sources differ from it. Outside a Git checkout, such as a build from a source
//! archive, nothing is recorded.

use std::path::Path;
use std::process::Command;

/// What a build is made from, relative to the workspace, whose changes make it differ
/// from its commit. Changes elsewhere, such as to the documentation, do not.
const SOURCES: [&str; 5] = ["api", "client/src", "crates", "Cargo.lock", "Cargo.toml"];

/// What `git` prints for `args`, if it runs and succeeds.
fn git(args: &[&str]) -> Option<String> {
    // UTC for a date read back in the local time zone, as the contract says.
    let output = Command::new("git")
        .args(args)
        .env("TZ", "UTC")
        .output()
        .ok()?;
    let text = String::from_utf8(output.stdout).ok()?;
    output.status.success().then(|| text.trim().to_owned())
}

/// Whether the Git checkout here is this workspace, rather than some other project's
/// that a source archive was unpacked into.
fn in_own_checkout() -> bool {
    let manifest = std::env::var_os("CARGO_MANIFEST_DIR");
    let workspace = manifest
        .as_deref()
        .map(Path::new)
        .and_then(|crate_dir| crate_dir.join("../..").canonicalize().ok());
    let top = git(&["rev-parse", "--show-toplevel"]);
    let top = top.and_then(|path| Path::new(&path).canonicalize().ok());
    workspace.is_some() && workspace == top
}

fn main() {
    println!("cargo::rerun-if-changed=build.rs");
    if !in_own_checkout() {
        return;
    }
    // Written at every commit and checkout, in a worktree as well.
    if let Some(log) = git(&["rev-parse", "--git-path", "logs/HEAD"]) {
        println!("cargo::rerun-if-changed={log}");
    }
    // The script runs again when the sources change, to notice that they have.
    for source in SOURCES {
        println!("cargo::rerun-if-changed=../../{source}");
    }
    let status = ["-C", "../..", "status", "--porcelain", "--"];
    let changes = git(&[status.as_slice(), SOURCES.as_slice()].concat());
    let dirty = changes.is_some_and(|changes| !changes.is_empty());
    let described = git(&[
        "log",
        "-1",
        "--format=%h %cd",
        "--date=format-local:%Y-%m-%d",
    ]);
    if let Some((commit, date)) = described.as_deref().and_then(|text| text.split_once(' ')) {
        let mark = if dirty { "-dirty" } else { "" };
        println!("cargo::rustc-env=TORRPLAY_COMMIT={commit}{mark}");
        println!("cargo::rustc-env=TORRPLAY_COMMIT_DATE={date}");
    }
}
