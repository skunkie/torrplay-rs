// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! OBS-06: `torrplay --log-format json` writes only JSON records to standard
//! error, each naming the run the system resource reports and, while it
//! serves a request, the request's ID. OBS-09: the log files keep a run's
//! records for the next run's API.

// The helpers that start and stop the process serve only these tests.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::net::Ipv4Addr;
use std::time::{Duration, Instant};

use serde_json::Value;
use torrplay::LogFormat;

mod process;

use process::Process;

async fn start(data: &std::path::Path, format: LogFormat, level: &str) -> Process {
    Process::start(data, Ipv4Addr::LOCALHOST, 0, format, level)
        .await
        .unwrap_or_else(|error| panic!("{error:#}"))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn json_records_name_the_run_and_the_request() {
    let data = tempfile::TempDir::new().expect("a data directory");
    // Responses that succeed are debug messages.
    let mut process = start(data.path(), LogFormat::Json, "debug").await;
    let system = process
        .client()
        .get(process.url("/api/v1/system"))
        .send()
        .await
        .expect("a system response");
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
    let deadline = Instant::now() + Duration::from_secs(10);
    let records = loop {
        let mut records = process.records().await.expect("JSON on standard error");
        if let Some(index) = records
            .iter()
            .position(|record| record["request_id"] == request_id.as_str())
        {
            records.truncate(index + 1);
            break records;
        }
        assert!(
            Instant::now() < deadline,
            "the response was not logged\n{}",
            process.stderr().await
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    let found = records.last().expect("the response is logged");
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
    process.kill().await.expect("torrplay stops");
}

#[cfg(unix)]
async fn json(process: &Process, url: impl reqwest::IntoUrl) -> Value {
    let response = process.client().get(url).send().await.expect("an answer");
    response.json().await.expect("JSON")
}

/// What the log files under `data` hold, to tell a record that was lost from
/// one the API did not find.
#[cfg(unix)]
fn log_files_text(data: &std::path::Path) -> String {
    let logs = data.join("logs");
    let Ok(entries) = std::fs::read_dir(&logs) else {
        return format!("{} cannot be read", logs.display());
    };
    let mut paths: Vec<_> = entries.flatten().map(|entry| entry.path()).collect();
    paths.sort();
    paths
        .iter()
        .map(|path| {
            let text = std::fs::read_to_string(path).unwrap_or_else(|error| error.to_string());
            format!("{}:\n{text}", path.display())
        })
        .collect()
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn startup_reports_an_unavailable_peer_port() {
    let data = tempfile::TempDir::new().expect("a data directory");
    let occupied_peer = std::net::TcpListener::bind("[::]:0").expect("a busy peer port");
    let peer_port = occupied_peer.local_addr().expect("the peer address").port();
    let error = Process::start(
        data.path(),
        Ipv4Addr::LOCALHOST,
        peer_port,
        LogFormat::Text,
        "info",
    )
    .await
    .expect_err("the engine cannot start on a busy port");
    let error = format!("{error:#}");
    assert!(
        error.contains("torrplay exited before it became ready"),
        "{error}"
    );
    assert!(error.contains("the engine did not start"), "{error}");
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_next_run_reads_the_last_runs_records() {
    let data = tempfile::TempDir::new().expect("a data directory");
    // Responses that succeed are debug messages.
    let mut first = start(data.path(), LogFormat::Text, "debug").await;
    let system = first
        .client()
        .get(first.url("/api/v1/system"))
        .send()
        .await
        .expect("a system response");
    let request_id = system.headers()["x-request-id"]
        .to_str()
        .unwrap()
        .to_owned();
    let first_run: Value = serde_json::from_slice(&system.bytes().await.unwrap()).unwrap();
    first.terminate().await.expect("the first run stops");

    let mut second = start(data.path(), LogFormat::Text, "info").await;
    let page = json(
        &second,
        second.url(&format!("/api/v1/log-records?contains={request_id}")),
    )
    .await;
    let records = page["items"].as_array().expect("records");
    assert_eq!(
        records.len(),
        1,
        "{request_id} in {page}\nthe log files hold:\n{}",
        log_files_text(data.path())
    );
    assert_eq!(records[0]["run_id"], first_run["run_id"]);
    assert_eq!(records[0]["fields"]["route"], "/api/v1/system");
    let latest = page["latest_sequence"].as_u64().unwrap();
    assert!(latest > records[0]["sequence"].as_u64().unwrap(), "{page}");
    let second_run = json(&second, second.url("/api/v1/system")).await;
    assert_ne!(second_run["run_id"], first_run["run_id"]);
    second.terminate().await.expect("the second run stops");
}

/// LIF-10: a shutdown that runs out of time shows where it stopped, because
/// each step that can wait is logged before it begins.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_logs_each_step_in_order() {
    let data = tempfile::TempDir::new().expect("a data directory");
    let mut first = start(data.path(), LogFormat::Text, "info").await;
    first.terminate().await.expect("the first run stops");

    let mut second = start(data.path(), LogFormat::Text, "info").await;
    let mut last = 0;
    for step in [
        "stopping the engine",
        "flushing the data of disk torrents",
        "stopping the BitTorrent session",
        "stopping the engine's tasks",
        "recording the clean shutdown",
    ] {
        let url = reqwest::Url::parse_with_params(
            &second.url("/api/v1/log-records"),
            [("contains", step)],
        )
        .unwrap();
        let page = json(&second, url).await;
        let records = page["items"].as_array().expect("records");
        let sequence = records
            .iter()
            .find(|record| record["message"] == step)
            .and_then(|record| record["sequence"].as_u64())
            .unwrap_or_else(|| panic!("{step} is logged: {page}"));
        assert!(sequence > last, "{step} follows the step before it: {page}");
        last = sequence;
    }
    second.terminate().await.expect("the second run stops");
}
