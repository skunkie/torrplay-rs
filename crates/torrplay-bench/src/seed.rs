// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! A local seeder: a librqbit session that serves fixtures from the
//! generated content and reaches no other peer. The benchmark runs
//! three, each on its own shaped link.

use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::Context;
use librqbit::{
    AddTorrent, AddTorrentOptions, ListenerMode, ListenerOptions, Session, SessionOptions,
};
use torrplay_fixtures::{FixtureSpec, Layout};

use crate::fixtures::FixtureDir;

/// Starts a session listening on `listen` that seeds `specs` from `dir`,
/// and returns once each has passed its initial check.
pub async fn start(
    dir: &FixtureDir,
    specs: &[FixtureSpec],
    listen: SocketAddr,
) -> anyhow::Result<Arc<Session>> {
    let content = dir.content();
    let session = Session::new_with_opts(
        content.clone(),
        SessionOptions {
            dht: None,
            disable_trackers: true,
            disable_local_service_discovery: true,
            fastresume: false,
            persistence: None,
            ipv4_only: listen.is_ipv4(),
            listen: Some(ListenerOptions {
                mode: ListenerMode::TcpOnly,
                listen_addr: listen,
                enable_upnp_port_forwarding: false,
                ipv4_only: listen.is_ipv4(),
                ..Default::default()
            }),
            ..Default::default()
        },
    )
    .await
    .context("starting the seeding session")?;
    for spec in specs {
        // librqbit expects the content directory of a multi-file torrent,
        // and the parent of a single file.
        let folder = match spec.layout {
            Layout::MultiFile { .. } => content.join(&spec.torrent_name),
            Layout::SingleFile { .. } => content.clone(),
        };
        let handle = session
            .add_torrent(
                AddTorrent::from_bytes(dir.read_torrent(&spec.id)?),
                Some(AddTorrentOptions {
                    output_folder: Some(
                        folder
                            .to_str()
                            .context("fixture paths must be UTF-8")?
                            .to_owned(),
                    ),
                    overwrite: true,
                    ..Default::default()
                }),
            )
            .await
            .with_context(|| format!("adding {}", spec.id))?
            .into_handle()
            .with_context(|| format!("{} was not added", spec.id))?;
        tracing::info!(fixture = spec.id, "checking content");
        handle
            .wait_until_completed()
            .await
            .with_context(|| format!("checking {}", spec.id))?;
    }
    tracing::info!(listen = %listen, "seeding");
    Ok(session)
}
