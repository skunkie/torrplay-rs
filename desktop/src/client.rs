// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! The service edition's client: the window on the page of the TorrPlay service that
//! runs on this computer, with no engine of its own. Closing it leaves the service
//! running.

use std::fs::File;
use std::io::Write;
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Mutex;
use std::time::Duration;

use anyhow::Context;
use tauri::{AppHandle, Manager, Url};
use tauri_plugin_dialog::{DialogExt, MessageDialogKind};

use crate::opening::{self, Request};
use crate::owner;
use crate::window::{self, Notice, Pages};

/// The variable that gives the service its listen address, which tells the client
/// where to find it. The service's default is every interface on port 8642.
const LISTEN_VARIABLE: &str = "TORRPLAY_LISTEN";
const LISTEN_DEFAULT: &str = "[::]:8642";
/// The most the client writes to its log in a run, which starts it afresh.
const LOG_MAX_BYTES: u64 = 1 << 20;
/// How long the service may take to accept a connection before it counts as not
/// answering. One on this computer accepts at once, or refuses at once.
const ANSWER_TIMEOUT: Duration = Duration::from_secs(2);

const USAGE: &str = "\
Usage: torrplay-desktop [TORRENT_FILE | MAGNET_URI]...

Opens the page of the TorrPlay service on this computer, at the port of the
listen address its settings file gives in TORRPLAY_LISTEN [default: [::]:8642]:
/etc/torrplay/torrplay.env, or %ProgramData%\\TorrPlay\\torrplay.env on Windows.
TORRPLAY_LISTEN in the client's own environment takes its place. A torrent file
or magnet URI opens on the add page, in the running client if there is one.

Options:
  -h, --help     Print help
  -V, --version  Print the version
";

/// What the command line asks for.
#[derive(Debug, PartialEq)]
enum Command {
    Open(Vec<Request>),
    Help,
    Version,
}

fn parse(arguments: impl IntoIterator<Item = String>, directory: &Path) -> Result<Command, String> {
    let (options, requests) = opening::split_arguments(arguments, directory, &[]);
    match options.first().map(String::as_str) {
        None => Ok(Command::Open(requests)),
        Some("-h" | "--help") => Ok(Command::Help),
        Some("-V" | "--version") => Ok(Command::Version),
        Some(other) => Err(format!("unexpected argument '{other}'")),
    }
}

/// The service's settings file, which its service manager reads as its environment.
fn settings_path() -> Option<PathBuf> {
    if cfg!(windows) {
        std::env::var_os("ProgramData")
            .map(|data| PathBuf::from(data).join("TorrPlay").join("torrplay.env"))
    } else {
        Some(PathBuf::from("/etc/torrplay/torrplay.env"))
    }
}

/// The listen address `settings`, the contents of a settings file, gives: its last
/// `TORRPLAY_LISTEN=` line, quoted or not, as a service manager reads it.
fn listen_in(settings: &str) -> Option<String> {
    settings
        .lines()
        .filter_map(|line| {
            let value = line
                .trim()
                .strip_prefix(LISTEN_VARIABLE)?
                .strip_prefix('=')?;
            let value = value.trim();
            let unquoted = ['"', '\'']
                .iter()
                .find_map(|quote| value.strip_prefix(*quote)?.strip_suffix(*quote))
                .unwrap_or(value);
            Some(unquoted.to_owned())
        })
        .next_back()
}

/// The service's listen address, with where it came from for messages: `own`, the
/// client's own `TORRPLAY_LISTEN`, or else the one in the settings file at `settings`,
/// or else the default. A settings file that is there but cannot be read is an error,
/// since its address would otherwise give way to the default unseen.
fn service_listen(
    own: Option<String>,
    settings: Option<&Path>,
) -> anyhow::Result<(String, String)> {
    if let Some(own) = own {
        return Ok((own, "the client's own environment".to_owned()));
    }
    if let Some(path) = settings {
        match std::fs::read_to_string(path) {
            Ok(contents) => {
                if let Some(listen) = listen_in(&contents) {
                    return Ok((listen, path.display().to_string()));
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error).with_context(|| {
                    format!(
                        "the service's settings file, {}, cannot be read",
                        path.display()
                    )
                });
            }
        }
    }
    Ok((LISTEN_DEFAULT.to_owned(), "the default".to_owned()))
}

/// The service's page and the address it is at, from its listen address, `listen`.
fn service_url(listen: &str) -> anyhow::Result<(Url, SocketAddr)> {
    let listening: SocketAddr = listen
        .parse()
        .with_context(|| format!("{LISTEN_VARIABLE} is not an address and port: {listen}"))?;
    let url = window::window_url(listening)?;
    let address = url
        .socket_addrs(|| None)
        .ok()
        .and_then(|addresses| addresses.into_iter().next())
        .context("the service's page has no address")?;
    Ok((url, address))
}

/// Whether something accepts connections at `address`.
fn answers(address: SocketAddr) -> bool {
    TcpStream::connect_timeout(&address, ANSWER_TIMEOUT).is_ok()
}

/// How to start the service, in the system's language.
fn how_to_start(locale: Option<&str>) -> &'static str {
    let russian = locale.is_some_and(|locale| locale.starts_with("ru"));
    if cfg!(target_os = "linux") {
        if russian {
            "Запустите её командой: sudo systemctl start torrplay"
        } else {
            "Start it with: sudo systemctl start torrplay"
        }
    } else if cfg!(windows) {
        if russian {
            "Запустите её в окне «Службы» или командой: sc start torrplay"
        } else {
            "Start it in Services, or with: sc start torrplay"
        }
    } else if russian {
        "Запустите службу TorrPlay на этом компьютере."
    } else {
        "Start the TorrPlay service on this computer."
    }
}

/// Why the client does not open what answers at the service's address. On Linux a
/// program of the user's own would have passed, so there the message says so.
fn not_the_service(locale: Option<&str>) -> &'static str {
    let russian = locale.is_some_and(|locale| locale.starts_with("ru"));
    match (cfg!(target_os = "linux"), russian) {
        (true, true) => {
            "Её запустила не служба TorrPlay и не вы, поэтому клиент не открывает её: \
             страница могла бы прочитать ваш вход в TorrPlay."
        }
        (true, false) => {
            "Neither the TorrPlay service nor you started it, so the client does not open \
             it: its page could read your TorrPlay sign-in."
        }
        (false, true) => {
            "Это не служба TorrPlay, поэтому клиент не открывает её: страница могла бы \
             прочитать ваш вход в TorrPlay."
        }
        (false, false) => {
            "It is not the TorrPlay service, so the client does not open it: its page \
             could read your TorrPlay sign-in."
        }
    }
}

/// Logs `message` and shows it under `notice`, then ends the client, since nothing
/// shows standard error to someone who opened it from a menu.
fn fail(app: &AppHandle, notice: Notice, message: &str) {
    tracing::error!(message, "the client cannot go on");
    eprintln!("torrplay-desktop: {message}");
    app.dialog()
        .message(message)
        .title(window::notice_text(
            notice,
            sys_locale::get_locale().as_deref(),
        ))
        .kind(MessageDialogKind::Error)
        .blocking_show();
    app.exit(1);
}

/// Opens the service's page, or says why it cannot.
fn open(app: &AppHandle) {
    let own = std::env::var(LISTEN_VARIABLE).ok();
    let found = service_listen(own, settings_path().as_deref()).and_then(|(listen, source)| {
        let (url, address) =
            service_url(&listen).with_context(|| format!("{LISTEN_VARIABLE} from {source}"))?;
        Ok((url, address, source))
    });
    let (url, address, source) = match found {
        Ok(found) => found,
        Err(error) => return fail(app, Notice::ClientCouldNotStart, &format!("{error:#}")),
    };
    let locale = sys_locale::get_locale();
    if !answers(address) {
        let message = format!("{url} ({source})\n\n{}", how_to_start(locale.as_deref()));
        return fail(app, Notice::ServiceNotAnswering, &message);
    }
    match owner::trusted(address) {
        Ok(true) => {}
        Ok(false) => {
            let message = format!("{url}\n\n{}", not_the_service(locale.as_deref()));
            return fail(app, Notice::NotTheService, &message);
        }
        Err(error) => {
            let error = error.context("checking who listens at the service's address");
            return fail(app, Notice::ClientCouldNotStart, &format!("{error:#}"));
        }
    }
    tracing::info!(%url, source, "opening the service's page");
    if let Err(error) = window::open_window(app, url) {
        fail(app, Notice::ClientCouldNotStart, &format!("{error:#}"));
    }
}

/// A file that takes at most `limit` bytes, and drops what comes after.
struct Capped {
    file: File,
    left: u64,
}

impl Write for Capped {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let taken = bytes
            .len()
            .min(usize::try_from(self.left).unwrap_or(usize::MAX));
        self.file.write_all(&bytes[..taken])?;
        self.left -= taken as u64;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.file.flush()
    }
}

/// Logs to standard error and to `client.log` in the client's log directory, which
/// each run starts afresh, so what went wrong last is there to read.
fn start_logging(app: &AppHandle) {
    use tracing_subscriber::fmt::writer::MakeWriterExt;
    let file = app.path().app_log_dir().and_then(|directory| {
        std::fs::create_dir_all(&directory)?;
        Ok(File::create(directory.join("client.log"))?)
    });
    let builder = tracing_subscriber::fmt().with_ansi(false);
    let started = match file {
        Ok(file) => {
            let capped = Mutex::new(Capped {
                file,
                left: LOG_MAX_BYTES,
            });
            builder.with_writer(capped.and(std::io::stderr)).try_init()
        }
        Err(error) => {
            eprintln!("torrplay-desktop: the client's log cannot be written: {error}");
            builder.with_writer(std::io::stderr).try_init()
        }
    };
    if let Err(error) = started {
        eprintln!("torrplay-desktop: logging cannot start: {error}");
    }
}

/// The client's entry point.
pub fn main() -> ExitCode {
    let directory = std::env::current_dir().unwrap_or_default();
    let requests = match parse(std::env::args().skip(1), &directory) {
        Ok(Command::Open(requests)) => requests,
        Ok(Command::Help) => {
            print!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Ok(Command::Version) => {
            println!("torrplay-desktop {}", env!("CARGO_PKG_VERSION"));
            return ExitCode::SUCCESS;
        }
        Err(error) => {
            eprintln!("torrplay-desktop: {error}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    match run(requests) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("torrplay-desktop: {error:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(requests: Vec<Request>) -> anyhow::Result<()> {
    let app = tauri::Builder::default()
        // First, so a second launch ends before the rest of it starts anything.
        .plugin(tauri_plugin_single_instance::init(
            |app, arguments, directory| {
                let (_, requests) = opening::split_arguments(
                    arguments.into_iter().skip(1),
                    Path::new(&directory),
                    &[],
                );
                if let Some(window) = app.get_webview_window(window::MAIN_WINDOW) {
                    window::bring_forward(&window);
                }
                for request in requests {
                    window::open_request(app, request);
                }
            },
        ))
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .manage(Pages::default())
        .setup(move |app| {
            let handle = app.handle().clone();
            start_logging(&handle);
            for request in requests {
                window::open_request(&handle, request);
            }
            // Off the window's thread, since a dialog waits there for an answer.
            std::thread::spawn(move || open(&handle));
            Ok(())
        })
        .build(tauri::generate_context!())
        .context("starting the client")?;
    app.run(|_app, _event| {
        // How macOS asks an application to open something, rather than a command line.
        #[cfg(target_os = "macos")]
        if let tauri::RunEvent::Opened { urls } = _event {
            for request in urls.iter().filter_map(opening::request_of_url) {
                window::open_request(_app, request);
            }
        }
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::net::TcpListener;
    use std::path::PathBuf;

    use super::*;

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(ToString::to_string).collect()
    }

    #[test]
    fn the_command_line_names_only_what_to_open() {
        let here = Path::new("/here");
        assert_eq!(
            parse(strings(&["invented.torrent"]), here),
            Ok(Command::Open(vec![Request::File(PathBuf::from(
                "/here/invented.torrent"
            ))]))
        );
        assert_eq!(parse(strings(&["--help"]), here), Ok(Command::Help));
        assert_eq!(parse(strings(&["-V"]), here), Ok(Command::Version));
        // The service's options are the service's, not the client's.
        assert!(parse(strings(&["--data-dir", "/data"]), here).is_err());
        assert!(parse(strings(&["invented.mkv"]), here).is_err());
    }

    #[test]
    fn the_service_is_found_at_its_listen_port_on_loopback() -> anyhow::Result<()> {
        let (url, address) = service_url(LISTEN_DEFAULT)?;
        assert_eq!(url.as_str(), "http://127.0.0.1:8642/");
        assert_eq!(address.to_string(), "127.0.0.1:8642");
        assert_eq!(
            service_url("0.0.0.0:9000")?.0.as_str(),
            "http://127.0.0.1:9000/"
        );
        assert!(service_url("every interface").is_err());
        Ok(())
    }

    #[test]
    fn the_listen_address_comes_from_the_client_then_the_settings_then_the_default()
    -> anyhow::Result<()> {
        let directory = tempfile::tempdir()?;
        let settings = directory.path().join("torrplay.env");
        std::fs::write(&settings, "TORRPLAY_LISTEN=[::]:9000\n")?;
        let own = || Some("127.0.0.1:9100".to_owned());
        assert_eq!(service_listen(own(), Some(&settings))?.0, "127.0.0.1:9100");
        let (listen, source) = service_listen(None, Some(&settings))?;
        assert_eq!(listen, "[::]:9000");
        assert_eq!(source, settings.display().to_string(), "named in messages");
        std::fs::write(&settings, "# Nothing set\n")?;
        assert_eq!(service_listen(None, Some(&settings))?.0, LISTEN_DEFAULT);
        let missing = directory.path().join("missing.env");
        assert_eq!(service_listen(None, Some(&missing))?.0, LISTEN_DEFAULT);
        assert_eq!(service_listen(None, None)?.0, LISTEN_DEFAULT);
        Ok(())
    }

    #[test]
    fn a_settings_file_that_cannot_be_read_is_an_error() -> anyhow::Result<()> {
        let directory = tempfile::tempdir()?;
        // A directory where the file belongs cannot be read as one, whoever runs this.
        let unreadable = directory.path().join("torrplay.env");
        std::fs::create_dir(&unreadable)?;
        let error = service_listen(None, Some(&unreadable)).map(|(listen, _)| listen);
        assert!(error.is_err(), "{error:?}");
        Ok(())
    }

    #[test]
    fn the_settings_file_gives_the_listen_address_as_a_service_manager_reads_it() {
        let settings = "# Invented settings\nTORRPLAY_LISTEN=[::]:9000\nTORRPLAY_LOG=debug\n";
        assert_eq!(listen_in(settings).as_deref(), Some("[::]:9000"));
        assert_eq!(
            listen_in("TORRPLAY_LISTEN=\"0.0.0.0:1\"\nTORRPLAY_LISTEN='0.0.0.0:2'").as_deref(),
            Some("0.0.0.0:2"),
            "the last one, without its quotes"
        );
        assert_eq!(
            listen_in("# TORRPLAY_LISTEN=[::]:9000\nTORRPLAY_LISTENER=x"),
            None
        );
    }

    #[test]
    fn the_log_stops_at_its_limit() -> anyhow::Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("client.log");
        let mut log = Capped {
            file: File::create(&path)?,
            left: 5,
        };
        assert_eq!(log.write(b"0123")?, 4);
        assert_eq!(log.write(b"4567")?, 4, "the rest is dropped, not an error");
        log.flush()?;
        assert_eq!(std::fs::read(&path)?, b"01234");
        Ok(())
    }

    #[test]
    fn a_service_answers_only_while_it_listens() -> anyhow::Result<()> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let address = listener.local_addr()?;
        assert!(answers(address));
        drop(listener);
        assert!(!answers(address));
        Ok(())
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_listener_of_this_users_own_is_trusted() -> anyhow::Result<()> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        assert!(owner::trusted(listener.local_addr()?)?);
        Ok(())
    }

    #[test]
    fn how_to_start_the_service_follows_the_systems_language() {
        for message in [how_to_start, not_the_service] {
            let english = message(Some("en-US"));
            assert_eq!(message(None), english);
            assert_ne!(message(Some("ru-RU")), english);
        }
    }
}
