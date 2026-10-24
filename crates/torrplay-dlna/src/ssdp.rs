// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! SSDP: announcing the media server and answering searches, as the
//! contract's Discovery section describes.
//!
//! One task owns the socket. It answers each `M-SEARCH` after a random
//! delay, from a bounded set of pending answers, re-announces
//! periodically, and on stop says goodbye before it ends, so stopping
//! releases the socket predictably.

use std::collections::hash_map::RandomState;
use std::hash::BuildHasher;
use std::io;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::time::Duration;

use socket2::{Domain, Protocol, Socket, Type};
use tokio::net::UdpSocket;
use tokio::sync::oneshot;
use tokio::task::{JoinHandle, JoinSet};

use crate::description::DEVICE_TYPE;
use crate::services::{CONNECTION_MANAGER, CONTENT_DIRECTORY};

/// The SSDP multicast group.
pub const SSDP_GROUP: Ipv4Addr = Ipv4Addr::new(239, 255, 255, 250);

/// The SSDP port.
pub const SSDP_PORT: u16 = 1900;

/// How long announcements stay valid.
const MAX_AGE_SECONDS: u64 = 1800;

/// The most search answers waiting for their delay at once; further
/// searches wait for the next one.
const PENDING_ANSWERS: usize = 64;

/// The largest datagram read.
const DATAGRAM_BYTES: usize = 2048;

/// How an [`Announcer`] runs.
#[derive(Clone, Debug)]
pub struct Config {
    /// The device's UUID, without the `uuid:` prefix.
    pub uuid: String,
    /// The device description's URL.
    pub location: String,
    /// The `SERVER` header's product tokens.
    pub server: String,
    /// The local address the socket binds: `0.0.0.0:1900` in production.
    pub bind: SocketAddrV4,
    /// The interface to join the multicast group on and send from, or
    /// `None` not to join, for tests.
    pub interface: Option<Ipv4Addr>,
    /// Where announcements go: the multicast group in production.
    pub announce_to: SocketAddr,
    /// How often to announce again.
    pub announce_every: Duration,
    /// The longest delay before answering a search, below the request's
    /// `MX`.
    pub answer_delay_limit: Duration,
}

impl Config {
    /// The production configuration for a device at `location`, announced
    /// from `interface`.
    pub fn new(uuid: String, location: String, interface: Ipv4Addr) -> Self {
        Self {
            uuid,
            location,
            server: format!(
                "{}/1 UPnP/1.0 TorrPlay/{}",
                std::env::consts::OS,
                env!("CARGO_PKG_VERSION")
            ),
            bind: SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, SSDP_PORT),
            interface: Some(interface),
            announce_to: SocketAddr::V4(SocketAddrV4::new(SSDP_GROUP, SSDP_PORT)),
            announce_every: Duration::from_secs(300),
            answer_delay_limit: Duration::from_secs(5),
        }
    }

    /// Every notification type, with its unique service name.
    fn targets(&self) -> Vec<(String, String)> {
        let udn = format!("uuid:{}", self.uuid);
        let mut targets = vec![
            (
                "upnp:rootdevice".to_owned(),
                format!("{udn}::upnp:rootdevice"),
            ),
            (udn.clone(), udn.clone()),
        ];
        for kind in [DEVICE_TYPE, CONTENT_DIRECTORY, CONNECTION_MANAGER] {
            targets.push((kind.to_owned(), format!("{udn}::{kind}")));
        }
        targets
    }

    fn notify(&self, target: &str, usn: &str, alive: bool) -> String {
        if alive {
            format!(
                "NOTIFY * HTTP/1.1\r\nHOST: {SSDP_GROUP}:{SSDP_PORT}\r\nCACHE-CONTROL: max-age={MAX_AGE_SECONDS}\r\nLOCATION: {}\r\nNT: {target}\r\nNTS: ssdp:alive\r\nSERVER: {}\r\nUSN: {usn}\r\n\r\n",
                self.location, self.server
            )
        } else {
            format!(
                "NOTIFY * HTTP/1.1\r\nHOST: {SSDP_GROUP}:{SSDP_PORT}\r\nNT: {target}\r\nNTS: ssdp:byebye\r\nUSN: {usn}\r\n\r\n"
            )
        }
    }

    fn answer(&self, target: &str, usn: &str) -> String {
        format!(
            "HTTP/1.1 200 OK\r\nCACHE-CONTROL: max-age={MAX_AGE_SECONDS}\r\nEXT:\r\nLOCATION: {}\r\nSERVER: {}\r\nST: {target}\r\nUSN: {usn}\r\n\r\n",
            self.location, self.server
        )
    }
}

/// A search request: what it searches for and its `MX` delay.
#[derive(Debug, PartialEq, Eq)]
struct Search {
    target: String,
    mx: Duration,
}

/// Reads an `M-SEARCH` request; `None` for any other datagram.
fn parse_search(datagram: &[u8]) -> Option<Search> {
    let text = std::str::from_utf8(datagram).ok()?;
    let mut lines = text.split("\r\n");
    if !lines
        .next()?
        .trim()
        .eq_ignore_ascii_case("M-SEARCH * HTTP/1.1")
    {
        return None;
    }
    let (mut man, mut target, mut mx) = (None, None, None);
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim();
        match name.trim().to_ascii_uppercase().as_str() {
            "MAN" => man = Some(value.trim_matches('"').to_owned()),
            "ST" => target = Some(value.to_owned()),
            "MX" => mx = value.parse::<u64>().ok(),
            _ => {}
        }
    }
    if man.as_deref() != Some("ssdp:discover") {
        return None;
    }
    Some(Search {
        target: target?,
        // A unicast search may omit MX, and is answered at once.
        mx: Duration::from_secs(mx.unwrap_or(0)),
    })
}

/// A running SSDP announcer.
#[derive(Debug)]
pub struct Announcer {
    local: SocketAddr,
    stop: oneshot::Sender<()>,
    task: JoinHandle<io::Result<()>>,
}

impl Announcer {
    /// Binds the socket, announces the device twice, and runs until
    /// [`stop`](Self::stop).
    pub async fn start(config: Config) -> io::Result<Self> {
        let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
        // Other media servers on the host may hold the port too.
        socket.set_reuse_address(true)?;
        socket.set_nonblocking(true)?;
        socket.bind(&SocketAddr::V4(config.bind).into())?;
        if let Some(interface) = config.interface {
            socket.join_multicast_v4(&SSDP_GROUP, &interface)?;
            socket.set_multicast_if_v4(&interface)?;
            // UPnP's default, which keeps announcements on the local network.
            socket.set_multicast_ttl_v4(2)?;
        }
        let socket = UdpSocket::from_std(socket.into())?;
        let local = socket.local_addr()?;
        for _ in 0..2 {
            announce(&socket, &config, true).await?;
        }
        let (stop, stopped) = oneshot::channel();
        let task = tokio::spawn(run(socket, config, stopped));
        Ok(Self { local, stop, task })
    }

    /// The socket's local address.
    pub fn local_addr(&self) -> SocketAddr {
        self.local
    }

    /// Says goodbye and waits for the task, reporting its failure.
    pub async fn stop(self) -> io::Result<()> {
        let _ = self.stop.send(());
        self.task
            .await
            .map_err(|error| io::Error::other(format!("the SSDP task panicked: {error}")))?
    }
}

async fn announce(socket: &UdpSocket, config: &Config, alive: bool) -> io::Result<()> {
    for (target, usn) in config.targets() {
        socket
            .send_to(
                config.notify(&target, &usn, alive).as_bytes(),
                config.announce_to,
            )
            .await?;
    }
    Ok(())
}

async fn run(
    socket: UdpSocket,
    config: Config,
    mut stopped: oneshot::Receiver<()>,
) -> io::Result<()> {
    let socket = std::sync::Arc::new(socket);
    let mut interval = tokio::time::interval(config.announce_every);
    // The first tick is at once, and start already announced.
    interval.tick().await;
    let mut answers = JoinSet::new();
    let random = RandomState::new();
    let mut buffer = [0; DATAGRAM_BYTES];
    let result = loop {
        tokio::select! {
            _ = &mut stopped => break Ok(()),
            _ = interval.tick() => {
                if let Err(error) = announce(&socket, &config, true).await {
                    tracing::warn!(%error, "an SSDP announcement could not be sent");
                }
            }
            Some(answered) = answers.join_next(), if !answers.is_empty() => {
                if let Ok(Err(error)) = answered {
                    tracing::debug!(%error, "an SSDP answer could not be sent");
                }
            }
            received = socket.recv_from(&mut buffer), if answers.len() < PENDING_ANSWERS => {
                let (length, from) = match received {
                    Ok(received) => received,
                    Err(error) => {
                        tracing::debug!(%error, "an SSDP datagram could not be read");
                        continue;
                    }
                };
                let Some(search) = parse_search(&buffer[..length]) else {
                    continue;
                };
                let replies: Vec<String> = config
                    .targets()
                    .into_iter()
                    .filter(|(target, _)| search.target == "ssdp:all" || *target == search.target)
                    .map(|(target, usn)| config.answer(&target, &usn))
                    .collect();
                if replies.is_empty() {
                    continue;
                }
                let limit = search.mx.min(config.answer_delay_limit);
                let delay = if limit.is_zero() {
                    Duration::ZERO
                } else {
                    let millis = u64::try_from(limit.as_millis()).unwrap_or(u64::MAX);
                    Duration::from_millis(random.hash_one((from, answers.len())) % millis)
                };
                let socket = std::sync::Arc::clone(&socket);
                answers.spawn(async move {
                    tokio::time::sleep(delay).await;
                    for reply in replies {
                        socket.send_to(reply.as_bytes(), from).await?;
                    }
                    io::Result::Ok(())
                });
            }
        }
    };
    answers.shutdown().await;
    announce(&socket, &config, false).await?;
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn searches_parse_with_their_target_and_delay() {
        let search = b"M-SEARCH * HTTP/1.1\r\nHOST: 239.255.255.250:1900\r\nMAN: \"ssdp:discover\"\r\nMX: 3\r\nST: ssdp:all\r\n\r\n";
        assert_eq!(
            parse_search(search),
            Some(Search {
                target: "ssdp:all".into(),
                mx: Duration::from_secs(3)
            })
        );
        let lowercase = b"m-search * HTTP/1.1\r\nman: ssdp:discover\r\nst: upnp:rootdevice\r\n\r\n";
        assert_eq!(parse_search(lowercase).unwrap().mx, Duration::ZERO);
        assert_eq!(parse_search(b"NOTIFY * HTTP/1.1\r\n\r\n"), None);
        assert_eq!(
            parse_search(b"M-SEARCH * HTTP/1.1\r\nMAN: \"other\"\r\nST: ssdp:all\r\n\r\n"),
            None
        );
        assert_eq!(parse_search(&[0xff, 0xfe]), None);
    }

    #[test]
    fn every_target_has_its_unique_service_name() {
        let config = Config::new(
            "4b1c0b2a-6f5e-4d3c-8b2a-1f0e9d8c7b6a".into(),
            "http://192.168.1.10:8080/dlna/description.xml".into(),
            Ipv4Addr::new(192, 168, 1, 10),
        );
        let targets = config.targets();
        assert_eq!(targets.len(), 5);
        assert!(targets.contains(&(
            "upnp:rootdevice".into(),
            "uuid:4b1c0b2a-6f5e-4d3c-8b2a-1f0e9d8c7b6a::upnp:rootdevice".into()
        )));
        let alive = config.notify(&targets[0].0, &targets[0].1, true);
        assert!(
            alive.contains("NTS: ssdp:alive\r\n")
                && alive.contains("LOCATION: http://192.168.1.10:8080/dlna/description.xml\r\n")
        );
        let byebye = config.notify(&targets[0].0, &targets[0].1, false);
        assert!(byebye.contains("NTS: ssdp:byebye\r\n") && !byebye.contains("LOCATION"));
    }
}
