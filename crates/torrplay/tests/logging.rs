// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! OBS-06: `torrplay --log-format json` writes only JSON records to standard
//! error, each naming the run the system resource reports and, while it
//! serves a request, the request's ID.

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
