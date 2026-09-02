//! Bounded local SQL snippet indexing and tabstop expansion.

use std::collections::HashMap;
use std::ops::Range;

use sift_protocol::{DialectId, SnippetScope, SqlSnippet};

pub const MAX_SNIPPETS: usize = 2_000;
pub const MAX_SNIPPET_BODY_BYTES: usize = 64 * 1024;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum SnippetError {
    #[error("snippet collection exceeds {MAX_SNIPPETS} entries")]
    TooMany,
    #[error("snippet `{0}` body exceeds {MAX_SNIPPET_BODY_BYTES} bytes")]
    BodyTooLarge(String),
    #[error("snippet `{0}` has an invalid trigger")]
    InvalidTrigger(String),
    #[error("snippet contains an unterminated tabstop")]
    UnterminatedTabstop,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tabstop {
    pub number: u32,
    pub range: Range<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Expansion {
    pub text: String,
    pub tabstops: Vec<Tabstop>,
}

#[derive(Debug, Clone, Default)]
pub struct SnippetIndex {
    by_trigger: HashMap<String, Vec<SqlSnippet>>,
}

impl SnippetIndex {
    pub fn build(snippets: impl IntoIterator<Item = SqlSnippet>) -> Result<Self, SnippetError> {
        let mut by_trigger: HashMap<String, Vec<SqlSnippet>> = HashMap::new();
        let mut count = 0;
        for snippet in snippets {
            count += 1;
            if count > MAX_SNIPPETS {
                return Err(SnippetError::TooMany);
            }
            validate(&snippet)?;
            by_trigger
                .entry(snippet.trigger.to_ascii_lowercase())
                .or_default()
                .push(snippet);
        }
        for snippets in by_trigger.values_mut() {
            snippets.sort_by_key(|snippet| std::cmp::Reverse(scope_rank(snippet.scope)));
        }
        Ok(Self { by_trigger })
    }

    pub fn matching(&self, prefix: &str, dialect: &DialectId, limit: usize) -> Vec<&SqlSnippet> {
        let prefix = prefix.to_ascii_lowercase();
        let mut matches = self
            .by_trigger
            .iter()
            .filter(|(trigger, _)| trigger.starts_with(&prefix))
            .flat_map(|(_, snippets)| snippets)
            .filter(|snippet| snippet.dialects.is_empty() || snippet.dialects.contains(dialect))
            .collect::<Vec<_>>();
        matches.sort_by(|left, right| {
            left.trigger
                .len()
                .cmp(&right.trigger.len())
                .then_with(|| right.revision.cmp(&left.revision))
                .then_with(|| left.trigger.cmp(&right.trigger))
        });
        matches.truncate(limit);
        matches
    }
}

pub fn validate(snippet: &SqlSnippet) -> Result<(), SnippetError> {
    if snippet.body.len() > MAX_SNIPPET_BODY_BYTES {
        return Err(SnippetError::BodyTooLarge(snippet.trigger.clone()));
    }
    if snippet.trigger.is_empty()
        || snippet.trigger.len() > 64
        || !snippet
            .trigger
            .bytes()
            .all(|byte| byte == b'_' || byte == b'-' || byte.is_ascii_alphanumeric())
    {
        return Err(SnippetError::InvalidTrigger(snippet.trigger.clone()));
    }
    expand(&snippet.body).map(|_| ())
}

pub fn expand(body: &str) -> Result<Expansion, SnippetError> {
    let mut text = String::with_capacity(body.len());
    let mut tabstops = Vec::new();
    let bytes = body.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'$' {
            let width = body[index..].chars().next().map_or(1, char::len_utf8);
            text.push_str(&body[index..index + width]);
            index += width;
            continue;
        }
        if bytes.get(index + 1) == Some(&b'{') {
            let Some(close) = body[index + 2..].find('}') else {
                return Err(SnippetError::UnterminatedTabstop);
            };
            let end = index + close + 2;
            let contents = &body[index + 2..end];
            let (number, default) = contents
                .split_once(':')
                .map_or((contents, ""), |(number, default)| (number, default));
            if let Ok(number) = number.parse::<u32>() {
                let start = text.len();
                text.push_str(default);
                tabstops.push(Tabstop {
                    number,
                    range: start..text.len(),
                });
                index = end + 1;
                continue;
            }
        } else {
            let digits = &body[index + 1..];
            let len = digits.bytes().take_while(u8::is_ascii_digit).count();
            if len > 0 {
                let number = digits[..len].parse().unwrap_or(0);
                tabstops.push(Tabstop {
                    number,
                    range: text.len()..text.len(),
                });
                index += len + 1;
                continue;
            }
        }
        text.push('$');
        index += 1;
    }
    tabstops.sort_by_key(|tabstop| (tabstop.number == 0, tabstop.number));
    Ok(Expansion { text, tabstops })
}

pub fn builtins() -> Vec<SqlSnippet> {
    let pg = DialectId::new("sift/postgresql").expect("valid dialect");
    let tsql = DialectId::new("sift/tsql").expect("valid dialect");
    vec![
        builtin(
            "sel",
            "Select rows",
            "SELECT ${1:*}\nFROM ${2:table}\nWHERE ${3:condition};$0",
            vec![pg.clone(), tsql.clone()],
        ),
        builtin(
            "ins",
            "Insert row",
            "INSERT INTO ${1:table} (${2:columns})\nVALUES (${3:values});$0",
            vec![pg.clone(), tsql.clone()],
        ),
        builtin(
            "upd",
            "Update rows",
            "UPDATE ${1:table}\nSET ${2:column} = ${3:value}\nWHERE ${4:condition};$0",
            vec![pg.clone(), tsql.clone()],
        ),
        builtin(
            "cte",
            "Common table expression",
            "WITH ${1:name} AS (\n  ${2:SELECT 1}\n)\nSELECT * FROM ${1:name};$0",
            vec![pg, tsql],
        ),
    ]
}

fn builtin(trigger: &str, title: &str, body: &str, dialects: Vec<DialectId>) -> SqlSnippet {
    SqlSnippet {
        id: None,
        tenant_id: None,
        workspace_id: None,
        owner_principal_id: None,
        trigger: trigger.into(),
        title: title.into(),
        description: "Built-in SQL template".into(),
        body: body.into(),
        dialects,
        scope: SnippetScope::BuiltIn,
        revision: 1,
    }
}

const fn scope_rank(scope: SnippetScope) -> u8 {
    match scope {
        SnippetScope::BuiltIn => 0,
        SnippetScope::Tenant => 1,
        SnippetScope::Workspace => 2,
        SnippetScope::Personal => 3,
        SnippetScope::Catalog => 4,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expands_ordered_tabstops_without_leaving_markers() {
        let expansion = expand("SELECT ${1:*} FROM ${2:users} WHERE id = $3;$0").unwrap();
        assert_eq!(expansion.text, "SELECT * FROM users WHERE id = ;");
        assert_eq!(
            expansion
                .tabstops
                .iter()
                .map(|stop| stop.number)
                .collect::<Vec<_>>(),
            vec![1, 2, 3, 0]
        );
    }

    #[test]
    fn index_filters_dialects_and_bounds_imports() {
        let index = SnippetIndex::build(builtins()).unwrap();
        let pg = DialectId::new("sift/postgresql").unwrap();
        assert_eq!(index.matching("se", &pg, 10)[0].trigger, "sel");
        let template = builtins().remove(0);
        let oversized = (0..=MAX_SNIPPETS).map(|index| SqlSnippet {
            trigger: format!("s{index}"),
            ..template.clone()
        });
        assert!(matches!(
            SnippetIndex::build(oversized),
            Err(SnippetError::TooMany)
        ));
    }

    #[test]
    fn malformed_and_oversized_snippets_fail_closed() {
        assert_eq!(expand("${1:oops"), Err(SnippetError::UnterminatedTabstop));
        let mut snippet = builtins().remove(0);
        snippet.body = "x".repeat(MAX_SNIPPET_BODY_BYTES + 1);
        assert!(matches!(
            validate(&snippet),
            Err(SnippetError::BodyTooLarge(_))
        ));
    }
}
