// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! Local seeders for engine tests, on loopback sessions without DHT,
//! trackers, local service discovery, or UPnP, and waits with a bound.

#![allow(dead_code)]

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

/// The longest any single step of a test may take.
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

/// Polls `check` every 20 ms until it returns `Some`, for at most
/// [`STEP_TIMEOUT`].
pub async fn eventually<T>(
    what: &str,
    mut check: impl FnMut() -> anyhow::Result<Option<T>>,
) -> anyhow::Result<T> {
    bounded(what, async {
        loop {
            if let Some(value) = check()? {
                return Ok(value);
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
}

/// Options for a session that reaches peers only by address.
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

/// Returns the folder librqbit expects as `output_folder` for content that
/// [`FixtureSpec::write_content`] wrote to `root`.
fn output_folder_for(spec: &FixtureSpec, root: &Path) -> anyhow::Result<String> {
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
    /// The content on disk.
    pub root: PathBuf,
    _dir: TempDir,
}

impl Seeder {
    /// Writes the fixture's content, adds `metainfo` over it, and waits until
    /// the initial check confirms every piece.
    pub async fn start(spec: &FixtureSpec, metainfo: &[u8]) -> anyhow::Result<Self> {
        let dir = tempfile::tempdir()?;
        let root = spec.write_content(dir.path())?;
        let session = Session::new_with_opts(dir.path().to_path_buf(), session_options()).await?;
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
            root,
            _dir: dir,
        })
    }

    /// Stops the seeding session.
    pub async fn stop(self) {
        self.session.stop().await;
    }
}
