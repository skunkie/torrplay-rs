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
//! from its readers' positions, and the [`playback::Playback`] that shares
//! readahead across torrents, keeps idle readers, and signals active
//! playback.

mod adapter;
pub mod cache;
pub mod disk;
pub mod evictor;
mod inspect;
pub mod playback;
pub mod preload;
mod reader;
pub mod scheduler;
pub mod storage;
#[cfg(feature = "test-support")]
pub mod test_support;

pub use adapter::{
    CACHE_CAPACITY_BYTES_DEFAULT, EngineConfig, LibrqbitEngine, NetworkConfig, PEER_LIMIT_DEFAULT,
    PEER_PORT_DEFAULT, PIECE_WAIT_DEFAULT, REQUEST_WINDOW_BLOCKS_DEFAULT,
};
pub use reader::RangeReader;

/// Raises the process's soft limit on open files as far as the system
/// allows and returns the new limit. Every peer connection holds a socket,
/// and librqbit keeps up to 128 peers per torrent, so the 256 that a macOS
/// terminal starts processes with runs out within a few busy torrents, and
/// then the API cannot accept connections either.
pub fn raise_open_file_limit() -> anyhow::Result<u64> {
    librqbit::try_increase_nofile_limit()
}
