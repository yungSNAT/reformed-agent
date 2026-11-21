use std::collections::HashSet;

use base64::engine::general_purpose::STANDARD;
use base64::Engine as _;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use serde_json::Value;

use crate::McpError;

const MAX_HEADER_BYTES: usize = 16 * 1024;
const MAX_HEADER_COUNT: usize = 64;
const MAX_SAFE_INTEGER: i64 = (1 << 53) - 1;

#[derive(Clone, Copy)]
enum Primitive {
    String,
    Integer,
    Boolean,
}

pub(crate) struct ParameterHeader {
    name: HeaderName,
    property_path: Vec<String>,
    primitive: Primitive,
}

impl ParameterHeader {
    pub(crate) fn from_schema(schema: &Value) -> Result<Vec<Self>, &'static str> {
        if !schema.is_object() {
            return Err("tool inputSchema must be a JSON Schema object");
        }
        let mut headers = Vec::new();
        collect_annotations(schema, &mut Vec::new(), true, &mut headers)?;
        let unique: HashSet<_> = headers.iter().map(|header| &header.name).collect();
        if unique.len() != headers.len() {
            return Err("duplicate x-mcp-header name");
        }
        Ok(headers)
    }

    fn value_at(&self, arguments: &Value) -> Result<Option<HeaderValue>, McpError> {
        let value = self
            .property_path
            .iter()
            .try_fold(arguments, |value, component| value.get(component));
        let Some(value) = value.filter(|value| !value.is_null()) else {
            return Ok(None);
        };
        let encoded = match self.primitive {
            Primitive::String => value.as_str().map(encode_value).transpose()?,
            Primitive::Integer => value
                .as_i64()
                .filter(|number| (-MAX_SAFE_INTEGER..=MAX_SAFE_INTEGER).contains(number))
                .map(|number| encode_value(&number.to_string()))
                .transpose()?,
            Primitive::Boolean => value
                .as_bool()
                .map(|flag| HeaderValue::from_static(if flag { "true" } else { "false" })),
        };
        encoded.map(Some).ok_or_else(|| {
            McpError::InvalidParams("Invalid value for a mirrored MCP parameter".into())
        })
    }
}

fn collect_annotations(
    schema: &Value,
    path: &mut Vec<String>,
    reachable: bool,
    headers: &mut Vec<ParameterHeader>,
) -> Result<(), &'static str> {
    let Some(schema) = schema.as_object() else {
        return Ok(());
    };
    if let Some(annotation) = schema.get("x-mcp-header") {
        if !reachable || path.is_empty() || schema.contains_key("$ref") {
            return Err("x-mcp-header must be on a statically reachable property");
        }
        let suffix = annotation
            .as_str()
            .filter(|suffix| !suffix.is_empty())
            .ok_or("x-mcp-header must be a nonempty name")?;
        let name = HeaderName::from_bytes(format!("mcp-param-{suffix}").as_bytes())
            .map_err(|_| "x-mcp-header name is invalid")?;
        let primitive = match schema.get("type").and_then(Value::as_str) {
            Some("string") => Primitive::String,
            Some("integer") => Primitive::Integer,
            Some("boolean") => Primitive::Boolean,
            _ => return Err("x-mcp-header property must be a string, integer or boolean"),
        };
        if headers.len() == MAX_HEADER_COUNT {
            return Err("tool schema mirrors too many MCP headers");
        }
        headers.push(ParameterHeader {
            name,
            property_path: path.clone(),
            primitive,
        });
    }

    if let Some(properties) = schema.get("properties").and_then(Value::as_object) {
        for (property, child) in properties {
            path.push(property.clone());
            collect_annotations(child, path, reachable, headers)?;
            path.pop();
        }
    }

    // This list covers dialect keywords whose values are themselves schemas.
    // Instance examples and defaults can freely use the name "x-mcp-header".
    for key in [
        "additionalItems",
        "additionalProperties",
        "contains",
        "contentSchema",
        "else",
        "if",
        "items",
        "not",
        "propertyNames",
        "then",
        "unevaluatedItems",
        "unevaluatedProperties",
    ] {
        match schema.get(key) {
            Some(Value::Array(children)) => {
                for child in children {
                    collect_annotations(child, path, false, headers)?;
                }
            }
            Some(child) => collect_annotations(child, path, false, headers)?,
            None => {}
        }
    }
    for key in ["allOf", "anyOf", "oneOf", "prefixItems"] {
        if let Some(children) = schema.get(key).and_then(Value::as_array) {
            for child in children {
                collect_annotations(child, path, false, headers)?;
            }
        }
    }
    for key in [
        "$defs",
        "definitions",
        "dependencies",
        "dependentSchemas",
        "patternProperties",
    ] {
        if let Some(children) = schema.get(key).and_then(Value::as_object) {
            for child in children.values() {
                collect_annotations(child, path, false, headers)?;
            }
        }
    }
    Ok(())
}

fn encode_value(value: &str) -> Result<HeaderValue, McpError> {
    if value.len() > MAX_HEADER_BYTES {
        return Err(header_limit());
    }
    let ambiguous = value.starts_with("=?base64?") && value.ends_with("?=");
    let visible = value
        .as_bytes()
        .iter()
        .all(|byte| matches!(*byte, b'\t' | b' '..=b'~'));
    let wire = if ambiguous || !visible || value.trim() != value {
        format!("=?base64?{}?=", STANDARD.encode(value.as_bytes()))
    } else {
        value.to_owned()
    };
    let mut header = HeaderValue::from_str(&wire)
        .map_err(|_| McpError::InvalidParams("Invalid MCP header value".into()))?;
    header.set_sensitive(true);
    Ok(header)
}

pub(crate) fn add_protocol_headers(
    headers: &mut HeaderMap,
    message: &Value,
    annotation_headers: &[ParameterHeader],
) -> Result<(), McpError> {
    let method = message["method"]
        .as_str()
        .ok_or_else(|| McpError::InvalidParams("MCP request method must be a string".into()))?;
    let version = message
        .pointer("/params/_meta/io.modelcontextprotocol~1protocolVersion")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            McpError::InvalidParams("MCP request is missing protocol metadata".into())
        })?;
    let plain = |value: &str| {
        HeaderValue::from_str(value)
            .map_err(|_| McpError::InvalidParams("Invalid standard MCP header".into()))
    };
    headers.insert("mcp-method", plain(method)?);
    headers.insert("mcp-protocol-version", plain(version)?);

    let name = match method {
        "tools/call" | "prompts/get" => Some(&message["params"]["name"]),
        "resources/read" => Some(&message["params"]["uri"]),
        _ => None,
    };
    if let Some(name) = name {
        let name = name.as_str().ok_or_else(|| {
            McpError::InvalidParams("MCP request is missing its name or URI".into())
        })?;
        headers.insert("mcp-name", encode_value(name)?);
    }
    for annotation in annotation_headers {
        if let Some(value) = annotation.value_at(&message["params"]["arguments"])? {
            headers.insert(annotation.name.clone(), value);
        }
    }
    if headers.len() > MAX_HEADER_COUNT
        || headers
            .iter()
            .map(|(name, value)| name.as_str().len() + value.len() + 4)
            .sum::<usize>()
            > MAX_HEADER_BYTES
    {
        return Err(header_limit());
    }
    Ok(())
}

pub(crate) fn protocol_owned_header(name: &HeaderName) -> bool {
    name.as_str().starts_with("mcp-")
        || matches!(
            name.as_str(),
            "accept"
                | "connection"
                | "content-length"
                | "content-type"
                | "expect"
                | "host"
                | "keep-alive"
                | "last-event-id"
                | "proxy-authorization"
                | "te"
                | "trailer"
                | "transfer-encoding"
                | "upgrade"
        )
}

fn header_limit() -> McpError {
    McpError::InvalidParams("MCP request headers exceed the transport limit".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn mirrored_value_preserves_unicode_whitespace_and_literal_sentinels() {
        for value in ["雪", " padded ", "=?base64?literal?="] {
            let wire = encode_value(value).unwrap();
            let encoded = wire
                .to_str()
                .unwrap()
                .strip_prefix("=?base64?")
                .unwrap()
                .strip_suffix("?=")
                .unwrap();
            assert_eq!(STANDARD.decode(encoded).unwrap(), value.as_bytes());
        }
        assert_eq!(encode_value("plain\ttext").unwrap(), "plain\ttext");
    }

    #[test]
    fn annotated_tools_reject_unreachable_duplicate_or_invalid_header_names() {
        for schema in [
            Value::Null,
            json!(true),
            json!({"items":{"type":"string","x-mcp-header":"Array"}}),
            json!({"properties":{"a":{"type":"string","x-mcp-header":"Same"},"b":{"type":"boolean","x-mcp-header":"same"}}}),
            json!({"properties":{"a":{"type":"string","x-mcp-header":" two words "}}}),
        ] {
            assert!(ParameterHeader::from_schema(&schema).is_err());
        }
        let schema = json!({"examples":[{"x-mcp-header":"just example data"}]});
        assert!(ParameterHeader::from_schema(&schema).unwrap().is_empty());
    }
}
