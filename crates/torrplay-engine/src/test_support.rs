// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! Local seeders for tests, behind the `test-support` feature: librqbit
//! sessions that listen on an ephemeral loopback port and disable DHT,
//! trackers, local service discovery, UPnP port forwarding, and fastresume,
//! so peers reach each other only through explicit addresses and no test
//! touches a public swarm.

use std::future::Future;
use std::net::{Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use librqbit::{
    AddTorrent, AddTorrentOptions, ListenerMode, ListenerOptions, ManagedTorrent, Session,
    SessionOptions,
};
use tempfile::TempDir;
use torrplay_fixtures::{FixtureSpec, Layout};

/// The longest a seeder takes to start.
const START_TIMEOUT: Duration = Duration::from_secs(30);

async fn within<T>(
    what: &str,
    future: impl Future<Output = anyhow::Result<T>>,
) -> anyhow::Result<T> {
    tokio::time::timeout(START_TIMEOUT, future)
        .await
        .with_context(|| format!("{what} did not finish within {START_TIMEOUT:?}"))?
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
    /// The content on disk: the file of a single-file torrent, or the
    /// directory of a multi-file one.
    pub root: PathBuf,
    _dir: TempDir,
}

impl Seeder {
    /// Writes the fixture's content, adds `metainfo` over it, and waits until
    /// the initial check confirms every piece.
    pub async fn start(spec: &FixtureSpec, metainfo: &[u8]) -> anyhow::Result<Self> {
        Self::start_with_peers(spec, metainfo, &[]).await
    }

    /// Like [`start`](Self::start), and also connects to `peers`, so a
    /// leecher can gain this seeder after it started.
    pub async fn start_with_peers(
        spec: &FixtureSpec,
        metainfo: &[u8],
        peers: &[SocketAddr],
    ) -> anyhow::Result<Self> {
        let dir = tempfile::tempdir()?;
        let root = spec.write_content(dir.path())?;
        let session = new_session(dir.path()).await?;
        let handle = within(
            "adding the seeded torrent",
            session.add_torrent(
                AddTorrent::from_bytes(metainfo.to_vec()),
                Some(AddTorrentOptions {
                    output_folder: Some(output_folder_for(spec, &root)?),
                    overwrite: true,
                    initial_peers: (!peers.is_empty()).then(|| peers.to_vec()),
                    ..Default::default()
                }),
            ),
        )
        .await?
        .into_handle()
        .context("the seeded torrent was not added")?;
        within("the seeder's initial check", handle.wait_until_completed()).await?;
        let addr = session
            .listen_addr()
            .context("the seeder session has no listen address")?;
        Ok(Self {
            session,
            handle,
            addr,
            root,
            _dir: dir,
        })
    }
    /// Stops the seeding session.
    pub async fn stop(self) {
        self.session.stop().await;
    }
}
