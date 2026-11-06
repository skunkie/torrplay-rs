// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! The URLs other devices may reach the application at, which the system
//! resource lists so a client can offer them in a link.

use std::net::{IpAddr, SocketAddr};

use torrplay_dlna::network::local_addresses;

/// The most URLs listed, as the contract bounds them.
const URL_LIMIT: usize = 32;

/// The URLs of `listen`, which is `None` while the application was not told
/// its address. An interface that cannot be read lists nothing rather than
/// fail the system resource, which also describes the run.
pub(crate) async fn read_urls(listen: Option<SocketAddr>) -> Vec<String> {
    let Some(listen) = listen else {
        return Vec::new();
    };
    let address = listen.ip().to_canonical();
    if address.is_loopback() {
        return Vec::new();
    }
    if !address.is_unspecified() {
        return vec![url_of(address, listen.port())];
    }
    match tokio::task::spawn_blocking(local_addresses).await {
        Ok(Ok(addresses)) => urls_of(listen, addresses),
        Ok(Err(error)) => {
            tracing::warn!(%error, "the network interfaces could not be read");
            Vec::new()
        }
        Err(error) => {
            tracing::warn!(%error, "reading the network interfaces panicked");
            Vec::new()
        }
    }
}

/// The URLs of `addresses`, the likeliest first, that a listener on `listen`
/// accepts, at most [`URL_LIMIT`].
fn urls_of(listen: SocketAddr, addresses: Vec<IpAddr>) -> Vec<String> {
    addresses
        .into_iter()
        .filter(|address| listen.is_ipv6() || address.is_ipv4())
        .take(URL_LIMIT)
        .map(|address| url_of(address, listen.port()))
        .collect()
}

fn url_of(address: IpAddr, port: u16) -> String {
    format!("http://{}", SocketAddr::new(address, port))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(text: &str) -> IpAddr {
        text.parse().unwrap()
    }

    #[test]
    fn a_dual_stack_listener_lists_both_with_brackets_for_ipv6() {
        let urls = urls_of(
            "[::]:8642".parse().unwrap(),
            vec![ip("192.168.1.10"), ip("fd12::1")],
        );
        assert_eq!(urls, ["http://192.168.1.10:8642", "http://[fd12::1]:8642"]);
    }

    #[test]
    fn an_ipv4_listener_lists_no_ipv6_address() {
        let urls = urls_of(
            "0.0.0.0:80".parse().unwrap(),
            vec![ip("10.0.0.5"), ip("fd12::1")],
        );
        assert_eq!(urls, ["http://10.0.0.5:80"]);
    }

    #[test]
    fn the_urls_are_bounded() {
        let many = (1..=100).map(|n| ip(&format!("10.0.0.{n}"))).collect();
        assert_eq!(
            urls_of("0.0.0.0:80".parse().unwrap(), many).len(),
            URL_LIMIT
        );
    }

    #[tokio::test]
    async fn one_address_lists_itself_and_loopback_lists_nothing() {
        for (listen, expected) in [
            ("192.168.1.10:8642", vec!["http://192.168.1.10:8642"]),
            (
                "[::ffff:192.168.1.10]:8642",
                vec!["http://192.168.1.10:8642"],
            ),
            ("[fd12::1]:8642", vec!["http://[fd12::1]:8642"]),
            ("169.254.1.1:8642", vec!["http://169.254.1.1:8642"]),
            ("127.0.0.1:8642", vec![]),
            ("[::1]:8642", vec![]),
        ] {
            assert_eq!(
                read_urls(Some(listen.parse().unwrap())).await,
                expected,
                "{listen}"
            );
        }
        assert!(read_urls(None).await.is_empty());
    }

    #[tokio::test]
    async fn every_interface_lists_no_loopback_address_with_the_listeners_port() {
        for url in read_urls(Some("0.0.0.0:8642".parse().unwrap())).await {
            assert!(
                url.ends_with(":8642") && !url.contains("127.0.0.1"),
                "{url}"
            );
        }
    }
}
