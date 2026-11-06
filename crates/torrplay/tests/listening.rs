// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! OBS-11: the system resource lists the URLs of the addresses `torrplay`
//! listens on.

// The helpers that start and stop the process serve only these tests.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::net::{IpAddr, UdpSocket};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;

mod ports;

use ports::free_ports;

/// The address this machine would send to the internet from, if it has one.
/// Connecting a UDP socket sends nothing, and the documentation address
/// answers no one.
fn outgoing_address() -> Option<IpAddr> {
    let socket = UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("192.0.2.1:9").ok()?;
    let address = socket.local_addr().ok()?.ip();
    (!address.is_unspecified() && !address.is_loopback()).then_some(address)
}

/// The system resource's URLs from `torrplay` listening on `host`, and the
/// port it uses.
async fn urls_on(host: &str) -> (Vec<String>, u16) {
    let data = tempfile::TempDir::new().expect("a data directory");
    let (port, peer_port) = free_ports().expect("ports");
    let mut child = Command::new(env!("CARGO_BIN_EXE_torrplay"))
        .args(["--no-dht", "--listen"])
        .arg(format!("{host}:{port}"))
        .arg("--peer-port")
        .arg(peer_port.to_string())
        .arg("--data-dir")
        .arg(data.path())
        .env_remove("TORRPLAY_LISTEN")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("torrplay starts");
    let url = format!("http://127.0.0.1:{port}/api/v1/system");
    let deadline = Instant::now() + Duration::from_secs(30);
    let response = loop {
        match reqwest::get(&url).await {
            Ok(response) => break response,
            Err(error) => {
                let exit = child.try_wait().expect("its status");
                if exit.is_some() || Instant::now() >= deadline {
                    child.kill().expect("torrplay stops");
                    let output = child.wait_with_output().expect("its exit");
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    panic!("torrplay did not listen ({exit:?}): {error}\n{stderr}");
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        }
    };
    let body: Value = response.json().await.expect("JSON");
    child.kill().expect("torrplay stops");
    child.wait().expect("its exit");
    let urls = body["urls"]
        .as_array()
        .expect("URLs")
        .iter()
        .map(|url| url.as_str().expect("a URL").to_owned())
        .collect();
    (urls, port)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn listening_on_every_interface_lists_this_machines_addresses() {
    let (urls, port) = urls_on("0.0.0.0").await;
    for url in &urls {
        assert!(url.ends_with(&format!(":{port}")), "{url}");
        assert!(!url.contains("127.0.0.1"), "{url}");
    }
    // A machine with no network has nothing to list.
    if let Some(address) = outgoing_address() {
        assert!(
            urls.contains(&format!("http://{address}:{port}")),
            "{address} is in {urls:?}"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn listening_on_loopback_lists_nothing() {
    let (urls, _) = urls_on("127.0.0.1").await;
    assert_eq!(urls, Vec::<String>::new());
}
