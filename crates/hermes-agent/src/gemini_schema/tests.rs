//! Cases from upstream `tests/agent/test_gemini_schema.py`.

use super::*;
use serde_json::json;

fn clean(schema: Value) -> Value {
    Value::Object(sanitize_gemini_schema(&schema))
}

#[test]
fn strips_unknown_top_level_keys() {
    let cleaned = clean(json!({
        "$schema": "http://json-schema.org/draft-07/schema#",
        "type": "object",
        "additionalProperties": false,
        "properties": {"foo": {"type": "string"}}
    }));
    assert_eq!(
        cleaned,
        json!({"type": "object", "properties": {"foo": {"type": "string"}}})
    );
}

#[test]
fn keeps_string_enums() {
    let cleaned = clean(json!({"type": "string", "enum": ["pending", "done", "cancelled"]}));
    assert_eq!(cleaned["enum"], json!(["pending", "done", "cancelled"]));
}

#[test]
fn drops_integer_number_and_boolean_enums() {
    for (kind, values) in [
        ("integer", json!([60, 1440, 4320, 10080])),
        ("number", json!([0.5, 1.0])),
        ("boolean", json!([true, false])),
    ] {
        let cleaned = clean(json!({"type": kind, "enum": values, "description": "Minutes"}));
        assert_eq!(
            cleaned,
            json!({"type": kind, "description": "Minutes"}),
            "{kind}"
        );
    }
}

#[test]
fn keeps_an_enum_of_numbers_written_as_strings() {
    let cleaned = clean(json!({"type": "string", "enum": ["60", "1440", "4320", "10080"]}));
    assert_eq!(cleaned["enum"], json!(["60", "1440", "4320", "10080"]));
}

#[test]
fn drops_nested_integer_enums_in_properties_and_items() {
    let cleaned = clean(json!({
        "type": "object",
        "properties": {
            "auto_archive_duration": {"type": "integer", "enum": [60, 1440]},
            "status": {"type": "string", "enum": ["active", "archived"]},
            "sizes": {"type": "array", "items": {"type": "integer", "enum": [1, 2]}}
        }
    }));
    let properties = &cleaned["properties"];
    assert_eq!(
        properties["auto_archive_duration"],
        json!({"type": "integer"})
    );
    assert_eq!(properties["status"]["enum"], json!(["active", "archived"]));
    assert_eq!(properties["sizes"]["items"], json!({"type": "integer"}));
}

#[test]
fn non_object_input_is_empty() {
    for input in [Value::Null, json!("not a schema"), json!([1, 2, 3])] {
        assert!(sanitize_gemini_schema(&input).is_empty());
    }
}

#[test]
fn required_keeps_only_names_its_node_defines() {
    assert_eq!(
        clean(json!({"type": "object", "required": ["a", "b"]})),
        json!({"type": "object"})
    );
    let cleaned = clean(json!({
        "properties": {"a": {"type": "string"}, "b": {"type": "integer"}},
        "required": ["a", "ghost", 42, null, "b"]
    }));
    assert_eq!(cleaned["required"], json!(["a", "b"]));
}

#[test]
fn required_is_pruned_inside_items_and_any_of() {
    let cleaned = clean(json!({
        "type": "object",
        "properties": {
            "issue_fields": {
                "type": "array",
                "items": {"type": "object", "required": ["field_id", "value"]}
            }
        },
        "required": ["issue_fields"],
        "anyOf": [
            {"type": "object", "properties": {"x": {"type": "string"}}, "required": ["x", "ghost"]},
            {"type": "object", "required": ["orphan"]},
            "not a branch"
        ]
    }));
    assert_eq!(
        cleaned["properties"]["issue_fields"]["items"],
        json!({"type": "object"})
    );
    assert_eq!(cleaned["required"], json!(["issue_fields"]));
    assert_eq!(cleaned["anyOf"][0]["required"], json!(["x"]));
    assert_eq!(cleaned["anyOf"][1], json!({"type": "object"}));
    assert_eq!(cleaned["anyOf"].as_array().map(Vec::len), Some(2));
}

#[test]
fn empty_tool_parameters_become_an_empty_object_schema() {
    assert_eq!(
        sanitize_gemini_tool_parameters(&json!({})),
        json!({"type": "object", "properties": {}})
    );
}
