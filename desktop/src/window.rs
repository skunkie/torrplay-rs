// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! The window around the client's page, the same in both editions: what stays in it,
//! where downloads go, and how dropped and named files reach the page.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::Context;
use tauri::webview::{DownloadEvent, PageLoadEvent};
use tauri::{
    AppHandle, DragDropEvent, Manager, Url, WebviewUrl, WebviewWindow, WebviewWindowBuilder,
    WindowEvent,
};
use tauri_plugin_dialog::{DialogExt, MessageDialogKind};
use tauri_plugin_opener::OpenerExt;

use crate::opening::{self, Request};

/// The label of the application's one window.
pub const MAIN_WINDOW: &str = "main";

/// The address the window opens: the listener's port on loopback. It stays the same
/// between runs, since the page's stored sign-in and language belong to its origin.
pub fn window_url(listening: SocketAddr) -> anyhow::Result<Url> {
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
pub fn open_request(app: &AppHandle, request: Request) {
    let app = app.clone();
    std::thread::spawn(move || match opening::delivery_script(&request) {
        Ok(script) => app.state::<Pages>().deliver(&app, script),
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

/// Opens the window on `own`, the server's loopback page.
pub fn open_window(app: &AppHandle, own: Url) -> anyhow::Result<()> {
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
            app.state::<Pages>().page_loaded(app, loaded);
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

/// Something to tell the user.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Notice {
    #[cfg(feature = "engine")]
    CouldNotStart,
    #[cfg(feature = "engine")]
    StoppedWorking,
    #[cfg(feature = "client")]
    ServiceNotAnswering,
    #[cfg(feature = "client")]
    ClientCouldNotStart,
    #[cfg(feature = "client")]
    NotTheService,
    DownloadFailed,
    FileNotOpened,
}

/// `notice` in `locale`, like `ru-RU`. A failure's reason under it stays in the logs'
/// English.
pub fn notice_text(notice: Notice, locale: Option<&str>) -> &'static str {
    let russian = locale.is_some_and(|locale| locale.starts_with("ru"));
    match (notice, russian) {
        #[cfg(feature = "engine")]
        (Notice::CouldNotStart, false) => "TorrPlay could not start",
        #[cfg(feature = "engine")]
        (Notice::CouldNotStart, true) => "Не удалось запустить TorrPlay",
        #[cfg(feature = "engine")]
        (Notice::StoppedWorking, false) => "TorrPlay stopped working",
        #[cfg(feature = "engine")]
        (Notice::StoppedWorking, true) => "TorrPlay перестал работать",
        #[cfg(feature = "client")]
        (Notice::ServiceNotAnswering, false) => "The TorrPlay service does not answer",
        #[cfg(feature = "client")]
        (Notice::ServiceNotAnswering, true) => "Служба TorrPlay не отвечает",
        #[cfg(feature = "client")]
        (Notice::ClientCouldNotStart, false) => "The TorrPlay client could not start",
        #[cfg(feature = "client")]
        (Notice::ClientCouldNotStart, true) => "Не удалось запустить клиент TorrPlay",
        #[cfg(feature = "client")]
        (Notice::NotTheService, false) => {
            "Another program answers at the TorrPlay service's address"
        }
        #[cfg(feature = "client")]
        (Notice::NotTheService, true) => "По адресу службы TorrPlay отвечает другая программа",
        (Notice::DownloadFailed, false) => "The file could not be saved",
        (Notice::DownloadFailed, true) => "Не удалось сохранить файл",
        (Notice::FileNotOpened, false) => "The file could not be opened",
        (Notice::FileNotOpened, true) => "Не удалось открыть файл",
    }
}

/// What the page has yet to be handed, shared by the window and whatever opens files.
#[derive(Default)]
pub struct Pages {
    delivery: Mutex<Delivery>,
}

/// Whether the page can run scripts yet, and the one waiting until it can: the page
/// opens only the newest of what it is handed, so only that waits.
#[derive(Default)]
struct Delivery {
    loaded: bool,
    waiting: Option<String>,
}

impl Pages {
    /// Runs `script` in the page, or once it has loaded.
    pub fn deliver(&self, app: &AppHandle, script: String) {
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
    pub fn page_loaded(&self, app: &AppHandle, loaded: bool) {
        let Ok(mut delivery) = self.delivery.lock() else {
            return;
        };
        delivery.loaded = loaded;
        if loaded && let Some(script) = delivery.waiting.take() {
            run_in_page(app, &script);
        }
    }
}

fn run_in_page(app: &AppHandle, script: &str) {
    if let Some(window) = app.get_webview_window(MAIN_WINDOW)
        && let Err(error) = window.eval(script)
    {
        tracing::warn!(%error, "the page cannot be handed what was opened");
    }
}

/// Brings the window forward, as a second launch does.
pub fn bring_forward(window: &WebviewWindow) {
    if let Err(error) = window.unminimize().and_then(|()| window.set_focus()) {
        tracing::warn!(%error, "the window cannot be brought forward");
    }
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
            #[cfg(feature = "engine")]
            Notice::CouldNotStart,
            #[cfg(feature = "engine")]
            Notice::StoppedWorking,
            #[cfg(feature = "client")]
            Notice::ServiceNotAnswering,
            #[cfg(feature = "client")]
            Notice::ClientCouldNotStart,
            #[cfg(feature = "client")]
            Notice::NotTheService,
            Notice::DownloadFailed,
            Notice::FileNotOpened,
        ] {
            let english = notice_text(notice, Some("en-US"));
            assert_eq!(notice_text(notice, None), english);
            assert_eq!(notice_text(notice, Some("de-DE")), english);
            assert_ne!(notice_text(notice, Some("ru-RU")), english, "{notice:?}");
        }
    }
}
