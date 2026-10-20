// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! The benchmark's access traces parse, name only benchmark fixtures and
//! their files, and stay within the files they read.

// Helpers assert as tests do; clippy's test exemption covers only test
// functions.
#![allow(clippy::expect_used)]

use std::path::Path;

use torrplay_bench::fixtures;
use torrplay_bench::trace::{self, Action};

const TRACES: [&str; 7] = [
    "cold-start",
    "far-seeks",
    "concurrent",
    "sustained-eviction",
    "idle-resume",
    "player-ranges",
    "max-throughput",
];

#[test]
fn every_benchmark_trace_is_valid() -> anyhow::Result<()> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../bench/traces");
    let mut found: Vec<String> = std::fs::read_dir(&dir)?
        .map(|entry| {
            Ok(entry?
                .path()
                .file_stem()
                .expect("a name")
                .to_string_lossy()
                .into_owned())
        })
        .collect::<anyhow::Result<_>>()?;
    found.sort();
    let mut expected = TRACES.map(str::to_owned).to_vec();
    expected.sort();
    assert_eq!(found, expected, "the protocol's trace set");

    for name in TRACES {
        let trace = trace::parse(&std::fs::read_to_string(dir.join(format!("{name}.jsonl")))?)?;
        for (reader, events) in &trace.readers {
            for event in events {
                let spec = fixtures::by_name(&event.fixture)?;
                let file = spec
                    .files()
                    .into_iter()
                    .find(|file| file.path.last().map(String::as_str) == Some(event.file.as_str()))
                    .unwrap_or_else(|| {
                        panic!("{name}: {reader} reads no file of {}", event.fixture)
                    });
                if let Action::Request(request) = &event.action {
                    assert!(
                        request.range_start + request.consume_bytes <= file.size_bytes,
                        "{name}: {reader} reads past the end of {}",
                        event.file
                    );
                }
            }
        }
    }
    Ok(())
}
