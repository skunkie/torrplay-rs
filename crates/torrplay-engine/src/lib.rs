// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! The TorrPlay engine: the piece cache, librqbit integration, and the
//! adapter that keeps librqbit's types out of the rest of the application.
//!
//! The milestone 2 engine spike builds it up experiment by experiment. It
//! currently holds the [`cache::PieceCache`] and the memory storage that
//! librqbit uses through it.

pub mod cache;
pub mod storage;
