// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! RFC 7396 merge patches of JSON values, as Tauri merges `TAURI_CONFIG` into its
//! configuration. `build.rs` includes this file to merge a configuration the Tauri CLI
//! passes over the client's overlay; the crate compiles it only for its tests, which
//! build scripts cannot have.

use serde_json::Value;

/// Merges `patch` into `target`: objects merge member by member, `null` removes a
/// member, and anything else replaces the value.
pub fn merge(target: &mut Value, patch: Value) {
    let Value::Object(patch) = patch else {
        *target = patch;
        return;
    };
    if !target.is_object() {
        *target = Value::Object(serde_json::Map::new());
    }
    if let Value::Object(target) = target {
        for (key, value) in patch {
            if value.is_null() {
                target.remove(&key);
            } else {
                merge(target.entry(key).or_insert(Value::Null), value);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn a_patch_merges_objects_removes_nulls_and_replaces_the_rest() {
        // RFC 7396's cases, as Tauri's own merge applies them.
        for (target, patch, merged) in [
            (json!({"a": "b"}), json!({"a": "c"}), json!({"a": "c"})),
            (
                json!({"a": "b"}),
                json!({"b": "c"}),
                json!({"a": "b", "b": "c"}),
            ),
            (json!({"a": "b"}), json!({"a": null}), json!({})),
            (
                json!({"a": "b", "b": "c"}),
                json!({"a": null}),
                json!({"b": "c"}),
            ),
            (json!({"a": ["b"]}), json!({"a": "c"}), json!({"a": "c"})),
            (json!({"a": "c"}), json!({"a": ["b"]}), json!({"a": ["b"]})),
            (
                json!({"a": {"b": "c"}}),
                json!({"a": {"b": "d", "c": null}}),
                json!({"a": {"b": "d"}}),
            ),
            (
                json!({"a": [{"b": "c"}]}),
                json!({"a": [1]}),
                json!({"a": [1]}),
            ),
            (json!(["a", "b"]), json!(["c", "d"]), json!(["c", "d"])),
            (json!({"a": "b"}), json!(["c"]), json!(["c"])),
            (
                json!({"e": null}),
                json!({"a": 1}),
                json!({"e": null, "a": 1}),
            ),
            (
                json!([1, 2]),
                json!({"a": "b", "c": null}),
                json!({"a": "b"}),
            ),
            (
                json!({}),
                json!({"a": {"bb": {"ccc": null}}}),
                json!({"a": {"bb": {}}}),
            ),
        ] {
            let mut result = target.clone();
            merge(&mut result, patch.clone());
            assert_eq!(result, merged, "{target} patched with {patch}");
        }
    }

    #[test]
    fn the_cli_keeps_its_keys_and_the_client_the_rest_of_its_identity() {
        let mut client = json!({
            "productName": "TorrPlay Client",
            "identifier": "org.torrplay.client",
        });
        merge(
            &mut client,
            json!({"productName": "Invented", "bundle": {"active": true}}),
        );
        assert_eq!(
            client,
            json!({
                "productName": "Invented",
                "identifier": "org.torrplay.client",
                "bundle": {"active": true},
            })
        );
    }
}
