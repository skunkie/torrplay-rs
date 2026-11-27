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
#[cfg(target_os = "linux")]
use tauri_plugin_shell::ShellExt;
use torrplay_settings_file::SettingsFile;

use crate::opening::{self, Request};
use crate::owner;
use crate::window::{self, Notice, Pages};

/// The variable that gives the service its listen address, which tells the client
/// where to find it. The service's default is every interface on port 8642.
const LISTEN_VARIABLE: &str = "TORRPLAY_LISTEN";
const LISTEN_DEFAULT: &str = "[::]:8642";
/// The variable that names another settings file than the service's, so tests can
/// give the client one of their own on a computer with the service installed.
const SETTINGS_FILE_VARIABLE: &str = "TORRPLAY_SETTINGS_FILE";
/// The most the client writes to its log in a run, which starts it afresh.
const LOG_MAX_BYTES: u64 = 1 << 20;
/// How long the service may take to accept a connection before it counts as not
/// answering. One on this computer accepts at once, or refuses at once.
const ANSWER_TIMEOUT: Duration = Duration::from_secs(2);

/// The client's name in its messages: its executable's in the packages, which ADR 13
/// decides, whatever a development build's file is called.
const NAME: &str = "torrplay-client";

#[cfg(target_os = "linux")]
pub(crate) const HELP_PATH: &str = "/usr/share/doc/torrplay-client/help/index.html";

const USAGE: &str = "\
Usage: torrplay-client [TORRENT_FILE | MAGNET_URI]...

Opens the page of the TorrPlay service on this computer, at the port of the
listen address its settings file gives in TORRPLAY_LISTEN [default: [::]:8642]:
/etc/torrplay/torrplay.env, or %ProgramData%\\TorrPlay\\torrplay.env on Windows.
Where the file gives none, TORRPLAY_LISTEN in the client's own environment does,
as it does for the service on Windows. TORRPLAY_SETTINGS_FILE names another
settings file. A torrent file or magnet URI opens on the add page, in the
running client if there is one.

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

/// The service's listen address, with where it came from for messages: the one in the
/// settings file at `settings`, or else `own`, the client's own `TORRPLAY_LISTEN`, or
/// else the default. That is the service's order on Windows, where both inherit the
/// system's environment, so a system-wide variable cannot part them. A settings file
/// that is there but cannot be read is an error, since its address would otherwise
/// give way to another unseen.
fn service_listen(
    own: Option<String>,
    settings: Option<&Path>,
) -> anyhow::Result<(String, String)> {
    if let Some(path) = settings {
        let file = SettingsFile::read(path).with_context(|| {
            format!(
                "the service's settings file, {}, cannot be read",
                path.display()
            )
        })?;
        if let Some(listen) = file.as_ref().and_then(|file| file.get(LISTEN_VARIABLE)) {
            return Ok((listen.to_owned(), path.display().to_string()));
        }
    }
    if let Some(own) = own {
        return Ok((own, "the client's own environment".to_owned()));
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

/// How to start the service, in the system's language. On Linux it is `restart`,
/// which starts a stopped unit and one waiting out its restart backoff at once,
/// while `start` waits for the backoff.
fn how_to_start(locale: Option<&str>) -> &'static str {
    let russian = locale.is_some_and(|locale| locale.starts_with("ru"));
    if cfg!(target_os = "linux") {
        if russian {
            "Запустите её командой: sudo systemctl restart torrplay"
        } else {
            "Start it with: sudo systemctl restart torrplay"
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
    eprintln!("{NAME}: {message}");
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
    let settings = std::env::var_os(SETTINGS_FILE_VARIABLE)
        .map(PathBuf::from)
        .or_else(torrplay_settings_file::location);
    let found = service_listen(own, settings.as_deref()).and_then(|(listen, source)| {
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
            eprintln!("{NAME}: the client's log cannot be written: {error}");
            builder.with_writer(std::io::stderr).try_init()
        }
    };
    if let Err(error) = started {
        eprintln!("{NAME}: logging cannot start: {error}");
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
            println!("{NAME} {}", env!("CARGO_PKG_VERSION"));
            return ExitCode::SUCCESS;
        }
        Err(error) => {
            eprintln!("{NAME}: {error}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    match run(requests) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{NAME}: {error:#}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(target_os = "linux")]
fn help_labels(locale: Option<&str>) -> (&'static str, &'static str) {
    if locale.is_some_and(|locale| locale.starts_with("ru")) {
        ("Справка", "Справка по клиенту TorrPlay")
    } else {
        ("Help", "TorrPlay Client Help")
    }
}

#[cfg(target_os = "linux")]
fn help_error_message(path: &Path, locale: Option<&str>) -> String {
    let russian = locale.is_some_and(|locale| locale.starts_with("ru"));
    if !path.is_file() {
        return if russian {
            "Справка по клиенту TorrPlay недоступна."
        } else {
            "TorrPlay Client Help is unavailable."
        }
        .to_owned();
    }
    let message = if russian {
        "Не удалось открыть справку TorrPlay. Откройте этот файл в браузере:"
    } else {
        "Could not open TorrPlay Client Help. Open this file in your browser:"
    };
    format!("{message}\n\n{}", path.display())
}

#[cfg(target_os = "linux")]
pub(crate) fn install_help_header(window: &tauri::WebviewWindow) -> anyhow::Result<()> {
    use gtk::prelude::*;

    let native = window.gtk_window()?;
    let app = window.app_handle();
    let locale = sys_locale::get_locale();
    let (title, label) = help_labels(locale.as_deref());
    let header = gtk::HeaderBar::builder()
        .title(&app.package_info().name)
        .has_subtitle(false)
        .show_close_button(true)
        .build();
    if let Some(screen) = gtk::prelude::GtkWindowExt::screen(&native) {
        let style = gtk::CssProvider::new();
        style.load_from_data(
            b"headerbar.torrplay-client-header {
                  min-height: 32px;
                  padding-top: 2px;
                  padding-bottom: 2px;
              }
              headerbar.torrplay-client-header button {
                  min-height: 24px;
                  padding-top: 0;
                  padding-bottom: 0;
              }",
        )?;
        gtk::StyleContext::add_provider_for_screen(
            &screen,
            &style,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
        header.style_context().add_class("torrplay-client-header");
    }
    let help = gtk::Button::with_label(title);
    help.set_tooltip_text(Some(label));
    help.style_context().add_class("flat");
    let app = app.clone();
    help.connect_clicked(move |_| open_help(&app));
    header.pack_start(&help);
    native.set_titlebar(Some(&header));
    header.show_all();
    Ok(())
}

#[cfg(target_os = "linux")]
async fn launch_help<R: tauri::Runtime>(
    app: &AppHandle<R>,
    opener: &Path,
    path: &Path,
) -> anyhow::Result<()> {
    // The shell plugin pipes standard streams. Disconnect the command and its
    // browser from those pipes so termination does not depend on browser I/O.
    let status = app
        .shell()
        .command("/bin/sh")
        .args(["-c", "exec \"$1\" \"$2\" </dev/null >/dev/null 2>&1", "sh"])
        .arg(opener)
        .arg(path)
        .status()
        .await
        .context("launching xdg-open")?;
    anyhow::ensure!(
        status.success(),
        "xdg-open failed with exit code {:?}",
        status.code()
    );
    Ok(())
}

#[cfg(target_os = "linux")]
fn open_help(app: &AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        // Use the same launcher as the desktop entry and wait asynchronously for
        // its exit status. It can stay running until the HTML handler exits.
        let path = Path::new(HELP_PATH);
        if let Err(error) = launch_help(&app, Path::new("xdg-open"), path).await {
            tracing::warn!(%error, path = HELP_PATH, "could not open TorrPlay Client Help");
            let locale = sys_locale::get_locale();
            let (_, label) = help_labels(locale.as_deref());
            app.dialog()
                .message(help_error_message(path, locale.as_deref()))
                .title(label)
                .kind(MessageDialogKind::Error)
                .show(|_| {});
        }
    });
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
        .plugin(tauri_plugin_opener::init());
    #[cfg(target_os = "linux")]
    let app = app.plugin(tauri_plugin_shell::init());
    let app = app
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

    #[cfg(target_os = "linux")]
    #[test]
    fn the_help_button_and_applications_launcher_share_packaged_help() {
        let entry = include_str!("../../packaging/linux/org.torrplay.client.help.desktop");
        let command = format!("Exec=xdg-open {HELP_PATH}");
        assert!(entry.lines().any(|line| line == command));
        for (locale, field) in [("en-US", "Name"), ("ru-RU", "Name[ru]")] {
            let (_, label) = help_labels(Some(locale));
            let name = format!("{field}={label}");
            assert!(entry.lines().any(|line| line == name));
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn removed_help_is_reported_as_unavailable() -> anyhow::Result<()> {
        let directory = tempfile::tempdir()?;
        let help = directory.path().join("index.html");
        std::fs::write(&help, "offline help")?;
        for locale in ["en-US", "ru-RU"] {
            assert!(help_error_message(&help, Some(locale)).ends_with(&help.display().to_string()));
        }
        std::fs::remove_file(&help)?;
        assert_eq!(
            help_error_message(&help, Some("en-US")),
            "TorrPlay Client Help is unavailable."
        );
        assert_eq!(
            help_error_message(&help, Some("ru-RU")),
            "Справка по клиенту TorrPlay недоступна."
        );
        Ok(())
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_delayed_help_launcher_failure_is_reported() -> anyhow::Result<()> {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir()?;
        let help = directory.path().join("client help.html");
        std::fs::write(&help, "offline help")?;
        let launcher = directory.path().join("test launcher");
        std::fs::write(
            &launcher,
            "#!/bin/sh\ntest -f \"$1\" || exit 8\nsleep 0.4\nexit 7\n",
        )?;
        std::fs::set_permissions(&launcher, std::fs::Permissions::from_mode(0o700))?;
        let app = tauri::test::mock_builder()
            .plugin(tauri_plugin_shell::init())
            .build(tauri::test::mock_context(tauri::test::noop_assets()))?;
        let result = tauri::async_runtime::block_on(launch_help(app.handle(), &launcher, &help));
        assert!(matches!(result, Err(error) if error.to_string().contains("Some(7)")));
        Ok(())
    }

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
    fn the_listen_address_comes_from_the_settings_then_the_client_then_the_default()
    -> anyhow::Result<()> {
        let directory = tempfile::tempdir()?;
        let settings = directory.path().join("torrplay.env");
        std::fs::write(&settings, "TORRPLAY_LISTEN=[::]:9000\n")?;
        let own = || Some("127.0.0.1:9100".to_owned());
        let (listen, source) = service_listen(own(), Some(&settings))?;
        assert_eq!(
            listen, "[::]:9000",
            "the settings file before the environment"
        );
        assert_eq!(source, settings.display().to_string(), "named in messages");
        std::fs::write(&settings, "# Nothing set\n")?;
        assert_eq!(service_listen(own(), Some(&settings))?.0, "127.0.0.1:9100");
        assert_eq!(service_listen(None, Some(&settings))?.0, LISTEN_DEFAULT);
        let missing = directory.path().join("missing.env");
        assert_eq!(service_listen(own(), Some(&missing))?.0, "127.0.0.1:9100");
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
