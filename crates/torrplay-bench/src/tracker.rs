// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! A minimal HTTP tracker (BEP 3, BEP 23) that answers every announce with
//! the same seeders, so both implementations reach the benchmark's local
//! peers the same way, through the torrent's announce URL.

use std::net::{SocketAddr, SocketAddrV4};

use axum::Router;
use axum::http::header;
use axum::response::IntoResponse;
use axum::routing::get;
use tokio::net::TcpListener;

/// How often clients announce again, in seconds.
const INTERVAL_SECONDS: u64 = 60;

/// The bencoded announce response listing `peers` in compact form.
pub fn announce_response(peers: &[SocketAddrV4]) -> Vec<u8> {
    let mut compact = Vec::with_capacity(peers.len() * 6);
    for peer in peers {
        compact.extend_from_slice(&peer.ip().octets());
        compact.extend_from_slice(&peer.port().to_be_bytes());
    }
    let mut body = format!(
        "d8:completei{}e10:incompletei0e8:intervali{INTERVAL_SECONDS}e5:peers{}:",
        peers.len(),
        compact.len()
    )
    .into_bytes();
    body.extend_from_slice(&compact);
    body.push(b'e');
    body
}

/// Serves `/announce` on `listener` until the task is dropped.
pub async fn serve(listener: TcpListener, peers: Vec<SocketAddrV4>) -> anyhow::Result<()> {
    let body = announce_response(&peers);
    let router = Router::new().route(
        "/announce",
        get(move || {
            let body = body.clone();
            async move { ([(header::CONTENT_TYPE, "text/plain")], body).into_response() }
        }),
    );
    axum::serve(listener, router).await?;
    Ok(())
}

/// The number of peers in a compact announce response, as
/// [`announce_response`] writes it.
pub fn compact_peer_count(body: &[u8]) -> Option<usize> {
    let marker = b"5:peers";
    let start = body
        .windows(marker.len())
        .position(|window| window == marker)?
        + marker.len();
    let rest = &body[start..];
    let colon = rest.iter().position(|&byte| byte == b':')?;
    let length: usize = std::str::from_utf8(&rest[..colon]).ok()?.parse().ok()?;
    length.is_multiple_of(6).then_some(length / 6)
}

/// Asks the tracker at `announce` how many seeders it hands out.
pub async fn seeder_count(client: &reqwest::Client, announce: &str) -> anyhow::Result<usize> {
    let body = client
        .get(announce)
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await?
        .error_for_status()?
        .bytes()
        .await?;
    compact_peer_count(&body)
        .ok_or_else(|| anyhow::anyhow!("the tracker's response lists no peers"))
}

/// Parses a seeder address, which compact responses limit to IPv4.
pub fn parse_peer(text: &str) -> anyhow::Result<SocketAddrV4> {
    match text.parse::<SocketAddr>()? {
        SocketAddr::V4(peer) => Ok(peer),
        SocketAddr::V6(_) => anyhow::bail!("seeder {text} must be an IPv4 address"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn responses_list_peers_in_compact_form() {
        let peers = [
            "10.0.0.2:6881".parse().unwrap(),
            "127.0.0.1:256".parse().unwrap(),
        ];
        assert_eq!(
            announce_response(&peers),
            b"d8:completei2e10:incompletei0e8:intervali60e5:peers12:\
              \x0a\x00\x00\x02\x1a\xe1\x7f\x00\x00\x01\x01\x00e"
        );
        assert_eq!(compact_peer_count(&announce_response(&peers)), Some(2));
        assert_eq!(compact_peer_count(b"d8:intervali60ee"), None);
        assert!(parse_peer("[::1]:6881").is_err());
    }
}
