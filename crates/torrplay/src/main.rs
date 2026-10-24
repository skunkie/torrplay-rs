// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! The `torrplay` executable: parses the command line, starts logging and
//! the runtime, opens the store in the data directory, binds the API's
//! listener, and serves until a termination signal. `torrplay password`
//! changes a stopped application's password instead.

use std::process::ExitCode;
use std::time::Duration;

use anyhow::Context;
use tokio::net::TcpListener;
use torrplay::{Command, Config, PasswordChange, PasswordChanged, ShutdownTimings, USAGE};
use torrplay_core::credentials::AllowedHosts;
use torrplay_engine::LibrqbitEngine;
use torrplay_store::Store;

/// How long exiting waits for blocking tasks still running after shutdown.
const RUNTIME_SHUTDOWN: Duration = Duration::from_secs(1);

fn main() -> ExitCode {
    let config = match torrplay::parse_args(std::env::args().skip(1)) {
        Ok(Command::Run(config)) => config,
        Ok(Command::Password(change, config)) => return password(change, &config),
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
    torrplay::init_logging(config.log_level, config.log_filter.as_deref());
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
    let result = runtime.block_on(run(&config));
    // Shutdown has stopped the engine; blocking work still running must not
    // hold the process.
    runtime.shutdown_timeout(RUNTIME_SHUTDOWN);
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            tracing::error!(error = %format!("{error:#}"), "torrplay failed");
            ExitCode::FAILURE
        }
    }
}

async fn run(config: &Config) -> anyhow::Result<()> {
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
    let listener = TcpListener::bind(config.listen)
        .await
        .with_context(|| format!("binding the API listener on {}", config.listen))?;
    tracing::info!(address = %listener.local_addr()?, "the API is listening");
    torrplay::serve(
        listener,
        LibrqbitEngine::new(config.engine_config(folder.path(), &download_dir)),
        store,
        contents,
        AllowedHosts::new(config.allowed_hosts.clone()),
        ShutdownTimings::default(),
        termination(),
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

/// Completes on the first termination signal: SIGTERM or SIGINT on Unix,
/// Ctrl+C elsewhere.
async fn termination() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        match signal(SignalKind::terminate()) {
            Ok(mut terminate) => {
                tokio::select! {
                    _ = terminate.recv() => {}
                    _ = tokio::signal::ctrl_c() => {}
                }
            }
            Err(error) => {
                tracing::warn!(%error, "SIGTERM cannot be handled; waiting for SIGINT");
                let _ = tokio::signal::ctrl_c().await;
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}
