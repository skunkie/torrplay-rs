// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! TorrPlay's desktop application: the server in this process, and a window on its
//! own loopback address, which serves the same client as a browser gets. It takes
//! the server's command line. What a browser does around a page, the window does
//! here: saving downloads and handing other addresses to the system.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Mutex;
use std::time::Duration;

use anyhow::Context;
use tauri::webview::DownloadEvent;
use tauri::{AppHandle, Manager, RunEvent, Url, WebviewUrl, WebviewWindowBuilder};
use tauri_plugin_dialog::{DialogExt, MessageDialogKind};
use tauri_plugin_opener::OpenerExt;
use tokio::sync::oneshot;
use torrplay::{Command, Config, LogFormat};

/// How long exiting waits for blocking tasks still running after shutdown.
const RUNTIME_SHUTDOWN: Duration = Duration::from_secs(5);

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
                    false
                }
            }
        }
        DownloadEvent::Finished { path, success, .. } => {
            if success {
                if let Some(path) = path {
                    tracing::info!(path = %path.display(), "a download finished");
                    if let Err(error) = app.opener().reveal_item_in_dir(&path) {
                        tracing::warn!(%error, "the download cannot be shown");
                    }
                }
            } else {
                tracing::warn!("a download failed");
            }
            true
        }
        _ => true,
    }
}

fn open_window(app: &AppHandle, listening: SocketAddr) -> anyhow::Result<()> {
    let own = window_url(listening)?;
    let opener = app.clone();
    let saver = app.clone();
    WebviewWindowBuilder::new(app, MAIN_WINDOW, WebviewUrl::External(own.clone()))
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
        .build()
        .context("opening the window")?;
    Ok(())
}

/// Runs the server until `stop` fires, its sender is gone, or a termination signal
/// arrives. The application waits for this before it exits.
fn serve(config: &Config, app: &AppHandle, stop: oneshot::Receiver<()>) -> anyhow::Result<()> {
    let process_run = torrplay::Run::draw();
    let files = config
        .data_dir()
        .and_then(|data_dir| Ok(torrplay::LogFiles::open(&data_dir.join("logs"))?));
    let (files, files_failed) = match files {
        Ok(files) => (Some(files), None),
        Err(error) => (None, Some(error)),
    };
    let log_files = files.as_ref().map(|(files, _)| files.clone());
    let log = torrplay::init_logging(
        config.log_level.unwrap_or_default(),
        config.log_filter.as_deref(),
        config.log_format.unwrap_or(LogFormat::Text),
        &process_run,
        files,
    );
    if let Some(error) = files_failed {
        tracing::warn!(
            error = %format!("{error:#}"),
            "the log files could not be opened; logs go to standard error only"
        );
    }
    if let Err(error) = torrplay_engine::raise_open_file_limit() {
        tracing::warn!(error = %format!("{error:#}"), "the open file limit could not be raised");
    }
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("starting the runtime")?;
    let opener = app.clone();
    let result = runtime.block_on(async {
        // Before the listener binds, as in the executable. Logging out sends these, and
        // the store must still record a clean shutdown.
        let terminated = torrplay::termination();
        torrplay::run_server(
            config,
            log.clone(),
            process_run,
            log_files,
            move |listening| {
                if let Err(error) = open_window(&opener, listening) {
                    tracing::error!(error = %format!("{error:#}"), "no window");
                    opener.exit(1);
                }
            },
            async {
                tokio::select! {
                    // A dropped sender means the application is ending too.
                    _ = stop => {}
                    () = terminated => {}
                }
            },
        )
        .await
    });
    runtime.shutdown_timeout(RUNTIME_SHUTDOWN);
    log.flush();
    result
}

/// The heading of the dialog about a failed start, in `locale` like `ru-RU`. The
/// reason under it stays in the logs' English.
fn start_failed_title(locale: Option<&str>) -> &'static str {
    match locale {
        Some(locale) if locale.starts_with("ru") => "Не удалось запустить TorrPlay",
        _ => "TorrPlay could not start",
    }
}

/// Says why the application cannot run, since nothing shows standard error to someone
/// who opened it from a menu.
fn report_failure(app: &AppHandle, error: &anyhow::Error) {
    tracing::error!(error = %format!("{error:#}"), "torrplay failed");
    eprintln!("torrplay-desktop: {error:#}");
    app.dialog()
        .message(format!("{error:#}"))
        .title(start_failed_title(sys_locale::get_locale().as_deref()))
        .kind(MessageDialogKind::Error)
        .blocking_show();
}

/// Holds the signal that stops the server until the application asks to exit.
struct Stop(Mutex<Option<oneshot::Sender<()>>>);

fn main() -> ExitCode {
    let listen_variable = std::env::var_os(torrplay::LISTEN_VARIABLE)
        .map(|value| value.to_string_lossy().into_owned());
    let config = match torrplay::parse_args(std::env::args().skip(1), listen_variable) {
        Ok(Command::Run(config)) => config,
        Ok(_) => {
            eprintln!("torrplay-desktop: the desktop application only runs the server");
            return ExitCode::from(2);
        }
        Err(error) => {
            eprintln!("torrplay-desktop: {error}");
            return ExitCode::from(2);
        }
    };
    match run(config) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("torrplay-desktop: {error:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(config: Config) -> anyhow::Result<()> {
    let (stop, stopped) = oneshot::channel();
    let app = tauri::Builder::default()
        // First, so a second launch ends before the rest of it starts anything.
        .plugin(tauri_plugin_single_instance::init(|app, _, _| {
            if let Some(window) = app.get_webview_window(MAIN_WINDOW)
                && let Err(error) = window.unminimize().and_then(|()| window.set_focus())
            {
                tracing::warn!(%error, "the window cannot be brought forward");
            }
        }))
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .manage(Stop(Mutex::new(Some(stop))))
        .setup(move |app| {
            let handle = app.handle().clone();
            // The window opens from here once the server listens.
            std::thread::spawn(move || {
                let code = match serve(&config, &handle, stopped) {
                    Ok(()) => 0,
                    Err(error) => {
                        report_failure(&handle, &error);
                        1
                    }
                };
                handle.exit(code);
            });
            Ok(())
        })
        .build(tauri::generate_context!())
        .context("starting the application")?;
    app.run(|app, event| {
        if let RunEvent::ExitRequested {
            code: None, api, ..
        } = event
        {
            // The last window closed: the server shuts down in order first, and
            // ends the application with a code when it has.
            api.prevent_exit();
            if let Some(stop) = app
                .state::<Stop>()
                .0
                .lock()
                .ok()
                .and_then(|mut stop| stop.take())
            {
                let _ = stop.send(());
            }
        }
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
    fn the_failure_heading_follows_the_systems_language() {
        assert_eq!(
            start_failed_title(Some("ru-RU")),
            "Не удалось запустить TorrPlay"
        );
        for locale in [Some("en-US"), Some("de-DE"), None] {
            assert_eq!(start_failed_title(locale), "TorrPlay could not start");
        }
    }
}
