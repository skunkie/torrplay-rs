// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! The application: the server in this process, and the window on its loopback page.
//! It takes the server's command line.

use std::io::IsTerminal;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::process::ExitCode;
use std::sync::Mutex;

use anyhow::{Context, anyhow};
use tauri::{AppHandle, Manager, RunEvent, WebviewWindow};
use tauri_plugin_dialog::{DialogExt, MessageDialogKind};
use tokio::sync::oneshot;
use torrplay::{Command, Config, EngineDidNotStart, LogFormat};

use crate::opening::{self, Request};
use crate::window::{self, Notice, Pages};

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
                let opened =
                    window::window_url(listening).and_then(|own| window::open_window(&opener, own));
                if let Err(error) = opened {
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
        if let Some(window) = app.get_webview_window(window::MAIN_WINDOW)
            && let Err(error) = window.hide()
        {
            tracing::warn!(%error, "the window cannot be hidden");
        }
        app.dialog()
            .message(format!("{error:#}"))
            .title(window::notice_text(
                notice,
                sys_locale::get_locale().as_deref(),
            ))
            .kind(MessageDialogKind::Error)
            .blocking_show();
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
}

impl Shell {
    fn new(stop: oneshot::Sender<()>, options: Vec<String>) -> Self {
        Self {
            stop: Mutex::new(Some(stop)),
            stage: Mutex::new(Stage::Starting),
            relaunch: Mutex::new(None),
            options,
            window_failure: Mutex::new(None),
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
        if let Some(window) = window {
            window::bring_forward(&window);
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

/// The application's entry point.
pub fn main() -> ExitCode {
    let listen_variable = std::env::var_os(torrplay::LISTEN_VARIABLE)
        .map(|value| value.to_string_lossy().into_owned());
    let directory = std::env::current_dir().unwrap_or_default();
    let (options, requests) = opening::split_arguments(
        std::env::args().skip(1),
        &directory,
        &opening::options_with_values(torrplay::USAGE),
    );
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
                    &opening::options_with_values(torrplay::USAGE),
                );
                let now = app
                    .state::<Shell>()
                    .second_launch(app.get_webview_window(window::MAIN_WINDOW), requests);
                for request in now {
                    window::open_request(app, request);
                }
            },
        ))
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .manage(Shell::new(stop, options))
        .manage(Pages::default())
        .setup(move |app| {
            let handle = app.handle().clone();
            for request in requests {
                window::open_request(&handle, request);
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
                window::open_request(app, request);
            }
        }
        _ => {}
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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
