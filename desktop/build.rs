// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

use serde_json::Value;

#[path = "src/merge_patch.rs"]
mod merge_patch;

/// The configuration the client merges over `tauri.conf.json`: its own product name,
/// executable, and identifier, so its data, log, and single-instance lock are not the
/// application's, as ADR 13 decides.
const CLIENT_OVERLAY: &str = "tauri.client.conf.json";

fn main() {
    if std::env::var_os("CARGO_FEATURE_CLIENT").is_some() {
        apply_client_overlay();
    }
    tauri_build::build();
}

/// Has Tauri's context, which fixes the application's identity when it compiles, read
/// the client's overlay, as `tauri build --config` would. A configuration the Tauri CLI
/// passes in `TAURI_CONFIG` is merged over it, so the CLI's keys win and the client keeps
/// the rest of its identity whatever the CLI was given.
///
/// Only the context sees the result, through the compiler's environment. Tauri's own
/// build step reads `TAURI_CONFIG` from this script's environment, which a script cannot
/// change, so a plain `cargo build` of the client writes the application's name into its
/// Windows file properties; a build through the Tauri CLI, given the overlay, does not.
fn apply_client_overlay() {
    println!("cargo:rerun-if-changed={CLIENT_OVERLAY}");
    println!("cargo:rerun-if-env-changed=TAURI_CONFIG");
    let overlay = std::fs::read_to_string(CLIENT_OVERLAY)
        .unwrap_or_else(|error| panic!("reading {CLIENT_OVERLAY}: {error}"));
    let mut config: Value = serde_json::from_str(&overlay)
        .unwrap_or_else(|error| panic!("{CLIENT_OVERLAY} is not JSON: {error}"));
    if let Ok(given) = std::env::var("TAURI_CONFIG") {
        let given: Value = serde_json::from_str(&given)
            .unwrap_or_else(|error| panic!("TAURI_CONFIG is not JSON: {error}"));
        merge_patch::merge(&mut config, given);
    }
    // On one line, as a compiler environment variable must be.
    println!("cargo:rustc-env=TAURI_CONFIG={config}");
}
