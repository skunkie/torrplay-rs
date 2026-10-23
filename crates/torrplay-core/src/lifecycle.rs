// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! A torrent's state dimensions and engine readiness, as
//! `docs/lifecycle.md` defines them.

use std::fmt;
use std::time::Duration;

/// The client-writable target state of a torrent.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DesiredState {
    /// The torrent connects to the swarm, retrieves metadata, and fetches
    /// the pieces its readers need.
    Started,
    /// The torrent has no peer connections, readers, or streams.
    Paused,
}

impl DesiredState {
    /// Returns the contract's enum value.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Started => "started",
            Self::Paused => "paused",
        }
    }

    /// Parses the contract's enum value.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "started" => Some(Self::Started),
            "paused" => Some(Self::Paused),
            _ => None,
        }
    }
}

/// Where a torrent keeps piece data.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum StorageMode {
    /// The global piece cache, which evicts pieces; the torrent never
    /// uploads.
    #[default]
    Memory,
    /// Files in the torrent's directory in the download directory; the
    /// torrent uploads.
    Disk,
}

impl StorageMode {
    /// Returns the contract's enum value.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Memory => "memory",
            Self::Disk => "disk",
        }
    }

    /// Parses the contract's enum value.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "memory" => Some(Self::Memory),
            "disk" => Some(Self::Disk),
            _ => None,
        }
    }
}

/// The read-only state the engine reports for a torrent, derived from the
/// other state dimensions by [`ObservedState::derive`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ObservedState {
    /// Started without metadata.
    RetrievingMetadata,
    /// Started with metadata, connected to the swarm.
    Downloading,
    /// Paused, without a failure.
    Paused,
    /// Failed; the torrent's failure describes why.
    Failed,
}

impl ObservedState {
    /// Every observed state, in the contract's order.
    pub const ALL: [Self; 4] = [
        Self::RetrievingMetadata,
        Self::Downloading,
        Self::Paused,
        Self::Failed,
    ];

    /// Derives the observed state from the lifecycle table: a failure wins,
    /// then a paused desired state, then the availability of metadata.
    pub fn derive(
        desired_state: DesiredState,
        metadata_available: bool,
        failure: Option<&Failure>,
    ) -> Self {
        match (failure, desired_state, metadata_available) {
            (Some(_), _, _) => Self::Failed,
            (None, DesiredState::Paused, _) => Self::Paused,
            (None, DesiredState::Started, false) => Self::RetrievingMetadata,
            (None, DesiredState::Started, true) => Self::Downloading,
        }
    }

    /// Returns the contract's enum value.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::RetrievingMetadata => "retrieving_metadata",
            Self::Downloading => "downloading",
            Self::Paused => "paused",
            Self::Failed => "failed",
        }
    }

    /// Parses the contract's enum value.
    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|state| state.as_str() == value)
    }
}

/// Why a torrent failed, safe to show to clients.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Failure {
    /// An explanation of the failure, without credentials, private tracker
    /// parameters, or internal paths.
    pub detail: String,
}

/// A strong entity tag. Its text excludes the surrounding quotes, which
/// [`Display`](fmt::Display) adds.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct EntityTag(String);

impl EntityTag {
    /// Wraps an opaque tag. The tag must consist of characters RFC 9110
    /// allows in an entity tag, excluding `"`.
    pub fn new(opaque: impl Into<String>) -> Self {
        Self(opaque.into())
    }

    /// Returns the tag without quotes.
    pub fn opaque(&self) -> &str {
        &self.0
    }
}

/// Formats the tag as a header value, with quotes and without `W/`.
impl fmt::Display for EntityTag {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "\"{}\"", self.0)
    }
}

/// A torrent's writable configuration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Configuration {
    desired_state: DesiredState,
    storage_mode: StorageMode,
    version: u64,
}

impl Configuration {
    /// Creates the configuration of a new torrent in memory storage.
    pub fn new(desired_state: DesiredState) -> Self {
        Self {
            desired_state,
            storage_mode: StorageMode::Memory,
            version: 1,
        }
    }

    /// The configuration with `storage_mode` instead, for a new torrent.
    pub fn with_storage_mode(self, storage_mode: StorageMode) -> Self {
        Self {
            storage_mode,
            ..self
        }
    }

    /// Rebuilds a stored configuration: `None` for version 0, which no
    /// configuration has.
    pub fn from_stored(
        desired_state: DesiredState,
        storage_mode: StorageMode,
        version: u64,
    ) -> Option<Self> {
        (version >= 1).then_some(Self {
            desired_state,
            storage_mode,
            version,
        })
    }

    /// Returns the desired state.
    pub fn desired_state(&self) -> DesiredState {
        self.desired_state
    }

    /// Returns the storage mode the client asked for, which a transition
    /// reaches.
    pub fn storage_mode(&self) -> StorageMode {
        self.storage_mode
    }

    /// Returns the version the entity tag derives from, which starts at 1 and
    /// grows with every change of a writable member.
    pub fn version(&self) -> u64 {
        self.version
    }

    /// Returns the strong entity tag, which changes only when a writable
    /// member changes.
    pub fn etag(&self) -> EntityTag {
        EntityTag(self.version.to_string())
    }

    /// Sets the desired state and returns whether it changed. Setting the
    /// current value again keeps the entity tag.
    pub fn set_desired_state(&mut self, desired_state: DesiredState) -> bool {
        if self.desired_state == desired_state {
            return false;
        }
        self.desired_state = desired_state;
        // 2^64 changes cannot happen, so wrapping never repeats a tag.
        self.version = self.version.wrapping_add(1);
        true
    }
}

/// Whether the engine can serve requests that read or change its state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Readiness {
    /// The engine is ready.
    Ready,
    /// The engine is starting, stopped unexpectedly, or shutting down.
    Unavailable {
        /// When a retry may succeed, sent as `Retry-After` if known.
        retry_after: Option<Duration>,
    },
}

impl Readiness {
    /// Returns whether the engine is ready.
    pub fn is_ready(self) -> bool {
        self == Self::Ready
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn observed_state_follows_the_lifecycle_table() {
        let failure = Failure {
            detail: "A piece could not be written to storage.".into(),
        };
        let cases = [
            (
                DesiredState::Started,
                false,
                Some(&failure),
                ObservedState::Failed,
            ),
            (
                DesiredState::Started,
                true,
                Some(&failure),
                ObservedState::Failed,
            ),
            (
                DesiredState::Paused,
                false,
                Some(&failure),
                ObservedState::Failed,
            ),
            (
                DesiredState::Paused,
                true,
                Some(&failure),
                ObservedState::Failed,
            ),
            (DesiredState::Paused, false, None, ObservedState::Paused),
            (DesiredState::Paused, true, None, ObservedState::Paused),
            (
                DesiredState::Started,
                false,
                None,
                ObservedState::RetrievingMetadata,
            ),
            (
                DesiredState::Started,
                true,
                None,
                ObservedState::Downloading,
            ),
        ];
        for (desired, metadata, failure, expected) in cases {
            assert_eq!(
                ObservedState::derive(desired, metadata, failure),
                expected,
                "{desired:?}, metadata {metadata}, failure {}",
                failure.is_some()
            );
        }
    }

    #[test]
    fn a_stored_configuration_keeps_its_entity_tag() {
        let mut configuration = Configuration::new(DesiredState::Started);
        configuration.set_desired_state(DesiredState::Paused);
        let stored = Configuration::from_stored(
            DesiredState::Paused,
            StorageMode::Memory,
            configuration.version(),
        );
        assert_eq!(
            stored.as_ref().map(Configuration::etag),
            Some(configuration.etag())
        );
        assert_eq!(
            Configuration::from_stored(DesiredState::Paused, StorageMode::Disk, 0),
            None
        );
    }

    #[test]
    fn setting_the_same_desired_state_keeps_the_etag() {
        let mut configuration = Configuration::new(DesiredState::Started);
        let initial = configuration.etag();
        assert!(!configuration.set_desired_state(DesiredState::Started));
        assert_eq!(configuration.etag(), initial);

        assert!(configuration.set_desired_state(DesiredState::Paused));
        let paused = configuration.etag();
        assert_ne!(paused, initial);
        assert!(!configuration.set_desired_state(DesiredState::Paused));
        assert_eq!(configuration.etag(), paused);

        assert!(configuration.set_desired_state(DesiredState::Started));
        assert_ne!(configuration.etag(), initial, "tags are never reused");
        assert_ne!(configuration.etag(), paused);
    }

    #[test]
    fn entity_tags_format_as_strong_header_values() {
        let tag = Configuration::new(DesiredState::Paused).etag();
        assert_eq!(tag.to_string(), "\"1\"");
        assert_eq!(tag, EntityTag::new("1"));
    }

    #[test]
    fn enum_values_round_trip() {
        for state in ObservedState::ALL {
            assert_eq!(ObservedState::parse(state.as_str()), Some(state));
        }
        for state in [DesiredState::Started, DesiredState::Paused] {
            assert_eq!(DesiredState::parse(state.as_str()), Some(state));
        }
        assert_eq!(ObservedState::parse("seeding"), None);
        assert_eq!(DesiredState::parse("Started"), None);
    }
}
