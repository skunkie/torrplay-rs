// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! Stops a release build that would embed a web client that was never
//! built, with what to do, instead of rust-embed's error about a folder.

use std::path::Path;

const BUNDLE: &str = "../../client/dist";

fn main() {
    println!("cargo::rerun-if-changed={BUNDLE}");
    // Debug builds read the bundle at run time, so it may be missing.
    let embeds = std::env::var_os("CARGO_FEATURE_CLIENT").is_some()
        && std::env::var_os("CARGO_CFG_DEBUG_ASSERTIONS").is_none();
    if embeds && !Path::new(BUNDLE).join("index.html").exists() {
        println!(
            "cargo::error=the web client is not built: run `pnpm install && pnpm build` \
             in client/, or build with --no-default-features to leave it out"
        );
    }
}
