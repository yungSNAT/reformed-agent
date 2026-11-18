//! Tool parameter schemas cut to the subset Google's Gemini `Schema` accepts.
//!
//! Port of upstream `agent/gemini_schema.py`: Hermes tool schemas are
//! OpenAI-flavoured JSON Schema, and Gemini rejects the whole request over
//! one key outside its subset, a non-string `enum` value or a `required`
//! name its node does not define.

use serde_json::{Map, Value};

const ALLOWED_KEYS: &[&str] = &[
    "type",
    "format",
    "title",
    "description",
    "nullable",
    "enum",
    "maxItems",
    "minItems",
    "properties",
    "required",
    "minProperties",
    "maxProperties",
    "minLength",
    "maxLength",
    "pattern",
    "example",
    "anyOf",
    "propertyOrdering",
    "default",
    "items",
    "minimum",
    "maximum",
];

/// A Gemini-compatible copy of a schema node: an empty object for anything
/// but an object.
pub fn sanitize_gemini_schema(schema: &Value) -> Map<String, Value> {
    let Some(schema) = schema.as_object() else {
        return Map::new();
    };
    let mut cleaned = Map::new();
    for (key, value) in schema {
        if !ALLOWED_KEYS.contains(&key.as_str()) {
            continue;
        }
        let value = match key.as_str() {
            "properties" => {
                let Some(properties) = value.as_object() else {
                    continue;
                };
                Value::Object(
                    properties
                        .iter()
                        .map(|(name, property)| {
                            (
                                name.clone(),
                                Value::Object(sanitize_gemini_schema(property)),
                            )
                        })
                        .collect(),
                )
            }
            "items" => Value::Object(sanitize_gemini_schema(value)),
            "anyOf" => {
                let Some(branches) = value.as_array() else {
                    continue;
                };
                Value::Array(
                    branches
                        .iter()
                        .filter(|branch| branch.is_object())
                        .map(|branch| Value::Object(sanitize_gemini_schema(branch)))
                        .collect(),
                )
            }
            _ => value.clone(),
        };
        cleaned.insert(key.clone(), value);
    }

    // Gemini wants every enum value a string, even under a numeric or
    // boolean type, which other providers accept. Drop such an enum; the
    // type and description still guide the model and the tool validates.
    let typed = matches!(
        cleaned.get("type").and_then(Value::as_str),
        Some("integer" | "number" | "boolean")
    );
    let non_string_enum = cleaned
        .get("enum")
        .and_then(Value::as_array)
        .is_some_and(|values| values.iter().any(|value| !value.is_string()));
    if typed && non_string_enum {
        cleaned.remove("enum");
    }

    // Gemini checks `required` against the same node's `properties`, and MCP
    // servers send `required` without them; keep only the names defined.
    if let Some(required) = cleaned.get("required").and_then(Value::as_array) {
        let properties = cleaned.get("properties").and_then(Value::as_object);
        let defined: Vec<Value> = required
            .iter()
            .filter(|name| {
                name.as_str()
                    .is_some_and(|name| properties.is_some_and(|p| p.contains_key(name)))
            })
            .cloned()
            .collect();
        if defined.is_empty() {
            cleaned.remove("required");
        } else {
            cleaned.insert("required".to_string(), Value::Array(defined));
        }
    }

    cleaned
}

/// A tool's parameters as a valid Gemini object schema.
pub fn sanitize_gemini_tool_parameters(parameters: &Value) -> Value {
    let cleaned = sanitize_gemini_schema(parameters);
    if cleaned.is_empty() {
        return serde_json::json!({"type": "object", "properties": {}});
    }
    Value::Object(cleaned)
}

#[cfg(test)]
mod tests;
