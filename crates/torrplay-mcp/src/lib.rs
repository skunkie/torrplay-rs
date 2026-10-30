// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! TorrPlay's MCP server, as `docs/mcp.md` specifies it: tools and resources
//! answered by a running application's native API, over stdio or a
//! loopback Streamable HTTP listener. It never opens the database or starts
//! an engine.

use std::future::Future;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use rmcp::service::RunningService;
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use rmcp::{RoleServer, ServiceExt};

pub mod client;
mod server;

pub use client::Client;
pub use server::Server;

/// The application the server reaches without `--url` or `TORRPLAY_URL`.
pub const URL_DEFAULT: &str = "http://127.0.0.1:8080";

/// The Streamable HTTP listener's address without `--listen`.
pub const LISTEN_DEFAULT: SocketAddr =
    SocketAddr::new(std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST), 8090);

/// The path the Streamable HTTP transport serves.
pub const HTTP_PATH: &str = "/mcp";

/// How long the HTTP listener waits for requests in flight at shutdown.
pub const SHUTDOWN_GRACE: Duration = Duration::from_secs(5);

/// The origins a browser may send the HTTP transport requests from: only
/// loopback ones, which defeats DNS rebinding.
const LOOPBACK_ORIGINS: [&str; 3] = ["http://localhost:*", "http://127.0.0.1:*", "http://[::1]:*"];

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("the HTTP transport listens on loopback addresses only, not {0}")]
    NotLoopback(SocketAddr),
    #[error("the HTTP transport could not listen on {address}: {source}")]
    Listen {
        address: SocketAddr,
        source: std::io::Error,
    },
    #[error("the MCP session failed: {0}")]
    Session(String),
    #[error("the HTTP transport failed: {0}")]
    Http(std::io::Error),
}

/// Serves one MCP session over standard input and output until the client
/// closes its input.
pub async fn serve_stdio(server: Server) -> Result<(), Error> {
    session(server.serve(rmcp::transport::stdio()).await).await
}

/// Serves one MCP session over an in-memory pipe, for tests.
pub async fn serve_duplex(server: Server, transport: tokio::io::DuplexStream) -> Result<(), Error> {
    session(server.serve(transport).await).await
}

async fn session(
    started: Result<RunningService<RoleServer, Server>, impl std::fmt::Display>,
) -> Result<(), Error> {
    let running = started.map_err(|error| Error::Session(error.to_string()))?;
    running
        .waiting()
        .await
        .map(drop)
        .map_err(|error| Error::Session(error.to_string()))
}

/// A bound Streamable HTTP listener.
pub struct HttpListener {
    listener: tokio::net::TcpListener,
}

impl HttpListener {
    /// Binds `address`, which must be a loopback address.
    pub async fn bind(address: SocketAddr) -> Result<Self, Error> {
        if !address.ip().is_loopback() {
            return Err(Error::NotLoopback(address));
        }
        let listener = tokio::net::TcpListener::bind(address)
            .await
            .map_err(|source| Error::Listen { address, source })?;
        Ok(Self { listener })
    }

    /// The bound address.
    pub fn local_addr(&self) -> std::io::Result<SocketAddr> {
        self.listener.local_addr()
    }

    /// Serves MCP sessions at [`HTTP_PATH`] until `shutdown` completes, then
    /// for at most [`SHUTDOWN_GRACE`] while requests finish.
    pub async fn serve(
        self,
        server: Server,
        shutdown: impl Future<Output = ()> + Send + 'static,
    ) -> Result<(), Error> {
        let config = StreamableHttpServerConfig::default()
            .with_allowed_origins(LOOPBACK_ORIGINS.map(String::from));
        let sessions = config.cancellation_token.clone();
        let service = StreamableHttpService::new(
            move || Ok(server.clone()),
            Arc::new(LocalSessionManager::default()),
            config,
        );
        let router = axum::Router::new().nest_service(HTTP_PATH, service);
        let (stopping, stopped) = tokio::sync::oneshot::channel::<()>();
        let serving = axum::serve(self.listener, router).with_graceful_shutdown(async move {
            shutdown.await;
            // Open SSE streams would hold graceful shutdown forever.
            sessions.cancel();
            let _ = stopping.send(());
        });
        tokio::select! {
            served = serving.into_future() => served.map_err(Error::Http),
            () = async {
                let _ = stopped.await;
                tokio::time::sleep(SHUTDOWN_GRACE).await;
            } => {
                tracing::warn!("requests were still running after the shutdown grace period");
                Ok(())
            }
        }
    }
}
