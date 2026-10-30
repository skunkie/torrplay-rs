// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! OBS-06: `torrplay --log-format json` writes only JSON records to standard
//! error, each naming the run the system resource reports and, while it
//! serves a request, the request's ID. OBS-09: the log files keep a run's
//! records for the next run's API.

// The helpers that start and stop the process serve only these tests.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;

/// A port nothing listens on now.
fn free_port() -> std::io::Result<u16> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    Ok(listener.local_addr()?.port())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn json_records_name_the_run_and_the_request() {
    let data = tempfile::TempDir::new().expect("a data directory");
    let port = free_port().expect("a port");
    let mut child = Command::new(env!("CARGO_BIN_EXE_torrplay"))
        .args(["--log-format", "json", "--no-dht", "--listen"])
        .arg(format!("127.0.0.1:{port}"))
        .arg("--peer-port")
        .arg(free_port().expect("a peer port").to_string())
        .arg("--data-dir")
        .arg(data.path())
        .env_remove("TORRPLAY_LOG_FORMAT")
        .env_remove("TORRPLAY_LISTEN")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("torrplay starts");
    let stderr = child.stderr.take().expect("its standard error");
    let (lines, received) = std::sync::mpsc::channel();
    let reader = std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines() {
            let Ok(line) = line else { break };
            if lines.send(line).is_err() {
                break;
            }
        }
    });

    let client = reqwest::Client::new();
    let base = format!("http://127.0.0.1:{port}");
    let deadline = Instant::now() + Duration::from_secs(30);
    let system = loop {
        match client.get(format!("{base}/api/v1/system")).send().await {
            Ok(response) => break response,
            Err(_) if Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            Err(error) => panic!("torrplay did not listen: {error}"),
        }
    };
    let request_id = system
        .headers()
        .get("x-request-id")
        .expect("a request ID")
        .to_str()
        .unwrap()
        .to_owned();
    let body: Value = serde_json::from_slice(&system.bytes().await.unwrap()).unwrap();
    let run_id = body["run_id"].as_str().expect("a run ID").to_owned();

    // The response is logged once its headers are ready.
    let mut records = Vec::new();
    let found = loop {
        let line = received
            .recv_timeout(Duration::from_secs(10))
            .expect("the response is logged");
        let record: Value = serde_json::from_str(&line).expect("every line is JSON");
        records.push(record.clone());
        if record["request_id"] == request_id.as_str() {
            break record;
        }
    };
    assert_eq!(found["message"], "response");
    assert_eq!(found["fields"]["route"], "/api/v1/system");
    for record in &records {
        assert_eq!(record["run_id"], run_id.as_str(), "{record}");
    }
    let sequences: Vec<u64> = records
        .iter()
        .map(|record| record["sequence"].as_u64().unwrap())
        .collect();
    assert_eq!(
        sequences,
        (1..=records.len() as u64).collect::<Vec<_>>(),
        "sequences count up from 1 without gaps"
    );

    child.kill().expect("torrplay stops");
    child.wait().expect("its exit");
    reader.join().expect("the reader ends");
}

/// Starts torrplay on `data` with `args`, and returns it with its API's
/// base URL once it answers.
async fn start(data: &std::path::Path, args: &[&str]) -> (std::process::Child, String) {
    let port = free_port().expect("a port");
    let child = Command::new(env!("CARGO_BIN_EXE_torrplay"))
        .args(args)
        .args(["--no-dht", "--listen"])
        .arg(format!("127.0.0.1:{port}"))
        .arg("--peer-port")
        .arg(free_port().expect("a peer port").to_string())
        .arg("--data-dir")
        .arg(data)
        .env_remove("TORRPLAY_LOG_FORMAT")
        .env_remove("TORRPLAY_LISTEN")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("torrplay starts");
    let base = format!("http://127.0.0.1:{port}");
    let deadline = Instant::now() + Duration::from_secs(30);
    while reqwest::get(format!("{base}/readyz")).await.is_err() {
        assert!(Instant::now() < deadline, "torrplay did not listen");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    (child, base)
}

async fn json(url: &str) -> Value {
    let response = reqwest::get(url).await.expect("an answer");
    serde_json::from_slice(&response.bytes().await.expect("a body")).expect("JSON")
}

/// Asks torrplay to shut down as a service manager does, and waits for it.
#[cfg(unix)]
async fn terminate(mut child: std::process::Child) {
    let sent = Command::new("kill")
        .args(["-TERM", &child.id().to_string()])
        .status()
        .expect("kill runs");
    assert!(sent.success());
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Some(status) = child.try_wait().expect("its status") {
            assert!(status.success(), "{status}");
            return;
        }
        assert!(Instant::now() < deadline, "torrplay did not stop");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_next_run_reads_the_last_runs_records() {
    let data = tempfile::TempDir::new().expect("a data directory");
    let (first, base) = start(data.path(), &["--log-format", "text"]).await;
    let system = reqwest::get(format!("{base}/api/v1/system")).await.unwrap();
    let request_id = system.headers()["x-request-id"]
        .to_str()
        .unwrap()
        .to_owned();
    let first_run: Value = serde_json::from_slice(&system.bytes().await.unwrap()).unwrap();
    terminate(first).await;

    let (second, base) = start(data.path(), &["--log-format", "text"]).await;
    let page = json(&format!("{base}/api/v1/log-records?contains={request_id}")).await;
    let records = page["items"].as_array().expect("records");
    assert_eq!(records.len(), 1, "{page}");
    assert_eq!(records[0]["run_id"], first_run["run_id"]);
    assert_eq!(records[0]["fields"]["route"], "/api/v1/system");
    let latest = page["latest_sequence"].as_u64().unwrap();
    assert!(latest > records[0]["sequence"].as_u64().unwrap(), "{page}");
    let second_run = json(&format!("{base}/api/v1/system")).await;
    assert_ne!(second_run["run_id"], first_run["run_id"]);
    terminate(second).await;
}
