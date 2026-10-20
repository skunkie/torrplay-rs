// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! The application state the handlers share: the engine, the registry,
//! and what the server last learned from the engine about each torrent.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use torrplay_core::engine::{CacheStatistics, Engine, EngineError};
use torrplay_core::id::TorrentId;
use torrplay_core::lifecycle::{Failure, ObservedState, Readiness};
use torrplay_core::metadata::Metadata;
use torrplay_core::registry::{Registry, TorrentRecord};

use crate::problem::Problem;

/// How long a request body may take to arrive by default, as the contract
/// documents.
const DEFAULT_BODY_TIMEOUT: Duration = Duration::from_secs(30);

/// The state of a TorrPlay server over the engine `E`.
pub struct App<E> {
    engine: Arc<E>,
    registry: Registry,
    body_timeout: Duration,
    observations: Mutex<Observations>,
    shutting_down: AtomicBool,
}

/// What the server last learned from the engine. Requests answered from
/// the application's own records fall back on it while the engine is
/// unavailable.
#[derive(Default)]
struct Observations {
    torrents: HashMap<TorrentId, Observation>,
    cache_statistics: CacheStatistics,
}

#[derive(Clone, Debug, Default)]
struct Observation {
    /// The failure the engine last reported.
    engine_failure: Option<Failure>,
    /// Why retrieved metadata cannot be served. Pausing and starting the
    /// torrent clears it.
    metadata_failure: Option<Failure>,
}

/// A torrent as the API presents it: its record and its effective failure.
#[derive(Clone, Debug)]
pub(crate) struct TorrentView {
    pub(crate) record: TorrentRecord,
    pub(crate) failure: Option<Failure>,
}

impl TorrentView {
    pub(crate) fn observed_state(&self) -> ObservedState {
        ObservedState::derive(
            self.record.configuration.desired_state(),
            self.record.metadata.is_some(),
            self.failure.as_ref(),
        )
    }
}

impl<E: Engine> App<E> {
    /// Creates a server over `engine` with an empty registry.
    pub fn new(engine: Arc<E>) -> Self {
        Self::with_registry(engine, Registry::new())
    }

    /// Creates a server over `engine` with `registry`, so tests can choose
    /// the torrent IDs it issues.
    pub fn with_registry(engine: Arc<E>, registry: Registry) -> Self {
        Self {
            engine,
            registry,
            body_timeout: DEFAULT_BODY_TIMEOUT,
            observations: Mutex::new(Observations::default()),
            shutting_down: AtomicBool::new(false),
        }
    }

    /// Sets how long a request body may take to arrive, 30 seconds by
    /// default. Streamed responses have no such deadline.
    pub fn with_body_timeout(mut self, timeout: Duration) -> Self {
        self.body_timeout = timeout;
        self
    }

    pub(crate) fn body_timeout(&self) -> Duration {
        self.body_timeout
    }

    /// Starts shutting down: `/readyz` answers `503`, and requests that
    /// change state or start streams answer `engine-unavailable`, while
    /// other reads are still served. Draining streams and stopping the
    /// engine are the caller's.
    pub fn begin_shutdown(&self) {
        self.shutting_down.store(true, Ordering::SeqCst);
    }

    pub(crate) fn engine(&self) -> &E {
        &self.engine
    }

    pub(crate) fn registry(&self) -> &Registry {
        &self.registry
    }

    /// Fails with `engine-unavailable` unless the engine can serve requests
    /// that read or change its state.
    pub(crate) fn require_engine(&self) -> Result<(), Problem> {
        if self.shutting_down.load(Ordering::SeqCst) {
            return Err(Problem::engine_unavailable(
                "The application is shutting down.",
                None,
            ));
        }
        match self.engine.readiness() {
            Readiness::Ready => Ok(()),
            Readiness::Unavailable { retry_after } => Err(Problem::engine_unavailable(
                "The torrent engine is not ready.",
                retry_after,
            )),
        }
    }

    /// Returns the registered torrent `id` with what the engine reports
    /// about it now. Metadata the engine retrieved is validated and recorded;
    /// metadata that cannot be served fails the torrent. While the engine is
    /// unavailable, or does not hold the torrent yet, the last observation
    /// stands.
    pub(crate) async fn observe(&self, record: TorrentRecord) -> Result<TorrentView, Problem> {
        let mut record = record;
        match self.engine.status(&record.id).await {
            Ok(status) => {
                let mut metadata_failure = None;
                if record.metadata.is_none()
                    && let Some(reported) = status.metadata
                {
                    match Metadata::identify(&reported) {
                        Ok(metadata) => {
                            let metadata = Arc::new(metadata);
                            // A concurrent delete leaves nothing to record.
                            let _ = self
                                .registry
                                .record_metadata(record.id.as_str(), Arc::clone(&metadata));
                            record.metadata = self
                                .registry
                                .get(record.id.as_str())
                                .and_then(|current| current.metadata)
                                .or(Some(metadata));
                        }
                        Err(error) => {
                            metadata_failure = Some(Failure {
                                detail: error.to_string(),
                            });
                        }
                    }
                }
                // A torrent deleted meanwhile must not be observed again.
                // Deletion unregisters before it forgets, and this checks the
                // registry under the observations lock, so either the check
                // fails or the forgetting follows the update. The registry's
                // lock is only ever taken inside this one, never around it.
                let mut observations = self.observations();
                if self.registry.get(record.id.as_str()).is_some() {
                    let observation = observations.torrents.entry(record.id.clone()).or_default();
                    observation.engine_failure = status.failure;
                    if metadata_failure.is_some() {
                        observation.metadata_failure = metadata_failure;
                    }
                }
            }
            Err(EngineError::Unavailable { .. } | EngineError::UnknownTorrent) => {}
            Err(error) => return Err(error.into()),
        }
        let failure = self
            .observations()
            .torrents
            .get(&record.id)
            .and_then(|observation| {
                observation
                    .metadata_failure
                    .clone()
                    .or_else(|| observation.engine_failure.clone())
            });
        Ok(TorrentView { record, failure })
    }

    /// Clears a failure the server derived from retrieved metadata, when a
    /// paused torrent is started again.
    pub(crate) fn clear_metadata_failure(&self, id: &TorrentId) {
        if let Some(observation) = self.observations().torrents.get_mut(id) {
            observation.metadata_failure = None;
        }
    }

    /// Forgets a deleted torrent.
    pub(crate) fn forget(&self, id: &TorrentId) {
        self.observations().torrents.remove(id);
    }

    /// Returns the piece cache's statistics, or the last ones while the
    /// engine is unavailable.
    pub(crate) async fn cache_statistics(&self) -> Result<CacheStatistics, Problem> {
        match self.engine.cache_statistics().await {
            Ok(statistics) => {
                self.observations().cache_statistics = statistics;
                Ok(statistics)
            }
            Err(EngineError::Unavailable { .. }) => Ok(self.observations().cache_statistics),
            Err(error) => Err(error.into()),
        }
    }

    /// Locks the observations. Updates leave them consistent before any
    /// point that can panic, so a poisoned lock's state is still usable.
    fn observations(&self) -> MutexGuard<'_, Observations> {
        self.observations
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }
}
