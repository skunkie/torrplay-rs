// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! Which addresses are private, and the address the media server
//! advertises.

use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket};

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
/// address itself, or, for an unspecified one, the address of the
/// interface that routes to the SSDP multicast group. It must be private.
pub fn advertised_address(listen: SocketAddr) -> Result<Ipv4Addr, AddressError> {
    let address = match listen.ip() {
        ip if ip.is_unspecified() => route_to_group().map_err(AddressError::NoRoute)?,
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
            at("192.168.1.10:8080").unwrap(),
            Ipv4Addr::new(192, 168, 1, 10)
        );
        assert!(matches!(at("127.0.0.1:8080"), Err(AddressError::Loopback)));
        assert!(matches!(at("[::1]:8080"), Err(AddressError::Loopback)));
        assert!(matches!(
            at("8.8.8.8:8080"),
            Err(AddressError::NotPrivate(_))
        ));
        assert!(matches!(at("[fd12::1]:8080"), Err(AddressError::NoIpv4)));
    }
}
