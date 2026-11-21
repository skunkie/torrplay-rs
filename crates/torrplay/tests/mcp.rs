// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! MCP-08: `torrplay mcp` speaks only the protocol on standard output, takes
//! the application's origin from its environment, and ends when its input
//! closes. MCP-10: over HTTP, it shuts down on SIGTERM.

use std::net::{Ipv4Addr, SocketAddr};
use std::process::Stdio;
use std::time::Duration;

use serde_json::{Value, json};
use socket2::{Domain, Protocol, Socket, Type};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use torrplay::{LOG_FORMAT_VARIABLE, TOKEN_VARIABLE, URL_VARIABLE};

#[tokio::test]
async fn stdio_carries_only_protocol_messages() {
    // Hold the port without listening so the call fails at this origin.
    let unavailable =
        Socket::new(Domain::IPV4, Type::STREAM, Some(Protocol::TCP)).expect("a TCP socket");
    unavailable
        .bind(&SocketAddr::from((Ipv4Addr::LOCALHOST, 0)).into())
        .expect("a port");
    let address = unavailable
        .local_addr()
        .expect("the bound address")
        .as_socket()
        .expect("an IP address");
    let origin = format!("http://{address}");
    let mut child = Command::new(env!("CARGO_BIN_EXE_torrplay"))
        .args(["mcp", "--log-level", "debug"])
        .env(LOG_FORMAT_VARIABLE, "json")
        .env(URL_VARIABLE, &origin)
        .env(TOKEN_VARIABLE, "atk_secret_that_must_not_be_logged")
        .env("NO_PROXY", "127.0.0.1")
        .env("no_proxy", "127.0.0.1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .expect("torrplay mcp starts");
    let mut input = child.stdin.take().expect("its input");
    let mut output = BufReader::new(child.stdout.take().expect("its output")).lines();
    let messages = [
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": { "name": "test", "version": "1" }
            }
        }),
        json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": { "name": "list_torrents", "arguments": {} }
        }),
    ];
    let mut answers = Vec::new();
    for message in &messages {
        input
            .write_all(format!("{message}\n").as_bytes())
            .await
            .expect("the message is sent");
        if message.get("id").is_none() {
            continue;
        }
        let line = tokio::time::timeout(Duration::from_secs(20), output.next_line())
            .await
            .expect("an answer in time")
            .expect("its output")
            .expect("a line");
        let answer: Value = serde_json::from_str(&line).expect("every line is JSON-RPC");
        assert_eq!(answer["jsonrpc"], "2.0", "{line}");
        answers.push(answer);
    }
    assert_eq!(answers[0]["result"]["serverInfo"]["name"], "TorrPlay");
    let call = &answers[1]["result"];
    assert_eq!(call["isError"], true);
    let text = call["content"][0]["text"].as_str().expect("a text result");
    assert!(text.contains(&origin), "{text}");

    drop(input);
    let finished = tokio::time::timeout(Duration::from_secs(20), child.wait_with_output())
        .await
        .expect("it ends when its input closes")
        .expect("its exit");
    assert!(finished.status.success(), "{finished:?}");
    assert!(finished.stdout.is_empty(), "nothing follows the answers");
    let log = String::from_utf8_lossy(&finished.stderr);
    assert!(log.contains("serving MCP"), "{log}");
    assert!(!log.contains("atk_secret"), "the token stays out of logs");
}

/// MCP-10: a termination signal that arrives once the HTTP transport
/// listens shuts it down rather than ending the process.
#[cfg(unix)]
#[tokio::test]
async fn the_http_transport_stops_on_sigterm() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_torrplay"))
        .args([
            "mcp",
            "--transport",
            "http",
            "--listen",
            "127.0.0.1:0",
            "--log-level",
            "info",
        ])
        .env(LOG_FORMAT_VARIABLE, "json")
        .env_remove(URL_VARIABLE)
        .env_remove(TOKEN_VARIABLE)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .expect("torrplay mcp starts");
    let mut logs = BufReader::new(child.stderr.take().expect("its standard error")).lines();
    let mut startup_log = String::new();
    let bound: SocketAddr = tokio::time::timeout(Duration::from_secs(30), async {
        while let Some(line) = logs.next_line().await.expect("a log line") {
            let record: Value = serde_json::from_str(&line).expect("a JSON log record");
            if record["message"] == "serving MCP over Streamable HTTP" {
                return record["fields"]["address"]
                    .as_str()
                    .and_then(|text| text.parse().ok());
            }
            startup_log.push_str(&line);
            startup_log.push('\n');
        }
        None
    })
    .await
    .unwrap_or_else(|_| panic!("torrplay mcp did not listen in time\n{startup_log}"))
    .unwrap_or_else(|| panic!("torrplay mcp did not report a bound address\n{startup_log}"));
    assert_ne!(bound.port(), 0, "the listener reports its OS-assigned port");
    tokio::time::timeout(
        Duration::from_secs(5),
        tokio::net::TcpStream::connect(bound),
    )
    .await
    .expect("the connection completes in time")
    .expect("MCP accepts connections at the reported address");
    let pid = child.id().expect("its process ID");
    let sent = Command::new("kill")
        .args(["-TERM", &pid.to_string()])
        .status()
        .await
        .expect("kill runs");
    assert!(sent.success());
    let status = tokio::time::timeout(Duration::from_secs(20), child.wait())
        .await
        .expect("it stops in time")
        .expect("its exit");
    assert!(status.success(), "{status}");
}
