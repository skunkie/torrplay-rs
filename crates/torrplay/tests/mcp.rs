// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! MCP-08: `torrplay mcp` speaks only the protocol on standard output, takes
//! the application's origin from its environment, and ends when its input
//! closes.

use std::process::Stdio;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;

#[tokio::test]
async fn stdio_carries_only_protocol_messages() {
    // A port nothing listens on: the tool call fails, naming the origin.
    let unused = std::net::TcpListener::bind("127.0.0.1:0").expect("a port");
    let origin = format!("http://{}", unused.local_addr().expect("an address"));
    drop(unused);
    let mut child = Command::new(env!("CARGO_BIN_EXE_torrplay"))
        .args(["mcp", "--log-level", "debug"])
        .env("TORRPLAY_URL", &origin)
        .env("TORRPLAY_TOKEN", "atk_secret_that_must_not_be_logged")
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
