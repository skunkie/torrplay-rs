// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! The TorrPlay application: the HTTP server of `torrplay-server` over the
//! librqbit engine of `torrplay-engine`, with its startup and shutdown.
//!
//! [`serve`] opens the API before the engine is ready, as the lifecycle
//! requires: until the engine has started, `/readyz` answers `503` and
//! engine operations answer `engine-unavailable`. Both HTTP/1.1 and HTTP/2
//! over cleartext (prior knowledge) are served on the one listener.
//!
//! On shutdown it follows the lifecycle's order: readiness ends and state
//! changes are refused while the listener keeps accepting, streams in
//! progress get the drain period to finish and are then canceled, the
//! listener closes, and the engine stops. The whole sequence is bounded.

use std::collections::HashSet;
use std::future::{Future, IntoFuture};
use std::net::{Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use torrplay_core::id::TorrentId;
use torrplay_core::lifecycle::StorageMode;
use torrplay_engine::{DEFAULT_PEER_PORT, EngineConfig, LibrqbitEngine, NetworkConfig};
use torrplay_server::App;
use torrplay_store::{Contents, Store};
use tracing::Level;

/// The default API port, as the contract's server URL documents.
pub const DEFAULT_PORT: u16 = 8080;

/// How long the whole shutdown may take before the process exits anyway.
pub const DEFAULT_SHUTDOWN_LIMIT: Duration = Duration::from_secs(30);

/// How often the drain checks whether streams are still in progress.
const DRAIN_POLL: Duration = Duration::from_millis(50);

/// The usage text `--help` prints.
pub const USAGE: &str = "\
Usage: torrplay [OPTIONS]

Streams torrents through the TorrPlay REST API on a loopback address.

Options:
  --listen <ADDRESS>         The API's loopback address [default: 127.0.0.1:8080]
  --peer-port <PORT>         The BitTorrent peer port on every interface [default: 4240]
  --data-dir <DIR>           Where the torrents database lives, on a local
                             filesystem [default: the platform's data directory]
  --download-dir <DIR>       Where torrents in disk storage keep their files, a
                             directory for TorrPlay alone [default: downloads in
                             the data directory]
  --log-level <LEVEL>        error, warn, info, debug, or trace [default: info]
  --log-filter <TARGETS>     Per-target levels, as in librqbit=debug,torrplay=info,
                             replacing --log-level's filter, for diagnosing the engine
  --no-dht                   Find peers through trackers only, as benchmarks do
  -h, --help                 Print this help
  -V, --version              Print the version
";

/// The application's configuration from its command line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    /// The API's listen address, always a loopback address.
    pub listen: SocketAddr,
    /// The peer listener's port, on every interface.
    pub peer_port: u16,
    /// The most verbose level TorrPlay's own logs record; other crates log
    /// warnings and errors only.
    pub log_level: Level,
    /// Per-target levels in `tracing-subscriber`'s `Targets` syntax that
    /// replace the filter `log_level` sets, so other crates can log too.
    pub log_filter: Option<String>,
    /// Whether DHT finds peers.
    pub dht: bool,
    /// The data directory, or `None` for the platform's; see
    /// [`Config::data_dir`].
    pub data_dir: Option<PathBuf>,
    /// The download directory, or `None` for the data directory's; see
    /// [`Config::download_dir`].
    pub download_dir: Option<PathBuf>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            listen: (Ipv4Addr::LOCALHOST, DEFAULT_PORT).into(),
            peer_port: DEFAULT_PEER_PORT,
            log_level: Level::INFO,
            log_filter: None,
            dht: true,
            data_dir: None,
            download_dir: None,
        }
    }
}

impl Config {
    /// The data directory: `--data-dir`, or else the platform's data
    /// directory for applications, such as `~/.local/share/torrplay` on
    /// Linux.
    pub fn data_dir(&self) -> anyhow::Result<PathBuf> {
        if let Some(directory) = &self.data_dir {
            return Ok(directory.clone());
        }
        directories::ProjectDirs::from_path(PathBuf::from("torrplay"))
            .map(|directories| directories.data_dir().to_owned())
            .context("no home directory to put the data directory in; pass --data-dir")
    }

    /// The download directory, where torrents in disk storage keep their
    /// files: `--download-dir`, or else `downloads` in `data_dir`.
    pub fn download_dir(&self, data_dir: &Path) -> PathBuf {
        self.download_dir
            .clone()
            .unwrap_or_else(|| data_dir.join("downloads"))
    }

    /// The engine configuration for this configuration, with `folder` as
    /// the session's folder, `download_dir` as the download directory, and
    /// ADR 2's public network configuration.
    pub fn engine_config(&self, folder: &Path, download_dir: &Path) -> EngineConfig {
        let mut network = NetworkConfig::public();
        network.listen.set_port(self.peer_port);
        network.dht = self.dht;
        let mut config = EngineConfig::new(folder.to_path_buf(), network);
        config.download_dir = download_dir.to_path_buf();
        config
    }
}

/// What the command line asks for.
#[derive(Debug, PartialEq, Eq)]
pub enum Command {
    /// Run the application.
    Run(Config),
    /// Print [`USAGE`].
    Help,
    /// Print the version.
    Version,
}

/// A command line the application cannot run with.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum ArgsError {
    /// An option the application does not know.
    #[error("unknown option {0}")]
    Unknown(String),
    /// An option without its value.
    #[error("{0} needs a value")]
    MissingValue(String),
    /// A value the option cannot take.
    #[error("invalid value {value:?} for {option}: {reason}")]
    InvalidValue {
        /// The option.
        option: String,
        /// The value given.
        value: String,
        /// What the option needs.
        reason: &'static str,
    },
}

/// Parses the command line's arguments, without the program name.
pub fn parse_args(args: impl IntoIterator<Item = String>) -> Result<Command, ArgsError> {
    let mut config = Config::default();
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        let (option, inline) = match arg.split_once('=') {
            Some((option, value)) if option.starts_with("--") => {
                (option.to_owned(), Some(value.to_owned()))
            }
            _ => (arg, None),
        };
        let mut value = || {
            inline
                .clone()
                .or_else(|| args.next())
                .ok_or_else(|| ArgsError::MissingValue(option.clone()))
        };
        let invalid = |value: String, reason| ArgsError::InvalidValue {
            option: option.clone(),
            value,
            reason,
        };
        match option.as_str() {
            "-h" | "--help" => return Ok(Command::Help),
            "-V" | "--version" => return Ok(Command::Version),
            "--no-dht" if inline.is_none() => config.dht = false,
            "--listen" => {
                let value = value()?;
                let listen: SocketAddr = value
                    .parse()
                    .map_err(|_| invalid(value.clone(), "an IP address and port"))?;
                if !listen.ip().is_loopback() {
                    return Err(invalid(
                        value,
                        "a loopback address, since the API has no authentication",
                    ));
                }
                config.listen = listen;
            }
            "--peer-port" => {
                let value = value()?;
                config.peer_port = value
                    .parse()
                    .ok()
                    .filter(|&port| port != 0)
                    .ok_or_else(|| invalid(value, "a port from 1 to 65535"))?;
            }
            "--log-level" => {
                let value = value()?;
                config.log_level = value
                    .parse()
                    .map_err(|_| invalid(value.clone(), "error, warn, info, debug, or trace"))?;
            }
            "--data-dir" => {
                let value = value()?;
                if value.is_empty() {
                    return Err(invalid(value, "a directory"));
                }
                config.data_dir = Some(PathBuf::from(value));
            }
            "--download-dir" => {
                let value = value()?;
                if value.is_empty() {
                    return Err(invalid(value, "a directory"));
                }
                config.download_dir = Some(PathBuf::from(value));
            }
            "--log-filter" => {
                let value = value()?;
                if value
                    .parse::<tracing_subscriber::filter::Targets>()
                    .is_err()
                {
                    return Err(invalid(value, "targets with levels, as in librqbit=debug"));
                }
                config.log_filter = Some(value);
            }
            _ => return Err(ArgsError::Unknown(option)),
        }
    }
    Ok(Command::Run(config))
}

/// Records logs to standard error: TorrPlay's own at `level` and more
/// severe, other crates' warnings and errors only, unless `filter`, already
/// validated by [`parse_args`], sets every target's level instead.
pub fn init_logging(level: Level, filter: Option<&str>) {
    use tracing_subscriber::filter::{LevelFilter, Targets};
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;

    let level = LevelFilter::from_level(level);
    let default = || {
        [
            "torrplay",
            "torrplay_core",
            "torrplay_engine",
            "torrplay_server",
        ]
        .into_iter()
        .fold(
            Targets::new().with_default(LevelFilter::WARN),
            |filter, target| filter.with_target(target, level),
        )
    };
    let filter = filter
        .and_then(|filter| filter.parse::<Targets>().ok())
        .unwrap_or_else(default);
    tracing_subscriber::registry()
        .with(tracing_subscriber::fmt::layer().with_writer(std::io::stderr))
        .with(filter)
        .init();
}

/// The bounds of the shutdown sequence.
#[derive(Clone, Copy, Debug)]
pub struct ShutdownTimings {
    /// How long streams in progress may continue, for tests; `None` takes
    /// the drain period setting.
    pub drain: Option<Duration>,
    /// How long the whole sequence may take.
    pub limit: Duration,
}

impl Default for ShutdownTimings {
    fn default() -> Self {
        Self {
            drain: None,
            limit: DEFAULT_SHUTDOWN_LIMIT,
        }
    }
}

/// Serves the API on `listener` over `engine`, which must not be started
/// yet, and `store` with what it held, until `shutdown` completes, then
/// shuts down in the lifecycle's order.
///
/// The stored settings apply at once. The listener serves before the engine
/// starts, and the stored torrents are restored once it has. Fails if the engine cannot start, if restoring or
/// the server fails, or if shutdown takes longer than `timings.limit`.
pub async fn serve(
    listener: TcpListener,
    engine: Arc<LibrqbitEngine>,
    store: Store,
    contents: Contents,
    timings: ShutdownTimings,
    shutdown: impl Future<Output = ()>,
) -> anyhow::Result<()> {
    let Contents {
        torrents,
        settings,
        clean_shutdown,
    } = contents;
    if !clean_shutdown {
        tracing::warn!(
            "the last run did not shut down cleanly; disk torrents check their data in full"
        );
        engine.distrust_resume_bitfields();
    }
    // ADR 9: data a crash left in the download directory, of torrents that
    // are gone or no longer on disk, goes before anything is added.
    let keep: HashSet<TorrentId> = torrents
        .iter()
        .filter(|torrent| {
            torrent.storage_mode == StorageMode::Disk
                || torrent.configuration.storage_mode() == StorageMode::Disk
        })
        .map(|torrent| torrent.id.clone())
        .collect();
    let sweeping = engine.clone();
    match tokio::task::spawn_blocking(move || sweeping.sweep_download_dir(&keep)).await {
        Ok(Ok(deleted)) => {
            for path in deleted {
                tracing::info!(path = %path.display(), "deleted orphaned disk data");
            }
        }
        Ok(Err(error)) => tracing::warn!(%error, "the download directory could not be swept"),
        Err(error) => tracing::warn!(%error, "sweeping the download directory panicked"),
    }
    let app = Arc::new(App::new(engine.clone(), store).with_settings(settings));
    let (close_listener, listener_closed) = oneshot::channel::<()>();
    let mut server = tokio::spawn(
        axum::serve(listener, torrplay_server::router(app.clone()))
            .with_graceful_shutdown(async {
                // A dropped sender closes the listener too.
                let _ = listener_closed.await;
            })
            .into_future(),
    );
    let mut shutdown = std::pin::pin!(shutdown);

    let started = tokio::select! {
        started = engine.start() => started.context("the engine did not start"),
        () = &mut shutdown => Ok(()),
        served = &mut server => Err(server_ended(served)),
    };
    // Whether the engine started and the stored torrents were restored, so
    // the application runs until shutdown.
    let ready = match started {
        Ok(()) if engine.playback().is_some() => {
            let count = torrents.len();
            tokio::select! {
                restored = app.restore(torrents) => match restored {
                    Ok(()) => {
                        tracing::info!(torrents = count, "the engine is ready, with the stored torrents");
                        app.resume_transitions();
                        Ok(true)
                    }
                    Err(error) => {
                        Err(anyhow::Error::new(error).context("restoring the stored torrents"))
                    }
                },
                () = &mut shutdown => Ok(false),
                served = &mut server => Err(server_ended(served)),
            }
        }
        other => other.map(|()| false),
    };
    let result = match ready {
        Ok(true) => {
            tokio::select! {
                () = &mut shutdown => Ok(()),
                served = &mut server => Err(server_ended(served)),
            }
        }
        Ok(false) => Ok(()),
        Err(error) => Err(error),
    };
    if let Err(error) = &result {
        tracing::error!(error = %format!("{error:#}"), "stopping after a failure");
    }

    tracing::info!("shutting down");
    let sequence = async {
        app.begin_shutdown();
        drain(&engine, timings.drain.unwrap_or_else(|| app.drain_period())).await;
        engine.cancel_streams()?;
        let _ = close_listener.send(());
        if !server.is_finished() {
            server_result(server.await)?;
        }
        engine.shutdown().await?;
        app.record_clean_shutdown()
            .await
            .context("recording the clean shutdown")?;
        app.close_store().await.context("closing the store")
    };
    let stopped = tokio::time::timeout(timings.limit, sequence)
        .await
        .with_context(|| format!("shutdown did not finish within {:?}", timings.limit))
        .and_then(|stopped| stopped);
    if stopped.is_ok() {
        tracing::info!("stopped");
    }
    result.and(stopped)
}

/// Waits until no stream is in progress, for at most `period`.
async fn drain(engine: &LibrqbitEngine, period: Duration) {
    let streaming = || {
        engine
            .playback()
            .map(|playback| playback.stats())
            .is_some_and(|stats| stats.readers > stats.idle_readers)
    };
    let _ = tokio::time::timeout(period, async {
        while streaming() {
            tokio::time::sleep(DRAIN_POLL).await;
        }
    })
    .await;
}

fn server_result(
    served: Result<std::io::Result<()>, tokio::task::JoinError>,
) -> anyhow::Result<()> {
    served
        .context("the server task panicked")?
        .context("the server failed")
}

fn server_ended(served: Result<std::io::Result<()>, tokio::task::JoinError>) -> anyhow::Error {
    match server_result(served) {
        Ok(()) => anyhow::anyhow!("the server stopped unexpectedly"),
        Err(error) => error,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Command, ArgsError> {
        parse_args(args.iter().map(|&arg| arg.to_owned()))
    }

    #[test]
    fn the_engines_defaults_are_the_settings_defaults() {
        let engine = EngineConfig::new(PathBuf::new(), NetworkConfig::isolated(Vec::new()));
        let settings = torrplay_core::settings::Settings::DEFAULT;
        assert_eq!(
            (
                engine.piece_wait,
                engine.idle_timeout,
                engine.idle_readers_per_torrent,
                engine.idle_readers_total,
                engine.peer_limit,
                engine.cache_capacity_bytes,
            ),
            (
                settings.piece_wait,
                settings.idle_timeout,
                settings.idle_readers_per_torrent,
                settings.idle_readers_total,
                settings.peer_limit,
                settings.cache_capacity_bytes,
            ),
        );
    }

    #[test]
    fn no_arguments_run_with_the_defaults() {
        assert_eq!(parse(&[]), Ok(Command::Run(Config::default())));
        assert_eq!(
            Config::default().listen,
            "127.0.0.1:8080".parse::<SocketAddr>().unwrap()
        );
        assert_eq!(
            Config::default().download_dir(Path::new("/srv/torrplay")),
            Path::new("/srv/torrplay/downloads"),
            "the download directory defaults to one in the data directory"
        );
    }

    #[test]
    fn options_take_separate_or_inline_values() {
        let Ok(Command::Run(config)) = parse(&[
            "--listen",
            "[::1]:9000",
            "--peer-port=6881",
            "--log-level=debug",
            "--log-filter",
            "librqbit=debug,torrplay_engine=trace",
            "--data-dir=/srv/torrplay",
            "--download-dir",
            "/media/torrplay",
            "--no-dht",
        ]) else {
            panic!("the options parse");
        };
        assert_eq!(
            config,
            Config {
                listen: "[::1]:9000".parse().unwrap(),
                peer_port: 6881,
                log_level: Level::DEBUG,
                log_filter: Some("librqbit=debug,torrplay_engine=trace".into()),
                dht: false,
                data_dir: Some("/srv/torrplay".into()),
                download_dir: Some("/media/torrplay".into()),
            }
        );
        assert_eq!(
            config.data_dir().unwrap(),
            PathBuf::from("/srv/torrplay"),
            "an explicit data directory is used as given"
        );
        assert_eq!(parse(&["--version"]), Ok(Command::Version));
        assert_eq!(parse(&["--listen=x", "-h"]).ok(), None);
        assert_eq!(parse(&["-h", "--listen=x"]), Ok(Command::Help));
    }

    #[test]
    fn the_api_listens_on_loopback_only() {
        for address in ["0.0.0.0:8080", "192.0.2.1:8080", "[::]:8080"] {
            assert!(
                matches!(
                    parse(&["--listen", address]),
                    Err(ArgsError::InvalidValue { .. })
                ),
                "{address}"
            );
        }
    }

    #[test]
    fn invalid_values_and_unknown_options_are_refused() {
        for args in [
            &["--peer-port", "0"][..],
            &["--peer-port", "70000"],
            &["--log-level", "loud"],
            &["--log-filter", "librqbit=loud"],
            &["--data-dir="],
            &["--download-dir="],
        ] {
            assert!(
                matches!(parse(args), Err(ArgsError::InvalidValue { .. })),
                "{args:?}"
            );
        }
        assert_eq!(
            parse(&["--peer-port"]),
            Err(ArgsError::MissingValue("--peer-port".into()))
        );
        assert_eq!(
            parse(&["--cache-capacity", "1048576"]),
            Err(ArgsError::Unknown("--cache-capacity".into())),
            "the cache capacity is a setting"
        );
        assert_eq!(
            parse(&["--verbose"]),
            Err(ArgsError::Unknown("--verbose".into()))
        );
    }

    #[test]
    fn the_engine_uses_the_peer_port_on_every_interface() {
        let config = Config {
            peer_port: 6881,
            ..Config::default()
        };
        let engine = config.engine_config(Path::new("/session"), Path::new("/media/torrplay"));
        assert_eq!(engine.download_dir, Path::new("/media/torrplay"));
        assert_eq!(engine.network.listen.port(), 6881);
        assert!(engine.network.listen.ip().is_unspecified());
        assert!(engine.network.dht);
    }
}
