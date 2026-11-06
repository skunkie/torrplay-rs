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

mod ports;

use ports::free_ports;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn json_records_name_the_run_and_the_request() {
    let data = tempfile::TempDir::new().expect("a data directory");
    let (port, peer_port) = free_ports().expect("ports");
    let mut child = Command::new(env!("CARGO_BIN_EXE_torrplay"))
        // Responses that succeed are debug messages.
        .args([
            "--log-format",
            "json",
            "--log-level",
            "debug",
            "--no-dht",
            "--listen",
        ])
        .arg(format!("127.0.0.1:{port}"))
        .arg("--peer-port")
        .arg(peer_port.to_string())
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
            Err(error) => {
                let exit = child.try_wait().expect("its status");
                if exit.is_none() && Instant::now() < deadline {
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    continue;
                }
                // Its standard error ends only once it has stopped.
                child.kill().expect("torrplay stops");
                child.wait().expect("its exit");
                let output = received.iter().collect::<Vec<_>>().join("\n");
                panic!("torrplay did not listen ({exit:?}): {error}\n{output}");
            }
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
#[cfg(unix)]
async fn start(data: &std::path::Path, args: &[&str]) -> (std::process::Child, String) {
    let (port, peer_port) = free_ports().expect("ports");
    let mut child = Command::new(env!("CARGO_BIN_EXE_torrplay"))
        .args(args)
        .args(["--no-dht", "--listen"])
        .arg(format!("127.0.0.1:{port}"))
        .arg("--peer-port")
        .arg(peer_port.to_string())
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
        if let Some(status) = child.try_wait().expect("its status") {
            panic!("torrplay exited before it listened: {status}");
        }
        assert!(Instant::now() < deadline, "torrplay did not listen");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    (child, base)
}

#[cfg(unix)]
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
    // Responses that succeed are debug messages.
    let (first, base) = start(
        data.path(),
        &["--log-format", "text", "--log-level", "debug"],
    )
    .await;
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

/// LIF-10: a shutdown that runs out of time shows where it stopped, because
/// each step that can wait is logged before it begins.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_logs_each_step_in_order() {
    let data = tempfile::TempDir::new().expect("a data directory");
    let (first, base) = start(data.path(), &["--log-format", "text"]).await;
    // The engine's steps run once it has started.
    let deadline = Instant::now() + Duration::from_secs(30);
    while !reqwest::get(format!("{base}/readyz"))
        .await
        .is_ok_and(|response| response.status().is_success())
    {
        assert!(Instant::now() < deadline, "torrplay did not become ready");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    terminate(first).await;

    let (second, base) = start(data.path(), &["--log-format", "text"]).await;
    let mut last = 0;
    for step in [
        "stopping the engine",
        "flushing the data of disk torrents",
        "stopping the BitTorrent session",
        "stopping the engine's tasks",
        "recording the clean shutdown",
    ] {
        let url = reqwest::Url::parse_with_params(
            &format!("{base}/api/v1/log-records"),
            [("contains", step)],
        )
        .unwrap();
        let page = json(url.as_str()).await;
        let records = page["items"].as_array().expect("records");
        let sequence = records
            .iter()
            .find(|record| record["message"] == step)
            .and_then(|record| record["sequence"].as_u64())
            .unwrap_or_else(|| panic!("{step} is logged: {page}"));
        assert!(sequence > last, "{step} follows the step before it: {page}");
        last = sequence;
    }
    terminate(second).await;
}
