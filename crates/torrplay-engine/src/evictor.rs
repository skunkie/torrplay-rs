// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! The task that makes room for waiting reservations.
//!
//! The engine reserves room for a piece under the torrent's state lock, so
//! the reservation cannot evict there: eviction takes that lock. A reservation
//! that finds no room waits. A blocking pass releases retained buffers,
//! evicts verified and unpinned pieces through completion invalidation,
//! and wakes the torrents whose reservations now fit.
//!
//! When eviction is needed and no piece is eligible, the task can use ADR 5's
//! bounded fallback. With that fallback enabled, it releases the oldest idle
//! reader or ends the newest preload that pins cache pieces when there is
//! no idle reader to release.

use std::sync::Arc;

use anyhow::Context;
use tokio::task::JoinHandle;
use tokio::time::{Instant, sleep_until};
use tokio_util::sync::CancellationToken;

use crate::cache::{CacheError, PieceCache};
use crate::playback::Playback;

/// A running evictor for one [`PieceCache`].
pub struct Evictor {
    stop: CancellationToken,
    task: JoinHandle<anyhow::Result<()>>,
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
        let stop = CancellationToken::new();
        let task = tokio::spawn(run(cache, playback, stop.clone()));
        Self { stop, task }
    }

    /// Stops the evictor and returns its failure, if it failed. Detached
    /// buffers still held by in-flight copies are freed by their last holder.
    pub async fn shutdown(self) -> anyhow::Result<()> {
        self.stop.cancel();
        self.task.await.context("the evictor task panicked")?
    }
}

async fn run(
    cache: Arc<PieceCache>,
    playback: Option<Arc<Playback>>,
    stop: CancellationToken,
) -> anyhow::Result<()> {
    while !stop.is_cancelled() {
        let worker_cache = cache.clone();
        let worker_playback = playback.clone();
        // Reclamation can unmap many resident pages. Await the blocking
        // pass so shutdown drains it and passes for this cache stay ordered.
        let retry = tokio::task::spawn_blocking(move || {
            reconcile(&worker_cache, worker_playback.as_deref())
        })
        .await
        .context("the cache eviction pass failed")??;
        if retry {
            continue;
        }
        let deadline = cache.maintenance_deadline()?;
        tokio::select! {
            biased;
            () = stop.cancelled() => break,
            () = cache.room_wanted().notified() => {}
            () = async {
                match deadline {
                    Some(deadline) => sleep_until(deadline).await,
                    None => std::future::pending().await,
                }
            } => {}
        }
    }
    tokio::task::spawn_blocking(move || cache.stop_teardown_worker())
        .await
        .context("the final cache teardown failed")??;
    Ok(())
}

/// Returns whether a fresh pass should reassess the queue.
fn reconcile(cache: &PieceCache, playback: Option<&Playback>) -> Result<bool, CacheError> {
    cache.drain_teardown()?;
    cache.expire_reservations(Instant::now())?;
    let needed = cache.room_needed()?;
    if needed > 0 {
        let ready = cache.take_admissible_before_eviction()?;
        if !ready.is_empty() {
            for torrent in ready {
                torrent.reselect();
            }
            return Ok(true);
        }
        let Some(batches) = cache.waiting_eviction_candidates()? else {
            return Ok(true);
        };
        if batches.is_empty()
            && playback.is_some_and(|playback| {
                playback.release_oldest_idle_reader() || playback.end_newest_preload()
            })
        {
            return Ok(true);
        }
        for batch in batches {
            // A torrent that cannot invalidate, for example because it is
            // being deleted, frees nothing. Its reservation keeps waiting.
            let _ = batch.engine.invalidate(&batch.pieces);
        }
    }
    for torrent in cache.take_admissible()? {
        torrent.reselect();
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::sync::{Mutex, Weak};
    use std::thread::{self, ThreadId};
    use std::time::Duration;

    use tokio::sync::oneshot;

    use super::*;
    use crate::cache::{EngineTorrent, Reservation, TorrentKey};

    struct ObservedTorrent {
        cache: Weak<PieceCache>,
        key: TorrentKey,
        worker: Mutex<Option<oneshot::Sender<ThreadId>>>,
    }

    impl EngineTorrent for ObservedTorrent {
        fn invalidate(&self, pieces: &[u32]) -> anyhow::Result<usize> {
            let cache = self.cache.upgrade().context("the cache was released")?;
            for &piece in pieces {
                cache.evict(self.key, piece)?;
            }
            if let Some(worker) = self.worker.lock().unwrap().take() {
                let _ = worker.send(thread::current().id());
            }
            Ok(pieces.len())
        }

        fn reselect(&self) {}
    }

    #[tokio::test(flavor = "current_thread")]
    async fn invalidation_runs_off_the_async_worker() {
        let cache = PieceCache::new(16);
        let key = cache.register_torrent(16).unwrap();
        let (sent, observed) = oneshot::channel();
        cache
            .attach_engine(
                key,
                Arc::new(ObservedTorrent {
                    cache: Arc::downgrade(&cache),
                    key,
                    worker: Mutex::new(Some(sent)),
                }),
            )
            .unwrap();
        cache.seed(key, 0, &[1; 16]).unwrap();
        assert_eq!(cache.reserve(key, 1, 16).unwrap(), Reservation::Waiting);

        let evictor = Evictor::spawn(cache.clone());
        let worker = tokio::time::timeout(Duration::from_secs(5), observed).await;
        evictor.shutdown().await.unwrap();
        assert_ne!(worker.unwrap().unwrap(), thread::current().id());
        assert!(!cache.is_resident(key, 0).unwrap());
    }

    #[test]
    fn reclaiming_retained_room_admits_the_first_piece_before_eviction() {
        let cache = PieceCache::new(100);
        let key = cache.register_torrent(20).unwrap();
        cache
            .attach_engine(
                key,
                Arc::new(ObservedTorrent {
                    cache: Arc::downgrade(&cache),
                    key,
                    worker: Mutex::new(None),
                }),
            )
            .unwrap();
        cache.seed(key, 0, &[1; 80]).unwrap();
        cache.seed(key, 1, &[2; 20]).unwrap();
        cache.evict(key, 1).unwrap();
        assert_eq!(cache.reserve(key, 2, 20).unwrap(), Reservation::Waiting);
        assert_eq!(cache.reserve(key, 3, 20).unwrap(), Reservation::Waiting);

        assert!(reconcile(&cache, None).unwrap());
        assert_eq!(cache.stats().unwrap().waiting_pieces, 1);
        assert_eq!(cache.reserve(key, 2, 20).unwrap(), Reservation::Reserved);
        assert!(!reconcile(&cache, None).unwrap());
        assert!(cache.is_resident(key, 0).unwrap());

        assert_eq!(cache.reserve(key, 3, 20).unwrap(), Reservation::Waiting);
        reconcile(&cache, None).unwrap();
        assert!(!cache.is_resident(key, 0).unwrap());
        assert_eq!(cache.reserve(key, 3, 20).unwrap(), Reservation::Reserved);
    }

    #[test]
    fn lower_priority_work_waits_for_reselection_or_expiry() {
        for peer_retries in [true, false] {
            let cache = PieceCache::new(40);
            let first = cache.register_torrent(20).unwrap();
            let second = cache.register_torrent(20).unwrap();
            let filler = cache.register_torrent(20).unwrap();
            for key in [first, second, filler] {
                cache
                    .attach_engine(
                        key,
                        Arc::new(ObservedTorrent {
                            cache: Arc::downgrade(&cache),
                            key,
                            worker: Mutex::new(None),
                        }),
                    )
                    .unwrap();
            }
            cache.set_plan(first, HashSet::new(), &[0, 1]).unwrap();
            cache.set_plan(second, HashSet::new(), &[0, 1, 2]).unwrap();
            cache.seed(filler, 0, &[1; 20]).unwrap();
            cache.seed(filler, 1, &[2; 20]).unwrap();
            cache.evict(filler, 1).unwrap();
            for (key, piece) in [(first, 0), (first, 1), (second, 2)] {
                assert_eq!(cache.reserve(key, piece, 20).unwrap(), Reservation::Waiting);
            }

            assert!(reconcile(&cache, None).unwrap());
            // A second worker pass can run before either peer wakes up.
            assert!(!reconcile(&cache, None).unwrap());
            assert!(cache.is_resident(filler, 0).unwrap());
            assert_eq!(cache.reserve(first, 0, 20).unwrap(), Reservation::Reserved);

            let (key, piece) = if peer_retries {
                assert_eq!(cache.reserve(first, 1, 20).unwrap(), Reservation::Waiting);
                (first, 1)
            } else {
                let deadline = cache.maintenance_deadline().unwrap().unwrap();
                cache.expire_reservations(deadline).unwrap();
                (second, 2)
            };
            assert!(!reconcile(&cache, None).unwrap());
            assert!(!cache.is_resident(filler, 0).unwrap());
            assert_eq!(
                cache.reserve(key, piece, 20).unwrap(),
                Reservation::Reserved
            );
            if peer_retries {
                assert_eq!(cache.reserve(second, 2, 20).unwrap(), Reservation::Waiting);
            }
        }
    }
}
