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

use std::future::{Future, IntoFuture};
use std::net::{Ipv4Addr, SocketAddr};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use torrplay_engine::{
    DEFAULT_CACHE_CAPACITY_BYTES, DEFAULT_PEER_PORT, EngineConfig, LibrqbitEngine, NetworkConfig,
};
use torrplay_server::App;
use tracing::Level;

/// The default API port, as the contract's server URL documents.
pub const DEFAULT_PORT: u16 = 8080;

/// How long streams in progress may continue after shutdown begins.
pub const DEFAULT_DRAIN_PERIOD: Duration = Duration::from_secs(10);

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
  --cache-capacity <BYTES>   The piece cache capacity [default: 268435456]
  --log-level <LEVEL>        error, warn, info, debug, or trace [default: info]
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
    /// The piece cache capacity.
    pub cache_capacity_bytes: u64,
    /// The most verbose level TorrPlay's own logs record; other crates log
    /// warnings and errors only.
    pub log_level: Level,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            listen: (Ipv4Addr::LOCALHOST, DEFAULT_PORT).into(),
            peer_port: DEFAULT_PEER_PORT,
            cache_capacity_bytes: DEFAULT_CACHE_CAPACITY_BYTES,
            log_level: Level::INFO,
        }
    }
}

impl Config {
    /// The engine configuration for this configuration, with `folder` as
    /// the session's folder and ADR 2's public network configuration.
    pub fn engine_config(&self, folder: &Path) -> EngineConfig {
        let mut network = NetworkConfig::public();
        network.listen.set_port(self.peer_port);
        EngineConfig::new(folder.to_path_buf(), network)
            .with_cache_capacity(self.cache_capacity_bytes)
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
            "--cache-capacity" => {
                let value = value()?;
                config.cache_capacity_bytes = value
                    .parse()
                    .ok()
                    .filter(|&bytes| bytes > 0)
                    .ok_or_else(|| invalid(value, "a positive number of bytes"))?;
            }
            "--log-level" => {
                let value = value()?;
                config.log_level = value
                    .parse()
                    .map_err(|_| invalid(value.clone(), "error, warn, info, debug, or trace"))?;
            }
            _ => return Err(ArgsError::Unknown(option)),
        }
    }
    Ok(Command::Run(config))
}

/// Records logs to standard error: TorrPlay's own at `level` and more
/// severe, other crates' warnings and errors only.
pub fn init_logging(level: Level) {
    use tracing_subscriber::filter::{LevelFilter, Targets};
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;

    let level = LevelFilter::from_level(level);
    let filter = [
        "torrplay",
        "torrplay_core",
        "torrplay_engine",
        "torrplay_server",
    ]
    .into_iter()
    .fold(
        Targets::new().with_default(LevelFilter::WARN),
        |filter, target| filter.with_target(target, level),
    );
    tracing_subscriber::registry()
        .with(tracing_subscriber::fmt::layer().with_writer(std::io::stderr))
        .with(filter)
        .init();
}

/// The bounds of the shutdown sequence.
#[derive(Clone, Copy, Debug)]
pub struct ShutdownTimings {
    /// How long streams in progress may continue.
    pub drain: Duration,
    /// How long the whole sequence may take.
    pub limit: Duration,
}

impl Default for ShutdownTimings {
    fn default() -> Self {
        Self {
            drain: DEFAULT_DRAIN_PERIOD,
            limit: DEFAULT_SHUTDOWN_LIMIT,
        }
    }
}

/// Serves the API on `listener` over `engine`, which must not be started
/// yet, until `shutdown` completes, then shuts down in the lifecycle's
/// order.
///
/// The listener serves before the engine starts. Fails if the engine
/// cannot start, if the server fails, or if shutdown takes longer than
/// `timings.limit`.
pub async fn serve(
    listener: TcpListener,
    engine: Arc<LibrqbitEngine>,
    timings: ShutdownTimings,
    shutdown: impl Future<Output = ()>,
) -> anyhow::Result<()> {
    let app = Arc::new(App::new(engine.clone()));
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
    let result = match started {
        Ok(()) if engine.playback().is_some() => {
            tracing::info!("the engine is ready");
            tokio::select! {
                () = &mut shutdown => Ok(()),
                served = &mut server => Err(server_ended(served)),
            }
        }
        other => other,
    };
    if let Err(error) = &result {
        tracing::error!(error = %format!("{error:#}"), "stopping after a failure");
    }

    tracing::info!("shutting down");
    let sequence = async {
        app.begin_shutdown();
        drain(&engine, timings.drain).await;
        engine.cancel_streams()?;
        let _ = close_listener.send(());
        if !server.is_finished() {
            server_result(server.await)?;
        }
        engine.shutdown().await
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
    fn no_arguments_run_with_the_defaults() {
        assert_eq!(parse(&[]), Ok(Command::Run(Config::default())));
        assert_eq!(
            Config::default().listen,
            "127.0.0.1:8080".parse::<SocketAddr>().unwrap()
        );
    }

    #[test]
    fn options_take_separate_or_inline_values() {
        let Ok(Command::Run(config)) = parse(&[
            "--listen",
            "[::1]:9000",
            "--peer-port=6881",
            "--cache-capacity",
            "1048576",
            "--log-level=debug",
        ]) else {
            panic!("the options parse");
        };
        assert_eq!(
            config,
            Config {
                listen: "[::1]:9000".parse().unwrap(),
                peer_port: 6881,
                cache_capacity_bytes: 1_048_576,
                log_level: Level::DEBUG,
            }
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
            &["--cache-capacity", "0"],
            &["--cache-capacity", "1GiB"],
            &["--log-level", "loud"],
        ] {
            assert!(
                matches!(parse(args), Err(ArgsError::InvalidValue { .. })),
                "{args:?}"
            );
        }
        assert_eq!(
            parse(&["--cache-capacity"]),
            Err(ArgsError::MissingValue("--cache-capacity".into()))
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
            cache_capacity_bytes: 1024,
            ..Config::default()
        };
        let engine = config.engine_config(Path::new("/session"));
        assert_eq!(engine.network.listen.port(), 6881);
        assert!(engine.network.listen.ip().is_unspecified());
        assert_eq!(engine.cache_capacity_bytes, 1024);
        assert_eq!(engine.readahead_bytes, 512);
    }
}
