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

use std::sync::Arc;

use anyhow::Context;
use tokio::sync::Notify;
use tokio::task::JoinHandle;

use crate::cache::{CacheError, PieceCache};

/// A running evictor for one [`PieceCache`].
pub struct Evictor {
    stop: Arc<Notify>,
    task: JoinHandle<Result<(), CacheError>>,
}

impl Evictor {
    /// Starts the evictor on the current Tokio runtime.
    pub fn spawn(cache: Arc<PieceCache>) -> Self {
        let stop = Arc::new(Notify::new());
        let task = tokio::spawn(run(cache, stop.clone()));
        Self { stop, task }
    }

    /// Stops the evictor and returns its failure, if it failed.
    pub async fn shutdown(self) -> anyhow::Result<()> {
        // A stored permit stops the task even if it is not waiting yet.
        self.stop.notify_one();
        Ok(self.task.await.context("the evictor task panicked")??)
    }
}

async fn run(cache: Arc<PieceCache>, stop: Arc<Notify>) -> Result<(), CacheError> {
    loop {
        let shortfall = cache.waiting_shortfall()?;
        if shortfall > 0 {
            for batch in cache.eviction_candidates(shortfall)? {
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
