// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! `torrplay service`: the application as the Windows service of the service
//! edition, which the service manager starts. It reads its `TORRPLAY_` settings
//! from `%ProgramData%\TorrPlay\torrplay.env`, since the service manager gives a
//! service no environment of its own, keeps its data in that directory, and
//! maps the service's stop control, and the system's shutdown, to the shutdown
//! sequence that a termination signal begins elsewhere.
//!
//! The service manager refuses controls while a service stops, so a second
//! stop cannot hurry shutdown as a second signal does; the sequence's own limit
//! ends it. A usage error, such as an invalid `TORRPLAY_LISTEN`, and a failure
//! once started stop the service with a service-specific exit code, 2 and 1, as
//! the executable's are, which the service manager does not count as a crash,
//! so its restarts after crashes leave them stopped.

use std::process::ExitCode;

#[cfg(not(windows))]
pub fn main(_arguments: Vec<String>) -> ExitCode {
    eprintln!(
        "torrplay: `torrplay service` runs only under the Windows service manager; \
         elsewhere, a service manager such as systemd runs `torrplay` itself"
    );
    ExitCode::from(2)
}

#[cfg(windows)]
pub use windows::main;

#[cfg(windows)]
mod windows {
    use std::ffi::OsString;
    use std::process::ExitCode;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicU8, Ordering};
    use std::time::Duration;

    use torrplay::{Command, Config, LogFormat, StopRequests};
    use torrplay_settings_file::SettingsFile;
    use windows_service::service::{
        ServiceControl, ServiceControlAccept, ServiceExitCode, ServiceState, ServiceStatus,
        ServiceType,
    };
    use windows_service::service_control_handler::{
        self, ServiceControlHandlerResult, ServiceStatusHandle,
    };
    use windows_service::{define_windows_service, service_dispatcher};

    /// The service's name with the service manager, which the client looks up too.
    const SERVICE: &str = "torrplay";

    /// How long the service manager is told starting may take: opening a large
    /// store included, before the API listens.
    const START_WAIT: Duration = Duration::from_secs(60);

    /// How long it is told stopping may take: the shutdown sequence's limit and
    /// the process's exit.
    const STOP_WAIT: Duration = Duration::from_secs(35);

    /// What the service runs, decided before the service manager calls it: the
    /// configuration and log format, or why the command line or settings are not
    /// valid. The service manager's entry point takes no context of its own.
    static PREPARED: Mutex<Option<Result<(Config, LogFormat), String>>> = Mutex::new(None);

    /// The exit code the service stopped with, for the process's own.
    static EXIT: AtomicU8 = AtomicU8::new(0);

    define_windows_service!(ffi_service_main, service_main);

    /// Runs `torrplay service` with `arguments`, its whole command line after
    /// the executable's name.
    pub fn main(arguments: Vec<String>) -> ExitCode {
        let prepared = prepare(arguments);
        match PREPARED.lock() {
            Ok(mut slot) => *slot = Some(prepared),
            Err(poisoned) => *poisoned.into_inner() = Some(prepared),
        }
        // Returns once the service has stopped.
        match service_dispatcher::start(SERVICE, ffi_service_main) {
            Ok(()) => ExitCode::from(EXIT.load(Ordering::SeqCst)),
            Err(error) => {
                eprintln!(
                    "torrplay: `torrplay service` runs only when the Windows service \
                     manager starts it: {error}"
                );
                ExitCode::from(2)
            }
        }
    }

    /// The configuration of `arguments` with the settings file's variables, which
    /// take the place of the process's environment, and the data directory there
    /// unless `--data-dir` names another.
    fn prepare(arguments: Vec<String>) -> Result<(Config, LogFormat), String> {
        let directory = torrplay_settings_file::windows_directory()
            .ok_or("there is no ProgramData directory for the service's data")?;
        let path = directory.join(torrplay_settings_file::FILE_NAME);
        let settings = SettingsFile::read(&path)
            .map_err(|error| {
                format!(
                    "the settings file {} cannot be read: {error}",
                    path.display()
                )
            })?
            .unwrap_or_default();
        let variable = |name: &str| {
            settings.get(name).map(str::to_owned).or_else(|| {
                std::env::var_os(name).map(|value| value.to_string_lossy().into_owned())
            })
        };
        let mut config = match torrplay::parse_args(arguments, variable(torrplay::LISTEN_VARIABLE))
        {
            Ok(Command::Service(config)) => config,
            Ok(_) => return Err("`service` takes the options of a run only".to_owned()),
            Err(error) => return Err(error.to_string()),
        };
        if config.data_dir.is_none() {
            config.data_dir = Some(directory);
        }
        // The service has no terminal, so JSON unless the settings say otherwise.
        let format = torrplay::log_format(
            config.log_format,
            variable(torrplay::LOG_FORMAT_VARIABLE),
            false,
        )
        .map_err(|error| error.to_string())?;
        Ok((config, format))
    }

    fn service_main(_arguments: Vec<OsString>) {
        let prepared = match PREPARED.lock() {
            Ok(mut slot) => slot.take(),
            Err(poisoned) => poisoned.into_inner().take(),
        };
        let code = run(prepared.unwrap_or_else(|| Err("the service was not prepared".to_owned())));
        EXIT.store(code, Ordering::SeqCst);
    }

    fn report(
        status: ServiceStatusHandle,
        state: ServiceState,
        controls: ServiceControlAccept,
        code: u8,
        wait: Duration,
    ) {
        let exit_code = match code {
            0 => ServiceExitCode::Win32(0),
            code => ServiceExitCode::ServiceSpecific(code.into()),
        };
        let result = status.set_service_status(ServiceStatus {
            service_type: ServiceType::OWN_PROCESS,
            current_state: state,
            controls_accepted: controls,
            exit_code,
            checkpoint: 0,
            wait_hint: wait,
            process_id: None,
        });
        if let Err(error) = result {
            tracing::warn!(%error, ?state, "the service manager did not take the service's state");
        }
    }

    /// Runs the service and returns its exit code: 0, 1 when the application
    /// failed, or 2 for a usage error.
    fn run(prepared: Result<(Config, LogFormat), String>) -> u8 {
        let (stopping, mut stops) = tokio::sync::mpsc::unbounded_channel();
        let handler = move |control| match control {
            ServiceControl::Stop | ServiceControl::Preshutdown => {
                let _ = stopping.send(());
                ServiceControlHandlerResult::NoError
            }
            ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
            _ => ServiceControlHandlerResult::NotImplemented,
        };
        let Ok(status) = service_control_handler::register(SERVICE, handler) else {
            return 1;
        };
        let none = ServiceControlAccept::empty();
        report(status, ServiceState::StartPending, none, 0, START_WAIT);
        let (config, format) = match prepared {
            Ok(prepared) => prepared,
            Err(message) => {
                // Where the service logs, so the reason is not lost with standard
                // error, which a service has nowhere.
                let config = Config {
                    data_dir: torrplay_settings_file::windows_directory(),
                    ..Config::default()
                };
                let startup = torrplay::start_logging(&config, LogFormat::Json);
                tracing::error!(%message, "the service's command line or settings are not valid");
                startup.log.flush();
                report(status, ServiceState::Stopped, none, 2, Duration::ZERO);
                return 2;
            }
        };
        let startup = torrplay::start_logging(&config, format);
        let log = startup.log.clone();
        let runtime = match tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
        {
            Ok(runtime) => runtime,
            Err(error) => {
                tracing::error!(%error, "the runtime did not start");
                log.flush();
                report(status, ServiceState::Stopped, none, 1, Duration::ZERO);
                return 1;
            }
        };
        let stop = StopRequests::new(async move {
            // The handler, which holds the sender, lasts as long as the process.
            if stops.recv().await.is_none() {
                std::future::pending::<()>().await;
            }
            tracing::info!("the service manager asked the service to stop");
            report(status, ServiceState::StopPending, none, 0, STOP_WAIT);
        });
        let listening = |_| {
            report(
                status,
                ServiceState::Running,
                ServiceControlAccept::STOP | ServiceControlAccept::PRESHUTDOWN,
                0,
                Duration::ZERO,
            );
        };
        let result = runtime.block_on(torrplay::run_server(&config, startup, listening, stop));
        runtime.shutdown_timeout(torrplay::RUNTIME_SHUTDOWN);
        let code = match result {
            Ok(()) => 0,
            Err(error) => {
                tracing::error!(error = %format!("{error:#}"), "torrplay failed");
                1
            }
        };
        log.flush();
        report(status, ServiceState::Stopped, none, code, Duration::ZERO);
        code
    }
}
