// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! Every example in the contract survives a round trip through its
//! generated type, so the types neither drop nor rename what the document
//! shows: required `null` members stay `null`, and omitted members stay
//! omitted.

use serde::{Serialize, de::DeserializeOwned};
use serde_json::Value;
use torrplay_api as api;

const DOCUMENT: &str = include_str!("../../../api/openapi.yaml");
const SCHEMAS: &str = "#/components/schemas/";

fn round_trip<T: DeserializeOwned + Serialize>(example: &Value) -> Result<Value, String> {
    let typed: T = serde_json::from_value(example.clone()).map_err(|error| error.to_string())?;
    serde_json::to_value(typed).map_err(|error| error.to_string())
}

/// Round-trips `example` through the type generated for `schema`, or
/// returns `None` for a schema with no type in this table.
fn through(schema: &str, example: &Value) -> Option<Result<Value, String>> {
    Some(match schema {
        "CacheStatistics" => round_trip::<api::CacheStatistics>(example),
        "CreationDesiredState" => round_trip::<api::CreationDesiredState>(example),
        "DesiredState" => round_trip::<api::DesiredState>(example),
        "DuplicateTorrentProblem" => round_trip::<api::DuplicateTorrentProblem>(example),
        "EntryPoint" => round_trip::<api::EntryPoint>(example),
        "Health" => round_trip::<api::Health>(example),
        "ObservedState" => round_trip::<api::ObservedState>(example),
        "Problem" => round_trip::<api::Problem>(example),
        "Settings" => round_trip::<api::Settings>(example),
        "SettingsPatch" => round_trip::<api::SettingsPatch>(example),
        "Torrent" => round_trip::<api::Torrent>(example),
        "TorrentConfiguration" => round_trip::<api::TorrentConfiguration>(example),
        "TorrentConfigurationPatch" => round_trip::<api::TorrentConfigurationPatch>(example),
        "TorrentCreation" => round_trip::<api::TorrentCreation>(example),
        "TorrentFile" => round_trip::<api::TorrentFile>(example),
        "TorrentFilePage" => round_trip::<api::TorrentFilePage>(example),
        "TorrentFileUpload" => round_trip::<api::TorrentFileUpload>(example),
        "TorrentPage" => round_trip::<api::TorrentPage>(example),
        "TorrentStatistics" => round_trip::<api::TorrentStatistics>(example),
        "ValidationProblem" => round_trip::<api::ValidationProblem>(example),
        _ => return None,
    })
}

/// Collects `(schema, JSON Pointer, example)` for every example of a component
/// schema: media type examples whose schema is a component reference, and
/// the examples of the component definitions themselves.
fn collect<'a>(node: &'a Value, location: &str, found: &mut Vec<(&'a str, String, &'a Value)>) {
    match node {
        Value::Object(map) => {
            let schema = map
                .get("schema")
                .and_then(|schema| schema.get("$ref"))
                .and_then(Value::as_str)
                .and_then(|reference| reference.strip_prefix(SCHEMAS));
            if let (Some(schema), Some(Value::Object(examples))) = (schema, map.get("examples")) {
                for (name, example) in examples {
                    if let Some(value) = example.get("value") {
                        found.push((schema, format!("{location}/examples/{name}"), value));
                    }
                }
            }
            for (key, child) in map {
                let token = key.replace('~', "~0").replace('/', "~1");
                collect(child, &format!("{location}/{token}"), found);
            }
        }
        Value::Array(items) => {
            for (index, child) in items.iter().enumerate() {
                collect(child, &format!("{location}/{index}"), found);
            }
        }
        _ => {}
    }
}

#[test]
fn every_component_example_round_trips_through_its_generated_type() {
    let document: Value = serde_saphyr::from_str(DOCUMENT).expect("the contract parses");
    let mut found = Vec::new();
    collect(&document["paths"], "#/paths", &mut found);
    collect(
        &document["components"]["responses"],
        "#/components/responses",
        &mut found,
    );
    let definitions = document["components"]["schemas"]
        .as_object()
        .expect("the contract defines schemas");
    for (schema, definition) in definitions {
        if let Some(Value::Array(examples)) = definition.get("examples") {
            for (index, example) in examples.iter().enumerate() {
                found.push((
                    schema,
                    format!("{SCHEMAS}{schema}/examples/{index}"),
                    example,
                ));
            }
        }
    }
    assert!(!found.is_empty(), "the walk finds the contract's examples");

    for (schema, location, example) in found {
        let Some(result) = through(schema, example) else {
            panic!("{location}: add {schema} to the table of generated types");
        };
        match result {
            Ok(serialized) => {
                assert_eq!(&serialized, example, "{location} changes in a round trip")
            }
            Err(error) => panic!("{location} does not deserialize as {schema}: {error}"),
        }
    }
}
