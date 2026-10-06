//! Bounded, offline validation of reviewed external tool schemas and headers.
use jsonschema::Validator;
use serde_json::Value;
use std::collections::HashSet;

#[derive(Clone)]
struct Offline;
impl jsonschema::Retrieve for Offline {
    fn retrieve(
        &self,
        _: &jsonschema::Uri<String>,
    ) -> Result<Value, Box<dyn std::error::Error + Send + Sync>> {
        Err("External schema retrieval is unavailable".into())
    }
}

struct ParameterHeader {
    name: String,
    path: Vec<String>,
    kind: String,
}

pub(crate) struct ToolSchema {
    validator: Validator,
    headers: Vec<ParameterHeader>,
}

fn bounded(node: &Value, depth: usize, remaining: &mut usize, schema: bool) -> Result<(), String> {
    if depth > 32 || *remaining == 0 {
        return Err("External schema exceeds its structural limit".into());
    }
    *remaining -= 1;
    match node {
        Value::Object(fields) => {
            for (key, value) in fields {
                if schema
                    && matches!(key.as_str(), "$ref" | "$dynamicRef")
                    && !value
                        .as_str()
                        .is_some_and(|reference| reference.starts_with('#'))
                {
                    return Err("External schemas may only use local references".into());
                }
                bounded(value, depth + 1, remaining, schema)?;
            }
        }
        Value::Array(values) => {
            for value in values {
                bounded(value, depth + 1, remaining, schema)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn collect_headers(
    node: &Value,
    path: &[String],
    reachable: bool,
    names: &mut HashSet<String>,
    headers: &mut Vec<ParameterHeader>,
) -> Result<(), String> {
    match node {
        Value::Object(fields) => {
            if let Some(annotation) = fields.get("x-mcp-header") {
                let name = annotation
                    .as_str()
                    .ok_or("External parameter header name must be a string")?;
                let kind = fields
                    .get("type")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if !reachable
                    || path.is_empty()
                    || name.is_empty()
                    || name.len() > 128
                    || !name.bytes().all(|byte| {
                        byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&byte)
                    })
                    || !matches!(kind, "string" | "integer" | "boolean")
                    || !names.insert(name.to_ascii_lowercase())
                    || headers.len() >= 32
                {
                    return Err("External parameter header annotation is invalid".into());
                }
                headers.push(ParameterHeader {
                    name: name.into(),
                    path: path.to_vec(),
                    kind: kind.into(),
                });
            }
            for (key, value) in fields {
                if key == "properties" {
                    if let Some(properties) = value.as_object() {
                        for (property, schema) in properties {
                            let mut nested = path.to_vec();
                            nested.push(property.clone());
                            collect_headers(schema, &nested, reachable, names, headers)?;
                        }
                    } else {
                        return Err("External schema properties must be an object".into());
                    }
                } else {
                    collect_headers(value, path, false, names, headers)?;
                }
            }
        }
        Value::Array(values) => {
            for value in values {
                collect_headers(value, path, false, names, headers)?;
            }
        }
        _ => {}
    }
    Ok(())
}

impl ToolSchema {
    pub(crate) fn compile(schema: &Value, parameter_headers: bool) -> Result<Self, String> {
        if !schema.is_object()
            || serde_json::to_vec(schema)
                .map_err(|_| "External schema is invalid")?
                .len()
                > 128 * 1024
        {
            return Err("External schema exceeds its byte limit or is not an object".into());
        }
        bounded(schema, 0, &mut 4096, true)?;
        let mut headers = Vec::new();
        if parameter_headers {
            collect_headers(schema, &[], true, &mut HashSet::new(), &mut headers)?;
        }
        let validator = jsonschema::options()
            .with_draft(jsonschema::Draft::Draft202012)
            .with_retriever(Offline)
            .build(schema)
            .map_err(|_| "External tool schema is invalid or uses unavailable references")?;
        Ok(Self { validator, headers })
    }

    pub(crate) fn validate(&self, arguments: &Value) -> Result<(), String> {
        bounded(arguments, 0, &mut 4096, false)?;
        if serde_json::to_vec(arguments)
            .map_err(|_| "External arguments are invalid")?
            .len()
            > 64 * 1024
            || !self.validator.is_valid(arguments)
        {
            return Err("External arguments or result do not match the reviewed schema".into());
        }
        Ok(())
    }

    pub(crate) fn parameter_headers(
        &self,
        arguments: &Value,
    ) -> Result<Vec<(String, String)>, String> {
        self.validate(arguments)?;
        let mut values = Vec::new();
        for header in &self.headers {
            let value = header
                .path
                .iter()
                .try_fold(arguments, |value, key| value.get(key));
            let Some(value) = value.filter(|value| !value.is_null()) else {
                continue;
            };
            let value = match header.kind.as_str() {
                "string" => value.as_str().map(str::to_owned),
                "boolean" => value.as_bool().map(|value| value.to_string()),
                "integer" => value
                    .as_i64()
                    .filter(|value| {
                        (-9_007_199_254_740_991..=9_007_199_254_740_991).contains(value)
                    })
                    .map(|value| value.to_string())
                    .or_else(|| {
                        value
                            .as_f64()
                            .filter(|value| {
                                value.fract() == 0.0 && value.abs() <= 9_007_199_254_740_991.0
                            })
                            .map(|value| format!("{value:.0}"))
                    }),
                _ => None,
            }
            .ok_or("External parameter header value has an invalid type or unsafe integer")?;
            if crate::ai_external_transport::encoded_header(&value).len() > 8192 {
                return Err("External parameter header exceeds its byte limit".into());
            }
            values.push((header.name.clone(), value));
        }
        if values
            .iter()
            .map(|(name, value)| {
                name.len() + crate::ai_external_transport::encoded_header(value).len()
            })
            .sum::<usize>()
            > 16 * 1024
        {
            return Err("External parameter headers exceed their total byte limit".into());
        }
        Ok(values)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn offline_schema_refs_and_exact_nested_header_paths() {
        let schema = json!({"type":"object","properties":{
            "location":{"type":"object","properties":{"region":{"type":"string","x-mcp-header":"Region"}}},
            "count":{"type":"integer","x-mcp-header":"Count"},
            "enabled":{"type":"boolean","x-mcp-header":"Enabled"},
            "key":{"$ref":"#/$defs/key"}},"$defs":{"key":{"type":"string"}}});
        let compiled = ToolSchema::compile(&schema, true).unwrap();
        let mut headers = compiled
            .parameter_headers(
                &json!({"location":{"region":"世界"},"count":42,"enabled":true,"key":"value"}),
            )
            .unwrap();
        headers.sort();
        assert_eq!(
            headers,
            vec![
                ("Count".into(), "42".into()),
                ("Enabled".into(), "true".into()),
                ("Region".into(), "世界".into())
            ]
        );
        assert!(compiled
            .parameter_headers(&json!({"count":9_007_199_254_740_992_i64}))
            .is_err());
        assert!(compiled.parameter_headers(&json!({"count":"42"})).is_err());
        assert!(compiled.parameter_headers(&json!({})).unwrap().is_empty());
        assert!(
            ToolSchema::compile(&json!({"$ref":"https://example.invalid/schema"}), true).is_err()
        );
    }

    #[test]
    fn malformed_headers_and_unbounded_schemas_are_rejected() {
        for schema in [
            json!({"type":"string","x-mcp-header":"Root"}),
            json!({"properties":{"a":{"type":"number","x-mcp-header":"A"}}}),
            json!({"properties":{"a":{"type":"string","x-mcp-header":"bad\r\nheader"}}}),
            json!({"properties":{"a":{"type":"string","x-mcp-header":"A"},"b":{"type":"string","x-mcp-header":"a"}}}),
            json!({"allOf":[{"properties":{"a":{"type":"string","x-mcp-header":"A"}}}]}),
            json!({"properties":{"a":{"items":{"type":"string","x-mcp-header":"A"}}}}),
        ] {
            assert!(ToolSchema::compile(&schema, true).is_err());
        }
        let mut deep = json!({"type":"string"});
        for _ in 0..40 {
            deep = json!({"properties":{"nested":deep}});
        }
        assert!(ToolSchema::compile(&deep, true).is_err());
        assert!(ToolSchema::compile(&json!({"enum":vec![0;5000]}), true).is_err());
    }
}
