// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! Whether what listens at the service's address is the service, rather than another
//! user's program. The service's port needs no privilege, so while the service is
//! stopped anyone on the computer could listen there, and a page served at that origin
//! could read the sign-in the client's page stores. On Linux a listener of the person
//! running the client counts too, since the kernel names a socket's user; Windows names
//! its process, so there only the running service's counts.

use std::net::SocketAddr;

use anyhow::bail;

/// The service's account on Linux, and its name with the Windows service manager. On
/// Linux the check looks the account up in `/etc/passwd`, so the service runs as a
/// `torrplay` account that `sysusers.d` creates, never as a systemd dynamic user, whose
/// ID is not there.
#[cfg(any(target_os = "linux", all(windows, feature = "client")))]
const SERVICE: &str = "torrplay";

/// Whether everything that listens at `address` is the service's or this user's,
/// with an error when nothing listens there or the system cannot say.
pub fn trusted(address: SocketAddr) -> anyhow::Result<bool> {
    #[cfg(target_os = "linux")]
    return linux::trusted(address);
    #[cfg(all(windows, feature = "client"))]
    return windows::trusted(address);
    #[cfg(not(any(target_os = "linux", all(windows, feature = "client"))))]
    {
        let _ = address;
        bail!("this system cannot say who listens at the service's address");
    }
}

/// Whether a listener on `local` takes connections to `address`: on its port, at its
/// address or every address.
fn covers(local: SocketAddr, address: SocketAddr) -> bool {
    local.port() == address.port() && (local.ip() == address.ip() || local.ip().is_unspecified())
}

/// What the owners of the listeners at an address make of it.
fn judge(owners: &[u32], trusted: &[u32]) -> anyhow::Result<bool> {
    if owners.is_empty() {
        bail!("nothing listens at the service's address");
    }
    Ok(owners.iter().all(|owner| trusted.contains(owner)))
}

#[cfg_attr(
    not(any(target_os = "linux", all(windows, feature = "client"))),
    allow(dead_code)
)]
mod tables {
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

    /// The kernel's state number of a listening TCP socket.
    const LISTEN: &str = "0A";

    /// The users that own the listening sockets at `address` in `table`, the contents
    /// of `/proc/net/tcp` or `/proc/net/tcp6`.
    pub fn listening_owners(table: &str, address: SocketAddr) -> Vec<u32> {
        table
            .lines()
            .skip(1)
            .filter_map(|line| {
                let fields: Vec<&str> = line.split_whitespace().collect();
                let local = socket_address(fields.get(1)?)?;
                let state = *fields.get(3)?;
                let owner = fields.get(7)?.parse().ok()?;
                (state == LISTEN && super::covers(local, address)).then_some(owner)
            })
            .collect()
    }

    /// An address as the kernel's tables write it: the address's 32-bit words in
    /// hexadecimal, each as the host reads it, a colon, and the port.
    fn socket_address(text: &str) -> Option<SocketAddr> {
        let (address, port) = text.split_once(':')?;
        let port = u16::from_str_radix(port, 16).ok()?;
        let mut bytes = Vec::with_capacity(16);
        for index in 0..address.len() / 8 {
            let word = u32::from_str_radix(address.get(index * 8..index * 8 + 8)?, 16).ok()?;
            bytes.extend(word.to_ne_bytes());
        }
        let ip = match bytes.len() {
            4 => IpAddr::V4(Ipv4Addr::from(<[u8; 4]>::try_from(bytes).ok()?)),
            16 => IpAddr::V6(Ipv6Addr::from(<[u8; 16]>::try_from(bytes).ok()?)),
            _ => return None,
        };
        Some(SocketAddr::new(ip, port))
    }

    /// The processes that listen at `address` in `output`, what `netstat -ano` prints on
    /// Windows. A listener's remote address has port 0, which is the same in every
    /// language, unlike the state's name.
    #[cfg_attr(not(all(windows, feature = "client")), allow(dead_code))]
    pub fn netstat_listeners(output: &str, address: SocketAddr) -> Vec<u32> {
        output
            .lines()
            .filter_map(|line| {
                let fields: Vec<&str> = line.split_whitespace().collect();
                if fields.first() != Some(&"TCP") {
                    return None;
                }
                let local: SocketAddr = fields.get(1)?.parse().ok()?;
                let remote: SocketAddr = fields.get(2)?.parse().ok()?;
                let process = fields.last()?.parse().ok()?;
                (remote.port() == 0 && super::covers(local, address)).then_some(process)
            })
            .collect()
    }

    /// The user ID of `name` in `passwd`, the contents of `/etc/passwd`.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub fn user_id(passwd: &str, name: &str) -> Option<u32> {
        passwd.lines().find_map(|line| {
            let mut fields = line.split(':');
            (fields.next()? == name).then(|| fields.nth(1)?.parse().ok())?
        })
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use std::net::SocketAddr;
    use std::os::unix::fs::MetadataExt;

    use anyhow::Context;

    use super::tables::{listening_owners, user_id};

    pub fn trusted(address: SocketAddr) -> anyhow::Result<bool> {
        let mut owners = Vec::new();
        for table in ["/proc/net/tcp", "/proc/net/tcp6"] {
            // A system without IPv6 has no table for it.
            if let Ok(contents) = std::fs::read_to_string(table) {
                owners.extend(listening_owners(&contents, address));
            }
        }
        let mut trusted = vec![
            std::fs::metadata("/proc/self")
                .context("reading who runs the client")?
                .uid(),
        ];
        let passwd = std::fs::read_to_string("/etc/passwd").context("reading /etc/passwd")?;
        trusted.extend(user_id(&passwd, super::SERVICE));
        super::judge(&owners, &trusted)
    }
}

#[cfg(all(windows, feature = "client"))]
mod windows {
    use std::net::SocketAddr;
    use std::os::windows::process::CommandExt;
    use std::path::PathBuf;
    use std::process::Command;

    use anyhow::{Context, bail};
    use windows_service::service::ServiceAccess;
    use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};

    use super::tables::netstat_listeners;

    /// What keeps a console program started from a window from opening a console.
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    /// Whether the processes that listen at `address` are the running service's. Unlike
    /// on Linux, a listener of this user's own is not trusted, since Windows names a
    /// listener's process rather than its user.
    pub fn trusted(address: SocketAddr) -> anyhow::Result<bool> {
        // From the system's own directory, so nothing earlier on the path stands in.
        let system = std::env::var_os("SystemRoot").context("no SystemRoot")?;
        let netstat = PathBuf::from(system).join("System32").join("netstat.exe");
        let output = Command::new(netstat)
            .arg("-ano")
            .creation_flags(CREATE_NO_WINDOW)
            .output()
            .context("listing the listening sockets")?;
        if !output.status.success() {
            bail!("netstat failed: {}", output.status);
        }
        let owners = netstat_listeners(&String::from_utf8_lossy(&output.stdout), address);
        let service = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)
            .and_then(|manager| manager.open_service(super::SERVICE, ServiceAccess::QUERY_STATUS))
            .and_then(|service| service.query_status())
            .context("asking the service manager for the TorrPlay service")?;
        let trusted: Vec<u32> = service.process_id.into_iter().collect();
        super::judge(&owners, &trusted)
    }
}

#[cfg(test)]
mod tests {
    use super::tables::{listening_owners, netstat_listeners, user_id};
    use super::*;

    fn address(text: &str) -> SocketAddr {
        text.parse()
            .unwrap_or_else(|error| panic!("{text}: {error}"))
    }

    // Port 8642 is 21C2; 127.0.0.1 is 0100007F in the host's byte order.
    const TCP: &str = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode
   0: 0100007F:21C2 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1001        0 1 1 0000000000000000 100 0 0 10 0
   1: 00000000:1F90 00000000:0000 0A 00000000:00000000 00:00000000 00000000   998        0 2 1 0000000000000000 100 0 0 10 0
   2: 0100007F:21C2 0100007F:D431 01 00000000:00000000 00:00000000 00000000  1000        0 3 1 0000000000000000 20 4 30 10 -1
";
    const TCP6: &str = "  sl  local_address                         remote_address                        st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode
   0: 00000000000000000000000000000000:21C2 00000000000000000000000000000000:0000 0A 00000000:00000000 00:00000000 00000000   998        0 4 1 0000000000000000 100 0 0 10 0
   1: 00000000000000000000000001000000:21C3 00000000000000000000000000000000:0000 0A 00000000:00000000 00:00000000 00000000  1001        0 5 1 0000000000000000 100 0 0 10 0
";

    #[test]
    fn the_owners_are_of_listeners_at_the_address_or_every_address() {
        let service = address("127.0.0.1:8642");
        assert_eq!(listening_owners(TCP, service), [1001], "not the connection");
        assert_eq!(listening_owners(TCP6, service), [998], "every address");
        assert_eq!(listening_owners(TCP, address("127.0.0.1:8080")), [998]);
        assert_eq!(
            listening_owners(TCP6, address("[::1]:8643")),
            [1001],
            "IPv6 loopback"
        );
        assert!(listening_owners(TCP, address("127.0.0.1:9000")).is_empty());
    }

    #[test]
    fn netstat_names_the_processes_that_listen_whatever_its_language() {
        let output = "
Active Connections

  Proto  Local Address          Foreign Address        State           PID
  TCP    0.0.0.0:135            0.0.0.0:0              LISTENING       1004
  TCP    127.0.0.1:8642         0.0.0.0:0              ABH\u{d6}REN         4321
  TCP    127.0.0.1:8642         127.0.0.1:50000        ESTABLISHED     4321
  TCP    [::]:8642              [::]:0                 LISTENING       4321
  TCP    [fe80::1%5]:8642       [::]:0                 LISTENING       7
  UDP    0.0.0.0:8642           *:*                                    99
";
        assert_eq!(
            netstat_listeners(output, address("127.0.0.1:8642")),
            [4321, 4321]
        );
        assert_eq!(netstat_listeners(output, address("127.0.0.1:135")), [1004]);
        assert!(netstat_listeners(output, address("127.0.0.1:9000")).is_empty());
    }

    #[test]
    fn the_services_user_is_found_by_name() {
        let passwd = "root:x:0:0:root:/root:/bin/sh\ntorrplay:x:998:998::/var/lib/torrplay:/usr/sbin/nologin\n";
        assert_eq!(user_id(passwd, "torrplay"), Some(998));
        assert_eq!(user_id(passwd, "torr"), None);
    }

    #[test]
    fn only_the_service_or_this_user_is_trusted() -> anyhow::Result<()> {
        assert!(judge(&[998], &[1000, 998])?);
        assert!(judge(&[998, 1000], &[1000, 998])?);
        assert!(
            !judge(&[998, 1001], &[1000, 998])?,
            "another user's listener too"
        );
        assert!(judge(&[], &[1000]).is_err(), "nothing listens");
        Ok(())
    }
}
