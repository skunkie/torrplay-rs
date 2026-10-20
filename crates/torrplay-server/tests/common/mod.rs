// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! A server on an ephemeral loopback port over the fake engine, and a
//! client whose every exchange is checked against the contract.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use http::{HeaderMap, Method, Request, Response, StatusCode, header};
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use torrplay_core::fake::FakeEngine;
use torrplay_core::id::{TORRENT_ID_RANDOM_BYTES, TorrentId};
use torrplay_core::registry::Registry;
use torrplay_server::{App, router};

/// A running server and its client.
pub struct TestServer {
    pub engine: Arc<FakeEngine>,
    pub app: Arc<App<FakeEngine>>,
    /// Every torrent ID the registry drew, in order.
    issued: Arc<Mutex<Vec<TorrentId>>>,
    addr: SocketAddr,
    base: String,
    client: reqwest::Client,
    shutdown: oneshot::Sender<()>,
    task: JoinHandle<std::io::Result<()>>,
}

/// A response with its body.
#[derive(Debug)]
pub struct Exchange {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub body: Vec<u8>,
}

impl Exchange {
    /// The body as JSON.
    pub fn json(&self) -> Value {
        serde_json::from_slice(&self.body).expect("the body is JSON")
    }

    /// A header as text.
    pub fn header(&self, name: impl header::AsHeaderName) -> Option<&str> {
        self.headers
            .get(name)
            .map(|value| value.to_str().expect("a text header"))
    }

    /// The problem type's name, after the tag URI base.
    pub fn problem(&self) -> String {
        let body = self.json();
        let problem_type = body["type"].as_str().expect("a problem type");
        problem_type
            .strip_prefix(torrplay_contract::PROBLEM_TYPE_BASE)
            .expect("a type under the base")
            .to_owned()
    }
}

/// What an exchange is checked against the contract for.
#[derive(Clone, Copy, PartialEq, Eq)]
enum RequestCheck {
    /// The request and the response.
    Checked,
    /// The response only, for requests the contract does not allow.
    Unchecked,
    /// Nothing, for paths the contract does not describe.
    None,
}

impl TestServer {
    /// Starts a server over an engine with the catalog's small fixtures.
    pub async fn start() -> Self {
        Self::start_with(true).await
    }

    /// Starts a server that draws torrent IDs from the operating system's
    /// random number generator, as a production server does.
    pub async fn start_with_random_ids() -> Self {
        Self::start_with(false).await
    }

    async fn start_with(sequential_ids: bool) -> Self {
        let engine = Arc::new(FakeEngine::with_catalog());
        // Sequential IDs, recorded so tests can address the engine's
        // torrents by the IDs the API reports.
        let issued = Arc::new(Mutex::new(Vec::new()));
        let mut next: u128 = 0;
        let record = Arc::clone(&issued);
        let registry = Registry::with_id_source(move || {
            next += 1;
            let bytes: [u8; TORRENT_ID_RANDOM_BYTES] = next.to_be_bytes();
            record
                .lock()
                .expect("the ID list")
                .push(TorrentId::from_random_bytes(bytes));
            Ok(bytes)
        });
        let app = if sequential_ids {
            Arc::new(App::with_registry(Arc::clone(&engine), registry))
        } else {
            Arc::new(App::new(Arc::clone(&engine)))
        };
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("a loopback port");
        let addr = listener.local_addr().expect("an address");
        let base = format!("http://{addr}");
        let (shutdown, signal) = oneshot::channel();
        let task = tokio::spawn(
            axum::serve(listener, router(Arc::clone(&app)))
                .with_graceful_shutdown(async {
                    let _ = signal.await;
                })
                .into_future(),
        );
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("a client");
        Self {
            engine,
            app,
            issued,
            addr,
            base,
            client,
            shutdown,
            task,
        }
    }

    /// Stops the server and waits for its task, checking its result.
    pub async fn stop(self) {
        drop(self.client);
        let _ = self.shutdown.send(());
        self.task
            .await
            .expect("the server task does not panic")
            .expect("the server stops cleanly");
    }

    /// Writes `head` and then `body` to a new connection, without ending
    /// the body, and returns the response's status line. A server that
    /// waits for the whole body never answers, and the read times out.
    pub async fn status_line_before_body_ends(&self, head: &str, body: Vec<u8>) -> String {
        let stream = TcpStream::connect(self.addr).await.expect("a connection");
        let (mut reader, mut writer) = stream.into_split();
        let head = head.to_owned();
        // The server may answer and close before every byte is written,
        // so a failed write is expected; the response decides the test.
        let writing = tokio::spawn(async move {
            writer.write_all(head.as_bytes()).await?;
            writer.write_all(&body).await?;
            writer.flush().await?;
            // Holds the connection open until the test has its response.
            std::future::pending::<()>().await;
            Ok::<_, std::io::Error>(())
        });
        let mut received = Vec::new();
        let mut buffer = [0; 1024];
        let read = tokio::time::timeout(Duration::from_secs(10), async {
            while !received.windows(2).any(|window| window == b"\r\n") {
                let count = reader.read(&mut buffer).await.expect("the response");
                assert_ne!(count, 0, "the server closed without answering");
                received.extend_from_slice(&buffer[..count]);
            }
        })
        .await;
        assert!(read.is_ok(), "no response before the body ended");
        // The writer may still be blocked on a body the server stopped
        // reading; it is stopped, and only a panic in it fails the test.
        writing.abort();
        if let Err(error) = writing.await {
            assert!(!error.is_panic(), "the writer panicked");
        }
        let line_end = received
            .windows(2)
            .position(|window| window == b"\r\n")
            .expect("a status line");
        String::from_utf8_lossy(&received[..line_end]).into_owned()
    }

    /// Sends a request and checks it and its response against the contract.
    pub async fn send(&self, request: Request<Vec<u8>>) -> Exchange {
        self.exchange(request, RequestCheck::Checked).await
    }

    /// Sends a request the contract may not allow, checking only the
    /// response.
    pub async fn send_unchecked(&self, request: Request<Vec<u8>>) -> Exchange {
        self.exchange(request, RequestCheck::Unchecked).await
    }

    /// Sends a request to a path the contract does not describe, checking
    /// nothing.
    pub async fn send_raw(&self, request: Request<Vec<u8>>) -> Exchange {
        self.exchange(request, RequestCheck::None).await
    }

    async fn exchange(&self, request: Request<Vec<u8>>, check: RequestCheck) -> Exchange {
        let (parts, body) = request.into_parts();
        let url = format!("{}{}", self.base, parts.uri);
        let mut outgoing = self
            .client
            .request(parts.method.clone(), &url)
            .headers(parts.headers.clone());
        if !body.is_empty() {
            outgoing = outgoing.body(body.clone());
        }
        let response = outgoing.send().await.expect("the server answers");
        let status = response.status();
        let headers = response.headers().clone();
        let bytes = response.bytes().await.expect("the body arrives").to_vec();

        let request = Request::from_parts(parts, body);
        let mut checked = Response::builder().status(status);
        for (name, value) in &headers {
            checked = checked.header(name, value);
        }
        let checked = checked.body(bytes.clone()).expect("a response");
        let contract = torrplay_contract::contract().expect("the contract parses");
        let result = match check {
            RequestCheck::Checked => contract.check(&request, &checked),
            RequestCheck::Unchecked => contract.check_response(&request, &checked),
            RequestCheck::None => Ok(()),
        };
        if let Err(violations) = result {
            panic!(
                "{} {} answered {status} against the contract: {violations}\n{}",
                request.method(),
                request.uri(),
                String::from_utf8_lossy(&bytes)
            );
        }
        Exchange {
            status,
            headers,
            body: bytes,
        }
    }

    pub async fn get(&self, path: &str) -> Exchange {
        self.send(
            request(Method::GET, path)
                .body(Vec::new())
                .expect("a request"),
        )
        .await
    }

    /// Creates a torrent from a magnet URI.
    pub async fn create_magnet(&self, magnet_uri: &str) -> Exchange {
        self.send(json_request(
            Method::POST,
            "/api/v1/torrents",
            &serde_json::json!({ "magnet_uri": magnet_uri }),
        ))
        .await
    }

    /// Creates a torrent from an uploaded torrent file.
    pub async fn upload(&self, torrent_file: &[u8], desired_state: Option<&str>) -> Exchange {
        self.send(form_request(torrent_file, desired_state)).await
    }

    /// Returns the ID of a torrent from its creation response.
    pub fn id_of(&self, exchange: &Exchange) -> TorrentId {
        assert_eq!(exchange.status, StatusCode::CREATED, "{exchange:?}");
        let body = exchange.json();
        let id = body["id"].as_str().expect("an ID");
        self.issued
            .lock()
            .expect("the ID list")
            .iter()
            .find(|issued| issued.as_str() == id)
            .cloned()
            .expect("the registry issued the ID")
    }
}

/// A request builder for `path`.
pub fn request(method: Method, path: &str) -> http::request::Builder {
    Request::builder().method(method).uri(path)
}

/// A request with a JSON body.
pub fn json_request(method: Method, path: &str, body: &Value) -> Request<Vec<u8>> {
    request(method, path)
        .header(header::CONTENT_TYPE, "application/json")
        .body(serde_json::to_vec(body).expect("JSON"))
        .expect("a request")
}

/// The boundary of the forms tests send.
pub const BOUNDARY: &str = "torrplay-test-boundary";

/// A creation request with a multipart form.
pub fn form_request(torrent_file: &[u8], desired_state: Option<&str>) -> Request<Vec<u8>> {
    let mut parts = vec![(
        "torrent_file",
        Some("application/x-bittorrent"),
        torrent_file.to_vec(),
    )];
    if let Some(state) = desired_state {
        parts.push(("desired_state", None, state.as_bytes().to_vec()));
    }
    form(&parts)
}

/// A creation request with the given form parts: name, part media type,
/// and data.
pub fn form(parts: &[(&str, Option<&str>, Vec<u8>)]) -> Request<Vec<u8>> {
    let mut body = Vec::new();
    for (name, media_type, data) in parts {
        body.extend_from_slice(format!("--{BOUNDARY}\r\n").as_bytes());
        let filename = if media_type.is_some() {
            "; filename=\"upload.torrent\""
        } else {
            ""
        };
        body.extend_from_slice(
            format!("Content-Disposition: form-data; name=\"{name}\"{filename}\r\n").as_bytes(),
        );
        if let Some(media_type) = media_type {
            body.extend_from_slice(format!("Content-Type: {media_type}\r\n").as_bytes());
        }
        body.extend_from_slice(b"\r\n");
        body.extend_from_slice(data);
        body.extend_from_slice(b"\r\n");
    }
    body.extend_from_slice(format!("--{BOUNDARY}--\r\n").as_bytes());
    request(Method::POST, "/api/v1/torrents")
        .header(
            header::CONTENT_TYPE,
            format!("multipart/form-data; boundary={BOUNDARY}"),
        )
        .body(body)
        .expect("a request")
}
