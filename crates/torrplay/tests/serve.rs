// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! The application through its API: readiness after startup, HTTP/2, and
//! the shutdown sequence of LIF-10, on loopback sessions that reach no
//! public swarm.

mod common;
mod ports;

use std::time::{Duration, Instant};

use common::{MIB, Running, alive_tasks, bounded, eventually, magnet, upload, upload_form};
use ports::free_ports;
use reqwest::{Client, StatusCode, Version};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpSocket, TcpStream};
use torrplay::ShutdownTimings;
use torrplay_engine::test_support::Seeder;
use torrplay_engine::{EngineConfig, LibrqbitEngine, NetworkConfig};
use torrplay_fixtures::catalog;

/// Streams wait for pieces far longer than any test.
fn patient(config: &mut EngineConfig) {
    config.piece_wait = Duration::from_secs(600);
}

/// LIF-10: readiness ends at once and new streams and state changes are
/// refused while reads are still served, a stream in progress gets the
/// drain period and is then canceled, and the listener closes.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn shutdown_drains_streams_then_closes_the_listener() -> anyhow::Result<()> {
    let drain = Duration::from_secs(1);
    let timings = ShutdownTimings {
        drain: Some(drain),
        connection_wait: Duration::from_secs(5),
        timeout: Duration::from_secs(20),
    };
    let mut running = Running::start(Vec::new(), timings, patient).await?;
    let client = Client::new();
    let created = upload(&running, &client, &catalog::single_small()).await?;
    let content = &created.content[0];

    // Without peers, the stream sends its headers and then waits.
    let stream = client.get(running.url(content)).send().await?;
    assert_eq!(stream.status(), StatusCode::OK);
    let streaming = tokio::spawn(stream.bytes());

    let begun = Instant::now();
    running.trigger();
    bounded("readiness ending", async {
        loop {
            let response = client.get(running.url("/readyz")).send().await;
            if response.is_ok_and(|response| response.status() == StatusCode::SERVICE_UNAVAILABLE) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await?;
    let (content_type, body) = upload_form(&catalog::multi_boundary().metainfo().bytes);
    let refused = client
        .post(running.url("/api/v1/torrents"))
        .header("content-type", content_type)
        .body(body)
        .send()
        .await?;
    assert_eq!(refused.status(), StatusCode::SERVICE_UNAVAILABLE);
    let new_stream = client.get(running.url(content)).send().await?;
    assert_eq!(new_stream.status(), StatusCode::SERVICE_UNAVAILABLE);
    let listed = client.get(running.url("/api/v1/torrents")).send().await?;
    assert_eq!(listed.status(), StatusCode::OK, "reads are still served");
    assert!(
        begun.elapsed() < drain,
        "all of this happened while draining"
    );

    let ended = bounded("the canceled stream", streaming).await??;
    assert!(ended.is_err(), "the stream is canceled, not completed");
    assert!(
        begun.elapsed() >= drain,
        "the stream ran until the drain period ended"
    );
    let address = running.address;
    running.stop().await?;
    assert!(
        tokio::net::TcpStream::connect(address).await.is_err(),
        "the listener is closed"
    );
    Ok(())
}

/// LIF-10's task cleanup: once `serve` returns, every task the application
/// started has ended, those of librqbit's session included, though a stream
/// and a metadata retrieval were in progress when shutdown began.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn shutdown_joins_every_task() -> anyhow::Result<()> {
    let before = alive_tasks();
    let timings = ShutdownTimings {
        drain: Some(Duration::from_millis(500)),
        connection_wait: Duration::from_secs(5),
        timeout: Duration::from_secs(20),
    };
    let running = Running::start(Vec::new(), timings, patient).await?;
    let client = Client::new();
    let created = upload(&running, &client, &catalog::single_small()).await?;
    magnet(&running, &client, &catalog::multi_boundary()).await?;
    let stream = client.get(running.url(&created.content[0])).send().await?;
    assert_eq!(stream.status(), StatusCode::OK);
    assert!(alive_tasks() > before);

    running.stop().await?;
    // The client's own connection tasks end once it is dropped.
    drop((stream, client));
    bounded("the client's tasks ending", async {
        while alive_tasks() > before {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .map_err(|error| anyhow::anyhow!("{error}: {} tasks alive, {before} before", alive_tasks()))?;
    Ok(())
}

/// Without streams in progress, shutdown does not wait for the drain
/// period.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn shutdown_without_streams_skips_the_drain() -> anyhow::Result<()> {
    let timings = ShutdownTimings {
        drain: Some(Duration::from_secs(60)),
        connection_wait: Duration::from_secs(5),
        timeout: Duration::from_secs(20),
    };
    let running = Running::start(Vec::new(), timings, |_| {}).await?;
    let begun = Instant::now();
    running.stop().await?;
    assert!(begun.elapsed() < Duration::from_secs(10));
    Ok(())
}

/// The timings of the tests of connections that will not finish, whose
/// limit they would run into, in which case the application's result fails.
const HELD_TIMINGS: ShutdownTimings = ShutdownTimings {
    drain: Some(Duration::from_millis(200)),
    connection_wait: Duration::from_millis(500),
    timeout: Duration::from_secs(10),
};

/// A connection that sent part of a request head and then nothing does not
/// hold shutdown.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_partial_request_head_does_not_hold_shutdown() -> anyhow::Result<()> {
    let running = Running::start(Vec::new(), HELD_TIMINGS, |_| {}).await?;
    let mut held = partial_head(&running).await?;
    assert!(running.stop_and_read_clean_shutdown().await?);
    assert_closed(&mut held).await
}

/// A pipelining client that stopped reading, whose responses wait for room in
/// the socket with no stream to cancel, does not hold shutdown.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_stalled_pipelining_client_does_not_hold_shutdown() -> anyhow::Result<()> {
    let running = Running::start(Vec::new(), HELD_TIMINGS, |_| {}).await?;
    let socket = TcpSocket::new_v4()?;
    socket.set_recv_buffer_size(4096)?;
    let (mut reader, mut writer) = socket.connect(running.address).await?.into_split();
    let request = format!("GET /livez HTTP/1.1\r\nHost: {}\r\n\r\n", running.address);
    // It ends when the server closes the connection.
    let flood =
        tokio::spawn(async move { while writer.write_all(request.as_bytes()).await.is_ok() {} });
    let mut first = [0; 16];
    assert!(reader.read(&mut first).await? > 0);
    // Long enough for the responses to fill the socket, which holds shutdown
    // well past its limit without closing connections.
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert!(running.stop_and_read_clean_shutdown().await?);
    flood.abort();
    let _ = flood.await;
    Ok(())
}

/// An HTTP/2 client that holds a stream without reading it, so the stream's
/// flow control window runs out, does not hold shutdown: canceling the stream
/// alone does not reach it, since hyper polls its body only with room to send.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_stalled_http2_stream_does_not_hold_shutdown() -> anyhow::Result<()> {
    const CAPACITY: u64 = 32 * MIB;
    let spec = catalog::large(CAPACITY);
    let seeder = Seeder::start(&spec, &spec.metainfo().bytes).await?;
    let running = Running::start(vec![seeder.addr], HELD_TIMINGS, |config| {
        *config = config.clone().with_cache_capacity(CAPACITY);
        patient(config);
    })
    .await?;
    let client = Client::builder().http2_prior_knowledge().build()?;
    let created = upload(&running, &client, &spec).await?;
    let stream = client.get(running.url(&created.content[0])).send().await?;
    assert_eq!(stream.version(), Version::HTTP_2);
    // Out of window once the bytes the body gave stop growing.
    let mut last = 0;
    eventually("the stream's window running out", async || {
        tokio::time::sleep(Duration::from_millis(500)).await;
        let now = streamed_bytes(&running, &client).await?;
        let stalled = now == last && now > 0;
        last = now;
        Ok(stalled.then_some(()))
    })
    .await?;
    assert!(running.stop_and_read_clean_shutdown().await?);
    drop(stream);
    seeder.stop().await;
    Ok(())
}

/// A profile creation whose body has begun to arrive, which keeps its handler
/// running until `rest` is sent.
async fn slow_request(running: &Running) -> anyhow::Result<TcpStream> {
    let mut stream = TcpStream::connect(running.address).await?;
    stream
        .write_all(
            b"POST /api/v1/profiles HTTP/1.1\r\nHost: 127.0.0.1\r\n\
              Content-Type: application/json\r\nContent-Length: 2\r\n\r\n{",
        )
        .await?;
    // The server reads it before shutdown begins.
    tokio::time::sleep(Duration::from_millis(200)).await;
    Ok(stream)
}

/// A handler still running when the connection wait ends gets to finish and
/// answer.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn shutdown_lets_a_running_handler_finish() -> anyhow::Result<()> {
    let mut running = Running::start(Vec::new(), HELD_TIMINGS, |_| {}).await?;
    let mut stream = slow_request(&running).await?;
    running.trigger();
    tokio::time::sleep(HELD_TIMINGS.connection_wait * 3).await;
    assert_answers(&mut stream).await?;
    assert!(running.stop_and_read_clean_shutdown().await?);
    Ok(())
}

/// Asked to stop again, a running handler still gets the connection wait.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_hurried_shutdown_lets_a_short_handler_finish() -> anyhow::Result<()> {
    let timings = ShutdownTimings {
        drain: Some(Duration::ZERO),
        connection_wait: Duration::from_secs(10),
        timeout: Duration::from_secs(60),
    };
    let mut running = Running::start(Vec::new(), timings, |_| {}).await?;
    let mut stream = slow_request(&running).await?;
    running.trigger();
    running.hurry();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_answers(&mut stream).await?;
    assert!(running.stop_and_read_clean_shutdown().await?);
    Ok(())
}

/// Asked to stop again, a handler that never finishes, as with a stalled
/// upload, gets only the connection wait, far less than the limit allows.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_hurried_shutdown_does_not_wait_for_a_stalled_handler() -> anyhow::Result<()> {
    let timings = ShutdownTimings {
        timeout: Duration::from_secs(120),
        ..HELD_TIMINGS
    };
    let mut running = Running::start(Vec::new(), timings, |_| {}).await?;
    let mut stream = slow_request(&running).await?;
    running.trigger();
    tokio::time::sleep(HELD_TIMINGS.connection_wait * 2).await;
    running.hurry();
    assert!(running.stop_and_read_clean_shutdown().await?);
    assert_closed(&mut stream).await
}

async fn assert_answers(stream: &mut TcpStream) -> anyhow::Result<()> {
    stream.write_all(b"}").await?;
    let mut head = [0; 12];
    bounded("the response", stream.read_exact(&mut head)).await??;
    assert!(head.starts_with(b"HTTP/1.1 "), "{head:?}");
    Ok(())
}

/// A handler that never finishes is closed before the shutdown's limit, with
/// time left for the rest of the shutdown.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_handler_that_never_finishes_does_not_hold_shutdown() -> anyhow::Result<()> {
    let running = Running::start(Vec::new(), HELD_TIMINGS, |_| {}).await?;
    let mut stream = slow_request(&running).await?;
    let (took, clean) = running.stop_timed_and_read_clean_shutdown().await?;
    assert!(clean);
    assert!(took < HELD_TIMINGS.timeout, "the shutdown took {took:?}");
    assert_closed(&mut stream).await
}

/// Asking to stop again skips the drain period and the wait for connections,
/// both far longer than the test, and the shutdown stays clean.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn asking_to_stop_again_skips_the_waits() -> anyhow::Result<()> {
    let timings = ShutdownTimings {
        drain: Some(Duration::from_secs(600)),
        connection_wait: Duration::from_secs(600),
        timeout: Duration::from_secs(600),
    };
    let mut running = Running::start(Vec::new(), timings, patient).await?;
    let client = Client::new();
    let created = upload(&running, &client, &catalog::single_small()).await?;
    // Without peers, the stream keeps the drain waiting.
    let stream = client.get(running.url(&created.content[0])).send().await?;
    let _held = partial_head(&running).await?;
    running.trigger();
    eventually("shutdown beginning", async || {
        let response = client.get(running.url("/readyz")).send().await?;
        Ok((response.status() == StatusCode::SERVICE_UNAVAILABLE).then_some(()))
    })
    .await?;
    running.hurry();
    assert!(
        bounded(
            "the hurried shutdown",
            running.stop_and_read_clean_shutdown()
        )
        .await??,
        "the shutdown is clean"
    );
    drop(stream);
    Ok(())
}

/// Asking to stop again after the drain, while connections are waited for,
/// skips what is left of that wait.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn asking_to_stop_again_ends_the_wait_for_connections() -> anyhow::Result<()> {
    let timings = ShutdownTimings {
        drain: Some(Duration::ZERO),
        connection_wait: Duration::from_secs(600),
        timeout: Duration::from_secs(600),
    };
    let mut running = Running::start(Vec::new(), timings, |_| {}).await?;
    let _held = partial_head(&running).await?;
    running.trigger();
    // No request can show the listener closed from the inside, so this gives
    // the sequence time to reach the wait; hurrying earlier also passes.
    tokio::time::sleep(Duration::from_millis(500)).await;
    running.hurry();
    assert!(
        bounded(
            "the hurried shutdown",
            running.stop_and_read_clean_shutdown()
        )
        .await??
    );
    Ok(())
}

/// The executable's signals: the first starts shutdown, a connection that
/// will not finish makes it wait, and the second ends that wait, with a clean
/// exit that the store records.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_second_signal_hurries_the_executables_shutdown() -> anyhow::Result<()> {
    use std::process::Stdio;
    use tokio::io::AsyncBufReadExt;
    use tokio::process::Command;

    let data = tempfile::tempdir()?;
    let (port, peer_port) = free_ports()?;
    let address = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    let mut child = Command::new(env!("CARGO_BIN_EXE_torrplay"))
        .args(["--no-dht", "--listen"])
        .arg(address.to_string())
        .arg("--peer-port")
        .arg(peer_port.to_string())
        .arg("--data-dir")
        .arg(data.path())
        .env_remove("TORRPLAY_LISTEN")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()?;
    let stderr = child.stderr.take().expect("piped standard error");
    let mut lines = tokio::io::BufReader::new(stderr).lines();
    let mut until = async |text: &str| -> anyhow::Result<()> {
        bounded(text, async {
            while let Some(line) = lines.next_line().await? {
                if line.contains(text) {
                    return Ok(());
                }
            }
            anyhow::bail!("the log ended before {text:?}")
        })
        .await?
    };
    until("the engine is ready").await?;
    let mut held = TcpStream::connect(address).await?;
    held.write_all(b"GET /livez HTTP/1.1\r\nHost: example.invalid\r\n")
        .await?;
    tokio::time::sleep(Duration::from_millis(200)).await;

    let pid = child.id().expect("its process ID").to_string();
    let terminate = async || -> anyhow::Result<()> {
        let sent = Command::new("kill").args(["-TERM", &pid]).status().await?;
        assert!(sent.success());
        Ok(())
    };
    terminate().await?;
    until("waiting for connections to close").await?;
    terminate().await?;
    until("asked again to stop").await?;
    let status = bounded("the executable's exit", child.wait()).await??;
    assert!(status.success(), "{status}");

    let (store, contents) = torrplay_store::Store::open(data.path())?;
    store.close().await?;
    assert!(contents.clean_shutdown);
    Ok(())
}

/// A connection that sent part of a request head and is left open.
async fn partial_head(running: &Running) -> anyhow::Result<TcpStream> {
    let mut stream = TcpStream::connect(running.address).await?;
    stream
        .write_all(b"GET /livez HTTP/1.1\r\nHost: example.invalid\r\n")
        .await?;
    // The server reads it before shutdown begins.
    tokio::time::sleep(Duration::from_millis(200)).await;
    Ok(stream)
}

async fn assert_closed(stream: &mut TcpStream) -> anyhow::Result<()> {
    let read = bounded("the connection closing", stream.read(&mut [0; 16])).await?;
    assert!(
        matches!(read, Ok(0) | Err(_)),
        "the server closed the connection: {read:?}"
    );
    Ok(())
}

async fn streamed_bytes(running: &Running, client: &Client) -> anyhow::Result<u64> {
    let metrics = client
        .get(running.url("/metrics"))
        .send()
        .await?
        .text()
        .await?;
    let value = metrics
        .lines()
        .find_map(|line| line.strip_prefix("torrplay_streamed_bytes_total "))
        .ok_or_else(|| anyhow::anyhow!("no streamed bytes metric"))?;
    Ok(value.parse()?)
}

/// The listener also serves HTTP/2 over cleartext to clients with prior
/// knowledge, and serves before the engine is ready.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn http2_is_served_beside_http1() -> anyhow::Result<()> {
    let running = Running::spawn(Vec::new(), ShutdownTimings::default(), |_| {}).await?;
    let client = Client::builder().http2_prior_knowledge().build()?;
    let live = client.get(running.url("/livez")).send().await?;
    assert_eq!(live.version(), Version::HTTP_2);
    assert_eq!(live.status(), StatusCode::OK);
    running.ready(&client).await?;
    let listed = client.get(running.url("/api/v1/torrents")).send().await?;
    assert_eq!(
        (listed.version(), listed.status()),
        (Version::HTTP_2, StatusCode::OK)
    );
    running.stop().await
}

/// An engine that cannot start stops the application with an error.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_engine_that_cannot_start_stops_the_application() -> anyhow::Result<()> {
    let taken = std::net::TcpListener::bind("127.0.0.1:0")?;
    let folder = tempfile::tempdir()?;
    let mut network = NetworkConfig::isolated(Vec::new());
    network.listen = taken.local_addr()?;
    let config = EngineConfig::new(folder.path().to_path_buf(), network);
    let (store, contents) = torrplay_store::Store::open(&folder.path().join("data"))?;
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let result = bounded(
        "the failed start",
        torrplay::serve(
            listener,
            LibrqbitEngine::new(config),
            store,
            contents,
            torrplay::ServeOptions::default(),
            ShutdownTimings::default(),
            torrplay::StopRequests::new(std::future::pending()),
        ),
    )
    .await?;
    let error = result.expect_err("the peer port is taken");
    assert!(
        format!("{error:#}").contains("the engine did not start"),
        "{error:#}"
    );
    Ok(())
}
