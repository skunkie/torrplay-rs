// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! Ports for tests that start the executable.

/// Ports nothing listens on now, for the API and the peers. Both are held
/// while choosing: a port just let go can be chosen again, and torrplay stops
/// when its two listeners ask for the same one.
pub fn free_ports() -> std::io::Result<(u16, u16)> {
    let api = std::net::TcpListener::bind("127.0.0.1:0")?;
    let peers = std::net::TcpListener::bind("127.0.0.1:0")?;
    Ok((api.local_addr()?.port(), peers.local_addr()?.port()))
}
