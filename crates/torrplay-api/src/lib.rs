// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! The wire types of `api/openapi.yaml`.
//!
//! openapi-to-rust generates `src/generated/` from the document, as
//! `docs/adr/0006-contract-validation.md` decides, and CI checks that the
//! committed output matches what the document produces. Change the document
//! and regenerate; never edit the generated files by hand.
//!
//! The types serialize the representations the server sends. One kind of
//! input needs more than they offer: [`TorrentConfigurationPatch`] reads a
//! `null` member the same as an absent one, so the server reads merge
//! patches itself.

// The generator's builders mark methods returning a `#[must_use]` builder
// as `#[must_use]` again.
#[allow(clippy::double_must_use)]
mod generated;

pub use generated::*;
