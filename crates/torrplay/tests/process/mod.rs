// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! Starts TorrPlay executables with OS-assigned ports and captures their logs.

// Each integration test crate uses a different subset of these methods.
#![allow(dead_code)]

use std::net::{Ipv4Addr, SocketAddr};
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde_json::Value;
use tempfile::NamedTempFile;
use tokio::process::{Child, Command};
use tokio::time::{Instant, sleep, timeout};
use torrplay::{ALLOWED_HOSTS_VARIABLE, LogFormat};

const WAIT: Duration = Duration::from_secs(30);
const POLL: Duration = Duration::from_millis(50);

#[derive(Debug)]
pub struct Process {
    child: Child,
    stderr: NamedTempFile,
    address: SocketAddr,
    client: reqwest::Client,
}

impl Process {
    pub async fn start(
        data: &Path,
        host: Ipv4Addr,
        peer_port: u16,
        format: LogFormat,
        level: &str,
    ) -> Result<Self> {
        let format_arg = match format {
            LogFormat::Json => "json",
            LogFormat::Text => "text",
        };
        let stderr = NamedTempFile::new()?;
        let mut child = Command::new(env!("CARGO_BIN_EXE_torrplay"))
            .args(["--no-dht", "--log-format", format_arg, "--log-level", level])
            .args([
                "--listen",
                &format!("{host}:0"),
                "--peer-port",
                &peer_port.to_string(),
            ])
            .arg("--data-dir")
            .arg(data)
            .env_remove(ALLOWED_HOSTS_VARIABLE)
            .env("NO_COLOR", "1")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(stderr.reopen()?)
            .kill_on_drop(true)
            .spawn()
            .context("starting torrplay")?;
        let client = reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(5))
            .build()?;
        let deadline = Instant::now() + WAIT;
        let mut address = None;
        loop {
            if let Some(status) = child.try_wait()? {
                bail!(
                    "torrplay exited before it became ready: {status}\n{}",
                    read_stderr(stderr.path()).await
                );
            }
            // The child binds the port directly. Reading its own log avoids a
            // gap between choosing a port and binding the listener.
            if address.is_none() {
                address = listening_address(&tokio::fs::read(stderr.path()).await?, format)?;
            }
            if let Some(address) = address {
                let loopback = SocketAddr::from((Ipv4Addr::LOCALHOST, address.port()));
                if client
                    .get(format!("http://{loopback}/readyz"))
                    .timeout(Duration::from_secs(1))
                    .send()
                    .await
                    .is_ok_and(|response| response.status().is_success())
                {
                    return Ok(Self {
                        child,
                        stderr,
                        address: loopback,
                        client,
                    });
                }
            }
            if Instant::now() >= deadline {
                let stderr = read_stderr(stderr.path()).await;
                timeout(WAIT, child.kill())
                    .await
                    .context("stopping torrplay after the readiness timeout")??;
                bail!("torrplay did not become ready\n{stderr}");
            }
            sleep(POLL).await;
        }
    }

    pub fn address(&self) -> SocketAddr {
        self.address
    }

    pub fn url(&self, path: &str) -> String {
        format!("http://{}{path}", self.address)
    }

    pub fn client(&self) -> &reqwest::Client {
        &self.client
    }

    pub async fn records(&self) -> Result<Vec<Value>> {
        let bytes = tokio::fs::read(self.stderr.path()).await?;
        json_lines(&bytes)
    }

    pub async fn stderr(&self) -> String {
        read_stderr(self.stderr.path()).await
    }

    pub async fn kill(&mut self) -> Result<()> {
        timeout(WAIT, self.child.kill())
            .await
            .context("stopping torrplay")??;
        Ok(())
    }

    #[cfg(unix)]
    pub async fn signal(&self) -> Result<()> {
        let pid = self.child.id().context("torrplay is still running")?;
        let status = Command::new("kill")
            .args(["-TERM", &pid.to_string()])
            .status()
            .await?;
        anyhow::ensure!(
            status.success(),
            "could not signal torrplay: {status}\n{}",
            self.stderr().await
        );
        Ok(())
    }

    #[cfg(unix)]
    pub async fn terminate(&mut self) -> Result<()> {
        self.signal().await?;
        self.wait_for_exit().await
    }

    #[cfg(unix)]
    pub async fn wait_for_exit(&mut self) -> Result<()> {
        let result = timeout(WAIT, self.child.wait()).await;
        let status = match result {
            Ok(status) => status?,
            Err(_) => bail!("torrplay did not stop\n{}", self.stderr().await),
        };
        anyhow::ensure!(
            status.success(),
            "torrplay exited with {status}\n{}",
            self.stderr().await
        );
        Ok(())
    }

    /// The signal test runs with JSON logs and waits for each shutdown phase.
    #[cfg(unix)]
    pub async fn wait_for_log(&mut self, message: &str) -> Result<()> {
        let deadline = Instant::now() + WAIT;
        loop {
            if self.records().await?.iter().any(|record| {
                record["message"]
                    .as_str()
                    .is_some_and(|text| text.starts_with(message))
            }) {
                return Ok(());
            }
            anyhow::ensure!(
                self.child.try_wait()?.is_none() && Instant::now() < deadline,
                "torrplay did not log {message:?}\n{}",
                self.stderr().await
            );
            sleep(POLL).await;
        }
    }
}

fn listening_address(bytes: &[u8], format: LogFormat) -> Result<Option<SocketAddr>> {
    let address = match format {
        LogFormat::Json => json_lines(bytes)?
            .into_iter()
            .find(|record| record["message"] == "the API is listening")
            .map(|record| {
                record["fields"]["address"]
                    .as_str()
                    .context("the API log address")
                    .map(str::to_owned)
            })
            .transpose()?,
        LogFormat::Text => complete_lines(bytes)
            .filter_map(|line| std::str::from_utf8(line).ok())
            .find_map(|line| line.split_once("the API is listening address="))
            .map(|(_, address)| address.trim().to_owned()),
    };
    address
        .map(|address| address.parse().context("the API listener address"))
        .transpose()
}

fn complete_lines(bytes: &[u8]) -> impl Iterator<Item = &[u8]> {
    // The child may still be writing the last line.
    bytes
        .split_inclusive(|&byte| byte == b'\n')
        .map_while(|line| line.strip_suffix(b"\n"))
}

fn json_lines(bytes: &[u8]) -> Result<Vec<Value>> {
    complete_lines(bytes)
        .map(|line| serde_json::from_slice(line).context("torrplay's standard error contains JSON"))
        .collect()
}

async fn read_stderr(path: &Path) -> String {
    match tokio::fs::read(path).await {
        Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
        Err(error) => format!("could not read torrplay's standard error: {error}"),
    }
}
