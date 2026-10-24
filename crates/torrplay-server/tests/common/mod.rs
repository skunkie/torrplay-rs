// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! A server on an ephemeral loopback port over the fake engine, and a
//! client whose every exchange is checked against the contract.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use http::{HeaderMap, Method, Request, Response, StatusCode, header};
use serde_json::Value;
use tempfile::TempDir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use torrplay_core::credentials::AllowedHosts;
use torrplay_core::fake::FakeEngine;
use torrplay_core::id::{TORRENT_ID_RANDOM_BYTES, TorrentId};
use torrplay_core::registry::Registry;
use torrplay_core::settings::Overrides;
use torrplay_server::{App, router};
use torrplay_store::{Store, StoredTorrent};

/// A running server and its client.
pub struct TestServer {
    pub engine: Arc<FakeEngine>,
    pub app: Arc<App<FakeEngine>>,
    /// Every torrent ID the registry drew, in order.
    issued: Arc<Mutex<Vec<TorrentId>>>,
    /// The data directory, which outlives restarts.
    data: Arc<TempDir>,
    /// Stored torrents not restored yet, for a server started without
    /// restoring them.
    unrestored: Mutex<Option<Vec<StoredTorrent>>>,
    /// How far the application's clock runs ahead, in seconds.
    clock_offset: Arc<AtomicU64>,
    allowed_hosts: AllowedHosts,
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
        Self::start_with(None).await
    }

    /// Starts a server whose request bodies must arrive within `timeout`.
    pub async fn start_with_body_timeout(timeout: Duration) -> Self {
        Self::start_with(Some(timeout)).await
    }

    async fn start_with(body_timeout: Option<Duration>) -> Self {
        let data = Arc::new(TempDir::new().expect("a data directory"));
        let engine = FakeEngine::with_catalog();
        Self::start_over(
            data,
            engine,
            body_timeout,
            true,
            AllowedHosts::default(),
            Overrides::default(),
        )
        .await
    }

    /// Starts a server that also allows requests to name `hosts`.
    pub async fn start_allowing(hosts: &[&str]) -> Self {
        let data = Arc::new(TempDir::new().expect("a data directory"));
        let hosts = AllowedHosts::new(
            hosts
                .iter()
                .map(|host| AllowedHosts::normalize(host).expect("a host name"))
                .collect(),
        );
        Self::start_over(
            data,
            FakeEngine::with_catalog(),
            None,
            true,
            hosts,
            Overrides::default(),
        )
        .await
    }

    /// Runs the application's clock `by` further ahead, for credentials'
    /// expiry.
    pub fn advance_clock(&self, by: Duration) {
        self.clock_offset.fetch_add(by.as_secs(), Ordering::SeqCst);
    }

    /// Stops the server and starts another over the same data directory and
    /// a new engine with the catalog's fixtures, as a restarted application
    /// does.
    pub async fn restart(self) -> Self {
        self.restart_with(FakeEngine::with_catalog(), true).await
    }

    /// Restarts over `engine`, restoring the stored torrents only if
    /// `restore`; [`finish_restore`](Self::finish_restore) restores them
    /// later.
    pub async fn restart_with(self, engine: FakeEngine, restore: bool) -> Self {
        let data = Arc::clone(&self.data);
        let hosts = self.allowed_hosts.clone();
        self.stop().await;
        Self::start_over(data, engine, None, restore, hosts, Overrides::default()).await
    }

    /// Restarts as a command line with `overrides` does.
    pub async fn restart_overriding(self, overrides: Overrides) -> Self {
        let data = Arc::clone(&self.data);
        let hosts = self.allowed_hosts.clone();
        self.stop().await;
        Self::start_over(
            data,
            FakeEngine::with_catalog(),
            None,
            true,
            hosts,
            overrides,
        )
        .await
    }

    /// Restores the torrents a server started without restoring holds.
    pub async fn finish_restore(&self) {
        let stored = self
            .unrestored
            .lock()
            .expect("the pending torrents")
            .take()
            .expect("torrents left to restore");
        self.app
            .restore(stored)
            .await
            .expect("the stored torrents are restored");
    }

    async fn start_over(
        data: Arc<TempDir>,
        engine: FakeEngine,
        body_timeout: Option<Duration>,
        restore: bool,
        allowed_hosts: AllowedHosts,
        overrides: Overrides,
    ) -> Self {
        let engine = Arc::new(engine);
        let (store, contents) = Store::open(data.path()).expect("the store opens");
        let stored = contents.torrents;
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
        let mut app = App::with_registry(Arc::clone(&engine), registry, store)
            .with_overrides(overrides)
            .with_settings(contents.settings)
            .with_viewing(contents.profiles, contents.progress, contents.dismissals)
            .with_credentials(contents.credentials)
            .with_allowed_hosts(allowed_hosts.clone())
            .with_media_server(torrplay_dlna::description::Device {
                uuid: contents.media_server_udn.clone(),
                version: "0.1.0".into(),
            });
        let clock_offset = Arc::new(AtomicU64::new(0));
        let offset = Arc::clone(&clock_offset);
        app = app.with_clock(Arc::new(move || {
            SystemTime::now() + Duration::from_secs(offset.load(Ordering::SeqCst))
        }));
        if let Some(timeout) = body_timeout {
            app = app.with_body_timeout(timeout);
        }
        let app = Arc::new(app);
        app.apply_dht()
            .await
            .expect("the fake engine takes the DHT switch");
        let unrestored = if restore {
            app.restore(stored)
                .await
                .expect("the stored torrents are restored");
            None
        } else {
            Some(stored)
        };
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("a loopback port");
        let addr = listener.local_addr().expect("an address");
        let base = format!("http://{addr}");
        let (shutdown, signal) = oneshot::channel();
        let task = tokio::spawn(
            axum::serve(
                listener,
                router(Arc::clone(&app)).into_make_service_with_connect_info::<SocketAddr>(),
            )
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
            data,
            unrestored: Mutex::new(unrestored),
            clock_offset,
            allowed_hosts,
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
        self.app
            .close_store()
            .await
            .expect("the store closes cleanly");
    }

    /// Writes a request with `headers` and then `body` to a new connection,
    /// without ending the body, and returns the response, checked against
    /// the contract. A server that waits for the whole body never answers,
    /// and the read times out.
    pub async fn send_before_body_ends(
        &self,
        method: Method,
        path: &str,
        headers: &[(&str, &str)],
        body: Vec<u8>,
    ) -> Exchange {
        let mut head = format!("{method} {path} HTTP/1.1\r\nHost: {}\r\n", self.addr);
        for (name, value) in headers {
            head.push_str(&format!("{name}: {value}\r\n"));
        }
        head.push_str("\r\n");
        let stream = TcpStream::connect(self.addr).await.expect("a connection");
        let (mut reader, mut writer) = stream.into_split();
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
        let read = tokio::time::timeout(Duration::from_secs(10), read_response(&mut reader)).await;
        // The writer may still be blocked on a body the server stopped
        // reading; it is stopped, and only a panic in it fails the test.
        writing.abort();
        if let Err(error) = writing.await {
            assert!(!error.is_panic(), "the writer panicked");
        }
        let response = read.expect("a response before the body ended");

        let mut request = request(method, path);
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        let request = request.body(Vec::<u8>::new()).expect("a request");
        let contract = torrplay_contract::contract().expect("the contract parses");
        if let Err(violations) = contract.check_response(&request, &response) {
            panic!(
                "{path} answered {} against the contract: {violations}",
                response.status()
            );
        }
        let (parts, body) = response.into_parts();
        Exchange {
            status: parts.status,
            headers: parts.headers,
            body,
        }
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

/// Reads one HTTP/1.1 response whose body has a `Content-Length`.
async fn read_response(reader: &mut (impl AsyncReadExt + Unpin)) -> Response<Vec<u8>> {
    let mut received = Vec::new();
    let mut buffer = [0; 4096];
    let head_end = loop {
        if let Some(end) = received.windows(4).position(|window| window == b"\r\n\r\n") {
            break end;
        }
        let count = reader.read(&mut buffer).await.expect("the response");
        assert_ne!(count, 0, "the server closed without answering");
        received.extend_from_slice(&buffer[..count]);
    };
    let head = String::from_utf8(received[..head_end].to_vec()).expect("a text head");
    let mut lines = head.split("\r\n");
    let status: u16 = lines
        .next()
        .and_then(|line| line.split(' ').nth(1))
        .and_then(|code| code.parse().ok())
        .expect("a status line");
    let mut response = Response::builder().status(status);
    let mut length = 0;
    for line in lines {
        let (name, value) = line.split_once(':').expect("a header line");
        if name.eq_ignore_ascii_case("content-length") {
            length = value.trim().parse().expect("a length");
        }
        response = response.header(name.trim(), value.trim());
    }
    let mut body = received[head_end + 4..].to_vec();
    while body.len() < length {
        let count = reader.read(&mut buffer).await.expect("the body");
        assert_ne!(count, 0, "the body ended early");
        body.extend_from_slice(&buffer[..count]);
    }
    response.body(body).expect("a response")
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

/// Collects the log lines written on this thread while it lives. Tests run
/// on a current-thread runtime, so the server's tasks log here too.
///
/// One global subscriber writes every thread's lines to that thread's
/// capture, if any. Per-thread default subscribers would miss events,
/// because `tracing` caches whether a callsite is enabled across threads.
pub struct LogCapture {
    lines: Arc<Mutex<Vec<u8>>>,
}

thread_local! {
    static CAPTURE: std::cell::RefCell<Option<Arc<Mutex<Vec<u8>>>>> =
        const { std::cell::RefCell::new(None) };
}

struct CaptureWriter;

impl std::io::Write for CaptureWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        CAPTURE.with(|capture| {
            if let Some(lines) = capture.borrow().as_ref() {
                lines
                    .lock()
                    .expect("the log buffer")
                    .extend_from_slice(bytes);
            }
        });
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl LogCapture {
    pub fn start() -> Self {
        static INSTALL: std::sync::Once = std::sync::Once::new();
        INSTALL.call_once(|| {
            let subscriber = tracing_subscriber::fmt()
                .with_max_level(tracing::Level::INFO)
                .with_writer(|| CaptureWriter)
                .finish();
            tracing::subscriber::set_global_default(subscriber)
                .expect("no other global subscriber");
        });
        let lines = Arc::new(Mutex::new(Vec::new()));
        CAPTURE.with(|capture| *capture.borrow_mut() = Some(Arc::clone(&lines)));
        Self { lines }
    }

    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.lines.lock().expect("the log buffer")).into_owned()
    }
}

impl Drop for LogCapture {
    fn drop(&mut self) {
        CAPTURE.with(|capture| *capture.borrow_mut() = None);
    }
}
