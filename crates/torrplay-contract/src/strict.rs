// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! The strict copy of the document that ADR 6 describes.
//!
//! Most contract schemas leave objects open, because clients must ignore
//! members added later. The server must not add members the contract does
//! not define, so tests validate against a copy whose objects are closed
//! with `unevaluatedProperties: false` where schemas are used: at `$ref`
//! sites outside `allOf`, at inline object schemas, and at schemas that
//! compose with `allOf`. Component definitions and `allOf` members stay
//! open, because `unevaluatedProperties` at the use site already sees the
//! members every part of a composition defines; closing the parts
//! separately would hide their siblings' members.

use serde_json::{Map, Value};

/// Returns the strict copy of `document`.
pub(crate) fn close_objects(mut document: Value) -> Value {
    let definitions = document
        .get_mut("components")
        .and_then(Value::as_object_mut)
        .and_then(|components| components.remove("schemas"));
    visit(&mut document, false);
    if let Some(mut definitions) = definitions {
        if let Value::Object(map) = &mut definitions {
            for definition in map.values_mut() {
                visit(definition, true);
            }
        }
        if let Some(Value::Object(components)) = document.get_mut("components") {
            components.insert("schemas".into(), definitions);
        }
    }
    document
}

fn close(map: &mut Map<String, Value>) {
    if !map.contains_key("additionalProperties") && !map.contains_key("unevaluatedProperties") {
        map.insert("unevaluatedProperties".into(), Value::Bool(false));
    }
}

fn visit(node: &mut Value, open: bool) {
    match node {
        Value::Object(map) => {
            let schema_like = map.contains_key("$ref")
                || map.contains_key("properties")
                || map.contains_key("allOf");
            if schema_like && !open {
                close(map);
            }
            for (key, child) in map.iter_mut() {
                match (key.as_str(), child) {
                    ("allOf", Value::Array(members)) => {
                        for member in members {
                            visit(member, true);
                        }
                    }
                    // Examples are data, not schemas.
                    ("examples" | "example", _) => {}
                    (_, child) => visit(child, false),
                }
            }
        }
        Value::Array(items) => {
            for child in items {
                visit(child, false);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn use_sites_close_and_definitions_stay_open() {
        let document = json!({
            "paths": {"/a": {"get": {"responses": {"200": {"content": {"application/json": {
                "schema": {"$ref": "#/components/schemas/Both"}
            }}}}}}},
            "components": {"schemas": {
                "Base": {"type": "object", "properties": {"a": {"type": "object", "properties": {}}}},
                "Both": {"allOf": [{"$ref": "#/components/schemas/Base"}, {"properties": {"b": {}}}]},
                "Closed": {"type": "object", "additionalProperties": false, "properties": {}},
            }},
        });
        let strict = close_objects(document);
        let schema = "/paths/~1a/get/responses/200/content/application~1json/schema";
        assert_eq!(
            strict.pointer(&format!("{schema}/unevaluatedProperties")),
            Some(&json!(false))
        );
        let schemas = &strict["components"]["schemas"];
        assert!(schemas["Base"].get("unevaluatedProperties").is_none());
        assert_eq!(
            schemas["Base"]["properties"]["a"]["unevaluatedProperties"],
            json!(false),
            "objects nested in a definition are use sites"
        );
        assert!(
            schemas["Both"]["allOf"][1]
                .get("unevaluatedProperties")
                .is_none()
        );
        assert!(schemas["Closed"].get("unevaluatedProperties").is_none());
    }
}
