// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! The `torrplay` executable: parses the command line, starts logging and
//! the runtime, opens the store in the data directory, binds the API's
//! loopback listener, and serves until a termination signal.

use std::process::ExitCode;
use std::time::Duration;

use anyhow::Context;
use tokio::net::TcpListener;
use torrplay::{Command, Config, ShutdownTimings, USAGE};
use torrplay_engine::LibrqbitEngine;
use torrplay_store::Store;

/// How long exiting waits for blocking tasks still running after shutdown.
const RUNTIME_SHUTDOWN: Duration = Duration::from_secs(1);

fn main() -> ExitCode {
    let config = match torrplay::parse_args(std::env::args().skip(1)) {
        Ok(Command::Run(config)) => config,
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
    let (store, stored) = tokio::task::spawn_blocking(move || Store::open(&opening))
        .await
        .context("opening the store")?
        .with_context(|| format!("opening the store in {}", data_dir.display()))?;
    tracing::info!(path = %store.path().display(), torrents = stored.len(), "the store is open");
    // Memory storage writes nothing to the session folder, so it lives only
    // as long as the process; torrents persist in the store.
    let folder = tempfile::tempdir().context("creating the session folder")?;
    let listener = TcpListener::bind(config.listen)
        .await
        .with_context(|| format!("binding the API listener on {}", config.listen))?;
    tracing::info!(address = %listener.local_addr()?, "the API is listening");
    torrplay::serve(
        listener,
        LibrqbitEngine::new(config.engine_config(folder.path())),
        store,
        stored,
        ShutdownTimings::default(),
        termination(),
    )
    .await
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
