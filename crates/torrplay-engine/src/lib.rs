// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! The TorrPlay engine: the piece cache, librqbit integration, and the
//! adapter that keeps librqbit's types out of the rest of the application.
//!
//! [`LibrqbitEngine`] implements the `torrplay-core` engine facade over
//! librqbit, so the server never sees a librqbit type. Beneath it are the
//! [`cache::PieceCache`], the [`evictor::Evictor`] that makes room in it, the
//! memory storage that librqbit uses through it, and the
//! [`scheduler::TorrentScheduler`] that decides what a torrent downloads
//! from its readers' positions.

mod adapter;
pub mod cache;
pub mod evictor;
mod inspect;
mod reader;
pub mod scheduler;
pub mod storage;

pub use adapter::{
    DEFAULT_CACHE_CAPACITY_BYTES, DEFAULT_PEER_PORT, DEFAULT_PIECE_WAIT,
    DEFAULT_REQUEST_WINDOW_BLOCKS, EngineConfig, LibrqbitEngine, NetworkConfig,
};
pub use reader::RangeReader;
