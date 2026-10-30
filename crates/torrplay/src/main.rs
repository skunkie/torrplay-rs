// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! The `torrplay` executable: parses the command line, starts logging and
//! the runtime, opens the store in the data directory, binds the API's
//! listener, and serves until a termination signal. `torrplay password`
//! changes a stopped application's password instead, and `torrplay mcp`
//! serves a running application's API to an MCP client.

use std::io::IsTerminal;
use std::process::ExitCode;
use std::time::Duration;

use anyhow::Context;
use torrplay::{
    Command, Config, McpConfig, PasswordChange, PasswordChanged, ShutdownTimings, USAGE,
};
use torrplay_core::credentials::AllowedHosts;
use torrplay_engine::LibrqbitEngine;
use torrplay_store::Store;

/// How long exiting waits for blocking tasks still running after shutdown.
const RUNTIME_SHUTDOWN: Duration = Duration::from_secs(1);

fn main() -> ExitCode {
    let listen_variable = std::env::var_os(torrplay::LISTEN_VARIABLE)
        .map(|value| value.to_string_lossy().into_owned());
    let config = match torrplay::parse_args(std::env::args().skip(1), listen_variable) {
        Ok(Command::Run(config)) => config,
        Ok(Command::Password(change, config)) => return password(change, &config),
        Ok(Command::Mcp(config)) => return mcp(config),
        Ok(Command::Help) => {
            print!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Ok(Command::Version) => {
            println!("torrplay {}", env!("CARGO_PKG_VERSION"));
            return ExitCode::SUCCESS;
        }
        Err(error) => {
            eprintln!("torrplay: {error}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    let variable = std::env::var_os(torrplay::LOG_FORMAT_VARIABLE)
        .map(|value| value.to_string_lossy().into_owned());
    let format =
        match torrplay::log_format(config.log_format, variable, std::io::stderr().is_terminal()) {
            Ok(format) => format,
            Err(error) => {
                eprintln!("torrplay: {error}\n\n{USAGE}");
                return ExitCode::from(2);
            }
        };
    let process_run = torrplay::Run::draw();
    // The log files live in the data directory; without one, records go to
    // standard error alone, and the failure is logged once logging starts.
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
        format,
        &process_run,
        files,
    );
    if let Some(error) = files_failed {
        tracing::warn!(
            error = %format!("{error:#}"),
            "the log files could not be opened; logs go to standard error only"
        );
    }
    match torrplay_engine::raise_open_file_limit() {
        Ok(limit) => tracing::info!(limit, "open file limit"),
        Err(error) => tracing::warn!(
            error = %format!("{error:#}"),
            "the open file limit could not be raised; busy torrents may exhaust it"
        ),
    }
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            tracing::error!(%error, "the runtime did not start");
            return ExitCode::FAILURE;
        }
    };
    let result = runtime.block_on(run(&config, log.clone(), process_run, log_files));
    // Shutdown has stopped the engine; blocking work still running must not
    // hold the process.
    runtime.shutdown_timeout(RUNTIME_SHUTDOWN);
    let code = match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            tracing::error!(error = %format!("{error:#}"), "torrplay failed");
            ExitCode::FAILURE
        }
    };
    log.flush();
    code
}

async fn run(
    config: &Config,
    log: torrplay::LogControl,
    run: torrplay::Run,
    log_files: Option<torrplay::LogFiles>,
) -> anyhow::Result<()> {
    // Before the listener binds, so the API never answers without them.
    let terminated = termination();
    let data_dir = config.data_dir()?;
    let opening = data_dir.clone();
    let (store, contents) = tokio::task::spawn_blocking(move || Store::open(&opening))
        .await
        .context("opening the store")?
        .with_context(|| format!("opening the store in {}", data_dir.display()))?;
    tracing::info!(
        path = %store.path().display(),
        torrents = contents.torrents.len(),
        "the store is open"
    );
    let download_dir = config.download_dir(&data_dir);
    tracing::info!(path = %download_dir.display(), "the download directory");
    // Memory storage writes nothing to the session folder, so it lives only
    // as long as the process; torrents persist in the store.
    let folder = tempfile::tempdir().context("creating the session folder")?;
    let listener = torrplay::bind_api(config.listen).with_context(|| {
        format!(
            "binding the API listener on {}",
            config.listen.unwrap_or(torrplay::DEFAULT_LISTEN)
        )
    })?;
    tracing::info!(address = %listener.local_addr()?, "the API is listening");
    torrplay::serve(
        listener,
        LibrqbitEngine::new(config.engine_config(folder.path(), &download_dir)),
        store,
        contents,
        torrplay::ServeOptions {
            allowed_hosts: AllowedHosts::new(config.allowed_hosts.clone()),
            overrides: config.overrides(),
            log: Some(log),
            run,
            log_files,
        },
        ShutdownTimings::default(),
        terminated,
    )
    .await
}

/// Runs `torrplay password`, reading a new password from standard input.
fn password(change: PasswordChange, config: &Config) -> ExitCode {
    let input = match change {
        PasswordChange::Set => {
            let mut line = String::new();
            match std::io::stdin().read_line(&mut line) {
                Ok(0) => None,
                Ok(_) => Some(line.trim_end_matches(['\r', '\n']).to_owned()),
                Err(error) => {
                    eprintln!("torrplay: reading the password: {error}");
                    return ExitCode::FAILURE;
                }
            }
        }
        PasswordChange::Remove => None,
    };
    let result = config.data_dir().and_then(|data_dir| {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?
            .block_on(torrplay::change_password(&data_dir, change, input))
    });
    match result {
        Ok(changed) => {
            eprintln!(
                "torrplay: {}",
                match changed {
                    PasswordChanged::Set => "the password is set; authentication is on",
                    PasswordChanged::Replaced => "the password is replaced",
                    PasswordChanged::Removed =>
                        "the password is removed and every token revoked; authentication is off",
                    PasswordChanged::NoneSet => "no password is set; authentication is off",
                }
            );
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("torrplay: {error:#}");
            ExitCode::FAILURE
        }
    }
}

/// Serves the MCP server, logging to standard error only: on stdio,
/// standard output carries nothing but protocol messages. It writes no log
/// files, which belong to the application's data directory.
fn mcp(config: McpConfig) -> ExitCode {
    let variable = std::env::var_os(torrplay::LOG_FORMAT_VARIABLE)
        .map(|value| value.to_string_lossy().into_owned());
    let format = match torrplay::log_format(None, variable, std::io::stderr().is_terminal()) {
        Ok(format) => format,
        Err(error) => {
            eprintln!("torrplay: {error}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    torrplay::init_logging(
        config.log_level.unwrap_or_default(),
        None,
        format,
        &torrplay::Run::draw(),
        None,
    );
    let url = match config.url {
        Some(url) => url,
        None => {
            let value = std::env::var(torrplay::URL_VARIABLE)
                .unwrap_or_else(|_| torrplay_mcp::DEFAULT_URL.to_owned());
            match torrplay::parse_origin(&value) {
                Some(url) => url,
                None => {
                    eprintln!(
                        "torrplay: {} must be an origin such as {}",
                        torrplay::URL_VARIABLE,
                        torrplay_mcp::DEFAULT_URL
                    );
                    return ExitCode::from(2);
                }
            }
        }
    };
    let token = std::env::var(torrplay::TOKEN_VARIABLE)
        .ok()
        .filter(|token| !token.is_empty());
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            tracing::error!(%error, "the runtime did not start");
            return ExitCode::FAILURE;
        }
    };
    let result = runtime.block_on(async {
        let client = torrplay_mcp::Client::new(url.clone(), token)?;
        let server = torrplay_mcp::Server::new(client);
        match config.http {
            None => {
                tracing::info!(application = %url, "serving MCP on standard input and output");
                torrplay_mcp::serve_stdio(server).await?;
            }
            Some(address) => {
                let listener = torrplay_mcp::HttpListener::bind(address).await?;
                tracing::info!(
                    application = %url,
                    address = %listener.local_addr()?,
                    path = torrplay_mcp::HTTP_PATH,
                    "serving MCP over Streamable HTTP"
                );
                listener.serve(server, termination()).await?;
            }
        }
        anyhow::Ok(())
    });
    runtime.shutdown_timeout(RUNTIME_SHUTDOWN);
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            tracing::error!(error = %format!("{error:#}"), "torrplay mcp failed");
            ExitCode::FAILURE
        }
    }
}

/// Completes on the first termination signal: SIGTERM or SIGINT on Unix,
/// Ctrl+C elsewhere.
/// Waits for SIGTERM or SIGINT, or Ctrl+C on Windows. The handlers are
/// installed when this is called, not when the future is first polled, so
/// a signal that arrives once the API listens shuts down gracefully instead
/// of ending the process.
fn termination() -> impl std::future::Future<Output = ()> {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let terminate = signal(SignalKind::terminate());
        let interrupt = signal(SignalKind::interrupt());
        async move {
            match (terminate, interrupt) {
                (Ok(mut terminate), Ok(mut interrupt)) => {
                    tokio::select! {
                        _ = terminate.recv() => {}
                        _ = interrupt.recv() => {}
                    }
                }
                (terminate, interrupt) => {
                    if let Err(error) = terminate.as_ref().and(interrupt.as_ref()) {
                        tracing::warn!(%error, "a termination signal cannot be handled");
                    }
                    match (terminate, interrupt) {
                        (Ok(mut signal), Err(_)) | (Err(_), Ok(mut signal)) => {
                            signal.recv().await;
                        }
                        _ => std::future::pending().await,
                    }
                }
            }
        }
    }
    #[cfg(not(unix))]
    {
        let interrupt = tokio::signal::windows::ctrl_c();
        async move {
            match interrupt {
                Ok(mut interrupt) => {
                    interrupt.recv().await;
                }
                Err(error) => {
                    tracing::warn!(%error, "Ctrl+C cannot be handled");
                    std::future::pending().await
                }
            }
        }
    }
}
