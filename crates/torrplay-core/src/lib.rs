// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! The engine-independent TorrPlay domain.
//!
//! This crate holds what the HTTP server needs to answer from the
//! application's own records, and the facade it uses for everything else:
//!
//! - [`id`]: torrent IDs, file IDs, and v1 info hashes, as
//!   `docs/adr/0004-resource-identity.md` defines them.
//! - [`metadata`]: a torrent's metadata with its file IDs, validated for
//!   duplicate paths and file IDs.
//! - [`lifecycle`]: desired and observed state, failures, the configuration
//!   and its entity tag, and engine readiness, as `docs/lifecycle.md`
//!   defines them.
//! - [`registry`]: the registered torrents, with atomic duplicate detection.
//! - [`pagination`]: page sizes and opaque cursors for collections.
//! - [`settings`]: the application's settings, their bounds and defaults,
//!   and merging explicit values with the defaults.
//! - [`redact`]: redaction of URLs and magnet URIs in text bound for logs.
//! - [`engine`]: the [`Engine`](engine::Engine) trait, the torrent engine
//!   facade, with its inputs, observations, and statistics.
//! - `fake`, behind the `fake` feature: a scripted engine over the
//!   `torrplay-fixtures` catalog for tests that run without a torrent engine.
//!
//! The crate has no HTTP or engine dependencies; mapping its types to the
//! representations of `api/openapi.yaml` is the server's job.

pub mod engine;
#[cfg(feature = "fake")]
pub mod fake;
pub mod id;
pub mod lifecycle;
pub mod metadata;
pub mod pagination;
pub mod preload;
pub mod redact;
pub mod registry;
pub mod settings;

mod base32;
