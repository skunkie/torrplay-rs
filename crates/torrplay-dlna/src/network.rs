// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! Which addresses are private, this machine's addresses, and the address
//! the media server advertises.

use std::collections::BTreeSet;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket};

use network_interface::{NetworkInterface, NetworkInterfaceConfig};

use crate::ssdp::{SSDP_GROUP, SSDP_PORT};

/// Whether `address` is on a private network: RFC 1918 or link-local IPv4,
/// unique-local or link-local IPv6, or loopback. An IPv4-mapped IPv6
/// address counts as its IPv4 address.
pub fn is_private(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(v4) => v4.is_private() || v4.is_link_local() || v4.is_loopback(),
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => is_private(IpAddr::V4(v4)),
            None => {
                let first = v6.segments()[0];
                v6.is_loopback() || first & 0xfe00 == 0xfc00 || first & 0xffc0 == 0xfe80
            }
        },
    }
}

/// Why the media server cannot be advertised.
#[derive(Debug, thiserror::Error)]
pub enum AddressError {
    /// The API listens on loopback, which televisions cannot reach.
    #[error("the API listens on loopback; pass --listen with a local network address")]
    Loopback,
    /// The address is not on a private network.
    #[error("{0} is not a private address, and the media server serves only local networks")]
    NotPrivate(IpAddr),
    /// SSDP needs IPv4, and the API listens on an IPv6 address only.
    #[error("the API listens on an IPv6 address; SSDP needs an IPv4 one")]
    NoIpv4,
    /// No interface routes to the SSDP multicast group.
    #[error("no network interface routes to the SSDP multicast group: {0}")]
    NoRoute(#[source] std::io::Error),
}

/// The IPv4 address to advertise for an API listening on `listen`: the
/// address itself, or, for an unspecified one, the likeliest address of this
/// machine, so a VPN that takes multicast's route is not advertised to
/// televisions. It must be private.
pub fn advertised_address(listen: SocketAddr) -> Result<Ipv4Addr, AddressError> {
    let address = match listen.ip() {
        ip if ip.is_unspecified() => {
            let addresses = local_addresses().unwrap_or_else(|error| {
                tracing::debug!(%error, "the network interfaces could not be read");
                Vec::new()
            });
            likeliest_ipv4(&addresses, route_to_group)?
        }
        ip if ip.is_loopback() => return Err(AddressError::Loopback),
        IpAddr::V4(v4) => v4,
        IpAddr::V6(v6) => v6.to_ipv4_mapped().ok_or(AddressError::NoIpv4)?,
    };
    if address.is_loopback() {
        return Err(AddressError::Loopback);
    }
    if !is_private(IpAddr::V4(address)) {
        return Err(AddressError::NotPrivate(IpAddr::V4(address)));
    }
    Ok(address)
}

/// The first IPv4 address of `addresses`, the likeliest first, or otherwise the
/// address of the `route` to the SSDP group. It may be a public one: a machine
/// whose network is public is not announced on a private bridge it also has.
fn likeliest_ipv4(
    addresses: &[IpAddr],
    route: impl FnOnce() -> std::io::Result<Ipv4Addr>,
) -> Result<Ipv4Addr, AddressError> {
    let first = addresses.iter().find_map(|address| match address {
        IpAddr::V4(v4) => Some(*v4),
        IpAddr::V6(_) => None,
    });
    match first {
        Some(address) => Ok(address),
        None => route().map_err(AddressError::NoRoute),
    }
}

/// This machine's addresses that another device could use, the likeliest first.
///
/// IPv4 comes before IPv6, then interfaces with a hardware address, then the
/// address that routes to the SSDP group, then the ranges a local network
/// usually uses, with those of container bridges last. Global IPv6 addresses
/// are listed only when nothing else is: most of them are temporary, so a link
/// built on one stops working.
pub fn local_addresses() -> std::io::Result<Vec<IpAddr>> {
    let addresses: Vec<Address> = NetworkInterface::show()
        .map_err(std::io::Error::other)?
        .into_iter()
        .flat_map(|interface| {
            let hardware = is_hardware_address(interface.mac_addr.as_deref());
            let addresses = interface.addr.into_iter();
            addresses.map(move |address| (address.ip(), hardware))
        })
        .collect();
    Ok(ranked(addresses, route_to_group().ok().map(IpAddr::V4)))
}

/// An address of this machine, and whether its interface has a hardware
/// address: a local network's has one, and a tunnel such as a VPN has none,
/// however the routes favor it.
type Address = (IpAddr, bool);

/// Whether `mac`, as the interface listing gives it, names real hardware.
fn is_hardware_address(mac: Option<&str>) -> bool {
    mac.is_some_and(|mac| mac.chars().any(|digit| digit != '0' && digit != ':'))
}

/// `addresses` as [`local_addresses`] lists them, with `preferred` the one
/// that routes to the SSDP group.
fn ranked(addresses: impl IntoIterator<Item = Address>, preferred: Option<IpAddr>) -> Vec<IpAddr> {
    let mut candidates: Vec<Address> = addresses
        .into_iter()
        .map(|(address, hardware)| (address.to_canonical(), hardware))
        .filter(|(address, _)| reachable(*address))
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
        .map(|(address, _)| address)
        .filter(|address| seen.insert(*address))
        .collect()
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

/// The local address a datagram to the SSDP group would leave from.
/// Connecting a UDP socket sends nothing.
fn route_to_group() -> std::io::Result<Ipv4Addr> {
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0))?;
    socket.connect((SSDP_GROUP, SSDP_PORT))?;
    match socket.local_addr()?.ip() {
        IpAddr::V4(v4) if !v4.is_unspecified() => Ok(v4),
        _ => Err(std::io::Error::other(
            "the route has no IPv4 source address",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(text: &str) -> IpAddr {
        text.parse().unwrap()
    }

    fn no_route() -> std::io::Result<Ipv4Addr> {
        Err(std::io::Error::other("no route"))
    }

    #[test]
    fn the_local_network_is_advertised_when_a_tunnel_has_the_route() {
        let local = ranked(
            [(ip("172.19.0.1"), false), (ip("192.168.1.233"), true)],
            Some(ip("172.19.0.1")),
        );
        let advertised = likeliest_ipv4(&local, no_route).unwrap();
        assert_eq!(advertised, Ipv4Addr::new(192, 168, 1, 233));
    }

    #[test]
    fn a_public_network_comes_before_a_private_bridge_it_also_has() {
        let local = ranked(
            [(ip("172.17.0.1"), true), (ip("203.0.113.7"), true)],
            Some(ip("203.0.113.7")),
        );
        let advertised = likeliest_ipv4(&local, no_route).unwrap();
        assert_eq!(advertised, Ipv4Addr::new(203, 0, 113, 7));
    }

    #[test]
    fn a_public_network_stays_first_when_a_tunnel_has_the_route() {
        let local = ranked(
            [
                (ip("172.17.0.1"), true),
                (ip("203.0.113.7"), true),
                (ip("172.19.0.1"), false),
            ],
            Some(ip("172.19.0.1")),
        );
        let advertised = likeliest_ipv4(&local, no_route).unwrap();
        assert_eq!(advertised, Ipv4Addr::new(203, 0, 113, 7));
    }

    #[test]
    fn the_advertised_address_falls_back_on_the_route_without_an_ipv4_address() {
        let route = Ipv4Addr::new(169, 254, 1, 1);
        for addresses in [vec![], vec![ip("fd12::1")]] {
            let advertised = likeliest_ipv4(&addresses, || Ok(route)).unwrap();
            assert_eq!(advertised, route, "{addresses:?}");
        }
        assert!(matches!(
            likeliest_ipv4(&[], no_route),
            Err(AddressError::NoRoute(_))
        ));
    }

    #[test]
    fn the_route_is_not_asked_for_when_an_address_is_known() {
        let advertised = likeliest_ipv4(&[ip("172.19.0.1")], || panic!("asked"));
        assert_eq!(advertised.unwrap(), Ipv4Addr::new(172, 19, 0, 1));
    }

    /// The unspecified address takes the first of this machine's addresses,
    /// whatever they are here, and not the route's answer.
    #[test]
    fn every_interface_advertises_the_likeliest_of_this_machines_addresses() {
        let first = local_addresses().unwrap().into_iter().find(IpAddr::is_ipv4);
        match (first, advertised_address("0.0.0.0:8642".parse().unwrap())) {
            (Some(IpAddr::V4(first)), Ok(advertised)) => assert_eq!(advertised, first),
            (Some(first), Err(AddressError::NotPrivate(address))) => {
                assert_eq!(address, first);
                assert!(!is_private(first));
            }
            // Without an IPv4 address, the route answers, if there is one.
            (None, _) => {}
            (first, other) => panic!("{first:?} gave {other:?}"),
        }
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
    fn addresses_put_the_likeliest_first_and_leave_out_what_no_device_can_use() {
        let addresses = ranked(
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
            addresses,
            [
                ip("192.168.1.10"),
                ip("10.0.0.5"),
                ip("172.17.0.1"),
                ip("100.84.7.2"),
                ip("fd12::1"),
            ]
        );
    }

    #[test]
    fn a_container_bridge_comes_after_the_local_network_when_a_tunnel_has_the_route() {
        let addresses = ranked(
            [
                (ip("172.17.0.1"), true),
                (ip("10.0.0.5"), true),
                (ip("192.168.1.233"), true),
                (ip("172.19.0.1"), false),
            ],
            Some(ip("172.19.0.1")),
        );
        assert_eq!(
            addresses,
            [
                ip("192.168.1.233"),
                ip("10.0.0.5"),
                ip("172.17.0.1"),
                ip("172.19.0.1"),
            ]
        );
    }

    #[test]
    fn the_route_outranks_the_usual_ranges() {
        let addresses = ranked(
            [(ip("192.168.1.2"), true), (ip("172.20.0.9"), true)],
            Some(ip("172.20.0.9")),
        );
        assert_eq!(addresses, [ip("172.20.0.9"), ip("192.168.1.2")]);
    }

    #[test]
    fn global_ipv6_addresses_are_listed_only_when_nothing_else_is() {
        let global = [(ip("2001:db8::1"), true), (ip("2001:db8::2"), true)];
        assert_eq!(ranked(global, None), [ip("2001:db8::1"), ip("2001:db8::2")]);
        for other in ["10.0.0.5", "fd12::1"] {
            let addresses = ranked([global[0], global[1], (ip(other), true)], None);
            assert_eq!(addresses, [ip(other)]);
        }
    }

    #[test]
    fn an_address_on_two_interfaces_is_listed_once() {
        let addresses = ranked([(ip("10.0.0.5"), true), (ip("10.0.0.5"), false)], None);
        assert_eq!(addresses, [ip("10.0.0.5")]);
    }

    #[test]
    fn private_addresses_are_local_networks_and_loopback() {
        for private in [
            "10.1.2.3",
            "172.16.0.1",
            "172.31.255.255",
            "192.168.1.10",
            "169.254.1.1",
            "127.0.0.1",
            "::1",
            "fd12:3456::1",
            "fe80::1",
            "::ffff:192.168.1.10",
        ] {
            assert!(is_private(private.parse().unwrap()), "{private}");
        }
        for public in [
            "8.8.8.8",
            "172.32.0.1",
            "100.64.0.1",
            "2001:db8::1",
            "::ffff:8.8.8.8",
        ] {
            assert!(!is_private(public.parse().unwrap()), "{public}");
        }
    }

    #[test]
    fn the_advertised_address_is_a_private_ipv4_one() {
        let at = |text: &str| advertised_address(text.parse().unwrap());
        assert_eq!(
            at("192.168.1.10:8642").unwrap(),
            Ipv4Addr::new(192, 168, 1, 10)
        );
        assert!(matches!(at("127.0.0.1:8642"), Err(AddressError::Loopback)));
        assert!(matches!(at("[::1]:8642"), Err(AddressError::Loopback)));
        assert!(matches!(
            at("8.8.8.8:8642"),
            Err(AddressError::NotPrivate(_))
        ));
        assert!(matches!(at("[fd12::1]:8642"), Err(AddressError::NoIpv4)));
    }
}
