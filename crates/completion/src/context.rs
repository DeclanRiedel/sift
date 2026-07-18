//! SQL context detection.
//!
//! Given `sql` and a byte offset `cursor`, decide what kind of
//! candidates should show up at that position. We use `sqlparser-rs`'s
//! tokenizer for robust lexing (comments, escapes, quoted idents per
//! dialect) but walk the token stream by hand — full parsing is
//! intolerant of the mid-edit inputs the endpoint is called with.

use sift_protocol::completion::CompletionContext;
use sift_protocol::Engine;
use sqlparser::dialect::{Dialect, MsSqlDialect, PostgreSqlDialect};
use sqlparser::tokenizer::{Token, Tokenizer, Whitespace, Word};
use std::collections::{HashMap, VecDeque};
use std::sync::{Mutex, OnceLock};

const TOKEN_CACHE_MAX_ENTRIES: usize = 128;

static TOKEN_CACHE: OnceLock<Mutex<TokenCache>> = OnceLock::new();

#[derive(Hash, PartialEq, Eq, Clone)]
struct TokenCacheKey {
    engine: Engine,
    preceding: String,
}

#[derive(Default)]
struct TokenCache {
    entries: HashMap<TokenCacheKey, Vec<Token>>,
    order: VecDeque<TokenCacheKey>,
}

/// Output of context detection: the classified context plus the byte
/// range of the partial identifier at the cursor that a client should
/// replace on accept.
pub struct ContextResult {
    pub context: CompletionContext,
    /// Sanitized byte offset used for detection. This is clamped to `sql.len()`
    /// and floored to a UTF-8 char boundary.
    pub cursor: usize,
    /// Byte offset where the partial identifier starts. `cursor` marks
    /// its end.
    pub prefix_start: usize,
    /// The partial identifier text (empty if cursor is at whitespace).
    pub prefix: String,
    /// Case-folded (lower) version of `prefix` for prefix matching.
    pub prefix_lower: String,
}

pub fn detect_context(sql: &str, cursor: usize, engine: Engine) -> ContextResult {
    let cursor = floor_char_boundary(sql, usize::min(cursor, sql.len()));
    let (prefix_start, prefix) = extract_prefix(sql, cursor);
    let prefix_lower = prefix.to_ascii_lowercase();

    // Tokenize everything up to the prefix. The prefix itself may be an
    // incomplete identifier that would confuse token classification.
    let preceding = &sql[..prefix_start];
    let tokens = tokenize_preceding(preceding, engine);

    let context = classify(&tokens, &prefix);

    ContextResult {
        context,
        cursor,
        prefix_start,
        prefix,
        prefix_lower,
    }
}

fn tokenize_preceding(preceding: &str, engine: Engine) -> Vec<Token> {
    let key = TokenCacheKey {
        engine,
        preceding: preceding.to_string(),
    };
    let cache = TOKEN_CACHE.get_or_init(|| Mutex::new(TokenCache::default()));
    if let Some(tokens) = cache.lock().unwrap().entries.get(&key).cloned() {
        return tokens;
    }

    let dialect: Box<dyn Dialect> = match engine {
        Engine::Postgres => Box::new(PostgreSqlDialect {}),
        Engine::SqlServer => Box::new(MsSqlDialect {}),
    };
    let tokens: Vec<Token> = Tokenizer::new(&*dialect, preceding)
        .tokenize()
        .unwrap_or_default()
        .into_iter()
        .filter(|t| !is_ignorable(t))
        .collect();

    let mut cache = cache.lock().unwrap();
    cache.entries.insert(key.clone(), tokens.clone());
    cache.order.push_back(key);
    while cache.order.len() > TOKEN_CACHE_MAX_ENTRIES {
        if let Some(oldest) = cache.order.pop_front() {
            cache.entries.remove(&oldest);
        }
    }
    tokens
}

fn is_ignorable(t: &Token) -> bool {
    matches!(
        t,
        Token::Whitespace(Whitespace::Space)
            | Token::Whitespace(Whitespace::Tab)
            | Token::Whitespace(Whitespace::Newline)
            | Token::Whitespace(Whitespace::SingleLineComment { .. })
            | Token::Whitespace(Whitespace::MultiLineComment(_))
    )
}

/// The heart of context detection. `tokens` is everything before the
/// partial identifier; `prefix` is the partial identifier itself.
fn classify(tokens: &[Token], _prefix: &str) -> CompletionContext {
    // Rule 1: qualified reference. The token immediately preceding the
    // prefix is a Period, and before that is an identifier.
    if let Some(Token::Period) = tokens.last() {
        let qualifier = tokens
            .get(tokens.len().wrapping_sub(2))
            .and_then(word_value);
        // Distinguish two dotted contexts:
        //   `SELECT u.| FROM users u`   → columns of alias `u`
        //   `SELECT * FROM public.|`    → objects in schema `public`
        // Heuristic: if the two tokens before the period are a
        // FROM/JOIN/INTO/UPDATE/TABLE clause lead, treat the qualifier
        // as a schema. Otherwise treat it as a table/alias qualifier.
        if let Some(q) = qualifier {
            let before_qualifier = tokens
                .get(tokens.len().wrapping_sub(3))
                .and_then(word_value)
                .unwrap_or_default();
            if is_table_slot_lead(before_qualifier) {
                return CompletionContext::ExpectingObjectInSchema {
                    schema: q.to_string(),
                };
            }
            return CompletionContext::ExpectingColumn {
                qualifier: Some(q.to_string()),
            };
        }
    }

    // Rule 2: last non-comma keyword decides the slot. Walk backwards
    // until we hit a keyword or run out of tokens.
    let mut i = tokens.len();
    while i > 0 {
        i -= 1;
        let Some(kw) = word_value(&tokens[i]) else {
            continue;
        };
        // Commas keep us in the same clause; skip past them.
        // (Commas are their own token — Token::Comma — not caught by
        // word_value, so they simply don't match here and we keep
        // walking.)
        if is_table_slot_lead(kw) {
            return CompletionContext::ExpectingTable;
        }
        if matches_case_insensitive(kw, &["SELECT", "WHERE", "SET", "BY", "ON", "HAVING"]) {
            return CompletionContext::ExpectingColumn { qualifier: None };
        }
        if matches_case_insensitive(kw, &["AND", "OR", "NOT", "IS", "IN", "LIKE", "BETWEEN"]) {
            return CompletionContext::ExpectingColumn { qualifier: None };
        }
        // A different keyword — we haven't landed in a slot we know.
        // Keep scanning; we might still be in a longer clause.
    }

    // Empty stream or no useful keywords: statement start.
    if tokens.is_empty() {
        CompletionContext::Statement
    } else {
        CompletionContext::Unknown
    }
}

fn is_table_slot_lead(word: &str) -> bool {
    matches_case_insensitive(word, &["FROM", "JOIN", "INTO", "UPDATE", "TABLE"])
}

fn matches_case_insensitive(word: &str, values: &[&str]) -> bool {
    values.iter().any(|value| word.eq_ignore_ascii_case(value))
}

fn word_value(t: &Token) -> Option<&str> {
    match t {
        Token::Word(Word { value, .. }) => Some(value.as_str()),
        _ => None,
    }
}

/// Walk backwards from `cursor` and return `(start, prefix)` for the
/// current partial identifier. Identifier chars are `[A-Za-z0-9_]` and
/// any non-ASCII char (matches PG's default identifier grammar).
/// Recognizes a leading `"` (PG) or `[` (MSSQL) as part of a quoted
/// identifier so the prefix survives quoting.
fn extract_prefix(sql: &str, cursor: usize) -> (usize, String) {
    let bytes = sql.as_bytes();
    let mut start = cursor;
    while start > 0 {
        let c = bytes[start - 1];
        if is_ident_byte(c) {
            start -= 1;
        } else {
            break;
        }
    }
    // Include a preceding opening quote (`"` or `[`) as part of the
    // prefix — the client's cursor sits inside the quoted region and
    // the completion should replace back to the quote.
    if start > 0 {
        let q = bytes[start - 1];
        if q == b'"' || q == b'[' {
            start -= 1;
        }
    }
    let text = sql[start..cursor].to_string();
    (start, text)
}

fn floor_char_boundary(value: &str, mut index: usize) -> usize {
    while index > 0 && !value.is_char_boundary(index) {
        index -= 1;
    }
    index
}

fn is_ident_byte(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_' || c >= 0x80
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mid_utf8_cursor_is_clamped_to_char_boundary() {
        let result = detect_context("SELECT 😀", 9, Engine::Postgres);
        assert_eq!(result.prefix, "");
        assert_eq!(result.prefix_start, 7);
    }

    #[test]
    fn lowercase_schema_qualified_table_slot_is_detected() {
        let result = detect_context("select * from public.", 21, Engine::Postgres);
        assert!(matches!(
            result.context,
            CompletionContext::ExpectingObjectInSchema { ref schema } if schema == "public"
        ));
    }
}
