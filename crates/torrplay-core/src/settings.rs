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
    pub minimum: u64,
    /// The greatest value.
    pub maximum: u64,
    /// The value while unset.
    pub default: u64,
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
/// The cache capacity, in bytes: 4 MiB to 64 GiB, 256 MiB by default.
pub const CACHE_CAPACITY_BYTES: Bounds = Bounds {
    name: "cache_capacity_bytes",
    minimum: 4 << 20,
    maximum: 64 << 30,
    default: 256 << 20,
};

/// How long a temporary torrent may go without a reader, in seconds.
pub const TEMPORARY_IDLE_SECONDS: Bounds = Bounds {
    name: "temporary_idle_seconds",
    minimum: 60,
    maximum: 86_400,
    default: 600,
};
/// The most temporary torrents the application holds.
pub const TEMPORARY_TORRENTS_MAX: Bounds = Bounds {
    name: "temporary_torrents_max",
    minimum: 1,
    maximum: 64,
    default: 16,
};

/// The name of the background downloads switch.
pub const BACKGROUND_DOWNLOADS: &str = "background_downloads";
/// Whether background downloads run while unset.
pub const BACKGROUND_DOWNLOADS_DEFAULT: bool = true;

/// The name of the DLNA media server switch.
pub const DLNA_ENABLED: &str = "dlna_enabled";
/// Whether the DLNA media server runs while unset.
pub const DLNA_ENABLED_DEFAULT: bool = false;

/// The name of the DLNA media server's friendly name.
pub const DLNA_NAME: &str = "dlna_name";
/// The friendly name while unset.
pub const DLNA_NAME_DEFAULT: &str = "TorrPlay";
/// The most characters a friendly name holds; UPnP recommends fewer than
/// 64.
pub const DLNA_NAME_MAX_CHARS: usize = 64;

/// The name of the DHT switch.
pub const DHT_ENABLED: &str = "dht_enabled";
/// Whether torrents use the DHT while unset.
pub const DHT_ENABLED_DEFAULT: bool = true;

/// The name of the log level setting.
pub const LOG_LEVEL: &str = "log_level";

/// The least severe level of the application's own log messages.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum LogLevel {
    /// Errors only.
    Error,
    /// Warnings and errors.
    Warn,
    /// Informational messages and above; the default.
    #[default]
    Info,
    /// Debugging messages and above.
    Debug,
    /// Everything.
    Trace,
}

impl LogLevel {
    /// Every level, least verbose first.
    pub const ALL: [Self; 5] = [
        Self::Error,
        Self::Warn,
        Self::Info,
        Self::Debug,
        Self::Trace,
    ];

    /// The level's name in the contract and on the command line.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::Warn => "warn",
            Self::Info => "info",
            Self::Debug => "debug",
            Self::Trace => "trace",
        }
    }

    /// The level named `name`, as [`as_str`](Self::as_str) spells it.
    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|level| level.as_str() == name)
    }
}

/// Every numeric setting, in the order representations list them; the
/// other settings follow them.
pub const ALL: [Bounds; 9] = [
    PIECE_WAIT_SECONDS,
    IDLE_TIMEOUT_SECONDS,
    IDLE_READERS_PER_TORRENT,
    IDLE_READERS_TOTAL,
    DRAIN_PERIOD_SECONDS,
    PEER_LIMIT,
    CACHE_CAPACITY_BYTES,
    TEMPORARY_IDLE_SECONDS,
    TEMPORARY_TORRENTS_MAX,
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
    /// The global piece cache's capacity.
    pub cache_capacity_bytes: u64,
    /// How long a temporary torrent may go without a reader.
    pub temporary_idle: Duration,
    /// The most temporary torrents the application holds.
    pub temporary_torrents_max: usize,
    /// Whether started torrents in disk storage download the pieces they
    /// lack while no reader exists.
    pub background_downloads: bool,
    /// Whether the DLNA media server announces the library to the local
    /// network.
    pub dlna_enabled: bool,
    /// Whether torrents find peers through the DHT.
    pub dht_enabled: bool,
    /// The least severe level of the application's own log messages.
    pub log_level: LogLevel,
}

impl Settings {
    /// Every setting at its default.
    pub const DEFAULT: Self = Self {
        piece_wait: Duration::from_secs(PIECE_WAIT_SECONDS.default),
        idle_timeout: Duration::from_secs(IDLE_TIMEOUT_SECONDS.default),
        idle_readers_per_torrent: IDLE_READERS_PER_TORRENT.default as usize,
        idle_readers_total: IDLE_READERS_TOTAL.default as usize,
        drain_period: Duration::from_secs(DRAIN_PERIOD_SECONDS.default),
        peer_limit: PEER_LIMIT.default as usize,
        cache_capacity_bytes: CACHE_CAPACITY_BYTES.default,
        temporary_idle: Duration::from_secs(TEMPORARY_IDLE_SECONDS.default),
        temporary_torrents_max: TEMPORARY_TORRENTS_MAX.default as usize,
        background_downloads: BACKGROUND_DOWNLOADS_DEFAULT,
        dlna_enabled: DLNA_ENABLED_DEFAULT,
        dht_enabled: DHT_ENABLED_DEFAULT,
        log_level: LogLevel::Info,
    };

    /// These settings as the overrides leave them in effect.
    pub fn overridden(self, overrides: &Overrides) -> Self {
        Self {
            dht_enabled: overrides.dht_enabled.unwrap_or(self.dht_enabled),
            log_level: match overrides.log_level {
                Some(LogOverride::Level(level)) => level,
                _ => self.log_level,
            },
            ..self
        }
    }
}

/// The settings command-line options override for one run. They are never
/// stored.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Overrides {
    /// `false` while `--no-dht` is given.
    pub dht_enabled: Option<bool>,
    /// What `--log-level` or `--log-filter` sets.
    pub log_level: Option<LogOverride>,
}

/// How a command-line option sets the log level.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LogOverride {
    /// One level for the application's messages.
    Level(LogLevel),
    /// Levels per component, from a filter.
    Custom,
}

/// The explicitly set values: `None` is unset.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Explicit {
    /// [`PIECE_WAIT_SECONDS`].
    pub piece_wait_seconds: Option<u64>,
    /// [`IDLE_TIMEOUT_SECONDS`].
    pub idle_timeout_seconds: Option<u64>,
    /// [`IDLE_READERS_PER_TORRENT`].
    pub idle_readers_per_torrent: Option<u64>,
    /// [`IDLE_READERS_TOTAL`].
    pub idle_readers_total: Option<u64>,
    /// [`DRAIN_PERIOD_SECONDS`].
    pub drain_period_seconds: Option<u64>,
    /// [`PEER_LIMIT`].
    pub peer_limit: Option<u64>,
    /// [`CACHE_CAPACITY_BYTES`].
    pub cache_capacity_bytes: Option<u64>,
    /// [`TEMPORARY_IDLE_SECONDS`].
    pub temporary_idle_seconds: Option<u64>,
    /// [`TEMPORARY_TORRENTS_MAX`].
    pub temporary_torrents_max: Option<u64>,
    /// [`BACKGROUND_DOWNLOADS`].
    pub background_downloads: Option<bool>,
    /// [`DLNA_ENABLED`].
    pub dlna_enabled: Option<bool>,
    /// [`DLNA_NAME`].
    pub dlna_name: Option<String>,
    /// [`DHT_ENABLED`].
    pub dht_enabled: Option<bool>,
    /// [`LOG_LEVEL`].
    pub log_level: Option<LogLevel>,
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
    /// The slot of the numeric setting named `name`, if there is one.
    pub fn slot(&mut self, name: &str) -> Option<&mut Option<u64>> {
        Some(match name {
            "piece_wait_seconds" => &mut self.piece_wait_seconds,
            "idle_timeout_seconds" => &mut self.idle_timeout_seconds,
            "idle_readers_per_torrent" => &mut self.idle_readers_per_torrent,
            "idle_readers_total" => &mut self.idle_readers_total,
            "drain_period_seconds" => &mut self.drain_period_seconds,
            "peer_limit" => &mut self.peer_limit,
            "cache_capacity_bytes" => &mut self.cache_capacity_bytes,
            "temporary_idle_seconds" => &mut self.temporary_idle_seconds,
            "temporary_torrents_max" => &mut self.temporary_torrents_max,
            _ => return None,
        })
    }

    /// Each numeric setting's value, explicit or default, in [`ALL`]'s
    /// order.
    pub fn values(&self) -> [u64; 9] {
        let value = |explicit: Option<u64>, bounds: Bounds| explicit.unwrap_or(bounds.default);
        [
            value(self.piece_wait_seconds, PIECE_WAIT_SECONDS),
            value(self.idle_timeout_seconds, IDLE_TIMEOUT_SECONDS),
            value(self.idle_readers_per_torrent, IDLE_READERS_PER_TORRENT),
            value(self.idle_readers_total, IDLE_READERS_TOTAL),
            value(self.drain_period_seconds, DRAIN_PERIOD_SECONDS),
            value(self.peer_limit, PEER_LIMIT),
            value(self.cache_capacity_bytes, CACHE_CAPACITY_BYTES),
            value(self.temporary_idle_seconds, TEMPORARY_IDLE_SECONDS),
            value(self.temporary_torrents_max, TEMPORARY_TORRENTS_MAX),
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
        let [_, _, per_torrent, total, _, _, _, _, _] = values;
        if let Some(name) = &self.dlna_name
            && let Err(detail) = check_dlna_name(name)
        {
            invalid.push(InvalidSetting {
                name: DLNA_NAME,
                detail,
            });
        }
        if invalid.iter().all(|invalid| invalid.name == DLNA_NAME) && per_torrent > total {
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
        let [
            piece_wait,
            idle_timeout,
            per_torrent,
            total,
            drain,
            peers,
            capacity,
            temporary_idle,
            temporary_max,
        ] = self.values();
        // The counts' bounds are far below any usize.
        Settings {
            piece_wait: Duration::from_secs(piece_wait),
            idle_timeout: Duration::from_secs(idle_timeout),
            idle_readers_per_torrent: per_torrent as usize,
            idle_readers_total: total as usize,
            drain_period: Duration::from_secs(drain),
            peer_limit: peers as usize,
            cache_capacity_bytes: capacity,
            temporary_idle: Duration::from_secs(temporary_idle),
            temporary_torrents_max: temporary_max as usize,
            background_downloads: self
                .background_downloads
                .unwrap_or(BACKGROUND_DOWNLOADS_DEFAULT),
            dlna_enabled: self.dlna_enabled.unwrap_or(DLNA_ENABLED_DEFAULT),
            dht_enabled: self.dht_enabled.unwrap_or(DHT_ENABLED_DEFAULT),
            log_level: self.log_level.unwrap_or_default(),
        }
    }

    /// The DLNA media server's friendly name, explicit or default.
    pub fn dlna_name(&self) -> &str {
        self.dlna_name.as_deref().unwrap_or(DLNA_NAME_DEFAULT)
    }
}

/// Checks a friendly name: 1 to 64 characters, none of them C0 controls or
/// DEL, as the contract's pattern says.
fn check_dlna_name(name: &str) -> Result<(), String> {
    let chars = name.chars().count();
    if !(1..=DLNA_NAME_MAX_CHARS).contains(&chars) || name.chars().any(|c| c < ' ' || c == '\u{7f}')
    {
        return Err(format!(
            "Must be 1 to {DLNA_NAME_MAX_CHARS} characters without control characters."
        ));
    }
    Ok(())
}

/// The stored settings: the explicit values and the version their entity
/// tag derives from.
#[derive(Clone, Debug, PartialEq, Eq)]
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
    fn background_downloads_are_on_unless_turned_off() {
        assert!(Explicit::default().effective().background_downloads);
        let off = Explicit {
            background_downloads: Some(false),
            ..Explicit::default()
        };
        assert!(!off.validate().unwrap().background_downloads);
        assert_ne!(off, Explicit::default(), "explicit false is kept");
    }

    #[test]
    fn the_media_server_is_off_unless_turned_on() {
        assert!(!Explicit::default().effective().dlna_enabled);
        let on = Explicit {
            dlna_enabled: Some(true),
            ..Explicit::default()
        };
        assert!(on.validate().unwrap().dlna_enabled);
    }

    #[test]
    fn the_dht_is_on_and_the_name_is_torrplay_unless_set() {
        let defaults = Explicit::default();
        assert!(defaults.effective().dht_enabled);
        assert_eq!(defaults.dlna_name(), "TorrPlay");
        assert_eq!(defaults.effective().log_level, LogLevel::Info);
        let set = Explicit {
            dht_enabled: Some(false),
            dlna_name: Some("Living room".into()),
            log_level: Some(LogLevel::Debug),
            ..Explicit::default()
        };
        let settings = set.validate().unwrap();
        assert!(!settings.dht_enabled);
        assert_eq!(settings.log_level, LogLevel::Debug);
        assert_eq!(set.dlna_name(), "Living room");
    }

    #[test]
    fn friendly_names_are_short_and_printable() {
        let valid = ["T", "Кино в гостиной", &"x".repeat(64), &"ё".repeat(64)];
        for name in valid {
            assert_eq!(check_dlna_name(name), Ok(()), "{name:?}");
        }
        let invalid = [
            "",
            &"x".repeat(65),
            "Living\nroom",
            "Tab\there",
            "Del\u{7f}",
        ];
        for name in invalid {
            let explicit = Explicit {
                dlna_name: Some(name.to_owned()),
                ..Explicit::default()
            };
            let names: Vec<&str> = explicit
                .validate()
                .unwrap_err()
                .iter()
                .map(|invalid| invalid.name)
                .collect();
            assert_eq!(names, ["dlna_name"], "{name:?}");
        }
    }

    #[test]
    fn overrides_win_over_stored_values() {
        let stored = Explicit {
            dht_enabled: Some(true),
            log_level: Some(LogLevel::Warn),
            ..Explicit::default()
        }
        .effective();
        assert_eq!(stored.overridden(&Overrides::default()), stored);
        let overridden = stored.overridden(&Overrides {
            dht_enabled: Some(false),
            log_level: Some(LogOverride::Level(LogLevel::Trace)),
        });
        assert!(!overridden.dht_enabled);
        assert_eq!(overridden.log_level, LogLevel::Trace);
        let custom = stored.overridden(&Overrides {
            dht_enabled: None,
            log_level: Some(LogOverride::Custom),
        });
        assert_eq!(
            custom.log_level,
            LogLevel::Warn,
            "a filter has no one level"
        );
    }

    #[test]
    fn log_levels_round_trip_their_names() {
        for level in LogLevel::ALL {
            assert_eq!(LogLevel::parse(level.as_str()), Some(level));
        }
        assert_eq!(LogLevel::parse("INFO"), None);
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
            *explicit.slot(bounds.name).unwrap() = Some(n as u64 + 100);
        }
        assert_eq!(
            explicit.values(),
            [100, 101, 102, 103, 104, 105, 106, 107, 108]
        );
        assert!(explicit.slot("readahead_bytes").is_none());
    }
}
