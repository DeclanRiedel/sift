//! Bounded data projection only. The server validates the complete input schema.
use serde_json::{Map, Value};

const MAX_FIELDS: usize = 16;
const MAX_OBJECT_DEPTH: usize = 4;
const MAX_SENSITIVITY_NODES: usize = 4096;
pub(crate) const MAX_FORM_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FieldKind {
    Text,
    Integer,
    Number,
    Boolean,
    Object,
    Array,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FormField {
    pub path: Vec<String>,
    pub label: String,
    pub kind: FieldKind,
    pub required: bool,
    pub masked: bool,
}
impl FormField {
    pub(crate) fn initial_text(&self) -> &'static str {
        if self.kind == FieldKind::Object {
            if self.required {
                "true"
            } else {
                "false"
            }
        } else {
            ""
        }
    }
}

pub(crate) fn form_fields(schema: &Value) -> Result<Vec<FormField>, String> {
    let mut fields = Vec::new();
    object_fields(schema, &[], false, &mut fields)?;
    Ok(fields)
}
fn object_fields(
    schema: &Value,
    path: &[String],
    inherited_mask: bool,
    fields: &mut Vec<FormField>,
) -> Result<(), String> {
    if path.len() > MAX_OBJECT_DEPTH {
        return Err("Form exceeds four nested object levels".into());
    }
    if schema.get("type").and_then(Value::as_str) != Some("object") {
        return Err("Form schema must declare an object".into());
    }
    let empty = Map::new();
    let properties = match schema.get("properties") {
        Some(Value::Object(value)) => value,
        None => &empty,
        _ => return Err("Form properties must be an object".into()),
    };
    let required = match schema.get("required") {
        Some(Value::Array(value)) => value.as_slice(),
        None => &[],
        _ => return Err("Form required names must be an array".into()),
    };
    if properties.len() > MAX_FIELDS || required.len() > properties.len() {
        return Err("Form exceeds the host field limit or requires unknown fields".into());
    }
    let mut seen = std::collections::HashSet::new();
    for name in required {
        let Some(name) = name.as_str() else {
            return Err("Form requires an invalid field".into());
        };
        if !properties.contains_key(name) || !seen.insert(name) {
            return Err("Form requires an unknown or duplicate field".into());
        }
    }
    let inherited_mask = inherited_mask || sensitive_hint(schema);
    for (key, definition) in properties {
        if key.is_empty() || key.len() > 128 || key.chars().any(char::is_control) {
            return Err("Form contains an invalid field name".into());
        }
        if fields.len() >= MAX_FIELDS {
            return Err("Form has too many controls for the host renderer".into());
        }
        let mut field_path = path.to_vec();
        field_path.push(key.clone());
        let kind = match definition.get("type").and_then(Value::as_str) {
            Some("string") => FieldKind::Text,
            Some("integer") => FieldKind::Integer,
            Some("number") => FieldKind::Number,
            Some("boolean") => FieldKind::Boolean,
            Some("object") => FieldKind::Object,
            Some("array") => FieldKind::Array,
            _ => return Err("Form property requires a supported explicit type".into()),
        };
        let title = definition
            .get("title")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(|s| s.chars().take(128).collect::<String>());
        let pointer = field_path
            .iter()
            .map(|key| key.replace('~', "~0").replace('/', "~1"))
            .collect::<Vec<_>>()
            .join("/");
        let label = if field_path.len() == 1 {
            title.unwrap_or_else(|| key.clone())
        } else {
            format!(
                "/{pointer}{}",
                title.map_or_else(String::new, |t| format!(" — {t}"))
            )
        };
        let label = label
            .chars()
            .map(|c| {
                if c.is_control() {
                    c.escape_default().to_string()
                } else {
                    c.to_string()
                }
            })
            .collect::<String>();
        let masked = inherited_mask
            || sensitive_hint(definition)
            || (kind == FieldKind::Array && aggregate_sensitive(definition));
        fields.push(FormField {
            path: field_path.clone(),
            label: match kind {
                FieldKind::Object => format!("{label} · include object (true/false)"),
                FieldKind::Array => format!("{label} · JSON array"),
                _ => label,
            },
            kind,
            required: seen.contains(key.as_str()),
            masked: masked && kind != FieldKind::Object,
        });
        if kind == FieldKind::Object {
            object_fields(definition, &field_path, masked, fields)?;
        }
    }
    Ok(())
}
fn sensitive_hint(schema: &Value) -> bool {
    direct_sensitive_hint(schema)
        || [
            "allOf",
            "anyOf",
            "oneOf",
            "if",
            "then",
            "else",
            "dependentSchemas",
        ]
        .iter()
        .filter_map(|key| schema.get(key))
        .any(aggregate_sensitive)
}
fn direct_sensitive_hint(schema: &Value) -> bool {
    schema.get("format").and_then(Value::as_str) == Some("password")
        || schema.get("writeOnly") == Some(&Value::Bool(true))
        || schema.get("$ref").is_some()
        || schema.get("$dynamicRef").is_some()
}
fn aggregate_sensitive(schema: &Value) -> bool {
    let mut pending = vec![schema];
    let mut visited = 0;
    while let Some(value) = pending.pop() {
        visited += 1;
        if visited > MAX_SENSITIVITY_NODES || direct_sensitive_hint(value) {
            return true;
        }
        let remaining =
            MAX_SENSITIVITY_NODES - visited - pending.len().min(MAX_SENSITIVITY_NODES - visited);
        match value {
            Value::Object(object) => {
                if object.len() > remaining {
                    return true;
                }
                pending.extend(object.values());
            }
            Value::Array(array) => {
                if array.len() > remaining {
                    return true;
                }
                pending.extend(array);
            }
            _ => {}
        }
    }
    false
}

pub(crate) fn form_arguments(fields: &[FormField], values: &[String]) -> Result<Value, String> {
    if fields.len() != values.len() {
        return Err("Form changed while editing".into());
    }
    let bytes = values
        .iter()
        .try_fold(0usize, |size, value| size.checked_add(value.len()))
        .ok_or("Form arguments exceed the host byte limit")?;
    if bytes > MAX_FORM_BYTES {
        return Err("Form arguments exceed 64 KiB".into());
    }
    let mut result = Value::Object(Map::new());
    let mut groups: Vec<(&[String], bool)> = Vec::new();
    for (field, raw) in fields.iter().zip(values) {
        let active = groups
            .iter()
            .all(|(path, included)| *included || !ancestor(path, &field.path));
        let value = raw.trim();
        if !active {
            let entered = if field.kind == FieldKind::Object {
                !field.required && !value.is_empty() && value != "false"
            } else if field.kind == FieldKind::Text {
                !raw.is_empty()
            } else {
                !value.is_empty()
            };
            if entered {
                return Err(format!(
                    "Enable the parent object before entering {}",
                    field.label
                ));
            }
            if field.kind == FieldKind::Object {
                groups.push((&field.path, false));
            }
            continue;
        }
        let parsed = match field.kind {
            FieldKind::Object => {
                let included = if value.is_empty() {
                    field.required
                } else {
                    value
                        .parse::<bool>()
                        .map_err(|_| format!("{} must be true or false", field.label))?
                };
                if field.required && !included {
                    return Err(format!("{} is required", field.label));
                }
                groups.push((&field.path, included));
                if !included {
                    continue;
                }
                Value::Object(Map::new())
            }
            FieldKind::Text => {
                if raw.is_empty() && !field.required {
                    continue;
                }
                Value::String(raw.clone())
            }
            _ => {
                if value.is_empty() {
                    if field.required {
                        return Err(format!("{} is required", field.label));
                    }
                    continue;
                }
                match field.kind {
                    FieldKind::Integer => value
                        .parse::<i64>()
                        .map(Value::from)
                        .or_else(|_| value.parse::<u64>().map(Value::from))
                        .map_err(|_| format!("{} must be a 64-bit integer", field.label))?,
                    FieldKind::Number => value
                        .parse::<f64>()
                        .ok()
                        .and_then(serde_json::Number::from_f64)
                        .map(Value::Number)
                        .ok_or_else(|| format!("{} must be a finite number", field.label))?,
                    FieldKind::Boolean => value
                        .parse::<bool>()
                        .map(Value::from)
                        .map_err(|_| format!("{} must be true or false", field.label))?,
                    FieldKind::Array => {
                        let parsed: Value = serde_json::from_str(value)
                            .map_err(|_| format!("{} must be a valid JSON array", field.label))?;
                        if !parsed.is_array() {
                            return Err(format!("{} must be a JSON array", field.label));
                        }
                        if !lossless_integer_tokens(value) {
                            return Err(format!(
                                "{} contains an integer outside the 64-bit JSON range",
                                field.label
                            ));
                        }
                        parsed
                    }
                    FieldKind::Text | FieldKind::Object => unreachable!(),
                }
            }
        };
        insert(&mut result, &field.path, parsed)?;
    }
    if serde_json::to_vec(&result)
        .map_err(|_| "Form arguments cannot be encoded")?
        .len()
        > MAX_FORM_BYTES
    {
        return Err("Encoded form arguments exceed 64 KiB".into());
    }
    Ok(result)
}
fn ancestor(parent: &[String], child: &[String]) -> bool {
    parent.len() < child.len() && child.starts_with(parent)
}
fn insert(result: &mut Value, path: &[String], value: Value) -> Result<(), String> {
    let (key, parents) = path.split_last().ok_or("Invalid form property path")?;
    let mut target = result;
    for parent in parents {
        target = target
            .get_mut(parent)
            .ok_or("Form parent object is unavailable")?;
    }
    target
        .as_object_mut()
        .ok_or("Form parent must be an object")?
        .insert(key.clone(), value);
    Ok(())
}
/// JSON is validated first. Inspect tokens outside strings before accepting an
/// integer that serde_json would otherwise round into its f64 representation.
fn lossless_integer_tokens(json: &str) -> bool {
    let bytes = json.as_bytes();
    let mut index = 0;
    let mut quoted = false;
    while index < bytes.len() {
        let byte = bytes[index];
        if quoted {
            match byte {
                b'\\' => index += 1,
                b'"' => quoted = false,
                _ => {}
            }
            index += 1;
            continue;
        }
        if byte == b'"' {
            quoted = true;
            index += 1;
            continue;
        }
        if byte == b'-' || byte.is_ascii_digit() {
            let start = index;
            index += 1;
            while index < bytes.len()
                && (bytes[index].is_ascii_digit()
                    || matches!(bytes[index], b'e' | b'E' | b'+' | b'-' | b'.'))
            {
                index += 1;
            }
            let token = &json[start..index];
            if !token.contains(['.', 'e', 'E'])
                && token.parse::<i64>().is_err()
                && token.parse::<u64>().is_err()
            {
                return false;
            }
        } else {
            index += 1;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn defaults(fields: &[FormField]) -> Vec<String> {
        fields
            .iter()
            .map(|field| field.initial_text().to_owned())
            .collect()
    }
    fn set(fields: &[FormField], values: &mut [String], path: &[&str], value: &str) {
        let index = fields
            .iter()
            .position(|field| {
                field
                    .path
                    .iter()
                    .map(String::as_str)
                    .eq(path.iter().copied())
            })
            .expect("field path");
        values[index] = value.into();
    }
    #[test]
    fn objects_preserve_presence_required_children_empty_strings_and_literal_keys() {
        let schema = json!({"type":"object","required":["required"],"properties":{
            "optional":{"type":"object","required":["count"],"properties":{"count":{"type":"integer"},"empty":{"type":"object"}}},
            "required":{"type":"object","required":["name"],"properties":{"name":{"type":"string"}}},
            "a/b":{"type":"object","properties":{"~key.dot":{"type":"string"}}}
        }});
        let fields = form_fields(&schema).unwrap();
        let mut values = defaults(&fields);
        assert_eq!(
            form_arguments(&fields, &values).unwrap(),
            json!({"required":{"name":""}})
        );
        set(&fields, &mut values, &["optional", "count"], "3");
        assert!(form_arguments(&fields, &values)
            .unwrap_err()
            .starts_with("Enable the parent object"));
        set(&fields, &mut values, &["optional"], "true");
        set(&fields, &mut values, &["optional", "empty"], "true");
        set(&fields, &mut values, &["a/b"], "true");
        set(&fields, &mut values, &["a/b", "~key.dot"], "  value  ");
        assert_eq!(
            form_arguments(&fields, &values).unwrap(),
            json!({"required":{"name":""},"optional":{"count":3,"empty":{}},"a/b":{"~key.dot":"  value  "}})
        );
        assert!(fields
            .iter()
            .any(|field| field.label.starts_with("/a~1b/~0key.dot")));
        set(&fields, &mut values, &["required"], "false");
        assert!(form_arguments(&fields, &values)
            .unwrap_err()
            .ends_with("is required"));
    }
    #[test]
    fn absent_ancestors_do_not_require_children_or_silently_drop_entered_groups() {
        let fields=form_fields(&json!({"type":"object","properties":{"group":{"type":"object","required":["child"],"properties":{"child":{"type":"object","required":["n"],"properties":{"n":{"type":"number"}}},"extra":{"type":"object"}}}}})).unwrap();
        let mut values = defaults(&fields);
        assert_eq!(form_arguments(&fields, &values).unwrap(), json!({}));
        set(&fields, &mut values, &["group", "extra"], "true");
        assert!(form_arguments(&fields, &values).is_err());
        set(&fields, &mut values, &["group", "extra"], "false");
        set(&fields, &mut values, &["group"], "true");
        assert!(form_arguments(&fields, &values).is_err());
        set(&fields, &mut values, &["group", "child", "n"], "1.25");
        assert_eq!(
            form_arguments(&fields, &values).unwrap(),
            json!({"group":{"child":{"n":1.25}}})
        );
    }
    #[test]
    fn arrays_keep_exact_integer_tokens_and_errors_never_echo_values() {
        let fields=form_fields(&json!({"type":"object","required":["rows"],"properties":{"rows":{"type":"array","items":{"type":"object","properties":{"password":{"type":"string","format":"password"}}}}}})).unwrap();
        assert!(fields[0].masked);
        let raw = r#"[18446744073709551615,-9223372036854775808,1.5e20,{"password":"private-sentinel","text":"184467440737095516160"}]"#;
        let args = form_arguments(&fields, &[raw.into()]).unwrap();
        assert_eq!(args["rows"][0].as_u64(), Some(u64::MAX));
        assert_eq!(args["rows"][1].as_i64(), Some(i64::MIN));
        for raw in [
            r#"[18446744073709551616]"#,
            r#"[-9223372036854775809]"#,
            r#"[1e999]"#,
            r#"{"password":"private-sentinel"}"#,
            r#"["private-sentinel",]"#,
        ] {
            let error = form_arguments(&fields, &[raw.into()]).unwrap_err();
            assert!(!error.contains("private-sentinel"));
        }
        assert_eq!(
            form_arguments(&fields, &["[]".into()]).unwrap(),
            json!({"rows":[]})
        );
    }
    #[test]
    fn secret_annotations_propagate_and_unknown_aggregate_schemas_mask_conservatively() {
        let fields=form_fields(&json!({"type":"object","properties":{
            "credentials":{"type":"object","writeOnly":true,"properties":{"user":{"type":"string"},"nested":{"type":"object","properties":{"password":{"type":"string"}}}}},
            "plain":{"type":"array","items":{"type":"integer"}},
            "reference":{"type":"array","items":{"$ref":"#/$defs/private"}}
        }})).unwrap();
        for field in &fields {
            if field.path[0] == "credentials" {
                assert_eq!(field.masked, field.kind != FieldKind::Object);
            }
            if field.path[0] == "plain" {
                assert!(!field.masked);
            }
            if field.path[0] == "reference" {
                assert!(field.masked);
            }
        }
        let composed=form_fields(&json!({"type":"object","allOf":[{"properties":{"token":{"writeOnly":true}}}],"properties":{"token":{"type":"string"}}})).unwrap();
        assert!(composed[0].masked);
        let composed=form_fields(&json!({"type":"object","properties":{"token":{"type":"string","allOf":[{"writeOnly":true}]}}})).unwrap();
        assert!(composed[0].masked);
        let wide = json!({"type":"array","enum":vec![Value::Null;5000]});
        assert!(aggregate_sensitive(&wide));
    }
    #[test]
    fn form_limits_refuse_depth_controls_raw_and_encoded_bytes() {
        let mut nested = json!({"type":"object"});
        for _ in 0..5 {
            nested = json!({"type":"object","properties":{"nested":nested}});
        }
        assert!(form_fields(&nested).is_err());
        let props: Map<_, _> = (0..17)
            .map(|i| (format!("f{i}"), json!({"type":"string"})))
            .collect();
        assert!(form_fields(&json!({"type":"object","properties":props})).is_err());
        let fields = form_fields(
            &json!({"type":"object","required":["value"],"properties":{"value":{"type":"string"}}}),
        )
        .unwrap();
        assert!(form_arguments(&fields, &["x".repeat(MAX_FORM_BYTES + 1)]).is_err());
        assert!(form_arguments(&fields, &["\0".repeat(MAX_FORM_BYTES / 2)]).is_err());
        assert!(form_fields(&json!({"type":"object","required":["missing"]})).is_err());
        assert!(form_fields(
            &json!({"type":"object","properties":{"unsupported":{"type":["string","null"]}}})
        )
        .is_err());
    }
}
