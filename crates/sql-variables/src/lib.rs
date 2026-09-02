//! Injection-safe compilation of Sift SQL templates into native binds.

use std::collections::{HashMap, HashSet};

use sift_protocol::{
    CompiledSqlVariables, Engine, RedactedSqlVariableDescriptor, SqlVariableBinding,
    SqlVariableKind, SqlVariableReference, SqlVariableSourceMapEntry, SqlVariableValue,
};

pub const MAX_VARIABLE_REFERENCES: usize = 100;
pub const MAX_LIST_VALUES: usize = 1_000;

/// Resolve duplicate names according to the documented scope precedence.
/// Callers can provide scopes in any order; returned bindings are stable by
/// name and contain no copied secret bytes (only opaque handles).
pub fn resolve_scopes(
    bindings: impl IntoIterator<Item = SqlVariableBinding>,
) -> Vec<SqlVariableBinding> {
    let mut resolved = std::collections::BTreeMap::<String, SqlVariableBinding>::new();
    for binding in bindings {
        match resolved.get(&binding.name) {
            Some(current) if current.scope >= binding.scope => {}
            _ => {
                resolved.insert(binding.name.clone(), binding);
            }
        }
    }
    resolved.into_values().collect()
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum CompileError {
    #[error("SQL template contains more than {MAX_VARIABLE_REFERENCES} variable references")]
    TooManyReferences,
    #[error("variable `{0}` is missing")]
    Missing(String),
    #[error("variable `{name}` expects {expected}")]
    WrongKind {
        name: String,
        expected: &'static str,
    },
    #[error("identifier variable `{0}` is not a valid qualified identifier")]
    InvalidIdentifier(String),
    #[error("list variable `{0}` cannot be empty")]
    EmptyList(String),
    #[error("list variable `{0}` exceeds the {MAX_LIST_VALUES} value limit")]
    ListTooLarge(String),
    #[error("compiled query exceeds the {0} bind-parameter limit")]
    TooManyBinds(usize),
    #[error("secret variable `{0}` must be resolved by the server secret store")]
    UnresolvedSecret(String),
    #[error("variable `{0}` has conflicting template kinds")]
    ConflictingKinds(String),
}

pub fn references(template: &str) -> Result<Vec<SqlVariableReference>, CompileError> {
    let mut found = Vec::new();
    scan_template(template, |start, end, body| {
        let (kind, name) = if let Some(name) = body.strip_prefix("ident:") {
            (SqlVariableKind::Identifier, name)
        } else if let Some(name) = body.strip_prefix("list:") {
            (SqlVariableKind::List, name)
        } else {
            (SqlVariableKind::Value, body)
        };
        if valid_name(name) {
            found.push(SqlVariableReference {
                name: name.to_owned(),
                kind,
                template_start: start as u32,
                template_end: end as u32,
            });
        }
    });
    if found.len() > MAX_VARIABLE_REFERENCES {
        return Err(CompileError::TooManyReferences);
    }
    let mut kinds = HashMap::new();
    for reference in &found {
        if kinds
            .insert(reference.name.as_str(), reference.kind)
            .is_some_and(|kind| kind != reference.kind)
        {
            return Err(CompileError::ConflictingKinds(reference.name.clone()));
        }
    }
    Ok(found)
}

pub fn compile(
    engine: Engine,
    template: &str,
    bindings: &[SqlVariableBinding],
) -> Result<CompiledSqlVariables, CompileError> {
    let references = references(template)?;
    let by_name = bindings
        .iter()
        .map(|binding| (binding.name.as_str(), binding))
        .collect::<HashMap<_, _>>();
    let mut sql = String::with_capacity(template.len());
    let mut params = Vec::new();
    let mut source_map = Vec::with_capacity(references.len());
    let mut descriptors = Vec::new();
    let mut described = HashSet::new();
    let mut template_cursor = 0;
    let native_offset = native_parameter_offset(engine, template);
    let bind_limit = match engine {
        Engine::Postgres => 65_535,
        Engine::SqlServer => 2_100,
    };

    for reference in references {
        let start = reference.template_start as usize;
        let end = reference.template_end as usize;
        sql.push_str(&template[template_cursor..start]);
        let compiled_start = sql.len();
        let binding = by_name
            .get(reference.name.as_str())
            .ok_or_else(|| CompileError::Missing(reference.name.clone()))?;
        let bind_count = match (&reference.kind, &binding.value) {
            (SqlVariableKind::Value, SqlVariableValue::Scalar(value)) => {
                params.push(value.clone());
                sql.push_str(&placeholder(engine, native_offset + params.len()));
                1
            }
            (SqlVariableKind::Identifier, SqlVariableValue::Identifier(identifier)) => {
                sql.push_str(
                    &quote_qualified_identifier(engine, identifier)
                        .ok_or_else(|| CompileError::InvalidIdentifier(reference.name.clone()))?,
                );
                0
            }
            (SqlVariableKind::List, SqlVariableValue::List(values)) => {
                if values.is_empty() {
                    return Err(CompileError::EmptyList(reference.name));
                }
                if values.len() > MAX_LIST_VALUES {
                    return Err(CompileError::ListTooLarge(reference.name));
                }
                for (index, value) in values.iter().enumerate() {
                    if index > 0 {
                        sql.push_str(", ");
                    }
                    params.push(value.clone());
                    sql.push_str(&placeholder(engine, native_offset + params.len()));
                }
                values.len()
            }
            (_, SqlVariableValue::SecretHandle(_)) => {
                return Err(CompileError::UnresolvedSecret(reference.name));
            }
            (SqlVariableKind::Value, _) => {
                return Err(CompileError::WrongKind {
                    name: reference.name,
                    expected: "a scalar value",
                });
            }
            (SqlVariableKind::Identifier, _) => {
                return Err(CompileError::WrongKind {
                    name: reference.name,
                    expected: "an identifier",
                });
            }
            (SqlVariableKind::List, _) => {
                return Err(CompileError::WrongKind {
                    name: reference.name,
                    expected: "a JSON array",
                });
            }
        };
        if native_offset + params.len() > bind_limit {
            return Err(CompileError::TooManyBinds(bind_limit));
        }
        let compiled_end = sql.len();
        source_map.push(SqlVariableSourceMapEntry {
            name: reference.name.clone(),
            template_start: reference.template_start,
            template_end: reference.template_end,
            compiled_start: compiled_start as u32,
            compiled_end: compiled_end as u32,
        });
        if described.insert(reference.name.clone()) {
            descriptors.push(RedactedSqlVariableDescriptor {
                name: reference.name,
                kind: reference.kind,
                scope: binding.scope,
                secret: matches!(binding.value, SqlVariableValue::SecretHandle(_)),
                bind_count: bind_count as u32,
            });
        }
        template_cursor = end;
    }
    sql.push_str(&template[template_cursor..]);
    Ok(CompiledSqlVariables {
        sql,
        params,
        source_map,
        descriptors,
    })
}

fn valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|ch| ch == '_' || ch.is_alphabetic())
        && chars.all(|ch| ch == '_' || ch.is_alphanumeric())
}

fn placeholder(engine: Engine, number: usize) -> String {
    match engine {
        Engine::Postgres => format!("${number}"),
        Engine::SqlServer => format!("@P{number}"),
    }
}

fn quote_qualified_identifier(engine: Engine, identifier: &str) -> Option<String> {
    if identifier.is_empty() || identifier.chars().any(|ch| ch.is_control()) {
        return None;
    }
    identifier
        .split('.')
        .map(|part| {
            if part.is_empty() {
                return None;
            }
            Some(match engine {
                Engine::Postgres => format!("\"{}\"", part.replace('"', "\"\"")),
                Engine::SqlServer => format!("[{}]", part.replace(']', "]]")),
            })
        })
        .collect::<Option<Vec<_>>>()
        .map(|parts| parts.join("."))
}

fn native_parameter_offset(engine: Engine, sql: &str) -> usize {
    let bytes = sql.as_bytes();
    let mut offset = 0;
    for index in 0..bytes.len() {
        let prefix = match engine {
            Engine::Postgres if bytes[index] == b'$' => 1,
            Engine::SqlServer
                if bytes[index] == b'@'
                    && bytes
                        .get(index + 1)
                        .is_some_and(|byte| matches!(byte, b'p' | b'P')) =>
            {
                2
            }
            _ => continue,
        };
        let digits = &sql[index + prefix..];
        let len = digits.bytes().take_while(u8::is_ascii_digit).count();
        if len > 0 {
            offset = offset.max(digits[..len].parse().unwrap_or(0));
        }
    }
    offset
}

#[derive(Clone, Copy)]
enum ScanState {
    Sql,
    SingleQuote,
    DoubleQuote,
    BracketIdentifier,
    LineComment,
    BlockComment,
}

fn scan_template(sql: &str, mut found: impl FnMut(usize, usize, &str)) {
    let bytes = sql.as_bytes();
    let mut index = 0;
    let mut state = ScanState::Sql;
    while index < bytes.len() {
        match state {
            ScanState::Sql if bytes[index..].starts_with(b"--") => {
                state = ScanState::LineComment;
                index += 2;
                continue;
            }
            ScanState::Sql if bytes[index..].starts_with(b"/*") => {
                state = ScanState::BlockComment;
                index += 2;
                continue;
            }
            ScanState::Sql if bytes[index] == b'\'' => state = ScanState::SingleQuote,
            ScanState::Sql if bytes[index] == b'"' => state = ScanState::DoubleQuote,
            ScanState::Sql if bytes[index] == b'[' => state = ScanState::BracketIdentifier,
            ScanState::Sql if bytes[index] == b'$' => {
                if let Some(delimiter_end) = dollar_quote_delimiter(bytes, index) {
                    let delimiter = &sql[index..delimiter_end];
                    index = sql[delimiter_end..]
                        .find(delimiter)
                        .map_or(bytes.len(), |offset| {
                            delimiter_end + offset + delimiter.len()
                        });
                    continue;
                }
            }
            ScanState::Sql if bytes[index..].starts_with(b"{{") => {
                if let Some(close) = sql[index + 2..].find("}}") {
                    let end = close + 4;
                    found(index, index + end, &sql[index + 2..index + close + 2]);
                    index += end;
                    continue;
                }
            }
            ScanState::SingleQuote if bytes[index] == b'\'' => {
                if bytes.get(index + 1) == Some(&b'\'') {
                    index += 2;
                    continue;
                } else {
                    state = ScanState::Sql;
                }
            }
            ScanState::DoubleQuote if bytes[index] == b'"' => {
                if bytes.get(index + 1) == Some(&b'"') {
                    index += 2;
                    continue;
                } else {
                    state = ScanState::Sql;
                }
            }
            ScanState::BracketIdentifier if bytes[index] == b']' => {
                if bytes.get(index + 1) == Some(&b']') {
                    index += 2;
                    continue;
                } else {
                    state = ScanState::Sql;
                }
            }
            ScanState::LineComment if bytes[index] == b'\n' => state = ScanState::Sql,
            ScanState::BlockComment if bytes[index..].starts_with(b"*/") => {
                state = ScanState::Sql;
                index += 2;
                continue;
            }
            _ => {}
        }
        index += sql[index..].chars().next().map_or(1, char::len_utf8);
    }
}

fn dollar_quote_delimiter(bytes: &[u8], start: usize) -> Option<usize> {
    let first = *bytes.get(start + 1)?;
    if first == b'$' {
        return Some(start + 2);
    }
    if !(first.is_ascii_alphabetic() || first == b'_') {
        return None;
    }
    let mut index = start + 2;
    while bytes
        .get(index)
        .is_some_and(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
    {
        index += 1;
    }
    (bytes.get(index) == Some(&b'$')).then_some(index + 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sift_protocol::{SqlVariableScope, SqlVariableValue, Value};

    fn binding(name: &str, value: SqlVariableValue) -> SqlVariableBinding {
        SqlVariableBinding {
            name: name.into(),
            value,
            scope: SqlVariableScope::RunPrompt,
        }
    }

    #[test]
    fn postgres_compiles_values_lists_and_quoted_identifiers() {
        let compiled = compile(
            Engine::Postgres,
            "select * from {{ident:table}} where tenant = $1 and id in ({{list:ids}}) and state = {{state}}",
            &[
                binding("table", SqlVariableValue::Identifier("sales.Order".into())),
                binding("ids", SqlVariableValue::List(vec![Value::Int64(4), Value::Int64(9)])),
                binding("state", SqlVariableValue::Scalar(Value::Text("ready".into()))),
            ],
        )
        .unwrap();
        assert_eq!(
            compiled.sql,
            "select * from \"sales\".\"Order\" where tenant = $1 and id in ($2, $3) and state = $4"
        );
        assert_eq!(compiled.params.len(), 3);
        assert_eq!(compiled.source_map.len(), 3);
    }

    #[test]
    fn sql_server_quotes_and_preserves_typed_nulls() {
        let compiled = compile(
            Engine::SqlServer,
            "select * from {{ident:table}} where id = {{id}}",
            &[
                binding(
                    "table",
                    SqlVariableValue::Identifier("dbo.weird]name".into()),
                ),
                binding(
                    "id",
                    SqlVariableValue::Scalar(Value::TypedNull {
                        type_name: "uniqueidentifier".into(),
                    }),
                ),
            ],
        )
        .unwrap();
        assert_eq!(
            compiled.sql,
            "select * from [dbo].[weird]]name] where id = @P1"
        );
        assert!(matches!(compiled.params[0], Value::TypedNull { .. }));
    }

    #[test]
    fn ignores_literals_comments_and_identifiers() {
        let refs =
            references("select '{{x}}', \"{{y}}\", [{{z}}] -- {{a}}\n/* {{b}} */ {{ok}}").unwrap();
        assert_eq!(
            refs.iter()
                .map(|item| item.name.as_str())
                .collect::<Vec<_>>(),
            vec!["ok"]
        );
    }

    #[test]
    fn scans_unicode_and_ignores_postgres_dollar_quotes() {
        let refs = references("select 'é', $$ {{hidden}} $$, {{café}}").unwrap();
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].name, "café");
    }

    #[test]
    fn rejects_empty_and_oversized_lists_and_raw_kind_conflicts() {
        assert_eq!(
            compile(
                Engine::Postgres,
                "select {{list:x}}",
                &[binding("x", SqlVariableValue::List(vec![]))]
            ),
            Err(CompileError::EmptyList("x".into()))
        );
        assert_eq!(
            references("select {{x}}, {{ident:x}}"),
            Err(CompileError::ConflictingKinds("x".into()))
        );
    }

    #[test]
    fn malicious_values_never_enter_sql() {
        let attack = "x'); drop table users; --";
        let compiled = compile(
            Engine::Postgres,
            "select {{value}}",
            &[binding(
                "value",
                SqlVariableValue::Scalar(Value::Text(attack.into())),
            )],
        )
        .unwrap();
        assert_eq!(compiled.sql, "select $1");
        assert_eq!(compiled.params, vec![Value::Text(attack.into())]);
    }

    #[test]
    fn scope_precedence_is_order_independent() {
        let low = SqlVariableBinding {
            name: "region".into(),
            value: SqlVariableValue::Scalar(Value::Text("tenant".into())),
            scope: SqlVariableScope::TenantDefault,
        };
        let high = SqlVariableBinding {
            name: "region".into(),
            value: SqlVariableValue::Scalar(Value::Text("prompt".into())),
            scope: SqlVariableScope::RunPrompt,
        };
        assert_eq!(resolve_scopes([high.clone(), low])[0], high);
    }

    #[test]
    fn secret_handle_never_appears_in_compiler_errors() {
        let handle = "vault://opaque/sentinel-DO-NOT-LEAK";
        let error = compile(
            Engine::Postgres,
            "select {{token}}",
            &[binding(
                "token",
                SqlVariableValue::SecretHandle(handle.into()),
            )],
        )
        .unwrap_err()
        .to_string();
        assert!(!error.contains(handle));
        assert!(error.contains("token"));
    }

    #[test]
    fn sql_server_total_bind_limit_is_enforced() {
        let values = vec![Value::Int64(1); MAX_LIST_VALUES];
        let error = compile(
            Engine::SqlServer,
            "select 1 where a in ({{list:x}}) or b in ({{list:x}}) or c in ({{list:x}})",
            &[binding("x", SqlVariableValue::List(values))],
        )
        .unwrap_err();
        assert_eq!(error, CompileError::TooManyBinds(2_100));
    }
}
