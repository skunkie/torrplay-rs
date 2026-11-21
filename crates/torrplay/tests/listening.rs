// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! OBS-11: the system resource lists the URLs of the addresses `torrplay`
//! listens on.

// The helpers that start and stop the process serve only these tests.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::net::{IpAddr, Ipv4Addr, UdpSocket};

use serde_json::Value;
use torrplay::LogFormat;

mod process;

use process::Process;

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
async fn urls_on(host: Ipv4Addr) -> (Vec<String>, u16) {
    let data = tempfile::TempDir::new().expect("a data directory");
    let mut process = Process::start(data.path(), host, 0, LogFormat::Json, "info")
        .await
        .unwrap_or_else(|error| panic!("{error:#}"));
    let port = process.address().port();
    let response = process
        .client()
        .get(process.url("/api/v1/system"))
        .send()
        .await
        .expect("a system response");
    let body: Value = response.json().await.expect("JSON");
    process.kill().await.expect("torrplay stops");
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
    let (urls, port) = urls_on(Ipv4Addr::UNSPECIFIED).await;
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
    let (urls, _) = urls_on(Ipv4Addr::LOCALHOST).await;
    assert_eq!(urls, Vec::<String>::new());
}
