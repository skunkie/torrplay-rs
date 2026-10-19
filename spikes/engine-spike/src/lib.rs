// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! Shared helpers for the engine spike's experiments: librqbit sessions that
//! use only loopback, a local seeder, and waits with a bound.
//!
//! Every session disables DHT, trackers, local service discovery, UPnP port
//! forwarding, and fastresume, and listens on an ephemeral loopback port, so
//! peers reach each other only through explicit addresses.

use std::future::Future;
use std::net::{Ipv4Addr, SocketAddr};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use librqbit::{
    AddTorrent, AddTorrentOptions, ListenerMode, ListenerOptions, ManagedTorrent, Session,
    SessionOptions,
};
use tempfile::TempDir;
use torrplay_fixtures::{FixtureSpec, Layout};

/// The longest any single step of an experiment may take.
pub const STEP_TIMEOUT: Duration = Duration::from_secs(30);

/// Awaits `future`, failing if it takes longer than [`STEP_TIMEOUT`].
pub async fn bounded<T>(
    what: &str,
    future: impl Future<Output = anyhow::Result<T>>,
) -> anyhow::Result<T> {
    tokio::time::timeout(STEP_TIMEOUT, future)
        .await
        .with_context(|| format!("{what} did not finish within {STEP_TIMEOUT:?}"))?
}

/// Returns options for a session that reaches peers only by address.
pub fn session_options() -> SessionOptions {
    SessionOptions {
        dht: None,
        disable_trackers: true,
        disable_local_service_discovery: true,
        fastresume: false,
        persistence: None,
        ipv4_only: true,
        listen: Some(ListenerOptions {
            mode: ListenerMode::TcpOnly,
            listen_addr: (Ipv4Addr::LOCALHOST, 0).into(),
            enable_upnp_port_forwarding: false,
            ipv4_only: true,
            ..Default::default()
        }),
        ..Default::default()
    }
}

/// Starts a session whose default output folder is `dir`.
pub async fn new_session(dir: &Path) -> anyhow::Result<Arc<Session>> {
    Session::new_with_opts(dir.to_path_buf(), session_options()).await
}

/// Returns the folder librqbit expects as `output_folder` for content that
/// [`FixtureSpec::write_content`] wrote to `root`: the content directory of a
/// multi-file torrent, or the parent of a single-file torrent's file.
pub fn output_folder_for(spec: &FixtureSpec, root: &Path) -> anyhow::Result<String> {
    let folder = match spec.layout {
        Layout::MultiFile { .. } => root,
        Layout::SingleFile { .. } => root.parent().context("content file has no parent")?,
    };
    folder
        .to_str()
        .map(str::to_owned)
        .context("temporary paths are UTF-8")
}

/// A local peer that has every piece of one fixture.
pub struct Seeder {
    /// The seeding session.
    pub session: Arc<Session>,
    /// The seeded torrent.
    pub handle: Arc<ManagedTorrent>,
    /// The address peers connect to.
    pub addr: SocketAddr,
    _dir: TempDir,
}

impl Seeder {
    /// Writes the fixture's content, adds `metainfo` over it, and waits until
    /// the initial check confirms every piece.
    pub async fn start(spec: &FixtureSpec, metainfo: &[u8]) -> anyhow::Result<Self> {
        let dir = tempfile::tempdir()?;
        let root = spec.write_content(dir.path())?;
        let session = new_session(dir.path()).await?;
        let handle = bounded(
            "adding the seeded torrent",
            session.add_torrent(
                AddTorrent::from_bytes(metainfo.to_vec()),
                Some(AddTorrentOptions {
                    output_folder: Some(output_folder_for(spec, &root)?),
                    overwrite: true,
                    ..Default::default()
                }),
            ),
        )
        .await?
        .into_handle()
        .context("the seeded torrent was not added")?;
        bounded("the seeder's initial check", handle.wait_until_completed()).await?;
        let addr = session
            .listen_addr()
            .context("the seeder session has no listen address")?;
        Ok(Self {
            session,
            handle,
            addr,
            _dir: dir,
        })
    }
}

/// A session that downloads into its own temporary folder.
pub struct Leecher {
    /// The downloading session.
    pub session: Arc<Session>,
    dir: TempDir,
}

impl Leecher {
    /// Starts a session with an empty temporary output folder.
    pub async fn start() -> anyhow::Result<Self> {
        let dir = tempfile::tempdir()?;
        let session = new_session(dir.path()).await?;
        Ok(Self { session, dir })
    }

    /// Returns options that connect only to `peers` and write below this
    /// leecher's folder.
    pub fn add_options(&self, peers: &[SocketAddr]) -> anyhow::Result<AddTorrentOptions> {
        Ok(AddTorrentOptions {
            output_folder: Some(
                self.dir
                    .path()
                    .join("download")
                    .to_str()
                    .context("temporary paths are UTF-8")?
                    .to_owned(),
            ),
            overwrite: true,
            initial_peers: Some(peers.to_vec()),
            ..Default::default()
        })
    }
}
