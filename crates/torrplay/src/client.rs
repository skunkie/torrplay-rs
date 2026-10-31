// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! The web client's bundle, which `pnpm build` writes to `client/dist`. A
//! release build embeds it; a debug build reads the directory as it is, so
//! a rebuilt client shows without rebuilding the server.

use std::sync::Arc;

use torrplay_server::{ClientFile, ClientFiles};

#[derive(rust_embed::RustEmbed)]
#[folder = "../../client/dist"]
struct Bundle;

/// The bundle's files, for the server to serve.
pub(crate) fn files() -> ClientFiles {
    Arc::new(|path: &str| {
        Bundle::get(path).map(|file| ClientFile {
            sha256: file.metadata.sha256_hash(),
            bytes: file.data,
        })
    })
}
