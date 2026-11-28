// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! Opens the reference installed with the Windows desktop packages or macOS app.

use std::path::{Path, PathBuf};

#[cfg(any(windows, all(target_os = "macos", feature = "engine")))]
use anyhow::Context;
#[cfg(any(windows, all(target_os = "macos", feature = "engine")))]
use tauri::AppHandle;
#[cfg(any(windows, all(target_os = "macos", feature = "engine")))]
use tauri_plugin_dialog::{DialogExt, MessageDialogKind};

#[cfg(any(target_os = "macos", test))]
fn bundle_help_path(executable: &Path) -> Option<PathBuf> {
    let binary_dir = executable.parent()?;
    let contents = binary_dir.parent()?;
    if binary_dir.file_name()? != "MacOS" || contents.file_name()? != "Contents" {
        return None;
    }
    let path = contents.join("Resources/help/index.html");
    path.is_file().then_some(path)
}

#[cfg(all(target_os = "macos", feature = "engine"))]
pub(crate) fn bundled_help() -> Option<PathBuf> {
    bundle_help_path(&std::env::current_exe().ok()?.canonicalize().ok()?)
}

/// NSIS and MSI install the guide beside the desktop executable.
#[cfg(any(windows, test))]
fn windows_help_path(executable: &Path) -> Option<PathBuf> {
    let path = executable.parent()?.join("help").join("index.html");
    path.is_file().then_some(path)
}

#[cfg(windows)]
fn bundled_help() -> Option<PathBuf> {
    windows_help_path(&std::env::current_exe().ok()?)
}

#[cfg(any(windows, all(target_os = "macos", feature = "engine")))]
pub(crate) fn help_labels(locale: Option<&str>) -> (&'static str, &'static str) {
    if locale.is_some_and(|locale| locale.starts_with("ru")) {
        ("Справка", "Справка TorrPlay")
    } else {
        ("Help", "TorrPlay Help")
    }
}

#[cfg(any(windows, all(target_os = "macos", feature = "engine")))]
pub(crate) fn open_help(app: &AppHandle) {
    use std::sync::atomic::{AtomicBool, Ordering};

    static OPENING_HELP: AtomicBool = AtomicBool::new(false);
    if OPENING_HELP.swap(true, Ordering::Relaxed) {
        return;
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        // The system opener can block while handing the file to the browser.
        let result = tauri::async_runtime::spawn_blocking(|| {
            let path = bundled_help().context("finding bundled offline help")?;
            tauri_plugin_opener::open_path(&path, None::<&str>)
                .with_context(|| format!("opening {}", path.display()))?;
            Ok::<_, anyhow::Error>(path)
        })
        .await
        .context("waiting for the offline help opener")
        .and_then(|result| result);
        OPENING_HELP.store(false, Ordering::Relaxed);
        let error = match result {
            Ok(path) => {
                tracing::info!(path = %path.display(), "opened offline help");
                return;
            }
            Err(error) => error,
        };
        tracing::error!(error = %format!("{error:#}"), "could not open TorrPlay Help");
        let locale = sys_locale::get_locale();
        let (_, label) = help_labels(locale.as_deref());
        let russian = locale
            .as_deref()
            .is_some_and(|locale| locale.starts_with("ru"));
        #[cfg(target_os = "macos")]
        let message = if russian {
            "Не удалось открыть справку TorrPlay. В Finder выберите «Показать содержимое \
             пакета» для TorrPlay.app и откройте Contents/Resources/help/index.html."
        } else {
            "Could not open TorrPlay Help. In Finder, choose Show Package Contents for \
             TorrPlay.app and open Contents/Resources/help/index.html."
        };
        #[cfg(windows)]
        let message = if russian {
            "Не удалось открыть справку TorrPlay. Откройте help\\index.html в папке \
             установки. Если файла нет, переустановите TorrPlay."
        } else {
            "Could not open TorrPlay Help. Open help\\index.html in the \
             installation folder. If the file is missing, reinstall TorrPlay."
        };
        app.dialog()
            .message(message)
            .title(label)
            .kind(MessageDialogKind::Error)
            .show(|_| {});
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn help_is_found_in_a_moved_bundle_with_resources() -> anyhow::Result<()> {
        let directory = tempfile::tempdir()?;
        let original = directory.path().join("TorrPlay.app");
        let bundle = original.join("Contents");
        let binary_dir = bundle.join("MacOS");
        let executable = binary_dir.join("torrplay-desktop");
        let help = bundle.join("Resources/help/index.html");
        std::fs::create_dir_all(&binary_dir)?;
        assert_eq!(bundle_help_path(&executable), None);
        std::fs::create_dir_all(bundle.join("Resources/help"))?;
        std::fs::write(&help, "offline help")?;
        assert_eq!(bundle_help_path(&executable), Some(help));
        let moved = directory.path().join("Moved TorrPlay.app");
        std::fs::rename(original, &moved)?;
        assert_eq!(
            bundle_help_path(&moved.join("Contents/MacOS/torrplay-desktop")),
            Some(moved.join("Contents/Resources/help/index.html"))
        );
        assert_eq!(
            bundle_help_path(&directory.path().join("torrplay-desktop")),
            None
        );
        Ok(())
    }

    #[test]
    fn windows_help_requires_an_installed_file() -> anyhow::Result<()> {
        let directory = tempfile::tempdir()?;
        let installed = directory.path().join("TorrPlay \u{96ea}");
        let client = installed.join("torrplay-client.exe");
        let application = installed.join("torrplay-desktop.exe");
        let help = installed.join("help").join("index.html");

        assert_eq!(windows_help_path(&client), None);
        std::fs::create_dir_all(&help)?;
        assert_eq!(windows_help_path(&client), None);
        std::fs::remove_dir(&help)?;
        std::fs::write(&help, "offline help")?;
        assert_eq!(windows_help_path(&client).as_ref(), Some(&help));
        assert_eq!(windows_help_path(&application), Some(help));
        Ok(())
    }
}
