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
use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use tokio::net::TcpListener;
use tokio::sync::oneshot;
pub use torrplay_core::credentials::AllowedHosts;
use torrplay_dlna::description::Device;
use url::Url;

#[cfg(feature = "client")]
mod client;
mod logging;
mod media_server;
use torrplay_core::id::TorrentId;
use torrplay_core::lifecycle::StorageMode;
pub use torrplay_core::settings::{LogLevel, LogOverride, Overrides};
use torrplay_engine::{EngineConfig, LibrqbitEngine, NetworkConfig, PEER_PORT_DEFAULT};
pub use torrplay_server::BackupCounts;
pub use torrplay_server::Run;
pub use torrplay_server::log_files::{LogFiles, LogWriter};
use torrplay_server::{App, delete_temporary_files};
use torrplay_store::{Contents, Store};

/// The default API port, as the contract's server URL documents.
pub const PORT_DEFAULT: u16 = 8642;

/// The API's default address: every interface, dual-stack.
pub const LISTEN_DEFAULT: SocketAddr =
    SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), PORT_DEFAULT);

/// The address the API falls back to where the system has no IPv6.
const FALLBACK_LISTEN: SocketAddr =
    SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), PORT_DEFAULT);

/// The environment variable that names the API's address where `--listen`
/// does not.
pub const LISTEN_VARIABLE: &str = "TORRPLAY_LISTEN";

/// The environment variable that chooses the log format where
/// `--log-format` does not.
pub const LOG_FORMAT_VARIABLE: &str = "TORRPLAY_LOG_FORMAT";

/// How long the whole shutdown may take before the process exits anyway.
pub const SHUTDOWN_TIMEOUT_DEFAULT: Duration = Duration::from_secs(30);

/// How often the drain checks whether streams are still in progress.
const DRAIN_POLL: Duration = Duration::from_millis(50);

/// The usage text `--help` prints.
pub const USAGE: &str = "\
Usage: torrplay [OPTIONS]
       torrplay password set [--data-dir <DIR>]
       torrplay password remove [--data-dir <DIR>]
       torrplay backup create [--output <FILE>] [--data-dir <DIR>]
       torrplay backup restore <FILE> [--data-dir <DIR>]
       torrplay mcp [--url <URL>] [--transport stdio|http] [--listen <ADDRESS>]

Streams torrents through the TorrPlay REST API. Authentication is off until a
password is set. `password set` reads a new password from the first line of
standard input, and `password remove` turns authentication off and revokes
every token; both need the application stopped. `backup create` writes the
archive GET /api/v1/backup returns to --output [default:
torrplay-backup-<UTC time>.zip], never over an existing file, and `backup
restore` replaces every record but the credentials with an archive's, as PUT
/api/v1/backup does, checking it whole first; both need the application
stopped, and the next start adds the restored torrents. `mcp` serves a running
application's API to an MCP client: it reaches the application at --url or
TORRPLAY_URL [default: http://127.0.0.1:8642], sends TORRPLAY_TOKEN, an api
access token, as its credential, and speaks on standard input and output, or
with --transport http at /mcp on --listen, a loopback address [default:
127.0.0.1:8643].

Options:
  --listen <ADDRESS>         The API's address, or TORRPLAY_LISTEN; until a
                             password is set, only local networks may use it
                             [default: [::]:8642, every interface]
  --allowed-host <NAME>      A host name requests may give besides IP addresses,
                             localhost, and .local names, such as a reverse
                             proxy's; repeat for each
  --peer-port <PORT>         The BitTorrent peer port on every interface [default: 4240]
  --data-dir <DIR>           Where the torrents database lives, on a local
                             filesystem [default: the platform's data directory]
  --download-dir <DIR>       Where torrents in disk storage keep their files, a
                             directory for TorrPlay alone [default: downloads in
                             the data directory]
  --log-format <FORMAT>      text or json, or TORRPLAY_LOG_FORMAT [default: text
                             when standard error is a terminal, else json]

These options override a setting for this run only, without storing it:
  --log-level <LEVEL>        error, warn, info, debug, or trace, instead of the
                             log_level setting
  --log-filter <TARGETS>     Per-target levels, as in librqbit=debug,torrplay=info,
                             instead of the log_level setting, for diagnosing the
                             engine
  --no-dht                   Find peers through trackers only, whatever the
                             dht_enabled setting says, as benchmarks do

The DLNA media server, its name, and the DHT are settings of the API.
  -h, --help                 Print this help
  -V, --version              Print the version
";

/// Binds the API's listener on `listen`, or on [`LISTEN_DEFAULT`] when it is
/// `None`, falling back to `0.0.0.0:8642` where the system has no IPv6. An
/// unspecified IPv6 address is dual-stack on every system, which some leave
/// IPv6-only by default, so IPv4 clients reach it as IPv4-mapped addresses.
pub fn bind_api(listen: Option<SocketAddr>) -> io::Result<TcpListener> {
    let Some(address) = listen else {
        return match bind(LISTEN_DEFAULT) {
            // Another process holding the port holds it on IPv4 too.
            Err(error)
                if !matches!(
                    error.kind(),
                    io::ErrorKind::AddrInUse | io::ErrorKind::PermissionDenied
                ) =>
            {
                tracing::warn!(%error, "no IPv6 listener; the API listens on IPv4 only");
                bind(FALLBACK_LISTEN)
            }
            bound => bound,
        };
    };
    bind(address)
}

fn bind(address: SocketAddr) -> io::Result<TcpListener> {
    use socket2::{Domain, Protocol, Socket, Type};
    let socket = Socket::new(
        Domain::for_address(address),
        Type::STREAM,
        Some(Protocol::TCP),
    )?;
    if address.is_ipv6() && address.ip().is_unspecified() {
        socket.set_only_v6(false)?;
    }
    // As the standard library's listeners do, so a restart need not wait
    // for the old connections' TIME_WAIT.
    #[cfg(unix)]
    socket.set_reuse_address(true)?;
    socket.bind(&address.into())?;
    socket.listen(1024)?;
    socket.set_nonblocking(true)?;
    TcpListener::from_std(socket.into())
}

/// The application's configuration from its command line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    /// The API's listen address, or `None` for [`LISTEN_DEFAULT`]; see
    /// [`bind_api`].
    pub listen: Option<SocketAddr>,
    /// The host names requests may give beyond those always allowed,
    /// normalized.
    pub allowed_hosts: Vec<String>,
    /// The peer listener's port, on every interface.
    pub peer_port: u16,
    /// The most verbose level TorrPlay's own logs record, overriding the
    /// `log_level` setting; other crates log warnings and errors only.
    pub log_level: Option<LogLevel>,
    /// Per-target levels in `tracing-subscriber`'s `Targets` syntax that
    /// replace the filter a level sets, so other crates can log too,
    /// overriding the `log_level` setting.
    pub log_filter: Option<String>,
    /// The format of the records on standard error, or `None` to leave it
    /// to [`log_format`].
    pub log_format: Option<LogFormat>,
    /// Whether DHT may find peers; `false` overrides the `dht_enabled`
    /// setting.
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
            listen: None,
            allowed_hosts: Vec::new(),
            peer_port: PEER_PORT_DEFAULT,
            log_level: None,
            log_filter: None,
            log_format: None,
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

    /// The settings this command line overrides for the run.
    pub fn overrides(&self) -> Overrides {
        Overrides {
            dht_enabled: (!self.dht).then_some(false),
            log_level: if self.log_filter.is_some() {
                Some(LogOverride::Custom)
            } else {
                self.log_level.map(LogOverride::Level)
            },
        }
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
    /// Change the password of the stopped application whose data directory
    /// the configuration names.
    Password(PasswordChange, Config),
    /// Back up or restore the stopped application whose data directory the
    /// configuration names.
    Backup(BackupAction, Config),
    /// Serve the MCP server of `docs/mcp.md`.
    Mcp(McpConfig),
    /// Print [`USAGE`].
    Help,
    /// Print the version.
    Version,
}

/// The options of `torrplay mcp`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct McpConfig {
    /// The application's origin, overriding [`URL_VARIABLE`].
    pub url: Option<Url>,
    /// The Streamable HTTP listener's address, with `--transport http`;
    /// `None` serves standard input and output.
    pub http: Option<SocketAddr>,
    /// The most verbose level the server logs, to standard error.
    pub log_level: Option<LogLevel>,
}

/// The environment variable `torrplay mcp` reads the application's origin
/// from.
pub const URL_VARIABLE: &str = "TORRPLAY_URL";

/// The environment variable `torrplay mcp` reads its access token from.
/// There is no option for it, since arguments are visible to every user.
pub const TOKEN_VARIABLE: &str = "TORRPLAY_TOKEN";

/// The application's origin `value`, if it is an HTTP or HTTPS URL without
/// a path, query, or credentials.
pub fn parse_origin(value: &str) -> Option<Url> {
    let url = Url::parse(value).ok()?;
    (matches!(url.scheme(), "http" | "https")
        && url.has_host()
        && url.username().is_empty()
        && url.password().is_none()
        && url.path() == "/"
        && url.query().is_none()
        && url.fragment().is_none())
    .then_some(url)
}

fn parse_backup(mut args: impl Iterator<Item = String>) -> Result<Command, ArgsError> {
    let create = match args.next().as_deref() {
        Some("create") => true,
        Some("restore") => false,
        Some("-h" | "--help") => return Ok(Command::Help),
        _ => return Err(ArgsError::BackupUsage),
    };
    let mut config = Config::default();
    let mut file = None;
    while let Some(arg) = args.next() {
        let (option, inline) = match arg.split_once('=') {
            Some((option, value)) if option.starts_with("--") => {
                (option.to_owned(), Some(value.to_owned()))
            }
            _ => (arg, None),
        };
        let mut path = |reason| {
            let value = inline
                .clone()
                .or_else(|| args.next())
                .ok_or_else(|| ArgsError::MissingValue(option.clone()))?;
            if value.is_empty() {
                return Err(ArgsError::InvalidValue {
                    option: option.clone(),
                    value,
                    reason,
                });
            }
            Ok(PathBuf::from(value))
        };
        match option.as_str() {
            "-h" | "--help" => return Ok(Command::Help),
            "--data-dir" => config.data_dir = Some(path("a directory")?),
            "--output" if create && file.is_none() => file = Some(path("a file")?),
            name if !create && file.is_none() && !name.starts_with('-') && !name.is_empty() => {
                file = Some(PathBuf::from(name));
            }
            _ => return Err(ArgsError::BackupUsage),
        }
    }
    let action = if create {
        BackupAction::Create { output: file }
    } else {
        BackupAction::Restore {
            archive: file.ok_or(ArgsError::BackupUsage)?,
        }
    };
    Ok(Command::Backup(action, config))
}

fn parse_mcp(args: impl Iterator<Item = String>) -> Result<Command, ArgsError> {
    let mut config = McpConfig::default();
    let mut http = false;
    let mut listen = None;
    let mut args = args.peekable();
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
            "--url" => {
                let value = value()?;
                config.url =
                    Some(parse_origin(&value).ok_or_else(|| {
                        invalid(value, "an origin such as http://127.0.0.1:8642")
                    })?);
            }
            "--transport" => match value()?.as_str() {
                "stdio" => http = false,
                "http" => http = true,
                other => return Err(invalid(other.to_owned(), "stdio or http")),
            },
            "--listen" => {
                let value = value()?;
                let address: SocketAddr = value
                    .parse()
                    .map_err(|_| invalid(value.clone(), "a loopback IP address and port"))?;
                if !address.ip().is_loopback() {
                    return Err(invalid(value, "a loopback IP address and port"));
                }
                listen = Some(address);
            }
            "--log-level" => {
                let value = value()?;
                config.log_level =
                    Some(LogLevel::parse(&value.to_ascii_lowercase()).ok_or_else(|| {
                        invalid(value.clone(), "error, warn, info, debug, or trace")
                    })?);
            }
            _ => return Err(ArgsError::Unknown(option)),
        }
    }
    if http {
        config.http = Some(listen.unwrap_or(torrplay_mcp::LISTEN_DEFAULT));
    } else if listen.is_some() {
        return Err(ArgsError::McpListen);
    }
    Ok(Command::Mcp(config))
}

/// What `torrplay backup` does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BackupAction {
    /// Write a backup to `output`, or to a file named after the time in the
    /// current directory.
    Create {
        /// The archive to write, which must not exist.
        output: Option<PathBuf>,
    },
    /// Restore the archive at `archive`.
    Restore {
        /// The archive to read.
        archive: PathBuf,
    },
}

/// A change `torrplay password` makes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PasswordChange {
    /// Set a new password, read from standard input.
    Set,
    /// Remove the password and revoke every token.
    Remove,
}

/// The format of the log records on standard error.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LogFormat {
    /// Lines for people, which may change between releases.
    Text,
    /// One JSON object per record, as `docs/observability.md` defines it.
    Json,
}

impl LogFormat {
    /// Reads a format's name.
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "text" => Some(Self::Text),
            "json" => Some(Self::Json),
            _ => None,
        }
    }
}

/// The log format: `chosen`, from `--log-format`, or else `variable`, the
/// value of [`LOG_FORMAT_VARIABLE`], or else text when standard error is a
/// `terminal` and JSON otherwise, so a container or a service gets JSON.
pub fn log_format(
    chosen: Option<LogFormat>,
    variable: Option<String>,
    terminal: bool,
) -> Result<LogFormat, ArgsError> {
    if let Some(format) = chosen {
        return Ok(format);
    }
    match variable {
        Some(value) => LogFormat::parse(&value).ok_or(ArgsError::InvalidValue {
            option: LOG_FORMAT_VARIABLE.to_owned(),
            value,
            reason: "text or json",
        }),
        None if terminal => Ok(LogFormat::Text),
        None => Ok(LogFormat::Json),
    }
}

/// A command line the application cannot run with.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum ArgsError {
    /// An option the application does not know.
    #[error("unknown option {0}")]
    Unknown(String),
    /// A `password` command other than `set` and `remove`, or an option
    /// it does not take.
    #[error("torrplay password takes set or remove, and only --data-dir")]
    PasswordUsage,
    /// A `backup` command other than `create` and `restore`, or an option or
    /// argument it does not take.
    #[error("torrplay backup takes create with --output or restore with a file, and --data-dir")]
    BackupUsage,
    /// `--listen` for `torrplay mcp` without `--transport http`.
    #[error("torrplay mcp takes --listen only with --transport http")]
    McpListen,
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

/// Parses the command line's arguments, without the program name, with the
/// value of [`LISTEN_VARIABLE`], which `--listen` overrides.
pub fn parse_args(
    args: impl IntoIterator<Item = String>,
    listen_variable: Option<String>,
) -> Result<Command, ArgsError> {
    let mut config = Config::default();
    if let Some(value) = listen_variable {
        config.listen = Some(value.parse().map_err(|_| ArgsError::InvalidValue {
            option: LISTEN_VARIABLE.to_owned(),
            value,
            reason: "an IP address and port",
        })?);
    }
    let mut args = args.into_iter().peekable();
    if args.peek().map(String::as_str) == Some("mcp") {
        args.next();
        return parse_mcp(args);
    }
    if args.peek().map(String::as_str) == Some("backup") {
        args.next();
        return parse_backup(args);
    }
    let password = if args.peek().map(String::as_str) == Some("password") {
        args.next();
        match args.next().as_deref() {
            Some("set") => Some(PasswordChange::Set),
            Some("remove") => Some(PasswordChange::Remove),
            Some("-h" | "--help") => return Ok(Command::Help),
            _ => return Err(ArgsError::PasswordUsage),
        }
    } else {
        None
    };
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
        if password.is_some() && !matches!(option.as_str(), "--data-dir" | "-h" | "--help") {
            return Err(ArgsError::PasswordUsage);
        }
        match option.as_str() {
            "-h" | "--help" => return Ok(Command::Help),
            "-V" | "--version" => return Ok(Command::Version),
            "--no-dht" if inline.is_none() => config.dht = false,
            "--listen" => {
                let value = value()?;
                config.listen = Some(
                    value
                        .parse()
                        .map_err(|_| invalid(value.clone(), "an IP address and port"))?,
                );
            }
            "--allowed-host" => {
                let value = value()?;
                let name = AllowedHosts::normalize(&value)
                    .ok_or_else(|| invalid(value, "a DNS name such as torrplay.home.arpa"))?;
                config.allowed_hosts.push(name);
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
                config.log_level =
                    Some(LogLevel::parse(&value.to_ascii_lowercase()).ok_or_else(|| {
                        invalid(value.clone(), "error, warn, info, debug, or trace")
                    })?);
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
            "--log-format" => {
                let value = value()?;
                config.log_format = Some(
                    LogFormat::parse(&value)
                        .ok_or_else(|| invalid(value.clone(), "text or json"))?,
                );
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
    Ok(match password {
        Some(change) => Command::Password(change, config),
        None => Command::Run(config),
    })
}

/// What [`change_password`] did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PasswordChanged {
    /// A password was set where none was, turning authentication on.
    Set,
    /// The password was replaced; access tokens keep working.
    Replaced,
    /// The password was removed and every token revoked.
    Removed,
    /// There was no password to remove.
    NoneSet,
}

/// Changes the password of the stopped application whose database is in
/// `data_dir`: sets `password`, the first line of the input, for
/// [`PasswordChange::Set`], or removes it and revokes every token. Fails if
/// the application is running, since it holds the database's lock.
pub async fn change_password(
    data_dir: &Path,
    change: PasswordChange,
    password: Option<String>,
) -> anyhow::Result<PasswordChanged> {
    use torrplay_core::credentials::{PasswordHash, SigningKey, normalize_password};

    let hash = match (change, password) {
        (PasswordChange::Set, Some(password)) => {
            let password = normalize_password(&password)
                .map_err(|error| anyhow::anyhow!("the password is invalid: {error}"))?;
            Some(
                tokio::task::spawn_blocking(move || PasswordHash::new(&password))
                    .await
                    .context("hashing the password")??,
            )
        }
        (PasswordChange::Set, None) => anyhow::bail!("no password was given on standard input"),
        (PasswordChange::Remove, _) => None,
    };
    let opening = data_dir.to_path_buf();
    let (store, contents) = tokio::task::spawn_blocking(move || Store::open(&opening))
        .await
        .context("opening the store")?
        .with_context(|| format!("opening the store in {}", data_dir.display()))?;
    let enabled = contents.credentials.password.is_some();
    let changed = match hash {
        Some(hash) => {
            store
                .set_password(&hash, std::time::SystemTime::now(), false)
                .await?;
            if enabled {
                PasswordChanged::Replaced
            } else {
                PasswordChanged::Set
            }
        }
        None if enabled => {
            store.remove_password(&SigningKey::new()?).await?;
            PasswordChanged::Removed
        }
        None => PasswordChanged::NoneSet,
    };
    store.close().await.context("closing the store")?;
    Ok(changed)
}

/// Writes a backup of the stopped application whose database is in
/// `data_dir` to `output`, or to a file named after `now` in the current
/// directory, returning its path. Fails if the application is running,
/// since it holds the database's lock, or if the file exists; a failure
/// deletes the partly written file.
pub async fn create_backup(
    data_dir: &Path,
    output: Option<&Path>,
    now: std::time::SystemTime,
) -> anyhow::Result<(PathBuf, BackupCounts)> {
    // Opening the store would create an empty database.
    let database = data_dir.join(torrplay_store::FILE_NAME);
    anyhow::ensure!(
        database.try_exists().unwrap_or(true),
        "{} holds no database to back up",
        data_dir.display()
    );
    let output = output.map_or_else(
        || PathBuf::from(torrplay_server::backup_file_name(now)),
        Path::to_path_buf,
    );
    let (store, _) = open_store(data_dir).await?;
    let file = std::fs::File::create_new(&output)
        .with_context(|| format!("creating {}", output.display()));
    let written = match file {
        Ok(file) => {
            let written = torrplay_server::create_backup(&store, file, now).await;
            if written.is_err() {
                // The file was created above, so it is this backup's.
                let _ = std::fs::remove_file(&output);
            }
            written.map_err(anyhow::Error::from)
        }
        Err(error) => Err(error),
    };
    store.close().await.context("closing the store")?;
    Ok((output, written?))
}

/// Restores the archive at `archive` to the stopped application whose
/// database is in `data_dir`, as `PUT /api/v1/backup` does: the archive is
/// checked whole before every record but the credentials is replaced. The
/// next start adds the restored torrents and deletes the disk data of those
/// no longer kept on disk. Fails if the application is running.
pub async fn restore_backup(data_dir: &Path, archive: &Path) -> anyhow::Result<BackupCounts> {
    anyhow::ensure!(archive.is_file(), "{} is not a file", archive.display());
    let (store, contents) = open_store(data_dir).await?;
    let restored = torrplay_server::restore_backup(
        &store,
        &contents,
        archive,
        torrplay_engine::inspect_source,
    )
    .await;
    store.close().await.context("closing the store")?;
    Ok(restored?)
}

async fn open_store(data_dir: &Path) -> anyhow::Result<(Store, Contents)> {
    let opening = data_dir.to_path_buf();
    tokio::task::spawn_blocking(move || Store::open(&opening))
        .await
        .context("opening the store")?
        .with_context(|| format!("opening the store in {}", data_dir.display()))
}

/// How long exiting and a panic wait for queued records to reach the log
/// files.
const LOG_FLUSH_LIMIT: Duration = Duration::from_secs(2);

/// Changes the level of TorrPlay's own log messages after
/// [`init_logging`], and flushes the log files.
#[derive(Clone)]
pub struct LogControl {
    set_level: Arc<dyn Fn(LogLevel) + Send + Sync>,
    files: Option<Arc<LogWriter>>,
}

impl LogControl {
    /// Records TorrPlay's own messages at `level` and more severe, and
    /// other crates' warnings and errors.
    pub fn set_level(&self, level: LogLevel) {
        (self.set_level)(level);
    }

    /// Waits a bounded time for the records logged so far to reach the log
    /// files, as exiting does.
    pub fn flush(&self) {
        if let Some(files) = &self.files {
            files.flush(LOG_FLUSH_LIMIT);
        }
    }
}

impl std::fmt::Debug for LogControl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("LogControl")
    }
}

/// The filter for TorrPlay's own messages at `level` and other crates'
/// warnings and errors.
fn level_filter(level: LogLevel) -> tracing_subscriber::filter::Targets {
    use tracing_subscriber::filter::{LevelFilter, Targets};

    let level = match level {
        LogLevel::Error => LevelFilter::ERROR,
        LogLevel::Warn => LevelFilter::WARN,
        LogLevel::Info => LevelFilter::INFO,
        LogLevel::Debug => LevelFilter::DEBUG,
        LogLevel::Trace => LevelFilter::TRACE,
    };
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
}

/// Records logs to standard error in `format`, and to `files` when they
/// are open: TorrPlay's own at `level` and more severe, other crates'
/// warnings and errors only, unless `filter`, already validated by
/// [`parse_args`], sets every target's level instead. Records carry `run`'s
/// ID and are counted in its counts. A panic is logged as an error and
/// flushed to the files before the process ends. The returned control
/// changes the level later and flushes the files.
pub fn init_logging(
    level: LogLevel,
    filter: Option<&str>,
    format: LogFormat,
    run: &Run,
    files: Option<(LogFiles, LogWriter)>,
) -> LogControl {
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::reload;
    use tracing_subscriber::util::SubscriberInitExt;

    let filter = filter
        .and_then(|filter| filter.parse().ok())
        .unwrap_or_else(|| level_filter(level));
    let (filter, handle) = reload::Layer::new(filter);
    let files = files.map(|(files, writer)| (files, Arc::new(writer)));
    let writer = files.as_ref().map(|(_, writer)| Arc::clone(writer));
    let (text, json) = match format {
        LogFormat::Text => (
            Some(tracing_subscriber::fmt::layer().with_writer(std::io::stderr)),
            None,
        ),
        LogFormat::Json => (None, Some(std::io::stderr as fn() -> io::Stderr)),
    };
    let records =
        (json.is_some() || files.is_some()).then(|| logging::JsonLayer::new(run.id(), json, files));
    tracing_subscriber::registry()
        .with(text)
        .with(records)
        .with(logging::CountingLayer(run.log_records()))
        .with(filter)
        .init();
    if let Some(writer) = &writer {
        let writer = Arc::clone(writer);
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            tracing::error!(panic = %info, "the application panicked");
            writer.flush(LOG_FLUSH_LIMIT);
            previous(info);
        }));
    }
    LogControl {
        set_level: Arc::new(move |level| {
            if let Err(error) = handle.reload(level_filter(level)) {
                tracing::warn!(%error, "the log level could not be changed");
            }
        }),
        files: writer,
    }
}

/// The bounds of the shutdown sequence.
#[derive(Clone, Copy, Debug)]
pub struct ShutdownTimings {
    /// How long streams in progress may continue, for tests; `None` takes
    /// the drain period setting.
    pub drain: Option<Duration>,
    /// How long the whole sequence may take.
    pub timeout: Duration,
}

impl Default for ShutdownTimings {
    fn default() -> Self {
        Self {
            drain: None,
            timeout: SHUTDOWN_TIMEOUT_DEFAULT,
        }
    }
}

/// What [`serve`] serves beyond the store's contents.
#[derive(Clone, Debug, Default)]
pub struct ServeOptions {
    /// The host names requests may give beyond those always allowed.
    pub allowed_hosts: AllowedHosts,
    /// The settings the command line overrides for the run.
    pub overrides: Overrides,
    /// Sets the level of the application's own logs to the `log_level`
    /// setting while no override sets it; `None` leaves logging alone, as
    /// tests do.
    pub log: Option<LogControl>,
    /// The process run, whose ID the system resource and log records give.
    pub run: Run,
    /// The log files the log records are read from, if they are written.
    pub log_files: Option<LogFiles>,
}

/// Serves the API on `listener` over `engine`, which must not be started
/// yet, and `store` with what it held, with `options`, until `shutdown`
/// completes, then shuts down in the lifecycle's order. The DLNA media
/// server is announced while its setting is on, and says goodbye before
/// the listener closes.
///
/// The stored settings apply at once. The listener serves before the engine
/// starts, and the stored torrents are restored once it has. Fails if the engine cannot start, if restoring or
/// the server fails, or if shutdown takes longer than `timings.timeout`.
pub async fn serve(
    listener: TcpListener,
    engine: Arc<LibrqbitEngine>,
    store: Store,
    contents: Contents,
    options: ServeOptions,
    timings: ShutdownTimings,
    shutdown: impl Future<Output = ()>,
) -> anyhow::Result<()> {
    let Contents {
        torrents,
        settings,
        profiles,
        progress,
        titles,
        temporary_progress,
        dismissals,
        credentials,
        media_server_udn,
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
    // Temporary files of backups that a crash left.
    if let Some(directory) = store.path().parent().map(Path::to_path_buf) {
        match tokio::task::spawn_blocking(move || delete_temporary_files(&directory)).await {
            Ok(Ok(deleted)) => {
                for path in deleted {
                    tracing::info!(path = %path.display(), "deleted a backup's temporary file");
                }
            }
            Ok(Err(error)) => {
                tracing::warn!(%error, "backups' temporary files could not be deleted")
            }
            Err(error) => tracing::warn!(%error, "deleting backups' temporary files panicked"),
        }
    }
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
    let app = Arc::new(
        with_client(App::new(engine.clone(), store))
            .with_overrides(options.overrides)
            .with_settings(settings)
            .with_viewing(profiles, progress, titles, temporary_progress, dismissals)
            .with_credentials(credentials)
            .with_allowed_hosts(options.allowed_hosts)
            .with_run(options.run)
            .with_log_files(options.log_files)
            .with_media_server(Device {
                uuid: media_server_udn.clone(),
                version: env!("CARGO_PKG_VERSION").to_owned(),
            }),
    );
    if let Ok(address) = listener.local_addr()
        && !address.ip().is_loopback()
        && !app.authentication_enabled()
    {
        tracing::warn!(
            %address,
            "authentication is off: anyone on a local network may use the API, and other \
             clients are refused until a password is set"
        );
    }
    let listen = listener.local_addr()?;
    // Deletes unwatched temporary torrents until shutdown.
    let mut retention = app.spawn_retention();
    // Saves the progress inferred from DLNA playback until shutdown.
    let mut inference = app.spawn_inference();
    let (stop_media_server, media_server_stopped) = oneshot::channel::<()>();
    let notifying = Arc::clone(&app);
    let media_server = tokio::spawn(media_server::supervise(
        app.media_server_setting(),
        media_server_udn,
        listen,
        media_server_stopped,
        move || {
            let app = Arc::clone(&notifying);
            async move { app.notify_media_server_subscribers().await }
        },
    ));
    let (close_listener, listener_closed) = oneshot::channel::<()>();
    let mut server = tokio::spawn(
        axum::serve(
            listener,
            torrplay_server::router(app.clone())
                .into_make_service_with_connect_info::<SocketAddr>(),
        )
        .with_graceful_shutdown(async {
            // A dropped sender closes the listener too.
            let _ = listener_closed.await;
        })
        .into_future(),
    );
    let mut shutdown = std::pin::pin!(shutdown);
    // The log level follows its setting unless the command line sets it.
    let logging = options
        .log
        .filter(|_| app.overrides().log_level.is_none())
        .map(|control| tokio::spawn(follow_log_level(app.log_level(), control)));
    // The engine starts with the DHT as the settings and overrides leave it.
    app.apply_dht().await.context("configuring the DHT")?;

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
        let _ = stop_media_server.send(());
        media_server
            .await
            .context("the DLNA media server's task panicked")?;
        let _ = close_listener.send(());
        if !server.is_finished() {
            server_result(server.await)?;
        }
        engine.shutdown().await?;
        app.record_clean_shutdown()
            .await
            .context("recording the clean shutdown")?;
        if let Some(logging) = &logging {
            logging.abort();
        }
        retention.abort();
        match (&mut retention).await {
            Ok(()) => {}
            Err(error) if error.is_cancelled() => {}
            Err(error) => {
                return Err(anyhow::Error::new(error).context("the retention task panicked"));
            }
        }
        inference.abort();
        match (&mut inference).await {
            Ok(()) => {}
            Err(error) if error.is_cancelled() => {}
            Err(error) => {
                return Err(anyhow::Error::new(error).context("the inference task panicked"));
            }
        }
        app.close_store().await.context("closing the store")
    };
    let stopped = tokio::time::timeout(timings.timeout, sequence)
        .await
        .with_context(|| format!("shutdown did not finish within {:?}", timings.timeout))
        .and_then(|stopped| stopped);
    if stopped.is_ok() {
        tracing::info!("stopped");
    }
    result.and(stopped)
}

/// Sets the log level to the setting's value, now and whenever it changes,
/// until the application's state is dropped.
async fn follow_log_level(mut level: tokio::sync::watch::Receiver<LogLevel>, control: LogControl) {
    loop {
        control.set_level(*level.borrow_and_update());
        if level.changed().await.is_err() {
            return;
        }
    }
}

/// Waits until no stream is in progress, for at most `period`.
async fn drain(engine: &LibrqbitEngine, period: Duration) {
    let streams = || {
        engine
            .playback()
            .map(|playback| playback.stats())
            .map_or(0, |stats| stats.readers.saturating_sub(stats.idle_readers))
    };
    let streaming = || streams() > 0;
    let active = streams();
    if active == 0 {
        return;
    }
    // A player left open keeps its stream going, so say why stopping waits.
    tracing::info!(
        streams = active,
        drain_seconds = period.as_secs(),
        "waiting for streams in progress to finish"
    );
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

/// Serves the embedded web client, when this build has one.
fn with_client(app: App<LibrqbitEngine>) -> App<LibrqbitEngine> {
    #[cfg(feature = "client")]
    let app = app.with_client_files(client::files());
    app
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Command, ArgsError> {
        parse_args(args.iter().map(|&arg| arg.to_owned()), None)
    }

    #[test]
    fn the_engines_defaults_are_the_settings_defaults() {
        let engine = EngineConfig::new(PathBuf::new(), NetworkConfig::isolated(Vec::new()));
        let settings = torrplay_core::settings::Settings::DEFAULT;
        assert_eq!(
            (
                engine.piece_wait,
                engine.idle_reader_timeout,
                engine.idle_reader_limit_per_torrent,
                engine.idle_reader_limit,
                engine.peer_limit,
                engine.cache_capacity_bytes,
            ),
            (
                settings.piece_wait,
                settings.idle_reader_timeout,
                settings.idle_reader_limit_per_torrent,
                settings.idle_reader_limit,
                settings.peer_limit,
                settings.cache_capacity_bytes,
            ),
        );
    }

    #[test]
    fn mcp_serves_stdio_unless_http_is_asked_for() {
        assert_eq!(parse(&["mcp"]), Ok(Command::Mcp(McpConfig::default())));
        let Ok(Command::Mcp(config)) = parse(&[
            "mcp",
            "--url=https://torrplay.home.arpa:8443",
            "--transport",
            "http",
            "--listen",
            "[::1]:9000",
            "--log-level",
            "DEBUG",
        ]) else {
            panic!("an mcp command");
        };
        assert_eq!(
            config.url.as_ref().map(Url::as_str),
            Some("https://torrplay.home.arpa:8443/")
        );
        assert_eq!(config.http, Some("[::1]:9000".parse().unwrap()));
        assert_eq!(config.log_level, Some(LogLevel::Debug));
        let Ok(Command::Mcp(config)) = parse(&["mcp", "--transport", "http"]) else {
            panic!("an mcp command");
        };
        assert_eq!(config.http, Some(torrplay_mcp::LISTEN_DEFAULT));
    }

    #[test]
    fn mcp_refuses_what_it_cannot_serve() {
        for args in [
            &["mcp", "--listen", "0.0.0.0:8643", "--transport", "http"][..],
            &["mcp", "--listen", "127.0.0.1:8643"],
            &["mcp", "--transport", "sse"],
            &["mcp", "--url", "ftp://127.0.0.1"],
            &["mcp", "--url", "http://127.0.0.1:8642/api/v1"],
            &["mcp", "--url", "http://user:secret@127.0.0.1:8642"],
            &["mcp", "--token", "atk"],
            &["mcp", "--data-dir", "/tmp"],
        ] {
            assert!(parse(args).is_err(), "{args:?}");
        }
    }

    #[test]
    fn no_arguments_run_with_the_defaults() {
        assert_eq!(parse(&[]), Ok(Command::Run(Config::default())));
        assert_eq!(Config::default().listen, None, "the default address");
        assert_eq!(LISTEN_DEFAULT, "[::]:8642".parse::<SocketAddr>().unwrap());
        assert_eq!(
            Config::default().download_dir(Path::new("/srv/torrplay")),
            Path::new("/srv/torrplay/downloads"),
            "the download directory defaults to one in the data directory"
        );
    }

    #[test]
    fn the_log_format_follows_the_option_then_the_variable_then_the_terminal() {
        let cases = [
            (Some(LogFormat::Text), Some("json"), false, LogFormat::Text),
            (None, Some("text"), false, LogFormat::Text),
            (None, Some("json"), true, LogFormat::Json),
            (None, None, true, LogFormat::Text),
            (None, None, false, LogFormat::Json),
        ];
        for (chosen, variable, terminal, expected) in cases {
            assert_eq!(
                log_format(chosen, variable.map(str::to_owned), terminal),
                Ok(expected),
                "{chosen:?} {variable:?} {terminal}"
            );
        }
        assert!(matches!(
            log_format(None, Some("yaml".into()), true),
            Err(ArgsError::InvalidValue { option, .. }) if option == LOG_FORMAT_VARIABLE
        ));
        assert!(matches!(
            parse(&["--log-format", "yaml"]),
            Err(ArgsError::InvalidValue { .. })
        ));
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
            "--log-format=json",
            "--no-dht",
        ]) else {
            panic!("the options parse");
        };
        assert_eq!(
            config,
            Config {
                listen: Some("[::1]:9000".parse().unwrap()),
                allowed_hosts: Vec::new(),
                peer_port: 6881,
                log_level: Some(LogLevel::Debug),
                log_filter: Some("librqbit=debug,torrplay_engine=trace".into()),
                log_format: Some(LogFormat::Json),
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
    fn the_api_may_listen_on_any_address() {
        for address in ["0.0.0.0:8642", "192.0.2.1:8642", "[::]:8642"] {
            let Ok(Command::Run(config)) = parse(&["--listen", address]) else {
                panic!("{address} parses");
            };
            assert_eq!(config.listen, Some(address.parse::<SocketAddr>().unwrap()));
        }
    }

    #[test]
    fn the_environment_names_the_address_unless_listen_does() {
        let run = |args: &[&str], variable: &str| {
            parse_args(
                args.iter().map(|&arg| arg.to_owned()),
                Some(variable.to_owned()),
            )
        };
        let Ok(Command::Run(config)) = run(&[], "127.0.0.1:9000") else {
            panic!("the variable parses");
        };
        assert_eq!(config.listen, Some("127.0.0.1:9000".parse().unwrap()));
        let Ok(Command::Run(config)) = run(&["--listen", "0.0.0.0:9001"], "127.0.0.1:9000") else {
            panic!("the option parses");
        };
        assert_eq!(
            config.listen,
            Some("0.0.0.0:9001".parse().unwrap()),
            "--listen wins"
        );
        assert_eq!(
            run(&[], "localhost"),
            Err(ArgsError::InvalidValue {
                option: LISTEN_VARIABLE.to_owned(),
                value: "localhost".into(),
                reason: "an IP address and port",
            })
        );
    }

    /// AUT-11.
    #[tokio::test]
    async fn an_unspecified_ipv6_address_takes_ipv4_clients_too() {
        let listener = match bind_api(Some("[::]:0".parse().unwrap())) {
            Ok(listener) => listener,
            // A system without IPv6 has nothing to check.
            Err(error) => {
                assert!(
                    bind_api(Some("0.0.0.0:0".parse().unwrap())).is_ok(),
                    "IPv4 binds: {error}"
                );
                return;
            }
        };
        let port = listener.local_addr().unwrap().port();
        let (accepted, connected) = tokio::join!(
            listener.accept(),
            tokio::net::TcpStream::connect((Ipv4Addr::LOCALHOST, port))
        );
        connected.unwrap();
        let (_, peer) = accepted.unwrap();
        assert_eq!(
            peer.ip().to_canonical(),
            IpAddr::V4(Ipv4Addr::LOCALHOST),
            "an IPv4 client arrives IPv4-mapped"
        );
    }

    #[test]
    fn allowed_hosts_repeat_and_are_normalized() {
        let Ok(Command::Run(config)) = parse(&[
            "--allowed-host",
            "TorrPlay.Home.Arpa.",
            "--allowed-host=nas.lan",
        ]) else {
            panic!("the hosts parse");
        };
        assert_eq!(config.allowed_hosts, ["torrplay.home.arpa", "nas.lan"]);
        assert!(matches!(
            parse(&["--allowed-host", "evil example"]),
            Err(ArgsError::InvalidValue { .. })
        ));
    }

    #[test]
    fn overriding_options_override_their_settings() {
        assert_eq!(Config::default().overrides(), Overrides::default());
        let Ok(Command::Run(config)) = parse(&["--no-dht", "--log-level", "TRACE"]) else {
            panic!("the options parse");
        };
        assert_eq!(
            config.overrides(),
            Overrides {
                dht_enabled: Some(false),
                log_level: Some(LogOverride::Level(LogLevel::Trace)),
            }
        );
        let Ok(Command::Run(config)) =
            parse(&["--log-level=warn", "--log-filter", "librqbit=debug"])
        else {
            panic!("the options parse");
        };
        assert_eq!(
            config.overrides().log_level,
            Some(LogOverride::Custom),
            "a filter sets levels per component"
        );
    }

    #[test]
    fn a_log_level_applies_to_torrplay_alone() {
        use tracing::Level;

        let debug = level_filter(LogLevel::Debug);
        assert!(debug.would_enable("torrplay_server::settings", &Level::DEBUG));
        assert!(!debug.would_enable("torrplay_server", &Level::TRACE));
        assert!(!debug.would_enable("librqbit::session", &Level::INFO));
        assert!(debug.would_enable("librqbit::session", &Level::WARN));
        let error = level_filter(LogLevel::Error);
        assert!(!error.would_enable("torrplay_engine", &Level::WARN));
    }

    #[test]
    fn the_media_server_name_is_no_option() {
        assert_eq!(
            parse(&["--dlna-name", "Living room"]),
            Err(ArgsError::Unknown("--dlna-name".into()))
        );
    }

    #[test]
    fn password_commands_take_only_a_data_directory() {
        let Ok(Command::Password(PasswordChange::Set, config)) =
            parse(&["password", "set", "--data-dir", "/srv/torrplay"])
        else {
            panic!("the command parses");
        };
        assert_eq!(config.data_dir, Some("/srv/torrplay".into()));
        assert!(matches!(
            parse(&["password", "remove"]),
            Ok(Command::Password(PasswordChange::Remove, _))
        ));
        assert_eq!(parse(&["password", "--help"]), Ok(Command::Help));
        for args in [
            &["password"][..],
            &["password", "show"],
            &["password", "set", "--listen", "0.0.0.0:80"],
        ] {
            assert_eq!(parse(args), Err(ArgsError::PasswordUsage), "{args:?}");
        }
    }

    #[test]
    fn backup_commands_take_a_file_and_a_data_directory() {
        assert_eq!(
            parse(&["backup", "create", "--data-dir", "/srv/torrplay"]),
            Ok(Command::Backup(
                BackupAction::Create { output: None },
                Config {
                    data_dir: Some("/srv/torrplay".into()),
                    ..Config::default()
                }
            ))
        );
        assert!(matches!(
            parse(&["backup", "create", "--output=saved.zip"]),
            Ok(Command::Backup(BackupAction::Create { output: Some(output) }, _))
                if output == Path::new("saved.zip")
        ));
        assert!(matches!(
            parse(&["backup", "restore", "saved.zip", "--data-dir", "/srv/torrplay"]),
            Ok(Command::Backup(BackupAction::Restore { archive }, _))
                if archive == Path::new("saved.zip")
        ));
        assert_eq!(parse(&["backup", "--help"]), Ok(Command::Help));
        for args in [
            &["backup"][..],
            &["backup", "show"],
            &["backup", "restore"],
            &["backup", "restore", "a.zip", "b.zip"],
            &["backup", "restore", "--output", "a.zip"],
            &["backup", "create", "saved.zip"],
            &["backup", "create", "--listen", "0.0.0.0:80"],
        ] {
            assert_eq!(parse(args), Err(ArgsError::BackupUsage), "{args:?}");
        }
        assert!(matches!(
            parse(&["backup", "create", "--output="]),
            Err(ArgsError::InvalidValue { .. })
        ));
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
