// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! The `torrplay` executable: parses the command line, starts logging and
//! the runtime, opens the store in the data directory, binds the API's
//! listener, and serves until a termination signal. `torrplay password`
//! changes a stopped application's password instead, `torrplay backup`
//! backs it up or restores it, `torrplay mcp` serves a running
//! application's API to an MCP client, and `torrplay service` runs as a
//! Windows service.

use std::io::IsTerminal;
use std::process::ExitCode;

mod service;

use torrplay::{
    BackupAction, Command, Config, LogLevel, McpConfig, PasswordChange, PasswordChanged, USAGE,
};

fn main() -> ExitCode {
    // Before parsing, since the service reads its settings from its settings
    // file rather than the environment, and reports a usage error to the service
    // manager rather than on standard error.
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    if arguments.first().map(String::as_str) == Some("service") {
        // Help and the version print as for any command, run by anyone.
        match torrplay::parse_args(arguments.iter().cloned(), None) {
            Ok(Command::Help) => {
                print!("{USAGE}");
                return ExitCode::SUCCESS;
            }
            Ok(Command::Version) => {
                println!("torrplay {}", env!("CARGO_PKG_VERSION"));
                return ExitCode::SUCCESS;
            }
            _ => return service::main(arguments),
        }
    }
    let listen_variable = std::env::var_os(torrplay::LISTEN_VARIABLE)
        .map(|value| value.to_string_lossy().into_owned());
    let config = match torrplay::parse_args(arguments, listen_variable) {
        Ok(Command::Run(config)) => config,
        // Only a first argument of `service` asks for it, which went above.
        Ok(Command::Service(_)) => return service::main(std::env::args().skip(1).collect()),
        Ok(Command::Password(change, config)) => return password(change, &config),
        Ok(Command::Backup(action, config)) => return backup(action, &config),
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
    let startup = torrplay::start_logging(&config, format);
    let log = startup.log.clone();
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
    let result = runtime.block_on(run(&config, startup));
    // Shutdown has stopped the engine; blocking work still running must not
    // hold the process.
    runtime.shutdown_timeout(torrplay::RUNTIME_SHUTDOWN);
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

async fn run(config: &Config, startup: torrplay::Startup) -> anyhow::Result<()> {
    // Before the listener binds, so the API never answers without them.
    let stop = torrplay::termination();
    torrplay::run_server(config, startup, |_| {}, stop).await
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

/// Backs up or restores a stopped application, logging warnings and errors
/// to standard error, where a failure's cause appears.
fn backup(action: BackupAction, config: &Config) -> ExitCode {
    let format = if std::io::stderr().is_terminal() {
        torrplay::LogFormat::Text
    } else {
        torrplay::LogFormat::Json
    };
    torrplay::init_logging(LogLevel::Warn, None, format, &torrplay::Run::draw(), None);
    let result = config.data_dir().and_then(|data_dir| {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        runtime.block_on(async {
            match &action {
                BackupAction::Create { output } => {
                    let (path, counts) = torrplay::create_backup(
                        &data_dir,
                        output.as_deref(),
                        std::time::SystemTime::now(),
                    )
                    .await?;
                    Ok(format!(
                        "wrote {} with {} torrents and {} profiles",
                        path.display(),
                        counts.torrents,
                        counts.profiles
                    ))
                }
                BackupAction::Restore { archive } => {
                    let counts = torrplay::restore_backup(&data_dir, archive).await?;
                    Ok(format!(
                        "restored {} torrents and {} profiles from {}; the next start adds the \
                         torrents",
                        counts.torrents,
                        counts.profiles,
                        archive.display()
                    ))
                }
            }
        })
    });
    match result {
        Ok(done) => {
            eprintln!("torrplay: {done}");
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
                .unwrap_or_else(|_| torrplay_mcp::URL_DEFAULT.to_owned());
            match torrplay::parse_origin(&value) {
                Some(url) => url,
                None => {
                    eprintln!(
                        "torrplay: {} must be an origin such as {}",
                        torrplay::URL_VARIABLE,
                        torrplay_mcp::URL_DEFAULT
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
                let shutdown = torrplay::termination().shutdown;
                let listener = torrplay_mcp::HttpListener::bind(address).await?;
                tracing::info!(
                    application = %url,
                    address = %listener.local_addr()?,
                    path = torrplay_mcp::HTTP_PATH,
                    "serving MCP over Streamable HTTP"
                );
                listener.serve(server, shutdown).await?;
            }
        }
        anyhow::Ok(())
    });
    runtime.shutdown_timeout(torrplay::RUNTIME_SHUTDOWN);
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            tracing::error!(error = %format!("{error:#}"), "torrplay mcp failed");
            ExitCode::FAILURE
        }
    }
}
