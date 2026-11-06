// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! The URLs other devices may reach the application at, which the system
//! resource lists so a client can offer them in a link.

use std::collections::BTreeSet;
use std::net::{IpAddr, SocketAddr};

use network_interface::{NetworkInterface, NetworkInterfaceConfig};

/// The most URLs listed, as the contract bounds them.
const URL_LIMIT: usize = 32;

/// An address of this machine, and whether its interface has a hardware
/// address: a local network's has one, and a tunnel such as a VPN has none,
/// however the routes favor it.
type Address = (IpAddr, bool);

/// Whether `mac`, as the interface listing gives it, names real hardware.
fn is_hardware_address(mac: Option<&str>) -> bool {
    mac.is_some_and(|mac| mac.chars().any(|digit| digit != '0' && digit != ':'))
}

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
    let read = tokio::task::spawn_blocking(|| {
        let addresses: Vec<Address> = NetworkInterface::show()?
            .into_iter()
            .flat_map(|interface| {
                let hardware = is_hardware_address(interface.mac_addr.as_deref());
                let addresses = interface.addr.into_iter();
                addresses.map(move |address| (address.ip(), hardware))
            })
            .collect();
        let preferred = torrplay_dlna::network::route_to_group()
            .ok()
            .map(IpAddr::V4);
        Ok::<_, network_interface::Error>((addresses, preferred))
    })
    .await;
    match read {
        Ok(Ok((addresses, preferred))) => urls_of(listen, addresses, preferred),
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

/// The URLs of `addresses` for a listener on `listen`: those a listener on it
/// accepts and another device could use, the likeliest first, at most
/// [`URL_LIMIT`]. IPv4 comes before IPv6, then interfaces with a hardware
/// address, then `preferred`, then the address ranges by [`range_rank`].
///
/// Global IPv6 addresses are listed only when nothing else is: most of them are
/// temporary, so a link built on one stops working.
fn urls_of(
    listen: SocketAddr,
    addresses: impl IntoIterator<Item = Address>,
    preferred: Option<IpAddr>,
) -> Vec<String> {
    let takes_ipv6 = listen.is_ipv6();
    let mut candidates: Vec<Address> = addresses
        .into_iter()
        .map(|(address, hardware)| (address.to_canonical(), hardware))
        .filter(|(address, _)| reachable(*address) && (takes_ipv6 || address.is_ipv4()))
        .collect();
    if candidates
        .iter()
        .any(|(address, _)| !is_global_ipv6(*address))
    {
        candidates.retain(|(address, _)| !is_global_ipv6(*address));
    }
    candidates.sort_unstable_by_key(|&(address, hardware)| {
        (
            address.is_ipv6(),
            !hardware,
            Some(address) != preferred,
            range_rank(address),
            address,
        )
    });
    let mut seen = BTreeSet::new();
    candidates
        .into_iter()
        .filter(|(address, _)| seen.insert(*address))
        .take(URL_LIMIT)
        .map(|(address, _)| url_of(address, listen.port()))
        .collect()
}

fn url_of(address: IpAddr, port: u16) -> String {
    format!("http://{}", SocketAddr::new(address, port))
}

fn is_global_ipv6(address: IpAddr) -> bool {
    matches!(address, IpAddr::V6(v6) if !v6.is_unique_local())
}

/// How likely an address's range is to be the local network, lowest first:
/// home routers use 192.168.0.0/16, and container bridges 172.16.0.0/12.
fn range_rank(address: IpAddr) -> u8 {
    match address {
        IpAddr::V4(v4) if v4.octets()[..2] == [192, 168] => 0,
        IpAddr::V4(v4) if v4.octets()[0] == 10 => 1,
        IpAddr::V4(v4) if v4.octets()[0] == 172 && v4.octets()[1] & 0xf0 == 16 => 3,
        _ => 2,
    }
}

/// Whether another device could use `address` in a URL. A link-local IPv6
/// address needs a zone, which differs per device.
fn reachable(address: IpAddr) -> bool {
    let link_local = match address {
        IpAddr::V4(v4) => v4.is_link_local(),
        IpAddr::V6(v6) => v6.is_unicast_link_local(),
    };
    !address.is_loopback() && !address.is_unspecified() && !address.is_multicast() && !link_local
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(text: &str) -> IpAddr {
        text.parse().unwrap()
    }

    #[test]
    fn only_a_real_mac_address_names_hardware() {
        for (mac, expected) in [
            (Some("aa:2d:6c:45:d6:fb"), true),
            (Some("00:00:00:00:00:00"), false),
            (Some(""), false),
            (None, false),
        ] {
            assert_eq!(is_hardware_address(mac), expected, "{mac:?}");
        }
    }

    #[test]
    fn urls_put_the_likeliest_first_and_leave_out_what_no_device_can_use() {
        let urls = urls_of(
            "[::]:8642".parse().unwrap(),
            [
                (ip("127.0.0.1"), false),
                (ip("::1"), false),
                (ip("fe80::1"), false),
                (ip("169.254.1.1"), true),
                (ip("fd12::1"), false),
                (ip("172.17.0.1"), true),
                (ip("192.168.1.10"), true),
                (ip("100.84.7.2"), false),
                (ip("172.17.0.1"), true),
                (ip("::ffff:10.0.0.5"), true),
            ],
            Some(ip("192.168.1.10")),
        );
        assert_eq!(
            urls,
            [
                "http://192.168.1.10:8642",
                "http://10.0.0.5:8642",
                "http://172.17.0.1:8642",
                "http://100.84.7.2:8642",
                "http://[fd12::1]:8642",
            ]
        );
    }

    #[test]
    fn a_tunnel_the_routes_favor_comes_after_the_local_network() {
        let urls = urls_of(
            "[::]:8642".parse().unwrap(),
            [(ip("172.19.0.1"), false), (ip("192.168.1.233"), true)],
            Some(ip("172.19.0.1")),
        );
        assert_eq!(
            urls,
            ["http://192.168.1.233:8642", "http://172.19.0.1:8642"]
        );
    }

    #[test]
    fn a_container_bridge_comes_after_the_local_network_when_a_tunnel_has_the_route() {
        let urls = urls_of(
            "[::]:8642".parse().unwrap(),
            [
                (ip("172.17.0.1"), true),
                (ip("10.0.0.5"), true),
                (ip("192.168.1.233"), true),
                (ip("172.19.0.1"), false),
            ],
            Some(ip("172.19.0.1")),
        );
        assert_eq!(
            urls,
            [
                "http://192.168.1.233:8642",
                "http://10.0.0.5:8642",
                "http://172.17.0.1:8642",
                "http://172.19.0.1:8642",
            ]
        );
    }

    #[test]
    fn the_route_outranks_the_usual_ranges() {
        let urls = urls_of(
            "[::]:8642".parse().unwrap(),
            [(ip("192.168.1.2"), true), (ip("172.20.0.9"), true)],
            Some(ip("172.20.0.9")),
        );
        assert_eq!(urls, ["http://172.20.0.9:8642", "http://192.168.1.2:8642"]);
    }

    #[test]
    fn global_ipv6_addresses_are_listed_only_when_nothing_else_is() {
        let global = [(ip("2001:db8::1"), true), (ip("2001:db8::2"), true)];
        let listen = "[::]:80".parse().unwrap();
        assert_eq!(
            urls_of(listen, global, None),
            ["http://[2001:db8::1]:80", "http://[2001:db8::2]:80"]
        );
        for other in ["10.0.0.5", "fd12::1"] {
            let urls = urls_of(listen, [global[0], global[1], (ip(other), true)], None);
            assert_eq!(urls.len(), 1, "{urls:?}");
            assert!(!urls[0].contains("2001"), "{urls:?}");
        }
    }

    #[test]
    fn an_ipv4_listener_lists_no_ipv6_address() {
        let urls = urls_of(
            "0.0.0.0:80".parse().unwrap(),
            [(ip("fd12::1"), false), (ip("10.0.0.5"), true)],
            None,
        );
        assert_eq!(urls, ["http://10.0.0.5:80"]);
    }

    #[test]
    fn an_address_on_two_interfaces_is_listed_once() {
        let urls = urls_of(
            "0.0.0.0:80".parse().unwrap(),
            [(ip("10.0.0.5"), true), (ip("10.0.0.5"), false)],
            None,
        );
        assert_eq!(urls, ["http://10.0.0.5:80"]);
    }

    #[test]
    fn the_urls_are_bounded() {
        let many = (1..=100).map(|n| (ip(&format!("10.0.0.{n}")), true));
        assert_eq!(
            urls_of("0.0.0.0:80".parse().unwrap(), many, None).len(),
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
