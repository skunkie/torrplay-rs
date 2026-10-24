// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! The task that makes room for waiting reservations.
//!
//! The engine reserves room for a piece under the torrent's state lock, so
//! the reservation cannot evict there: eviction takes that lock. A
//! reservation that finds no room waits instead, and this task, which holds
//! no lock, evicts verified and unpinned pieces through completion
//! invalidation and then wakes the torrents whose reservations now fit.
//!
//! When nothing is evictable, the task takes ADR 5's bounded fallback:
//! it releases the oldest idle reader, whose pins then let pieces go.

use std::sync::Arc;

use anyhow::Context;
use tokio::sync::Notify;
use tokio::task::JoinHandle;

use crate::cache::{CacheError, PieceCache};
use crate::playback::Playback;

/// A running evictor for one [`PieceCache`].
pub struct Evictor {
    stop: Arc<Notify>,
    task: JoinHandle<Result<(), CacheError>>,
}

impl Evictor {
    /// Starts the evictor on the current Tokio runtime.
    pub fn spawn(cache: Arc<PieceCache>) -> Self {
        Self::start(cache, None)
    }

    /// Starts the evictor with the bounded fallback of releasing
    /// `playback`'s oldest idle readers when nothing is evictable.
    pub fn spawn_with_fallback(cache: Arc<PieceCache>, playback: Arc<Playback>) -> Self {
        Self::start(cache, Some(playback))
    }

    fn start(cache: Arc<PieceCache>, playback: Option<Arc<Playback>>) -> Self {
        let stop = Arc::new(Notify::new());
        let task = tokio::spawn(run(cache, playback, stop.clone()));
        Self { stop, task }
    }

    /// Stops the evictor and returns its failure, if it failed.
    pub async fn shutdown(self) -> anyhow::Result<()> {
        // A stored permit stops the task even if it is not waiting yet.
        self.stop.notify_one();
        Ok(self.task.await.context("the evictor task panicked")??)
    }
}

async fn run(
    cache: Arc<PieceCache>,
    playback: Option<Arc<Playback>>,
    stop: Arc<Notify>,
) -> Result<(), CacheError> {
    loop {
        let needed = cache.room_needed()?;
        if needed > 0 {
            let batches = cache.eviction_candidates(needed)?;
            if batches.is_empty()
                && playback.as_ref().is_some_and(|playback| {
                    playback.release_oldest_idle_reader() || playback.end_newest_preload()
                })
            {
                // The released pins may have made pieces evictable.
                continue;
            }
            for batch in batches {
                // A torrent that cannot invalidate, for example because it
                // is being deleted, frees nothing, and the reservation keeps
                // waiting for the next change.
                let _ = batch.engine.invalidate(&batch.pieces);
            }
        }
        for torrent in cache.take_admissible()? {
            torrent.reselect();
        }
        tokio::select! {
            () = cache.room_wanted().notified() => {}
            () = stop.notified() => return Ok(()),
        }
    }
}
