// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! DLNA-02, DLNA-09, and DLNA-19: the SSDP announcer over loopback, with
//! announcements sent to a test socket instead of the multicast group.

// The cases assert as tests do; clippy's test exemption covers only test
// functions.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::time::Duration;

use tokio::net::UdpSocket;
use torrplay_dlna::ssdp::{Announcer, Config};

const UUID: &str = "4b1c0b2a-6f5e-4d3c-8b2a-1f0e9d8c7b6a";

fn config(announce_to: SocketAddr) -> Config {
    Config {
        bind: SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0),
        interface: None,
        announce_to,
        announce_every: Duration::from_secs(3600),
        answer_delay_limit: Duration::from_millis(50),
        ..Config::new(
            UUID.into(),
            "http://192.168.1.10:8642/dlna/description.xml".into(),
            Ipv4Addr::LOCALHOST,
        )
    }
}

/// Receives datagrams until none arrives for `quiet`.
async fn receive(socket: &UdpSocket, quiet: Duration) -> Vec<String> {
    let mut received = Vec::new();
    let mut buffer = [0; 2048];
    while let Ok(Ok(length)) = tokio::time::timeout(quiet, socket.recv(&mut buffer)).await {
        received.push(String::from_utf8_lossy(&buffer[..length]).into_owned());
    }
    received
}

fn search(target: &str) -> String {
    format!(
        "M-SEARCH * HTTP/1.1\r\nHOST: 239.255.255.250:1900\r\nMAN: \"ssdp:discover\"\r\nMX: 1\r\nST: {target}\r\n\r\n"
    )
}

#[tokio::test]
async fn the_announcer_announces_answers_and_says_goodbye() {
    let listener = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let announcer = Announcer::start(config(listener.local_addr().unwrap()))
        .await
        .unwrap();
    let alive = receive(&listener, Duration::from_millis(300)).await;
    assert_eq!(alive.len(), 10, "each target twice: {alive:?}");
    assert!(
        alive
            .iter()
            .all(|notify| notify.contains("NTS: ssdp:alive"))
    );
    assert!(
        alive
            .iter()
            .any(|notify| notify.contains(&format!("USN: uuid:{UUID}::upnp:rootdevice")))
    );

    let searcher = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let to = announcer.local_addr();
    searcher
        .send_to(search("ssdp:all").as_bytes(), to)
        .await
        .unwrap();
    let all = receive(&searcher, Duration::from_millis(300)).await;
    assert_eq!(all.len(), 5, "{all:?}");
    assert!(
        all.iter()
            .all(|answer| answer.starts_with("HTTP/1.1 200 OK\r\n")
                && answer.contains("LOCATION: http://192.168.1.10:8642/dlna/description.xml\r\n"))
    );

    searcher
        .send_to(
            search("urn:schemas-upnp-org:device:MediaServer:1").as_bytes(),
            to,
        )
        .await
        .unwrap();
    let device = receive(&searcher, Duration::from_millis(300)).await;
    assert_eq!(device.len(), 1);
    assert!(device[0].contains("ST: urn:schemas-upnp-org:device:MediaServer:1\r\n"));

    searcher
        .send_to(
            search("urn:schemas-upnp-org:device:Printer:1").as_bytes(),
            to,
        )
        .await
        .unwrap();
    assert!(
        receive(&searcher, Duration::from_millis(300))
            .await
            .is_empty()
    );

    announcer.stop().await.unwrap();
    let goodbye = receive(&listener, Duration::from_millis(300)).await;
    assert_eq!(goodbye.len(), 5, "{goodbye:?}");
    assert!(
        goodbye
            .iter()
            .all(|notify| notify.contains("NTS: ssdp:byebye"))
    );
}

#[tokio::test]
async fn repeated_starts_and_stops_release_the_socket() {
    let listener = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let mut config = config(listener.local_addr().unwrap());
    // The same port each time; each stop must end its task, which says
    // goodbye on the way out.
    let first = Announcer::start(config.clone()).await.unwrap();
    let port = first.local_addr().port();
    first.stop().await.unwrap();
    config.bind.set_port(port);
    for _ in 0..3 {
        let announcer = Announcer::start(config.clone()).await.unwrap();
        assert_eq!(announcer.local_addr().port(), port);
        announcer.stop().await.unwrap();
    }
    let received = receive(&listener, Duration::from_millis(300)).await;
    let alive = received
        .iter()
        .filter(|notify| notify.contains("ssdp:alive"))
        .count();
    let byebye = received
        .iter()
        .filter(|notify| notify.contains("ssdp:byebye"))
        .count();
    assert_eq!(
        (alive, byebye),
        (4 * 10, 4 * 5),
        "every start announced and every stop said goodbye"
    );
}

/// Binds `port` the way UPnP libraries such as libupnp do: with
/// SO_REUSEADDR, and SO_REUSEPORT where the system has it.
fn bind_like_a_player(port: u16) -> std::io::Result<socket2::Socket> {
    use socket2::{Domain, Protocol, Socket, Type};
    let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    socket.set_reuse_address(true)?;
    #[cfg(all(unix, not(any(target_os = "solaris", target_os = "illumos"))))]
    socket.set_reuse_port(true)?;
    socket.bind(&SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, port)).into())?;
    Ok(socket)
}

#[tokio::test]
async fn other_upnp_programs_share_the_port() {
    let listener = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let mut config = config(listener.local_addr().unwrap());

    // A player started after the announcer, as a media player on the same
    // host is.
    let announcer = Announcer::start(config.clone()).await.unwrap();
    let port = announcer.local_addr().port();
    let player = bind_like_a_player(port);
    assert!(player.is_ok(), "a later player binds: {player:?}");
    drop(player);
    announcer.stop().await.unwrap();

    // And one already running when the announcer starts.
    let player = bind_like_a_player(port).unwrap();
    config.bind.set_port(port);
    let announcer = Announcer::start(config).await;
    assert!(announcer.is_ok(), "the announcer binds after a player");
    announcer.unwrap().stop().await.unwrap();
    drop(player);
}
