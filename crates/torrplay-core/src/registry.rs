// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! The registered torrents: the application's own record of each torrent's
//! identity, configuration, and metadata.
//!
//! Every operation takes one short synchronous lock and never awaits, so
//! callers may use the registry from async code but must not hold any value
//! it returns as a guard; it returns snapshots. Checking the v1 info hash and
//! registering a new torrent happen under the same lock, so concurrent
//! creations of one info hash register exactly one torrent.
//!
//! A persisting caller splits registration in two: [`Registry::prepare`]
//! checks the info hash and draws an ID without registering anything, the
//! caller stores the record, and [`Registry::insert`] registers it. Callers
//! serialize their writes, so nothing registers the info hash in between;
//! `insert` still refuses a taken ID or info hash.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::SystemTime;

use crate::id::{
    InfoHashV1, RandomnessError, TORRENT_ID_RANDOM_BYTES, TorrentId, random_torrent_id_bytes,
};
use crate::image::ImageInfo;
use crate::library::Library;
use crate::lifecycle::{Configuration, DesiredState, EntityTag, Retention, StorageMode};
use crate::metadata::Metadata;

/// The number of torrent IDs drawn before registration gives up. A 128-bit
/// random ID collides with a registered one practically never, so reaching
/// this bound means the random source is broken.
const ID_DRAWS: usize = 8;

/// The source of torrent ID bytes.
type IdSource = Box<dyn FnMut() -> Result<[u8; TORRENT_ID_RANDOM_BYTES], RandomnessError> + Send>;

/// A snapshot of a registered torrent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TorrentRecord {
    /// The torrent's ID.
    pub id: TorrentId,
    /// The torrent's v1 info hash, unique among registered torrents.
    pub info_hash: InfoHashV1,
    /// When the torrent was created.
    pub created_at: SystemTime,
    /// The writable configuration.
    pub configuration: Configuration,
    /// The storage mode in effect, which the configuration's becomes when a
    /// transition to it completes.
    pub storage_mode: StorageMode,
    /// The metadata, once an upload supplied it or the server recorded it
    /// after retrieval.
    pub metadata: Option<Arc<Metadata>>,
    /// The poster, if one was uploaded.
    pub poster: Option<ImageInfo>,
}

/// The input that registers a torrent.
#[derive(Clone, Debug)]
pub struct NewTorrent {
    /// The v1 info hash from inspecting the source.
    pub info_hash: InfoHashV1,
    /// The validated metadata of an uploaded torrent file, or `None` for a
    /// magnet URI.
    pub metadata: Option<Arc<Metadata>>,
    /// The state the torrent starts in.
    pub desired_state: DesiredState,
    /// The storage mode the torrent uses.
    pub storage_mode: StorageMode,
    /// Whether the application keeps the torrent.
    pub retention: Retention,
    /// The library members, normalized.
    pub library: Library,
    /// The creation time.
    pub created_at: SystemTime,
}

/// The result of [`Registry::register`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Registration {
    /// A new torrent was registered.
    Created(TorrentRecord),
    /// A torrent with the same v1 info hash already exists. It is unchanged,
    /// except that it takes the request's metadata if it had none.
    Duplicate {
        /// The existing torrent's ID.
        existing: TorrentId,
        /// Whether the existing torrent took the request's metadata. The
        /// caller must then supply the torrent file to the engine, which
        /// ends metadata retrieval.
        metadata_supplied: bool,
    },
}

/// The result of [`Registry::prepare`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Prepared {
    /// No torrent has the info hash; this record, with a newly drawn ID, is
    /// not registered yet.
    New(TorrentRecord),
    /// A torrent with the same v1 info hash already exists, unchanged.
    Duplicate {
        /// The existing torrent's ID.
        existing: TorrentId,
        /// Whether the existing torrent has no metadata and the new one
        /// brings it, so the caller should record it.
        supplies_metadata: bool,
    },
}

/// A record [`Registry::insert`] refused: a registered torrent has its ID or
/// info hash.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("a registered torrent has this ID or info hash")]
pub struct AlreadyRegistered;

/// A registration that could not complete.
#[derive(Debug, thiserror::Error)]
pub enum RegistryError {
    /// The random source failed.
    #[error(transparent)]
    Randomness(#[from] RandomnessError),
    /// Every drawn ID belonged to a registered torrent.
    #[error("no unused torrent ID was drawn in {ID_DRAWS} attempts")]
    NoUnusedId,
}

/// No registered torrent has the ID.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("no registered torrent has this ID")]
pub struct UnknownTorrent;

/// A configuration update that could not apply.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ConfigurationError {
    /// No registered torrent has the ID.
    #[error("no registered torrent has this ID")]
    NotFound,
    /// The expected entity tag is not the current one.
    #[error("the configuration's entity tag is {current}")]
    PreconditionFailed {
        /// The current entity tag.
        current: EntityTag,
    },
}

/// The result of a configuration update.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfigurationUpdate {
    /// The configuration after the update.
    pub configuration: Configuration,
    /// Whether a writable member changed.
    pub changed: bool,
}

/// The registered torrents.
pub struct Registry {
    state: Mutex<State>,
}

struct State {
    torrents: HashMap<TorrentId, Entry>,
    creation_order: BTreeMap<u64, TorrentId>,
    by_info_hash: HashMap<InfoHashV1, TorrentId>,
    next_sequence: u64,
    id_source: IdSource,
}

struct Entry {
    record: TorrentRecord,
    sequence: u64,
}

impl Default for Registry {
    fn default() -> Self {
        Self::new()
    }
}

impl Registry {
    /// Creates an empty registry that draws IDs from the operating system's
    /// cryptographically secure random number generator.
    pub fn new() -> Self {
        Self::with_id_source(random_torrent_id_bytes)
    }

    /// Creates an empty registry that draws ID bytes from `id_source`.
    pub fn with_id_source(
        id_source: impl FnMut() -> Result<[u8; TORRENT_ID_RANDOM_BYTES], RandomnessError>
        + Send
        + 'static,
    ) -> Self {
        Self {
            state: Mutex::new(State {
                torrents: HashMap::new(),
                creation_order: BTreeMap::new(),
                by_info_hash: HashMap::new(),
                next_sequence: 0,
                id_source: Box::new(id_source),
            }),
        }
    }

    /// Checks a new torrent's info hash and draws an ID for it, without
    /// registering anything; see the [module documentation](self).
    pub fn prepare(&self, new: NewTorrent) -> Result<Prepared, RegistryError> {
        let mut state = self.lock();
        if let Some(existing) = state.by_info_hash.get(&new.info_hash).cloned() {
            let supplies_metadata = new.metadata.is_some()
                && state
                    .torrents
                    .get(&existing)
                    .is_some_and(|entry| entry.record.metadata.is_none());
            return Ok(Prepared::Duplicate {
                existing,
                supplies_metadata,
            });
        }
        let id = state.draw_unused_id()?;
        Ok(Prepared::New(TorrentRecord {
            id,
            info_hash: new.info_hash,
            created_at: new.created_at,
            configuration: Configuration::new(new.desired_state)
                .with_storage_mode(new.storage_mode)
                .with_retention(new.retention)
                .with_library(new.library),
            storage_mode: new.storage_mode,
            metadata: new.metadata,
            poster: None,
        }))
    }

    /// Registers a prepared or restored record, after the torrents
    /// registered before it in creation order.
    pub fn insert(&self, record: TorrentRecord) -> Result<(), AlreadyRegistered> {
        let mut state = self.lock();
        if state.torrents.contains_key(&record.id)
            || state.by_info_hash.contains_key(&record.info_hash)
        {
            return Err(AlreadyRegistered);
        }
        state.insert(record);
        Ok(())
    }

    /// Registers a torrent, or reports the registered torrent with the same
    /// v1 info hash. A duplicate changes nothing, except that an existing
    /// torrent without metadata takes `new.metadata` if it is present.
    pub fn register(&self, new: NewTorrent) -> Result<Registration, RegistryError> {
        let mut state = self.lock();
        if let Some(existing) = state.by_info_hash.get(&new.info_hash).cloned() {
            let metadata_supplied = match (state.torrents.get_mut(&existing), new.metadata) {
                (Some(entry), Some(metadata)) if entry.record.metadata.is_none() => {
                    entry.record.metadata = Some(metadata);
                    true
                }
                _ => false,
            };
            return Ok(Registration::Duplicate {
                existing,
                metadata_supplied,
            });
        }
        let id = state.draw_unused_id()?;
        let record = TorrentRecord {
            id,
            info_hash: new.info_hash,
            created_at: new.created_at,
            configuration: Configuration::new(new.desired_state)
                .with_storage_mode(new.storage_mode)
                .with_retention(new.retention)
                .with_library(new.library),
            storage_mode: new.storage_mode,
            metadata: new.metadata,
            poster: None,
        };
        state.insert(record.clone());
        Ok(Registration::Created(record))
    }

    /// Returns the torrent with the given ID.
    pub fn get(&self, id: &str) -> Option<TorrentRecord> {
        self.lock()
            .torrents
            .get(id)
            .map(|entry| entry.record.clone())
    }

    /// Returns every registered torrent in creation order.
    pub fn list(&self) -> Vec<TorrentRecord> {
        let state = self.lock();
        state
            .creation_order
            .values()
            .filter_map(|id| state.torrents.get(id))
            .map(|entry| entry.record.clone())
            .collect()
    }

    /// Returns the number of registered torrents.
    pub fn len(&self) -> usize {
        self.lock().torrents.len()
    }

    /// Returns whether no torrent is registered.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Unregisters a torrent and returns its last record. Its info hash may
    /// be registered anew, under a new random ID.
    pub fn remove(&self, id: &str) -> Option<TorrentRecord> {
        let mut state = self.lock();
        let entry = state.torrents.remove(id)?;
        state.creation_order.remove(&entry.sequence);
        state.by_info_hash.remove(&entry.record.info_hash);
        Some(entry.record)
    }

    /// Records metadata the engine retrieved and returns whether the torrent
    /// had none before. Existing metadata is kept: metadata matching the
    /// info hash is the same whichever source supplied it.
    pub fn record_metadata(
        &self,
        id: &str,
        metadata: Arc<Metadata>,
    ) -> Result<bool, UnknownTorrent> {
        let mut state = self.lock();
        let entry = state.torrents.get_mut(id).ok_or(UnknownTorrent)?;
        if entry.record.metadata.is_some() {
            return Ok(false);
        }
        entry.record.metadata = Some(metadata);
        Ok(true)
    }

    /// Replaces a torrent's configuration with one its caller stored.
    pub fn set_configuration(
        &self,
        id: &str,
        configuration: Configuration,
    ) -> Result<(), UnknownTorrent> {
        let mut state = self.lock();
        let entry = state.torrents.get_mut(id).ok_or(UnknownTorrent)?;
        entry.record.configuration = configuration;
        Ok(())
    }

    /// Replaces torrent `id`'s poster with one its caller stored, or removes
    /// it.
    pub fn set_poster(&self, id: &str, poster: Option<ImageInfo>) -> Result<(), UnknownTorrent> {
        let mut state = self.lock();
        let entry = state.torrents.get_mut(id).ok_or(UnknownTorrent)?;
        entry.record.poster = poster;
        Ok(())
    }

    /// Records the storage mode in effect for torrent `id`.
    pub fn set_storage_mode(
        &self,
        id: &str,
        storage_mode: StorageMode,
    ) -> Result<(), UnknownTorrent> {
        let mut state = self.lock();
        let entry = state.torrents.get_mut(id).ok_or(UnknownTorrent)?;
        entry.record.storage_mode = storage_mode;
        Ok(())
    }

    /// Locks the state. Every update leaves the state consistent before it
    /// can panic, so a poisoned lock's state is still usable.
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl State {
    /// Registers a record whose ID and info hash are free.
    fn insert(&mut self, record: TorrentRecord) {
        let sequence = self.next_sequence;
        self.next_sequence += 1;
        self.creation_order.insert(sequence, record.id.clone());
        self.by_info_hash
            .insert(record.info_hash, record.id.clone());
        self.torrents
            .insert(record.id.clone(), Entry { record, sequence });
    }

    fn draw_unused_id(&mut self) -> Result<TorrentId, RegistryError> {
        for _ in 0..ID_DRAWS {
            let id = TorrentId::from_random_bytes((self.id_source)()?);
            // Checked against registered torrents only, as ADR 4 says:
            // 128 random bits make reissuing a deleted torrent's ID as
            // unlikely as any collision, and remembering every ID ever
            // issued would grow without bound.
            if !self.torrents.contains_key(&id) {
                return Ok(id);
            }
        }
        Err(RegistryError::NoUnusedId)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Barrier;
    use std::thread;
    use std::time::Duration;

    use super::*;
    use crate::metadata::{ReportedFile, ReportedMetadata};

    fn info_hash(n: u8) -> InfoHashV1 {
        InfoHashV1::from_bytes([n; 20])
    }

    fn new_torrent(n: u8, metadata: Option<Arc<Metadata>>) -> NewTorrent {
        NewTorrent {
            info_hash: info_hash(n),
            metadata,
            desired_state: DesiredState::Started,
            storage_mode: StorageMode::Memory,
            retention: Retention::Persisted,
            library: Default::default(),
            created_at: SystemTime::UNIX_EPOCH + Duration::from_secs(u64::from(n)),
        }
    }

    fn metadata() -> Arc<Metadata> {
        let reported = ReportedMetadata {
            name: b"movie.mkv".to_vec(),
            files: vec![ReportedFile {
                path: vec![b"movie.mkv".to_vec()],
                size_bytes: 10,
                padding: false,
            }],
        };
        Arc::new(Metadata::identify(&reported).unwrap())
    }

    fn created(registration: Registration) -> TorrentRecord {
        match registration {
            Registration::Created(record) => record,
            other => panic!("expected a new torrent, got {other:?}"),
        }
    }

    /// An ID source that yields the given byte patterns in order.
    fn scripted_ids(
        patterns: Vec<u8>,
    ) -> impl FnMut() -> Result<[u8; 16], RandomnessError> + Send + 'static {
        let mut patterns = patterns.into_iter();
        move || Ok([patterns.next().unwrap_or(0); 16])
    }

    #[test]
    fn lists_torrents_in_creation_order() {
        let registry = Registry::new();
        let ids: Vec<_> = (1..=5)
            .map(|n| created(registry.register(new_torrent(n, None)).unwrap()).id)
            .collect();
        registry.remove(ids[1].as_str()).unwrap();
        let listed: Vec<_> = registry
            .list()
            .into_iter()
            .map(|record| record.id)
            .collect();
        assert_eq!(
            listed,
            [
                ids[0].clone(),
                ids[2].clone(),
                ids[3].clone(),
                ids[4].clone()
            ]
        );
        assert_eq!(registry.len(), 4);
    }

    #[test]
    fn a_duplicate_info_hash_returns_the_existing_torrent_unchanged() {
        let registry = Registry::new();
        let first = created(registry.register(new_torrent(1, Some(metadata()))).unwrap());
        let mut again = new_torrent(1, None);
        again.desired_state = DesiredState::Paused;
        assert_eq!(
            registry.register(again).unwrap(),
            Registration::Duplicate {
                existing: first.id.clone(),
                metadata_supplied: false
            }
        );
        assert_eq!(registry.get(first.id.as_str()), Some(first));
    }

    #[test]
    fn a_duplicate_upload_supplies_missing_metadata_once() {
        let registry = Registry::new();
        let magnet = created(registry.register(new_torrent(1, None)).unwrap());
        let upload = || registry.register(new_torrent(1, Some(metadata()))).unwrap();
        assert_eq!(
            upload(),
            Registration::Duplicate {
                existing: magnet.id.clone(),
                metadata_supplied: true
            }
        );
        assert_eq!(
            upload(),
            Registration::Duplicate {
                existing: magnet.id.clone(),
                metadata_supplied: false
            },
            "a repeated upload finds the metadata already present"
        );
        let record = registry.get(magnet.id.as_str()).unwrap();
        assert_eq!(record.metadata, Some(metadata()));
        assert_eq!(record.configuration, magnet.configuration);
    }

    #[test]
    fn a_duplicate_magnet_supplies_nothing() {
        let registry = Registry::new();
        let magnet = created(registry.register(new_torrent(1, None)).unwrap());
        assert_eq!(
            registry.register(new_torrent(1, None)).unwrap(),
            Registration::Duplicate {
                existing: magnet.id.clone(),
                metadata_supplied: false
            }
        );
        assert_eq!(registry.get(magnet.id.as_str()).unwrap().metadata, None);
    }

    #[test]
    fn concurrent_creations_of_one_info_hash_register_one_torrent() {
        let registry = Registry::new();
        let threads = 16;
        let barrier = Barrier::new(threads);
        let results: Vec<Registration> = thread::scope(|scope| {
            let handles: Vec<_> = (0..threads)
                .map(|_| {
                    scope.spawn(|| {
                        barrier.wait();
                        registry.register(new_torrent(9, None)).unwrap()
                    })
                })
                .collect();
            handles
                .into_iter()
                .map(|handle| handle.join().unwrap())
                .collect()
        });
        let created: Vec<_> = results
            .iter()
            .filter_map(|result| match result {
                Registration::Created(record) => Some(record.id.clone()),
                Registration::Duplicate { .. } => None,
            })
            .collect();
        assert_eq!(created.len(), 1, "exactly one creation succeeds");
        for result in &results {
            if let Registration::Duplicate { existing, .. } = result {
                assert_eq!(existing, &created[0]);
            }
        }
        assert_eq!(registry.len(), 1);
    }

    #[test]
    fn deleted_torrents_are_gone_and_their_info_hash_gets_a_new_id() {
        let registry = Registry::new();
        let first = created(registry.register(new_torrent(1, None)).unwrap());
        assert_eq!(registry.remove(first.id.as_str()), Some(first.clone()));
        assert_eq!(registry.get(first.id.as_str()), None);
        assert_eq!(
            registry.remove(first.id.as_str()),
            None,
            "a repeated delete"
        );
        let second = created(registry.register(new_torrent(1, None)).unwrap());
        assert_ne!(second.id, first.id);
        assert!(registry.list().iter().all(|record| record.id != first.id));
    }

    #[test]
    fn an_id_colliding_with_a_registered_torrent_is_drawn_again() {
        let registry = Registry::with_id_source(scripted_ids(vec![1, 1, 2]));
        let first = created(registry.register(new_torrent(1, None)).unwrap());
        let second = created(registry.register(new_torrent(2, None)).unwrap());
        assert_eq!(first.id, TorrentId::from_random_bytes([1; 16]));
        assert_eq!(second.id, TorrentId::from_random_bytes([2; 16]));
    }

    #[test]
    fn a_stuck_id_source_fails_registration_after_bounded_draws() {
        let registry = Registry::with_id_source(|| Ok([5; 16]));
        created(registry.register(new_torrent(1, None)).unwrap());
        assert!(matches!(
            registry.register(new_torrent(2, None)),
            Err(RegistryError::NoUnusedId)
        ));
        assert_eq!(registry.len(), 1, "the failed registration left no trace");
    }

    #[test]
    fn a_prepared_torrent_registers_only_when_inserted() {
        let registry = Registry::new();
        let Prepared::New(record) = registry.prepare(new_torrent(1, None)).unwrap() else {
            panic!("a new info hash");
        };
        assert!(registry.is_empty(), "preparing registers nothing");
        assert!(matches!(
            registry.prepare(new_torrent(1, None)).unwrap(),
            Prepared::New(_)
        ));
        registry.insert(record.clone()).unwrap();
        assert_eq!(registry.get(record.id.as_str()), Some(record.clone()));
        assert_eq!(registry.insert(record.clone()), Err(AlreadyRegistered));
        assert_eq!(
            registry.prepare(new_torrent(1, Some(metadata()))).unwrap(),
            Prepared::Duplicate {
                existing: record.id.clone(),
                supplies_metadata: true,
            }
        );
        assert!(
            registry.get(record.id.as_str()).unwrap().metadata.is_none(),
            "preparing a duplicate changes nothing"
        );
    }

    #[test]
    fn inserted_records_keep_their_configuration_and_order() {
        let registry = Registry::new();
        let mut paused = Configuration::new(DesiredState::Started);
        paused.set_desired_state(DesiredState::Paused);
        let records: Vec<TorrentRecord> = (1..=3)
            .map(|n| TorrentRecord {
                id: TorrentId::from_random_bytes([n; TORRENT_ID_RANDOM_BYTES]),
                info_hash: info_hash(n),
                created_at: SystemTime::UNIX_EPOCH + Duration::from_secs(u64::from(n)),
                configuration: paused.clone(),
                storage_mode: StorageMode::Memory,
                metadata: None,
                poster: None,
            })
            .collect();
        for record in &records {
            registry.insert(record.clone()).unwrap();
        }
        assert_eq!(registry.list(), records);
        let mut started = paused.clone();
        started.set_desired_state(DesiredState::Started);
        registry
            .set_configuration(records[0].id.as_str(), started.clone())
            .unwrap();
        assert_eq!(
            registry.get(records[0].id.as_str()).unwrap().configuration,
            started
        );
        assert_eq!(
            registry.set_configuration("trr_01", started),
            Err(UnknownTorrent)
        );
    }

    #[test]
    fn retrieved_metadata_is_recorded_once() {
        let registry = Registry::new();
        let record = created(registry.register(new_torrent(1, None)).unwrap());
        assert_eq!(
            registry.record_metadata(record.id.as_str(), metadata()),
            Ok(true)
        );
        assert_eq!(
            registry.record_metadata(record.id.as_str(), metadata()),
            Ok(false)
        );
        assert_eq!(
            registry.record_metadata("trr_01", metadata()),
            Err(UnknownTorrent)
        );
    }
}
