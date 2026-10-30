// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! The language-neutral search cases in `fixtures/search-cases.json`, which
//! the client's matcher runs as well, so both match and rank alike without
//! typo tolerance.

// The cases assert as tests do; clippy's test exemption covers only test
// functions.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use serde_json::Value;
use torrplay_core::search::{Query, search};

const CASES: &str = include_str!("../../../fixtures/search-cases.json");

/// SRC-01.
#[test]
fn the_shared_cases_match_and_rank_as_listed() {
    let cases: Value = serde_json::from_str(CASES).expect("the cases parse");
    // Each item is its name and its title, empty when it has none, in
    // library order.
    let library: Vec<(&str, &str)> = cases["library"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| {
            (
                item["name"].as_str().unwrap(),
                item["title"].as_str().unwrap(),
            )
        })
        .collect();
    let cases = cases["cases"].as_array().unwrap();
    assert!(!cases.is_empty());
    for case in cases {
        let query = Query::new(case["query"].as_str().unwrap(), false);
        let found: Vec<&str> = search(library.iter(), &query, |(name, title)| vec![*title, *name])
            .into_iter()
            .map(|(name, _)| *name)
            .collect();
        let expected: Vec<&str> = case["want"]
            .as_array()
            .unwrap()
            .iter()
            .map(|name| name.as_str().unwrap())
            .collect();
        assert_eq!(found, expected, "{}", case["name"]);
    }
}
