// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! The application's settings: the explicitly set values the store keeps,
//! the effective settings they give with the current defaults, their
//! bounds, and merge patches of them.
//!
//! Only explicit values are stored, so a value set to `0` stays distinct
//! from one never set, and a default a later release adds or changes applies
//! wherever the value was never set. [`Explicit::effective`] merges them
//! with [`Settings::DEFAULT`].

use std::time::Duration;

use crate::lifecycle::EntityTag;

/// One setting's name, bounds, and default.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Bounds {
    /// The member name in the contract.
    pub name: &'static str,
    /// The least value.
    pub minimum: u32,
    /// The greatest value.
    pub maximum: u32,
    /// The value while unset.
    pub default: u32,
}

/// The piece wait bound, in seconds.
pub const PIECE_WAIT_SECONDS: Bounds = Bounds {
    name: "piece_wait_seconds",
    minimum: 1,
    maximum: 600,
    default: 60,
};
/// The idle timeout, in seconds.
pub const IDLE_TIMEOUT_SECONDS: Bounds = Bounds {
    name: "idle_timeout_seconds",
    minimum: 0,
    maximum: 3600,
    default: 30,
};
/// The idle reader limit per torrent.
pub const IDLE_READERS_PER_TORRENT: Bounds = Bounds {
    name: "idle_readers_per_torrent",
    minimum: 0,
    maximum: 64,
    default: 4,
};
/// The idle reader limit across torrents.
pub const IDLE_READERS_TOTAL: Bounds = Bounds {
    name: "idle_readers_total",
    minimum: 0,
    maximum: 256,
    default: 16,
};
/// The drain period, in seconds, within the 30-second shutdown limit.
pub const DRAIN_PERIOD_SECONDS: Bounds = Bounds {
    name: "drain_period_seconds",
    minimum: 0,
    maximum: 25,
    default: 10,
};
/// The peer limit per torrent.
pub const PEER_LIMIT: Bounds = Bounds {
    name: "peer_limit",
    minimum: 1,
    maximum: 500,
    default: 50,
};

/// Every setting, in the order representations list them.
pub const ALL: [Bounds; 6] = [
    PIECE_WAIT_SECONDS,
    IDLE_TIMEOUT_SECONDS,
    IDLE_READERS_PER_TORRENT,
    IDLE_READERS_TOTAL,
    DRAIN_PERIOD_SECONDS,
    PEER_LIMIT,
];

/// The effective settings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Settings {
    /// How long a stream waits for a missing piece, and a peer may hold
    /// requests without delivering.
    pub piece_wait: Duration,
    /// How long a reader stays idle after its stream ends.
    pub idle_timeout: Duration,
    /// The most idle readers one torrent keeps.
    pub idle_readers_per_torrent: usize,
    /// The most idle readers all torrents keep together.
    pub idle_readers_total: usize,
    /// How long streams may continue after a termination signal.
    pub drain_period: Duration,
    /// The most peers one torrent connects to at a time.
    pub peer_limit: usize,
}

impl Settings {
    /// Every setting at its default.
    pub const DEFAULT: Self = Self {
        piece_wait: Duration::from_secs(PIECE_WAIT_SECONDS.default as u64),
        idle_timeout: Duration::from_secs(IDLE_TIMEOUT_SECONDS.default as u64),
        idle_readers_per_torrent: IDLE_READERS_PER_TORRENT.default as usize,
        idle_readers_total: IDLE_READERS_TOTAL.default as usize,
        drain_period: Duration::from_secs(DRAIN_PERIOD_SECONDS.default as u64),
        peer_limit: PEER_LIMIT.default as usize,
    };
}

/// The explicitly set values: `None` is unset.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Explicit {
    /// [`PIECE_WAIT_SECONDS`].
    pub piece_wait_seconds: Option<u32>,
    /// [`IDLE_TIMEOUT_SECONDS`].
    pub idle_timeout_seconds: Option<u32>,
    /// [`IDLE_READERS_PER_TORRENT`].
    pub idle_readers_per_torrent: Option<u32>,
    /// [`IDLE_READERS_TOTAL`].
    pub idle_readers_total: Option<u32>,
    /// [`DRAIN_PERIOD_SECONDS`].
    pub drain_period_seconds: Option<u32>,
    /// [`PEER_LIMIT`].
    pub peer_limit: Option<u32>,
}

/// A value a setting cannot take, with the setting's name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InvalidSetting {
    /// The member name.
    pub name: &'static str,
    /// What the value must be, phrased for an error entry.
    pub detail: String,
}

impl Explicit {
    /// The slot of the setting named `name`, if there is one.
    pub fn slot(&mut self, name: &str) -> Option<&mut Option<u32>> {
        Some(match name {
            "piece_wait_seconds" => &mut self.piece_wait_seconds,
            "idle_timeout_seconds" => &mut self.idle_timeout_seconds,
            "idle_readers_per_torrent" => &mut self.idle_readers_per_torrent,
            "idle_readers_total" => &mut self.idle_readers_total,
            "drain_period_seconds" => &mut self.drain_period_seconds,
            "peer_limit" => &mut self.peer_limit,
            _ => return None,
        })
    }

    /// Each setting's value, explicit or default, in [`ALL`]'s order.
    pub fn values(&self) -> [u32; 6] {
        let value = |explicit: Option<u32>, bounds: Bounds| explicit.unwrap_or(bounds.default);
        [
            value(self.piece_wait_seconds, PIECE_WAIT_SECONDS),
            value(self.idle_timeout_seconds, IDLE_TIMEOUT_SECONDS),
            value(self.idle_readers_per_torrent, IDLE_READERS_PER_TORRENT),
            value(self.idle_readers_total, IDLE_READERS_TOTAL),
            value(self.drain_period_seconds, DRAIN_PERIOD_SECONDS),
            value(self.peer_limit, PEER_LIMIT),
        ]
    }

    /// Checks every value against its bounds and the settings against each
    /// other, and returns the effective settings or every problem found.
    pub fn validate(&self) -> Result<Settings, Vec<InvalidSetting>> {
        let values = self.values();
        let mut invalid: Vec<InvalidSetting> = ALL
            .iter()
            .zip(values)
            .filter(|(bounds, value)| !(bounds.minimum..=bounds.maximum).contains(value))
            .map(|(bounds, _)| InvalidSetting {
                name: bounds.name,
                detail: format!(
                    "Must be an integer from {} to {}.",
                    bounds.minimum, bounds.maximum
                ),
            })
            .collect();
        let [_, _, per_torrent, total, _, _] = values;
        if invalid.is_empty() && per_torrent > total {
            invalid.push(InvalidSetting {
                name: IDLE_READERS_PER_TORRENT.name,
                detail: format!("Must not exceed idle_readers_total, {total}."),
            });
        }
        if invalid.is_empty() {
            Ok(self.effective())
        } else {
            Err(invalid)
        }
    }

    /// The effective settings: each explicit value, or its default.
    pub fn effective(&self) -> Settings {
        let [piece_wait, idle_timeout, per_torrent, total, drain, peers] = self.values();
        Settings {
            piece_wait: Duration::from_secs(piece_wait.into()),
            idle_timeout: Duration::from_secs(idle_timeout.into()),
            idle_readers_per_torrent: per_torrent as usize,
            idle_readers_total: total as usize,
            drain_period: Duration::from_secs(drain.into()),
            peer_limit: peers as usize,
        }
    }
}

/// The stored settings: the explicit values and the version their entity
/// tag derives from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stored {
    /// The explicit values.
    pub explicit: Explicit,
    /// Starts at 1 and grows with every change.
    pub version: u64,
}

impl Default for Stored {
    fn default() -> Self {
        Self {
            explicit: Explicit::default(),
            version: 1,
        }
    }
}

impl Stored {
    /// The strong entity tag, which changes only when a value changes.
    pub fn etag(&self) -> EntityTag {
        EntityTag::new(self.version.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unset_values_take_the_defaults_and_explicit_zero_is_kept() {
        assert_eq!(Explicit::default().effective(), Settings::DEFAULT);
        let explicit = Explicit {
            idle_timeout_seconds: Some(0),
            idle_readers_total: Some(0),
            idle_readers_per_torrent: Some(0),
            ..Explicit::default()
        };
        let settings = explicit.validate().unwrap();
        assert_eq!(settings.idle_timeout, Duration::ZERO);
        assert_eq!(settings.idle_readers_total, 0);
        assert_eq!(settings.piece_wait, Settings::DEFAULT.piece_wait);
    }

    #[test]
    fn validation_reports_every_value_out_of_bounds() {
        let explicit = Explicit {
            piece_wait_seconds: Some(0),
            peer_limit: Some(501),
            drain_period_seconds: Some(25),
            ..Explicit::default()
        };
        let names: Vec<&str> = explicit
            .validate()
            .unwrap_err()
            .iter()
            .map(|invalid| invalid.name)
            .collect();
        assert_eq!(names, ["piece_wait_seconds", "peer_limit"]);
    }

    #[test]
    fn the_per_torrent_limit_must_not_exceed_the_total() {
        let explicit = Explicit {
            idle_readers_total: Some(3),
            ..Explicit::default()
        };
        let invalid = explicit.validate().unwrap_err();
        assert_eq!(
            invalid.len(),
            1,
            "the default per-torrent limit, 4, is too high"
        );
        assert_eq!(invalid[0].name, "idle_readers_per_torrent");
        assert!(
            Explicit {
                idle_readers_per_torrent: Some(3),
                ..explicit
            }
            .validate()
            .is_ok()
        );
    }

    #[test]
    fn the_defaults_lie_within_their_bounds() {
        for bounds in ALL {
            assert!(
                (bounds.minimum..=bounds.maximum).contains(&bounds.default),
                "{}",
                bounds.name
            );
        }
        assert!(Explicit::default().validate().is_ok());
    }

    #[test]
    fn every_setting_has_a_slot() {
        let mut explicit = Explicit::default();
        for (n, bounds) in ALL.iter().enumerate() {
            *explicit.slot(bounds.name).unwrap() = Some(n as u32 + 100);
        }
        assert_eq!(explicit.values(), [100, 101, 102, 103, 104, 105]);
        assert!(explicit.slot("cache_capacity_bytes").is_none());
    }
}
