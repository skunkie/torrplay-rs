// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! TorrPlay's desktop application: the server in this process, and a window on its
//! own loopback address, which serves the same client as a browser gets. It takes
//! the server's command line. What a browser does around a page, the window does
//! here: saving downloads, handing other addresses to the system, and taking torrent
//! files and magnet URIs from a drop, its command line, or the system.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod opening;

use std::io::IsTerminal;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Mutex;

use anyhow::{Context, anyhow};
use opening::Request;
use tauri::webview::{DownloadEvent, PageLoadEvent};
use tauri::{
    AppHandle, DragDropEvent, Manager, RunEvent, Url, WebviewUrl, WebviewWindow,
    WebviewWindowBuilder, WindowEvent,
};
use tauri_plugin_dialog::{DialogExt, MessageDialogKind};
use tauri_plugin_opener::OpenerExt;
use tokio::sync::oneshot;
use torrplay::{Command, Config, EngineDidNotStart, LogFormat};

/// The label of the application's one window.
const MAIN_WINDOW: &str = "main";

/// The address the window opens: the listener's port on loopback. It stays the same
/// between runs, since the page's stored sign-in and language belong to its origin.
fn window_url(listening: SocketAddr) -> anyhow::Result<Url> {
    let ip = match listening.ip() {
        ip if ip.is_unspecified() => IpAddr::V4(Ipv4Addr::LOCALHOST),
        ip => ip,
    };
    Url::parse(&format!(
        "http://{}/",
        SocketAddr::new(ip, listening.port())
    ))
    .context("the listener's address as a URL")
}

/// What to do with a navigation in the window.
#[derive(Debug, PartialEq, Eq)]
enum Navigation {
    /// Follow it in the window.
    Stay,
    /// Hand it to the system, such as Stremio for its own links.
    Outside,
    /// Ignore it: nothing in the client leads there.
    Refuse,
}

/// Only the application's own pages, and what a page makes for itself, stay in the
/// window, so that nothing else runs with the application's origin.
fn navigation(url: &Url, own: &Url) -> Navigation {
    match url.scheme() {
        "about" | "blob" | "data" => Navigation::Stay,
        _ if url.origin() == own.origin() => Navigation::Stay,
        "http" | "https" | "magnet" | "stremio" => Navigation::Outside,
        _ => Navigation::Refuse,
    }
}

/// `name` in `directory`, numbered like a browser's downloads if it is taken.
fn unused_path(directory: &Path, name: &str) -> PathBuf {
    let candidate = directory.join(name);
    if !candidate.exists() {
        return candidate;
    }
    let name = Path::new(name);
    let stem = name.file_stem().unwrap_or_default().to_string_lossy();
    let extension = name
        .extension()
        .map(|extension| format!(".{}", extension.to_string_lossy()))
        .unwrap_or_default();
    (1u32..)
        .map(|number| directory.join(format!("{stem} ({number}){extension}")))
        .find(|candidate| !candidate.exists())
        .unwrap_or(candidate)
}

/// Says that a download failed, since the page has already finished its part and
/// someone may otherwise think they have the file.
fn notify_download_failed(app: &AppHandle) {
    app.dialog()
        .message(notice_text(
            Notice::DownloadFailed,
            sys_locale::get_locale().as_deref(),
        ))
        .kind(MessageDialogKind::Error)
        .show(|_| {});
}

/// Sends a download to the system's downloads directory, which a page's link has no
/// way to choose here, and shows the file when it is done.
fn handle_download(app: &AppHandle, event: DownloadEvent<'_>) -> bool {
    match event {
        DownloadEvent::Requested { destination, .. } => {
            let name = destination.file_name().map_or_else(
                || "download".into(),
                |name| name.to_string_lossy().into_owned(),
            );
            // A system without configured user directories names none.
            let directory = app
                .path()
                .download_dir()
                .or_else(|_| app.path().home_dir().map(|home| home.join("Downloads")));
            match directory {
                Ok(directory) => {
                    if let Err(error) = std::fs::create_dir_all(&directory) {
                        tracing::warn!(%error, "the downloads directory cannot be created");
                    }
                    *destination = unused_path(&directory, &name);
                    true
                }
                Err(error) => {
                    tracing::error!(%error, "no downloads directory");
                    notify_download_failed(app);
                    false
                }
            }
        }
        DownloadEvent::Finished { path, success, .. } => {
            if success {
                if let Some(path) = path {
                    tracing::info!(path = %path.display(), "a download finished");
                    // On its own thread, since this runs on the window's and it waits for
                    // the file manager to answer.
                    let app = app.clone();
                    std::thread::spawn(move || {
                        if let Err(error) = app.opener().reveal_item_in_dir(&path) {
                            tracing::warn!(%error, "the download cannot be shown");
                        }
                    });
                }
            } else {
                tracing::warn!("a download failed");
                notify_download_failed(app);
            }
            true
        }
        _ => true,
    }
}

/// Reads `request` and hands it to the page, off the window's thread. A file that cannot
/// be read says so, since whoever asked for it is waiting to see it.
fn open_request(app: &AppHandle, request: Request) {
    let app = app.clone();
    std::thread::spawn(move || match opening::delivery_script(&request) {
        Ok(script) => app.state::<Shell>().deliver(&app, script),
        Err(error) => {
            tracing::warn!(error = %format!("{error:#}"), "a torrent file cannot be opened");
            app.dialog()
                .message(format!("{error:#}"))
                .title(notice_text(
                    Notice::FileNotOpened,
                    sys_locale::get_locale().as_deref(),
                ))
                .kind(MessageDialogKind::Error)
                .show(|_| {});
        }
    });
}

/// Follows a file dragged over the window. The application's handler is what gives real
/// paths on every system; the page gets none from a drag on some of them.
fn handle_drag(app: &AppHandle, window: &WebviewWindow, drag: &DragDropEvent) {
    let dragging = |active| {
        if let Err(error) = window.eval(opening::dragging_script(active)) {
            tracing::warn!(%error, "the page cannot be told of a drag");
        }
    };
    match drag {
        DragDropEvent::Enter { paths, .. } => {
            dragging(paths.iter().any(|path| opening::is_torrent_file(path)));
        }
        DragDropEvent::Leave => dragging(false),
        DragDropEvent::Drop { paths, .. } => {
            dragging(false);
            if let Some(request) = opening::dropped_request(paths) {
                open_request(app, request);
            }
        }
        _ => {}
    }
}

fn open_window(app: &AppHandle, listening: SocketAddr) -> anyhow::Result<()> {
    let own = window_url(listening)?;
    let opener = app.clone();
    let saver = app.clone();
    let window = WebviewWindowBuilder::new(app, MAIN_WINDOW, WebviewUrl::External(own.clone()))
        .title("TorrPlay")
        .inner_size(1280.0, 800.0)
        .min_inner_size(360.0, 480.0)
        .on_navigation(move |url| match navigation(url, &own) {
            Navigation::Stay => true,
            Navigation::Outside => {
                if let Err(error) = opener.opener().open_url(url.as_str(), None::<&str>) {
                    tracing::warn!(%error, scheme = url.scheme(), "a link cannot be opened");
                }
                false
            }
            Navigation::Refuse => {
                tracing::warn!(scheme = url.scheme(), "a navigation was refused");
                false
            }
        })
        .on_download(move |_, event| handle_download(&saver, event))
        .on_page_load(|window, payload| {
            let app = window.app_handle();
            let loaded = matches!(payload.event(), PageLoadEvent::Finished);
            app.state::<Shell>().page_loaded(app, loaded);
        })
        .build()
        .context("opening the window")?;
    let dragged = window.clone();
    let handle = app.clone();
    window.on_window_event(move |event| {
        if let WindowEvent::DragDrop(drag) = event {
            handle_drag(&handle, &dragged, drag);
        }
    });
    Ok(())
}

/// Runs the server until `stop` fires, its sender is gone, or a termination signal
/// arrives. The application waits for this before it exits.
fn serve(
    config: &Config,
    format: LogFormat,
    app: &AppHandle,
    stop: oneshot::Receiver<()>,
) -> anyhow::Result<()> {
    let startup = torrplay::start_logging(config, format);
    let log = startup.log.clone();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("starting the runtime")?;
    let opener = app.clone();
    let stopping = app.clone();
    let result = runtime.block_on(async {
        // Before the listener binds, as in the executable. Logging out sends these, and
        // the store must still record a clean shutdown.
        // A dropped sender means the application is ending too.
        let torrplay::StopRequests { shutdown, hurry } = torrplay::termination_or(async {
            let _ = stop.await;
        });
        torrplay::run_server(
            config,
            startup,
            move |listening| {
                let shell = opener.state::<Shell>();
                shell.advance(Stage::Running);
                if let Err(error) = open_window(&opener, listening) {
                    // Stops in order like any other way out, and is reported then.
                    shell.fail_window(error);
                    shell.request_stop();
                }
            },
            torrplay::StopRequests {
                shutdown: async {
                    shutdown.await;
                    stopping.state::<Shell>().advance(Stage::Stopping);
                },
                hurry,
            },
        )
        .await
    });
    runtime.shutdown_timeout(torrplay::RUNTIME_SHUTDOWN);
    log.flush();
    result
}

/// Runs `work`, with a panic as its error, so that the application hears of it and
/// does not stay running without its server.
fn catching_panic(work: impl FnOnce() -> anyhow::Result<()>) -> anyhow::Result<()> {
    catch_unwind(AssertUnwindSafe(work)).unwrap_or_else(|panic| {
        let message = panic
            .downcast_ref::<&str>()
            .map(ToString::to_string)
            .or_else(|| panic.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "no message".to_owned());
        Err(anyhow!("the server panicked: {message}"))
    })
}

/// How far the server has got, which says what a failure means.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Stage {
    Starting,
    Running,
    Stopping,
}

/// Something to tell the user.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Notice {
    CouldNotStart,
    StoppedWorking,
    DownloadFailed,
    FileNotOpened,
}

/// `notice` in `locale`, like `ru-RU`. A failure's reason under it stays in the logs'
/// English.
fn notice_text(notice: Notice, locale: Option<&str>) -> &'static str {
    let russian = locale.is_some_and(|locale| locale.starts_with("ru"));
    match (notice, russian) {
        (Notice::CouldNotStart, false) => "TorrPlay could not start",
        (Notice::CouldNotStart, true) => "Не удалось запустить TorrPlay",
        (Notice::StoppedWorking, false) => "TorrPlay stopped working",
        (Notice::StoppedWorking, true) => "TorrPlay перестал работать",
        (Notice::DownloadFailed, false) => "The file could not be saved",
        (Notice::DownloadFailed, true) => "Не удалось сохранить файл",
        (Notice::FileNotOpened, false) => "The file could not be opened",
        (Notice::FileNotOpened, true) => "Не удалось открыть файл",
    }
}

/// What to tell the user about `error` at `stage`, if anything: nobody is waiting on
/// an application they already closed, whose failure to stop is in the logs. The
/// listener opens before the engine starts, so running alone does not mean it worked.
fn failure_notice(stage: Stage, error: &anyhow::Error) -> Option<Notice> {
    match stage {
        Stage::Starting => Some(Notice::CouldNotStart),
        Stage::Running if error.downcast_ref::<EngineDidNotStart>().is_some() => {
            Some(Notice::CouldNotStart)
        }
        Stage::Running => Some(Notice::StoppedWorking),
        Stage::Stopping => None,
    }
}

/// Logs why the application is ending and, with a `notice`, says so in a dialog, since
/// nothing shows standard error to someone who opened it from a menu. The window goes
/// first, since its server has gone and it would only show a blank page behind this.
fn report_failure(app: &AppHandle, error: &anyhow::Error, notice: Option<Notice>) {
    tracing::error!(error = %format!("{error:#}"), "torrplay failed");
    eprintln!("torrplay-desktop: {error:#}");
    if let Some(notice) = notice {
        if let Some(window) = app.get_webview_window(MAIN_WINDOW)
            && let Err(error) = window.hide()
        {
            tracing::warn!(%error, "the window cannot be hidden");
        }
        app.dialog()
            .message(format!("{error:#}"))
            .title(notice_text(notice, sys_locale::get_locale().as_deref()))
            .kind(MessageDialogKind::Error)
            .blocking_show();
    }
}

fn run_in_page(app: &AppHandle, script: &str) {
    if let Some(window) = app.get_webview_window(MAIN_WINDOW)
        && let Err(error) = window.eval(script)
    {
        tracing::warn!(%error, "the page cannot be handed what was opened");
    }
}

/// What the window, the server's thread, and a second launch share.
struct Shell {
    /// Stops the server, until the application asks to exit.
    stop: Mutex<Option<oneshot::Sender<()>>>,
    stage: Mutex<Stage>,
    /// What someone asked to open by launching the application again while it was
    /// stopping, once there was such a launch: a new run opens it.
    relaunch: Mutex<Option<Vec<Request>>>,
    /// This run's options, which a new run keeps.
    options: Vec<String>,
    /// Why the window could not open.
    window_failure: Mutex<Option<anyhow::Error>>,
    /// What the page has yet to be handed, which waits for it to load.
    delivery: Mutex<Delivery>,
}

/// Whether the page can run scripts yet, and the one waiting until it can: the page
/// opens only the newest of what it is handed, so only that waits.
#[derive(Default)]
struct Delivery {
    loaded: bool,
    waiting: Option<String>,
}

impl Shell {
    fn new(stop: oneshot::Sender<()>, options: Vec<String>) -> Self {
        Self {
            stop: Mutex::new(Some(stop)),
            stage: Mutex::new(Stage::Starting),
            relaunch: Mutex::new(None),
            options,
            window_failure: Mutex::new(None),
            delivery: Mutex::new(Delivery::default()),
        }
    }

    /// Runs `script` in the page, or once it has loaded.
    fn deliver(&self, app: &AppHandle, script: String) {
        let Ok(mut delivery) = self.delivery.lock() else {
            return;
        };
        if delivery.loaded {
            drop(delivery);
            run_in_page(app, &script);
        } else {
            delivery.waiting = Some(script);
        }
    }

    /// Notes whether the page has finished loading, running what waited if it has.
    fn page_loaded(&self, app: &AppHandle, loaded: bool) {
        let Ok(mut delivery) = self.delivery.lock() else {
            return;
        };
        delivery.loaded = loaded;
        if loaded && let Some(script) = delivery.waiting.take() {
            run_in_page(app, &script);
        }
    }

    /// Stops the server, if that has not been asked already.
    fn request_stop(&self) {
        self.advance(Stage::Stopping);
        if let Some(stop) = self.stop.lock().ok().and_then(|mut stop| stop.take()) {
            let _ = stop.send(());
        }
    }

    fn fail_window(&self, error: anyhow::Error) {
        if let Ok(mut failure) = self.window_failure.lock() {
            *failure = Some(error);
        }
    }

    fn take_window_failure(&self) -> Option<anyhow::Error> {
        self.window_failure.lock().ok()?.take()
    }

    fn stage(&self) -> Stage {
        self.stage.lock().map_or(Stage::Stopping, |stage| *stage)
    }

    /// Moves on to `stage`, never back.
    fn advance(&self, stage: Stage) {
        if let Ok(mut current) = self.stage.lock() {
            *current = stage.max(*current);
        }
    }

    /// What a second launch does, returning what to open now: while this process is
    /// stopping, a new run starts when it has, whatever window it still shows, and opens
    /// `requests`. Otherwise the window comes forward, or is on its way.
    fn second_launch(&self, window: Option<WebviewWindow>, requests: Vec<Request>) -> Vec<Request> {
        if self.stage() == Stage::Stopping {
            if let Ok(mut relaunch) = self.relaunch.lock() {
                relaunch.get_or_insert_default().extend(requests);
            }
            return Vec::new();
        }
        if let Some(window) = window
            && let Err(error) = window.unminimize().and_then(|()| window.set_focus())
        {
            tracing::warn!(%error, "the window cannot be brought forward");
        }
        requests
    }

    /// The command line of a new run, if a launch asked for one.
    fn relaunch_arguments(&self) -> Option<Vec<std::ffi::OsString>> {
        let requests = self.relaunch.lock().ok()?.take()?;
        let program = std::env::args_os().next().unwrap_or_default();
        Some(opening::relaunch_arguments(
            program,
            &self.options,
            &requests,
        ))
    }
}

fn main() -> ExitCode {
    let listen_variable = std::env::var_os(torrplay::LISTEN_VARIABLE)
        .map(|value| value.to_string_lossy().into_owned());
    let directory = std::env::current_dir().unwrap_or_default();
    let (options, requests) = opening::split_arguments(std::env::args().skip(1), &directory);
    let config = match torrplay::parse_args(options.clone(), listen_variable) {
        Ok(Command::Run(config)) => config,
        Ok(Command::Help) => {
            println!(
                "torrplay-desktop takes the options of `torrplay [OPTIONS]` below. The other \
                 commands are only in the `torrplay` executable. A torrent file or magnet \
                 URI among the arguments opens on the add page, in the running \
                 application if there is one.\n"
            );
            print!("{}", torrplay::USAGE);
            return ExitCode::SUCCESS;
        }
        Ok(Command::Version) => {
            println!("torrplay-desktop {}", env!("CARGO_PKG_VERSION"));
            return ExitCode::SUCCESS;
        }
        Ok(_) => {
            eprintln!("torrplay-desktop: only the `torrplay` executable has this command");
            return ExitCode::from(2);
        }
        Err(error) => {
            eprintln!("torrplay-desktop: {error}\n\n{}", torrplay::USAGE);
            return ExitCode::from(2);
        }
    };
    let variable = std::env::var_os(torrplay::LOG_FORMAT_VARIABLE)
        .map(|value| value.to_string_lossy().into_owned());
    let format =
        match torrplay::log_format(config.log_format, variable, std::io::stderr().is_terminal()) {
            Ok(format) => format,
            Err(error) => {
                eprintln!("torrplay-desktop: {error}\n\n{}", torrplay::USAGE);
                return ExitCode::from(2);
            }
        };
    match run(config, format, options, requests) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("torrplay-desktop: {error:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(
    config: Config,
    format: LogFormat,
    options: Vec<String>,
    requests: Vec<Request>,
) -> anyhow::Result<()> {
    let (stop, stopped) = oneshot::channel();
    let app = tauri::Builder::default()
        // First, so a second launch ends before the rest of it starts anything.
        .plugin(tauri_plugin_single_instance::init(
            |app, arguments, directory| {
                let (_, requests) = opening::split_arguments(
                    arguments.into_iter().skip(1),
                    std::path::Path::new(&directory),
                );
                let now = app
                    .state::<Shell>()
                    .second_launch(app.get_webview_window(MAIN_WINDOW), requests);
                for request in now {
                    open_request(app, request);
                }
            },
        ))
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .manage(Shell::new(stop, options))
        .setup(move |app| {
            let handle = app.handle().clone();
            for request in requests {
                open_request(&handle, request);
            }
            // The window opens from here once the server listens.
            std::thread::spawn(move || {
                let result = catching_panic(|| serve(&config, format, &handle, stopped));
                let shell = handle.state::<Shell>();
                let code = if let Some(error) = shell.take_window_failure() {
                    report_failure(&handle, &error, Some(Notice::CouldNotStart));
                    1
                } else if let Err(error) = result {
                    report_failure(&handle, &error, failure_notice(shell.stage(), &error));
                    1
                } else {
                    0
                };
                // A new run, if a launch asked for one, starts once this has exited.
                handle.exit(code);
            });
            Ok(())
        })
        .build(tauri::generate_context!())
        .context("starting the application")?;
    app.run(|app, event| match event {
        RunEvent::ExitRequested {
            code: None, api, ..
        } => {
            // The last window closed: the server shuts down in order first, and
            // ends the application with a code when it has.
            api.prevent_exit();
            app.state::<Shell>().request_stop();
        }
        // The new run starts here, once the single instance's lock is let go, which Tauri
        // does only on cleaning up after this event.
        RunEvent::Exit => {
            if let Some(arguments) = app.state::<Shell>().relaunch_arguments() {
                app.cleanup_before_exit();
                let mut env = tauri::Env::default();
                env.args_os = arguments;
                tauri::process::restart(&env);
            }
        }
        // How macOS asks an application to open something, rather than a command line.
        #[cfg(target_os = "macos")]
        RunEvent::Opened { urls } => {
            for request in urls.iter().filter_map(opening::request_of_url) {
                open_request(app, request);
            }
        }
        _ => {}
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(text: &str) -> Url {
        Url::parse(text).unwrap_or_else(|error| panic!("{text}: {error}"))
    }

    #[test]
    fn window_url_uses_loopback_for_an_unspecified_listener() {
        for (listening, expected) in [
            ("[::]:8642", "http://127.0.0.1:8642/"),
            ("0.0.0.0:8642", "http://127.0.0.1:8642/"),
            ("127.0.0.1:9000", "http://127.0.0.1:9000/"),
        ] {
            let address = listening
                .parse()
                .unwrap_or_else(|error| panic!("{listening}: {error}"));
            let opened = window_url(address).map(|url| url.to_string()).ok();
            assert_eq!(opened.as_deref(), Some(expected), "{listening}");
        }
    }

    #[test]
    fn navigation_keeps_only_the_applications_pages_in_the_window() {
        let own = url("http://127.0.0.1:8642/");
        for (target, expected) in [
            ("http://127.0.0.1:8642/library", Navigation::Stay),
            ("about:blank", Navigation::Stay),
            ("blob:http://127.0.0.1:8642/some-id", Navigation::Stay),
            ("data:text/plain,invented", Navigation::Stay),
            ("http://127.0.0.1:9000/", Navigation::Outside),
            ("https://example.invalid/", Navigation::Outside),
            (
                "stremio://127.0.0.1:8642/stremio/manifest.json",
                Navigation::Outside,
            ),
            ("magnet:?xt=urn:btih:0123", Navigation::Outside),
            ("file:///etc/hostname", Navigation::Refuse),
            ("smb://example.invalid/share", Navigation::Refuse),
        ] {
            assert_eq!(navigation(&url(target), &own), expected, "{target}");
        }
    }

    #[test]
    fn unused_path_numbers_a_taken_name() -> anyhow::Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path();
        assert_eq!(unused_path(path, "backup.tar"), path.join("backup.tar"));
        std::fs::write(path.join("backup.tar"), "")?;
        assert_eq!(unused_path(path, "backup.tar"), path.join("backup (1).tar"));
        std::fs::write(path.join("backup (1).tar"), "")?;
        assert_eq!(unused_path(path, "backup.tar"), path.join("backup (2).tar"));
        std::fs::write(path.join("notes"), "")?;
        assert_eq!(unused_path(path, "notes"), path.join("notes (1)"));
        Ok(())
    }

    #[test]
    fn notices_follow_the_systems_language() {
        for notice in [
            Notice::CouldNotStart,
            Notice::StoppedWorking,
            Notice::DownloadFailed,
            Notice::FileNotOpened,
        ] {
            let english = notice_text(notice, Some("en-US"));
            assert_eq!(notice_text(notice, None), english);
            assert_eq!(notice_text(notice, Some("de-DE")), english);
            assert_ne!(notice_text(notice, Some("ru-RU")), english, "{notice:?}");
        }
    }

    #[test]
    fn a_failure_says_what_it_means_at_each_stage() {
        let other = || anyhow!("invented");
        let engine = || other().context(EngineDidNotStart);
        assert_eq!(
            failure_notice(Stage::Starting, &other()),
            Some(Notice::CouldNotStart)
        );
        assert_eq!(
            failure_notice(Stage::Running, &other()),
            Some(Notice::StoppedWorking)
        );
        assert_eq!(
            failure_notice(Stage::Running, &engine()),
            Some(Notice::CouldNotStart)
        );
        assert_eq!(failure_notice(Stage::Stopping, &other()), None);
        assert_eq!(failure_notice(Stage::Stopping, &engine()), None);
    }

    #[test]
    fn asking_to_stop_twice_stops_once() {
        let (stop, mut stopped) = oneshot::channel();
        let shell = Shell::new(stop, Vec::new());
        shell.request_stop();
        shell.request_stop();
        assert_eq!(shell.stage(), Stage::Stopping);
        assert_eq!(stopped.try_recv(), Ok(()));
    }

    #[test]
    fn a_stage_never_goes_back() {
        let (stop, _stopped) = oneshot::channel();
        let shell = Shell::new(stop, Vec::new());
        shell.advance(Stage::Stopping);
        shell.advance(Stage::Running);
        assert_eq!(shell.stage(), Stage::Stopping);
    }

    #[test]
    fn a_second_launch_starts_a_new_run_only_while_stopping() {
        let (stop, _stopped) = oneshot::channel();
        let shell = Shell::new(stop, vec!["--no-dht".to_owned()]);
        let request = || vec![Request::Magnet("magnet:?invented".to_owned())];
        assert_eq!(
            shell.second_launch(None, request()),
            request(),
            "opened now"
        );
        assert!(shell.relaunch_arguments().is_none(), "the window is coming");
        shell.advance(Stage::Stopping);
        assert!(
            shell.second_launch(None, request()).is_empty(),
            "kept for the new run"
        );
        let arguments = shell.relaunch_arguments().unwrap_or_default();
        assert_eq!(
            &arguments[1..],
            ["--no-dht", "magnet:?invented"].map(std::ffi::OsString::from)
        );
        assert!(shell.relaunch_arguments().is_none(), "taken once");
    }

    #[test]
    fn a_panic_becomes_an_error() {
        let error = catching_panic(|| panic!("invented")).map_err(|error| error.to_string());
        assert_eq!(error, Err("the server panicked: invented".to_owned()));
        assert!(catching_panic(|| Ok(())).is_ok());
    }
}
