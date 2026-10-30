// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! Temporary torrents, as the lifecycle's Retention section describes:
//! their sources, kept so persisting one can store it, and their activity,
//! from which they expire.
//!
//! A temporary torrent is active while a response streams its content, and
//! its idle time counts from when the last such response ended, from its
//! last preload's creation, or from its own creation.

use std::collections::HashMap;
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::task::{Context, Poll};
use std::time::{Duration, SystemTime};

use axum::body::{Body, Bytes, HttpBody};
use http_body::{Frame, SizeHint};
use torrplay_core::engine::Source;
use torrplay_core::id::TorrentId;

use crate::app::Clock;

/// The temporary torrents and what each is doing.
#[derive(Clone, Default)]
pub(crate) struct Temporaries(Arc<Mutex<HashMap<TorrentId, Temporary>>>);

struct Temporary {
    /// What the torrent was created from, with metadata once recorded.
    source: Source,
    /// Responses streaming its content.
    active: usize,
    /// When it last stopped being active, or was created.
    idle_since: SystemTime,
}

impl Temporaries {
    fn lock(&self) -> MutexGuard<'_, HashMap<TorrentId, Temporary>> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Records a new temporary torrent, idle from `now`.
    pub(crate) fn insert(&self, id: TorrentId, source: Source, now: SystemTime) {
        self.lock().insert(
            id,
            Temporary {
                source,
                active: 0,
                idle_since: now,
            },
        );
    }

    /// Forgets a torrent that was deleted or persisted, returning its
    /// source if it was temporary.
    pub(crate) fn remove(&self, id: &TorrentId) -> Option<Source> {
        self.lock().remove(id).map(|temporary| temporary.source)
    }

    /// Whether torrent `id` is temporary.
    pub(crate) fn contains(&self, id: &TorrentId) -> bool {
        self.lock().contains_key(id)
    }

    /// The source a temporary torrent would be stored with.
    pub(crate) fn source(&self, id: &TorrentId) -> Option<Source> {
        self.lock()
            .get(id)
            .map(|temporary| temporary.source.clone())
    }

    /// Replaces a temporary torrent's source with its torrent file, once its
    /// metadata is known; returns whether it is temporary.
    pub(crate) fn record_metainfo(&self, id: &TorrentId, metainfo: Vec<u8>) -> bool {
        match self.lock().get_mut(id) {
            Some(temporary) => {
                temporary.source = Source::Metainfo(metainfo);
                true
            }
            None => false,
        }
    }

    /// The number of temporary torrents.
    pub(crate) fn len(&self) -> usize {
        self.lock().len()
    }

    /// Restarts a temporary torrent's idle time at `now`, as a preload does.
    pub(crate) fn touch(&self, id: &TorrentId, now: SystemTime) {
        if let Some(temporary) = self.lock().get_mut(id) {
            temporary.idle_since = temporary.idle_since.max(now);
        }
    }

    /// Marks torrent `id` active until the guard drops, if it is temporary.
    pub(crate) fn activity(&self, id: &TorrentId, clock: &Clock) -> Option<Activity> {
        self.lock().get_mut(id)?.active += 1;
        Some(Activity {
            temporaries: self.clone(),
            id: id.clone(),
            clock: Arc::clone(clock),
        })
    }

    /// The temporary torrents idle for at least `limit` at `now`.
    pub(crate) fn expired(&self, now: SystemTime, limit: Duration) -> Vec<TorrentId> {
        self.lock()
            .iter()
            .filter(|(_, temporary)| {
                temporary.active == 0
                    && now
                        .duration_since(temporary.idle_since)
                        .is_ok_and(|idle| idle >= limit)
            })
            .map(|(id, _)| id.clone())
            .collect()
    }

    /// The idle temporary torrent idle longest, which makes room for a new
    /// one; ties go to the smaller ID, so the choice is deterministic.
    pub(crate) fn longest_idle(&self) -> Option<TorrentId> {
        self.lock()
            .iter()
            .filter(|(_, temporary)| temporary.active == 0)
            .min_by(|(a_id, a), (b_id, b)| {
                a.idle_since
                    .cmp(&b.idle_since)
                    .then_with(|| a_id.as_str().cmp(b_id.as_str()))
            })
            .map(|(id, _)| id.clone())
    }
}

/// Keeps a temporary torrent active; dropping it starts its idle time.
pub(crate) struct Activity {
    temporaries: Temporaries,
    id: TorrentId,
    clock: Clock,
}

impl Drop for Activity {
    fn drop(&mut self) {
        let now = (self.clock)();
        if let Some(temporary) = self.temporaries.lock().get_mut(&self.id) {
            temporary.active = temporary.active.saturating_sub(1);
            temporary.idle_since = temporary.idle_since.max(now);
        }
    }
}

/// A response body that keeps its torrent active until it ends or is
/// dropped, with the inner body's size so lengths stay exact.
pub(crate) struct ActiveBody {
    body: Body,
    _activity: Activity,
}

impl ActiveBody {
    pub(crate) fn new(body: Body, activity: Activity) -> Self {
        Self {
            body,
            _activity: activity,
        }
    }
}

impl HttpBody for ActiveBody {
    type Data = Bytes;
    type Error = axum::Error;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, axum::Error>>> {
        Pin::new(&mut self.body).poll_frame(cx)
    }

    fn is_end_stream(&self) -> bool {
        self.body.is_end_stream()
    }

    fn size_hint(&self) -> SizeHint {
        self.body.size_hint()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(n: u8) -> TorrentId {
        TorrentId::from_random_bytes([n; torrplay_core::id::TORRENT_ID_RANDOM_BYTES])
    }

    fn at(seconds: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(seconds)
    }

    #[test]
    fn idle_time_counts_from_the_last_activity() {
        let temporaries = Temporaries::default();
        let now = Arc::new(std::sync::atomic::AtomicU64::new(100));
        let clock: Clock = {
            let now = Arc::clone(&now);
            Arc::new(move || at(now.load(std::sync::atomic::Ordering::SeqCst)))
        };
        temporaries.insert(id(1), Source::MagnetUri("magnet:".into()), at(100));
        let limit = Duration::from_secs(60);
        assert!(temporaries.expired(at(159), limit).is_empty());
        assert_eq!(temporaries.expired(at(160), limit), [id(1)]);

        let activity = temporaries.activity(&id(1), &clock).unwrap();
        assert!(
            temporaries.expired(at(10_000), limit).is_empty(),
            "an active torrent never expires"
        );
        assert_eq!(temporaries.longest_idle(), None);
        now.store(500, std::sync::atomic::Ordering::SeqCst);
        drop(activity);
        assert!(temporaries.expired(at(559), limit).is_empty());
        assert_eq!(temporaries.expired(at(560), limit), [id(1)]);
        temporaries.touch(&id(1), at(600));
        assert!(temporaries.expired(at(620), limit).is_empty());
    }

    #[test]
    fn the_torrent_idle_longest_makes_room() {
        let temporaries = Temporaries::default();
        let clock: Clock = Arc::new(|| at(0));
        for (n, since) in [(1, 30), (2, 10), (3, 20)] {
            temporaries.insert(id(n), Source::MagnetUri("magnet:".into()), at(since));
        }
        assert_eq!(temporaries.longest_idle(), Some(id(2)));
        let _streaming = temporaries.activity(&id(2), &clock);
        assert_eq!(temporaries.longest_idle(), Some(id(3)));
        assert!(
            temporaries.activity(&id(9), &clock).is_none(),
            "not temporary"
        );
        assert_eq!(
            temporaries.remove(&id(3)),
            Some(Source::MagnetUri("magnet:".into()))
        );
        assert_eq!(temporaries.longest_idle(), Some(id(1)));
        assert_eq!(temporaries.len(), 2);
    }
}
