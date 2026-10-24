// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! The application state the handlers share: the engine, the registry, the
//! store, and what the server last learned from the engine about each
//! torrent.
//!
//! The registry is the application's own record; the store makes it
//! durable. Every mutation takes the write lane, an async mutex, stores its
//! change, and only then changes the registry, so a failed write leaves the
//! records as they were and nothing registers in between. The lane is never
//! taken while a synchronous lock is held, and engine calls stay outside it
//! wherever their order allows.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, SystemTime};

use tokio::sync::MutexGuard as LaneGuard;
use tokio::sync::watch;
use torrplay_core::credentials::{AllowedHosts, AttemptLimiter, Credentials};
use torrplay_core::engine::{CacheStatistics, Engine, EngineError, Source};
use torrplay_core::id::{ProfileId, TorrentId, random_torrent_id_bytes};
use torrplay_core::lifecycle::{Failure, ObservedState, Readiness, StorageMode};
use torrplay_core::metadata::Metadata;
use torrplay_core::profiles::{DEFAULT_PROFILE_NAME, Profile, Viewing};
use torrplay_core::registry::{Registry, TorrentRecord};
use torrplay_dlna::description::Device;
use torrplay_store::{
    Store, StoreError, StoredCredentials, StoredDismissal, StoredProgress, StoredSettings,
    StoredTorrent,
};

use crate::problem::Problem;

/// How long a request body may take to arrive by default, as the contract
/// documents.
const DEFAULT_BODY_TIMEOUT: Duration = Duration::from_secs(30);

/// The source of the current time for credentials' expiry, which tests
/// replace.
pub type Clock = Arc<dyn Fn() -> SystemTime + Send + Sync>;

/// The state of a TorrPlay server over the engine `E`.
pub struct App<E> {
    engine: Arc<E>,
    registry: Registry,
    viewing: Viewing,
    store: Store,
    /// Serializes mutations; see the [module documentation](self).
    writes: tokio::sync::Mutex<()>,
    body_timeout: Duration,
    observations: Mutex<Observations>,
    /// The stored settings, changed under the write lane after the store.
    settings: Mutex<StoredSettings>,
    /// Set until [`App::restore`] has registered the stored torrents.
    restoring: AtomicBool,
    shutting_down: AtomicBool,
    credentials: Credentials,
    /// Serializes changes to the credentials and password checks, so one
    /// password is hashed at a time.
    credential_writes: tokio::sync::Mutex<()>,
    limiter: Mutex<AttemptLimiter>,
    allowed_hosts: AllowedHosts,
    clock: Clock,
    media_server: Option<crate::dlna::MediaServer>,
    /// Whether the DLNA media server is on, for the task that announces it.
    dlna_enabled: watch::Sender<bool>,
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
    /// Why the engine refused the stored torrent at startup; only deletion
    /// clears it.
    restore_failure: Option<Failure>,
    /// The target of the storage mode transition in progress.
    transition: Option<StorageMode>,
    /// The target of the last failed transition and why it failed, until
    /// the next change of storage mode.
    storage_failure: Option<(StorageMode, String)>,
}

/// A torrent as the API presents it: its record and its effective failure.
#[derive(Clone, Debug)]
pub(crate) struct TorrentView {
    pub(crate) record: TorrentRecord,
    pub(crate) failure: Option<Failure>,
    pub(crate) transition: Option<StorageMode>,
    pub(crate) storage_failure: Option<(StorageMode, String)>,
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
    pub fn new(engine: Arc<E>, store: Store) -> Self {
        Self::with_registry(engine, Registry::new(), store)
    }

    /// Creates a server over `engine` with `registry`, so tests can choose
    /// the torrent IDs it issues.
    pub fn with_registry(engine: Arc<E>, registry: Registry, store: Store) -> Self {
        Self {
            engine,
            registry,
            viewing: Viewing::default(),
            store,
            writes: tokio::sync::Mutex::new(()),
            body_timeout: DEFAULT_BODY_TIMEOUT,
            observations: Mutex::new(Observations::default()),
            settings: Mutex::new(StoredSettings::default()),
            restoring: AtomicBool::new(true),
            shutting_down: AtomicBool::new(false),
            credentials: Credentials::empty(),
            credential_writes: tokio::sync::Mutex::new(()),
            limiter: Mutex::new(AttemptLimiter::default()),
            allowed_hosts: AllowedHosts::default(),
            clock: Arc::new(SystemTime::now),
            media_server: None,
            dlna_enabled: watch::Sender::new(false),
        }
    }

    /// Serves the DLNA media server's resources as `device`, while the
    /// `dlna_enabled` setting is on.
    pub fn with_media_server(mut self, device: Device) -> Self {
        self.media_server = Some(crate::dlna::MediaServer::new(device));
        self
    }

    pub(crate) fn media_server(&self) -> Option<&crate::dlna::MediaServer> {
        self.media_server.as_ref()
    }

    /// Sends the DLNA media server's due events: initial ones, and the
    /// system update ID when the content tree changed. The task that
    /// announces the media server calls it every 2 seconds while it is on.
    pub async fn notify_media_server_subscribers(&self) {
        crate::dlna::notify(self).await;
    }

    /// Follows the `dlna_enabled` setting, for the task that announces the
    /// media server.
    pub fn dlna_enabled(&self) -> watch::Receiver<bool> {
        self.dlna_enabled.subscribe()
    }

    /// Runs with the password, signing key, and access tokens the store held
    /// at startup.
    pub fn with_credentials(mut self, stored: StoredCredentials) -> Self {
        self.credentials = Credentials::new(stored.password, stored.key, stored.tokens);
        self
    }

    /// Allows requests to name `hosts` beyond those always allowed.
    pub fn with_allowed_hosts(mut self, hosts: AllowedHosts) -> Self {
        self.allowed_hosts = hosts;
        self
    }

    /// Reads the time for credentials' expiry from `clock`.
    pub fn with_clock(mut self, clock: Clock) -> Self {
        self.clock = clock;
        self
    }

    /// Whether a password is set, so requests need credentials.
    pub fn authentication_enabled(&self) -> bool {
        self.credentials.is_enabled()
    }

    pub(crate) fn credentials(&self) -> &Credentials {
        &self.credentials
    }

    /// Takes the lane of credential changes and password checks.
    pub(crate) async fn credential_lane(&self) -> LaneGuard<'_, ()> {
        self.credential_writes.lock().await
    }

    pub(crate) fn limiter(&self) -> MutexGuard<'_, AttemptLimiter> {
        // The limiter's every update is a whole assignment.
        self.limiter.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub(crate) fn allowed_hosts(&self) -> &AllowedHosts {
        &self.allowed_hosts
    }

    /// The current time for credentials' expiry.
    pub(crate) fn now(&self) -> SystemTime {
        (self.clock)()
    }

    /// Runs with the settings the store held at startup, and gives them to
    /// the engine. Stored values outside the current bounds count as unset,
    /// with a warning, and stay stored until a change replaces them.
    pub fn with_settings(self, stored: StoredSettings) -> Self {
        let explicit = crate::settings::usable(stored.explicit);
        if explicit != stored.explicit {
            tracing::warn!("stored settings outside the current bounds take their defaults");
        }
        let stored = StoredSettings { explicit, ..stored };
        // The engine holds no torrent yet, so it refuses nothing.
        if let Err(error) = self.engine.apply_settings(&explicit.effective()) {
            tracing::error!(%error, "the engine did not apply the stored settings");
        }
        self.set_settings(stored);
        self
    }

    /// Runs with the profiles, progress, and dismissals the store held at
    /// startup.
    pub fn with_viewing(
        self,
        profiles: Vec<Profile>,
        progress: Vec<StoredProgress>,
        dismissals: Vec<StoredDismissal>,
    ) -> Self {
        for profile in profiles {
            self.viewing.set_profile(profile);
        }
        for stored in progress {
            self.viewing.set_progress(
                &stored.profile,
                &stored.torrent,
                &stored.file,
                stored.progress,
            );
        }
        for stored in dismissals {
            self.viewing
                .set_dismissal(&stored.profile, &stored.entry, stored.at);
        }
        self
    }

    /// The profiles, their progress, and their dismissals, once the stored
    /// torrents are restored, as [`registry`](Self::registry) waits for.
    pub(crate) fn viewing(&self) -> Result<&Viewing, Problem> {
        self.registry()?;
        Ok(&self.viewing)
    }

    /// Creates and stores the default profile if there is none, so clients
    /// that never choose a profile still have one.
    async fn ensure_profile(&self) {
        let _lane = self.writes.lock().await;
        if !self.viewing.is_empty() {
            return;
        }
        let id = match random_torrent_id_bytes() {
            Ok(bytes) => ProfileId::from_random_bytes(bytes),
            Err(error) => {
                tracing::error!(%error, "no random bytes for the default profile's ID");
                return;
            }
        };
        let profile = Profile {
            id,
            name: DEFAULT_PROFILE_NAME.to_owned(),
            created_at: std::time::SystemTime::now(),
            avatar: None,
        };
        match self.store.insert_profile(&profile).await {
            Ok(()) => self.viewing.set_profile(profile),
            Err(error) => tracing::error!(%error, "the default profile could not be stored"),
        }
    }

    /// The stored settings.
    pub fn settings(&self) -> StoredSettings {
        *self.settings.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The effective drain period, for shutting down.
    pub fn drain_period(&self) -> Duration {
        self.settings().explicit.effective().drain_period
    }

    pub(crate) fn set_settings(&self, stored: StoredSettings) {
        *self.settings.lock().unwrap_or_else(PoisonError::into_inner) = stored;
        let on = stored.explicit.effective().dlna_enabled;
        self.dlna_enabled
            .send_if_modified(|enabled| std::mem::replace(enabled, on) != on);
        if !on && let Some(server) = &self.media_server {
            // Turned off, the media server forgets its control points.
            server.subscriptions.clear();
        }
    }

    /// Registers the torrents the store held at startup, in creation order,
    /// and adds them to the engine, which must be ready. Until this returns,
    /// `/readyz` answers `503` and torrent operations `engine-unavailable`,
    /// since the records are incomplete. A torrent the engine refuses stays
    /// registered as failed, with the refusal as its failure.
    pub async fn restore(&self, torrents: Vec<StoredTorrent>) -> Result<(), EngineError> {
        for stored in torrents {
            let source = match (&stored.torrent_file, &stored.magnet_uri) {
                (Some(bytes), _) => Source::TorrentFile(bytes.clone()),
                (None, Some(uri)) => Source::MagnetUri(uri.clone()),
                // The store returns no torrent without either.
                (None, None) => continue,
            };
            let mut refusal = None;
            let metadata = match self.engine.inspect(&source).await {
                Ok(inspection) if inspection.info_hash != stored.info_hash => {
                    refusal = Some(
                        "The stored torrent file does not match the torrent's info hash."
                            .to_owned(),
                    );
                    None
                }
                Ok(inspection) => match inspection
                    .metadata
                    .map(|reported| Metadata::identify(&reported))
                {
                    Some(Ok(metadata)) => Some(Arc::new(metadata)),
                    Some(Err(error)) => {
                        refusal = Some(error.to_string());
                        None
                    }
                    None => None,
                },
                Err(EngineError::Rejected(rejection)) => {
                    refusal = Some(rejection.to_string());
                    None
                }
                Err(error) => return Err(error),
            };
            let desired_state = stored.configuration.desired_state();
            let record = TorrentRecord {
                id: stored.id.clone(),
                info_hash: stored.info_hash,
                created_at: stored.created_at,
                configuration: stored.configuration,
                storage_mode: stored.storage_mode,
                metadata,
                poster: stored.poster,
            };
            if self.registry.insert(record).is_err() {
                // The store's keys are unique; a second record is skipped.
                tracing::error!(id = %stored.id, "a stored torrent was registered twice");
                continue;
            }
            if refusal.is_none() {
                // The storage mode in effect: one that a crash kept a
                // transition from reaching is not in effect yet.
                match self
                    .engine
                    .add_in(&stored.id, &source, desired_state, stored.storage_mode)
                    .await
                {
                    Ok(()) => {}
                    Err(EngineError::Rejected(rejection)) => refusal = Some(rejection.to_string()),
                    Err(error @ EngineError::InsufficientStorage { .. }) => {
                        refusal = Some(error.to_string());
                    }
                    Err(error) => return Err(error),
                }
            }
            if let Some(detail) = refusal {
                tracing::warn!(id = %stored.id, %detail, "a stored torrent could not be restored");
                self.observations()
                    .torrents
                    .entry(stored.id)
                    .or_default()
                    .restore_failure = Some(Failure { detail });
            }
        }
        self.ensure_profile().await;
        self.restoring.store(false, Ordering::SeqCst);
        Ok(())
    }

    /// Closes the store after the server has stopped writing.
    pub async fn close_store(&self) -> Result<(), StoreError> {
        let _lane = self.writes.lock().await;
        self.store.close().await
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

    /// The registry, once the stored torrents are restored: until then,
    /// `engine-unavailable`.
    pub(crate) fn registry(&self) -> Result<&Registry, Problem> {
        if self.restoring.load(Ordering::SeqCst) {
            return Err(Problem::engine_unavailable(
                "The stored torrents are being restored.",
                None,
            ));
        }
        Ok(&self.registry)
    }

    /// The store the server writes through to.
    pub fn store(&self) -> &Store {
        &self.store
    }

    /// Takes the write lane; see the [module documentation](self).
    pub(crate) async fn write_lane(&self) -> LaneGuard<'_, ()> {
        self.writes.lock().await
    }

    /// Why the torrent could not be restored at startup, if it could not.
    pub(crate) fn restore_failure(&self, id: &TorrentId) -> Option<Failure> {
        self.observations()
            .torrents
            .get(id)
            .and_then(|observation| observation.restore_failure.clone())
    }

    /// Fails with `engine-unavailable` once shutdown began, for changes that
    /// need only the application's own records and the store.
    pub(crate) fn require_running(&self) -> Result<(), Problem> {
        if self.shutting_down.load(Ordering::SeqCst) {
            return Err(Problem::engine_unavailable(
                "The application is shutting down.",
                None,
            ));
        }
        Ok(())
    }

    /// Fails with `engine-unavailable` unless the engine can serve requests
    /// that read or change its state.
    pub(crate) fn require_engine(&self) -> Result<(), Problem> {
        self.require_running()?;
        if self.restoring.load(Ordering::SeqCst) {
            return Err(Problem::engine_unavailable(
                "The stored torrents are being restored.",
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
                            if self.record_metadata(&record.id, &metadata).await? {
                                record.metadata = self
                                    .registry
                                    .get(record.id.as_str())
                                    .and_then(|current| current.metadata)
                                    .or(Some(metadata));
                            }
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
        let observation = self
            .observations()
            .torrents
            .get(&record.id)
            .cloned()
            .unwrap_or_default();
        let failure = observation
            .restore_failure
            .or(observation.metadata_failure)
            .or(observation.engine_failure);
        Ok(TorrentView {
            record,
            failure,
            transition: observation.transition,
            storage_failure: observation.storage_failure,
        })
    }

    /// Stores the torrent file of metadata the engine retrieved, then records
    /// the metadata. Returns whether it was recorded: not while the engine
    /// has no torrent file to give, nor for a torrent deleted meanwhile, nor
    /// when the store fails, which a later observation retries rather than
    /// failing the read that observed it.
    async fn record_metadata(
        &self,
        id: &TorrentId,
        metadata: &Arc<Metadata>,
    ) -> Result<bool, Problem> {
        let torrent_file = match self.engine.torrent_file(id).await {
            Ok(Some(bytes)) => bytes,
            Ok(None) | Err(EngineError::Unavailable { .. } | EngineError::UnknownTorrent) => {
                return Ok(false);
            }
            Err(error) => return Err(error.into()),
        };
        let _lane = self.write_lane().await;
        if self
            .registry
            .get(id.as_str())
            .is_none_or(|current| current.metadata.is_some())
        {
            return Ok(false);
        }
        if let Err(error) = self.store.set_torrent_file(id, torrent_file).await {
            tracing::error!(%error, %id, "retrieved metadata could not be stored");
            return Ok(false);
        }
        Ok(self
            .registry
            .record_metadata(id.as_str(), Arc::clone(metadata))
            .unwrap_or(false))
    }

    /// Clears a failure the server derived from retrieved metadata, when a
    /// paused torrent is started again.
    pub(crate) fn clear_metadata_failure(&self, id: &TorrentId) {
        if let Some(observation) = self.observations().torrents.get_mut(id) {
            observation.metadata_failure = None;
        }
    }

    /// Whether torrent `id` has a storage mode transition in progress.
    pub(crate) fn in_transition(&self, id: &TorrentId) -> bool {
        self.observations()
            .torrents
            .get(id)
            .is_some_and(|observation| observation.transition.is_some())
    }

    /// Runs torrent `id`'s transition to `target` in the background, the
    /// configuration already stored, and records its outcome: the storage
    /// mode in effect when it completes; the configuration returned to the
    /// storage mode in effect, and `storage_failure`, when it fails. One the
    /// engine's shutdown interrupts changes nothing, so the next start runs
    /// it again.
    pub(crate) fn start_transition(self: &Arc<Self>, id: TorrentId, target: StorageMode) {
        {
            let mut observations = self.observations();
            let observation = observations.torrents.entry(id.clone()).or_default();
            observation.transition = Some(target);
            observation.storage_failure = None;
        }
        let app = Arc::clone(self);
        tokio::spawn(async move {
            let result = app.engine.change_storage_mode(&id, target).await;
            let _lane = app.write_lane().await;
            let failure = match result {
                Ok(()) => {
                    if let Err(error) = app.store.set_storage_mode(&id, target).await {
                        // The next start runs the transition again.
                        tracing::error!(%error, %id, "a completed transition could not be stored");
                    }
                    let _ = app.registry.set_storage_mode(id.as_str(), target);
                    None
                }
                // Shutting down fails a transition too, which is an
                // interruption the next start finishes, not a failure.
                Err(EngineError::TransitionFailed { detail })
                    if !app.shutting_down.load(Ordering::SeqCst) =>
                {
                    app.revert_storage_mode(&id).await;
                    Some((target, detail))
                }
                Err(error) => {
                    tracing::warn!(%error, %id, "a storage mode transition was interrupted");
                    None
                }
            };
            if let Some(observation) = app.observations().torrents.get_mut(&id) {
                observation.transition = None;
                observation.storage_failure = failure;
            }
        });
    }

    /// Runs again the transitions a crash or a shutdown interrupted: those of
    /// torrents whose configured storage mode is not the one in effect. Call
    /// it once the stored torrents are restored.
    pub fn resume_transitions(self: &Arc<Self>) {
        for record in self.registry.list() {
            let target = record.configuration.storage_mode();
            if target != record.storage_mode && self.restore_failure(&record.id).is_none() {
                tracing::info!(id = %record.id, target = target.as_str(), "resuming a storage mode transition");
                self.start_transition(record.id, target);
            }
        }
    }

    /// Records that this run shut down cleanly, once the engine has stopped
    /// and flushed disk torrents' data.
    pub async fn record_clean_shutdown(&self) -> Result<(), StoreError> {
        let _lane = self.writes.lock().await;
        self.store.record_clean_shutdown().await
    }

    /// Returns torrent `id`'s configured storage mode to the one in effect,
    /// after a failed transition, under the write lane.
    async fn revert_storage_mode(&self, id: &TorrentId) {
        let Some(record) = self.registry.get(id.as_str()) else {
            return;
        };
        let mut configuration = record.configuration;
        if !configuration.set_storage_mode(record.storage_mode) {
            return;
        }
        if let Err(error) = self.store.set_configuration(id, &configuration).await {
            // The next start runs the transition again.
            tracing::error!(%error, %id, "a failed transition's configuration could not be reverted");
            return;
        }
        let _ = self.registry.set_configuration(id.as_str(), configuration);
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

/// Maps a failed store write to `internal-error`, logging its cause.
pub(crate) fn store_failed(error: StoreError) -> Problem {
    tracing::error!(%error, "a change could not be stored");
    Problem::internal()
}
