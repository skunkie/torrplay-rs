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
        "AccessToken" => round_trip::<api::AccessToken>(example),
        "AccessTokenCreation" => round_trip::<api::AccessTokenCreation>(example),
        "AccessTokenPage" => round_trip::<api::AccessTokenPage>(example),
        "AccessTokenScope" => round_trip::<api::AccessTokenScope>(example),
        "CacheStatistics" => round_trip::<api::CacheStatistics>(example),
        "Categories" => round_trip::<api::Categories>(example),
        "Category" => round_trip::<api::Category>(example),
        "ConfiguredKind" => round_trip::<Option<api::ConfiguredKind>>(example),
        "CreatedAccessToken" => round_trip::<api::CreatedAccessToken>(example),
        "CreationDesiredState" => round_trip::<api::CreationDesiredState>(example),
        "CreationRetention" => round_trip::<api::CreationRetention>(example),
        "CreationStorageMode" => round_trip::<api::CreationStorageMode>(example),
        "Description" => round_trip::<Option<api::Description>>(example),
        "DesiredState" => round_trip::<api::DesiredState>(example),
        "CreationConflictProblem" => round_trip::<api::CreationConflictProblem>(example),
        "EntryPoint" => round_trip::<api::EntryPoint>(example),
        "Genre" => round_trip::<api::Genre>(example),
        "Genres" => round_trip::<api::Genres>(example),
        "Health" => round_trip::<api::Health>(example),
        "ImageMediaType" => round_trip::<api::ImageMediaType>(example),
        "ObservedState" => round_trip::<api::ObservedState>(example),
        "Password" => round_trip::<api::Password>(example),
        "PasswordChange" => round_trip::<api::PasswordChange>(example),
        "PasswordText" => round_trip::<api::PasswordText>(example),
        "PlaybackToken" => round_trip::<api::PlaybackToken>(example),
        "PlaybackTokenRequest" => round_trip::<api::PlaybackTokenRequest>(example),
        "Preload" => round_trip::<api::Preload>(example),
        "PreloadCreation" => round_trip::<api::PreloadCreation>(example),
        "PreloadPage" => round_trip::<api::PreloadPage>(example),
        "PreloadRange" => round_trip::<api::PreloadRange>(example),
        "PreloadState" => round_trip::<api::PreloadState>(example),
        "Problem" => round_trip::<api::Problem>(example),
        "Profile" => round_trip::<api::Profile>(example),
        "ProfileConflictProblem" => round_trip::<api::ProfileConflictProblem>(example),
        "ProfileCreation" => round_trip::<api::ProfileCreation>(example),
        "ProfileKind" => round_trip::<api::ProfileKind>(example),
        "ProfileName" => round_trip::<api::ProfileName>(example),
        "Retention" => round_trip::<api::Retention>(example),
        "ProfilePage" => round_trip::<api::ProfilePage>(example),
        "ProfilePatch" => round_trip::<api::ProfilePatch>(example),
        "Progress" => round_trip::<api::Progress>(example),
        "ProgressPage" => round_trip::<api::ProgressPage>(example),
        "ProgressUpdate" => round_trip::<api::ProgressUpdate>(example),
        "Settings" => round_trip::<api::Settings>(example),
        "SettingsPatch" => round_trip::<api::SettingsPatch>(example),
        "StorageMode" => round_trip::<api::StorageMode>(example),
        "Title" => round_trip::<Option<api::Title>>(example),
        "Torrent" => round_trip::<api::Torrent>(example),
        "TorrentConfiguration" => round_trip::<api::TorrentConfiguration>(example),
        "TorrentConfigurationPatch" => round_trip::<api::TorrentConfigurationPatch>(example),
        "TorrentCreation" => round_trip::<api::TorrentCreation>(example),
        "TorrentFile" => round_trip::<api::TorrentFile>(example),
        "TorrentFilePage" => round_trip::<api::TorrentFilePage>(example),
        "TorrentFileUpload" => round_trip::<api::TorrentFileUpload>(example),
        "TorrentKind" => round_trip::<api::TorrentKind>(example),
        "TorrentPage" => round_trip::<api::TorrentPage>(example),
        "TorrentStatistics" => round_trip::<api::TorrentStatistics>(example),
        "UpNextEntry" => round_trip::<api::UpNextEntry>(example),
        "UpNextPage" => round_trip::<api::UpNextPage>(example),
        "ValidationProblem" => round_trip::<api::ValidationProblem>(example),
        "Viewing" => round_trip::<api::Viewing>(example),
        "Year" => round_trip::<Option<api::Year>>(example),
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
