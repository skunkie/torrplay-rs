// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! The TorrPlay engine: the piece cache, librqbit integration, and the
//! adapter that keeps librqbit's types out of the rest of the application.
//!
//! The milestone 2 engine spike builds it up experiment by experiment. It
//! currently holds the [`cache::PieceCache`], the [`evictor::Evictor`] that
//! makes room in it, the memory storage that librqbit uses through it, and
//! the [`scheduler::TorrentScheduler`] that decides what a torrent
//! downloads from its readers' positions.

pub mod cache;
pub mod evictor;
pub mod scheduler;
pub mod storage;
