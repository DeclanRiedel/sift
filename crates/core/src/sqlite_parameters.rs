//! Pure SQLite bind-slot discovery. Normalized SQL is for classification only.
use std::{
    collections::{HashMap, HashSet},
    ops::Range,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Syntax(&'static str),
    InvalidParameters(&'static str),
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Syntax(message) | Self::InvalidParameters(message) => message,
        })
    }
}
impl std::error::Error for Error {}
impl From<Error> for String {
    fn from(error: Error) -> Self {
        error.to_string()
    }
}
impl From<&'static str> for Error {
    fn from(message: &'static str) -> Self {
        Self::InvalidParameters(message)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Slot {
    pub aliases: Vec<String>,
}
impl Slot {
    pub fn label(&self, index: usize) -> String {
        let shown = self
            .aliases
            .iter()
            .take(8)
            .map(|alias| {
                let mut chars = alias.chars();
                let mut text = chars.by_ref().take(64).collect::<String>();
                if chars.next().is_some() {
                    text.push('…');
                }
                text
            })
            .collect::<Vec<_>>()
            .join(" / ");
        format!(
            "?{} · {}{}",
            index + 1,
            shown,
            if self.aliases.len() > 8 {
                format!(" · +{} aliases", self.aliases.len() - 8)
            } else {
                String::new()
            }
        )
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layout {
    pub slots: Vec<Slot>,
    pub normalized_sql: String,
}
pub fn layout(sql: &str, maximum: usize) -> Result<Layout, Error> {
    template_layout(sql, &[], maximum)
}
fn identifier(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'$' | 0x80..=0xff)
}
/// End of one native host-parameter token at an already identified token boundary.
pub fn parameter_end(sql: &str, start: usize) -> Result<Option<usize>, Error> {
    if !sql.is_char_boundary(start) {
        return Err(Error::InvalidParameters("invalid SQLite token boundary"));
    }
    let bytes = sql.as_bytes();
    let Some(prefix) = bytes.get(start).copied() else {
        return Ok(None);
    };
    let mut index = start + 1;
    if prefix == b'?' {
        while index < bytes.len() && bytes[index].is_ascii_digit() {
            index += 1;
        }
        return Ok(Some(index));
    }
    if !matches!(prefix, b':' | b'@' | b'$') {
        return Ok(None);
    }
    let mut letters = 0;
    while index < bytes.len() {
        if identifier(bytes[index]) {
            letters += 1;
            index += 1;
        } else if bytes[index] == b':' && bytes.get(index + 1) == Some(&b':') {
            index += 2;
        } else if bytes[index] == b'(' && letters > 0 {
            index += 1;
            while index < bytes.len()
                && !matches!(bytes[index], 9..=13 | 32)
                && bytes[index] != b')'
            {
                index += 1;
            }
            if bytes.get(index) != Some(&b')') {
                return Err(Error::Syntax(
                    "unterminated or whitespace-containing SQLite parameter suffix",
                ));
            }
            index += 1;
            break;
        } else {
            break;
        }
    }
    if letters == 0 {
        return Err(Error::Syntax("SQLite named parameter needs an identifier"));
    }
    Ok(Some(index))
}

/// Template spans must identify actual template references outside quoted text.
/// A template embedded in an atomic native bind name is refused.
pub fn template_layout(
    sql: &str,
    ignored: &[Range<usize>],
    maximum: usize,
) -> Result<Layout, Error> {
    if sql.as_bytes().contains(&0) {
        return Err(Error::Syntax("SQLite SQL cannot contain NUL"));
    }
    let mut prior = 0;
    for span in ignored {
        if span.start < prior
            || span.start >= span.end
            || span.end > sql.len()
            || !sql.is_char_boundary(span.start)
            || !sql.is_char_boundary(span.end)
        {
            return Err(Error::InvalidParameters("invalid SQLite template span"));
        }
        prior = span.end;
    }
    let maximum = maximum.min(32766);
    let bytes = sql.as_bytes();
    let mut index = 0;
    let mut ignored_index = 0;
    let mut slots = Vec::<Slot>::new();
    let mut names = HashMap::<&str, usize>::new();
    let mut aliases = HashSet::new();
    let mut normalized = String::with_capacity(sql.len());
    let mut copied = 0;
    while index < bytes.len() {
        while ignored
            .get(ignored_index)
            .is_some_and(|span| span.end <= index)
        {
            ignored_index += 1;
        }
        if let Some(span) = ignored
            .get(ignored_index)
            .filter(|span| span.start == index)
        {
            normalized.push_str(&sql[copied..index]);
            normalized.push(' ');
            index = span.end;
            copied = index;
            ignored_index += 1;
            continue;
        }
        let start = index;
        match bytes[index] {
            b'\'' | b'"' | b'`' | b'[' => {
                let close = if bytes[index] == b'[' {
                    b']'
                } else {
                    bytes[index]
                };
                let bracket = bytes[index] == b'[';
                index += 1;
                let mut closed = false;
                while index < bytes.len() {
                    if bytes[index] == close {
                        index += 1;
                        if !bracket && bytes.get(index) == Some(&close) {
                            index += 1;
                        } else {
                            closed = true;
                            break;
                        }
                    } else {
                        index += 1;
                    }
                }
                if !closed {
                    return Err(Error::Syntax("unterminated SQLite string or identifier"));
                }
                continue;
            }
            b'-' if bytes.get(index + 1) == Some(&b'-') => {
                index += 2;
                while index < bytes.len() && bytes[index] != b'\n' {
                    index += 1;
                }
                continue;
            }
            b'/' if bytes.get(index + 1) == Some(&b'*') => {
                index += 2;
                while index + 1 < bytes.len() && &bytes[index..index + 2] != b"*/" {
                    index += 1;
                }
                index = (index + 2).min(bytes.len());
                continue;
            }
            b'?' | b':' | b'@' | b'$' => {
                index = parameter_end(sql, index)?.expect("matched SQLite bind prefix");
            }
            b'#' => return Err(Error::InvalidParameters(
                "SQLite internal # parameters are unsupported; use ?, ?NNN, :name, @name or $name",
            )),
            b'a'..=b'z' | b'A'..=b'Z' | b'_' | 0x80..=0xff => {
                index += 1;
                while index < bytes.len() && identifier(bytes[index]) {
                    index += 1;
                }
                continue;
            }
            // Consume numeric identifier tails too: a dollar within a malformed
            // numeric token must not become an independent named bind.
            b'0'..=b'9' => {
                index += 1;
                while index < bytes.len() && identifier(bytes[index]) {
                    index += 1;
                }
                continue;
            }
            _ => {
                index += 1;
                continue;
            }
        }
        if ignored
            .get(ignored_index)
            .is_some_and(|span| span.start < index && span.end > start)
        {
            return Err(Error::InvalidParameters(
                "SQL template cannot be embedded in a SQLite parameter name",
            ));
        }
        let token = &sql[start..index];
        let slot = if token == "?" {
            slots.len() + 1
        } else if let Some(number) = token.strip_prefix('?') {
            number
                .parse::<usize>()
                .map_err(|_| "SQLite parameter index is too large")?
        } else if let Some(slot) = names.get(token) {
            *slot
        } else {
            let slot = slots.len() + 1;
            names.insert(token, slot);
            slot
        };
        if slot == 0 || slot > maximum {
            return Err(Error::InvalidParameters(
                "SQLite bind index is zero or exceeds the supported parameter limit",
            ));
        }
        slots.resize_with(slots.len().max(slot), || Slot {
            aliases: Vec::new(),
        });
        if aliases.insert((slot, token)) {
            slots[slot - 1].aliases.push(token.into());
        }
        normalized.push_str(&sql[copied..start]);
        normalized.push_str(&format!("?{slot}"));
        copied = index;
    }
    if slots.iter().any(|slot| slot.aliases.is_empty()) {
        return Err(Error::InvalidParameters(
            "SQLite parameters must use contiguous slots without gaps",
        ));
    }
    normalized.push_str(&sql[copied..]);
    Ok(Layout {
        slots,
        normalized_sql: normalized,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn aliases_follow_native_slot_order_without_interpreting_literals_or_identifiers() {
        let parsed=layout("SELECT :x,?1,@x,$x,?,?2,:x,'$skip',\"@skip\",`?skip`,[?skip], foo$bar /* :skip */ -- ?999\n",256).unwrap();
        assert_eq!(parsed.slots.len(), 4);
        assert_eq!(parsed.slots[0].aliases, vec![":x", "?1"]);
        assert_eq!(parsed.slots[1].aliases, vec!["@x", "?2"]);
        assert_eq!(parsed.slots[2].aliases, vec!["$x"]);
        assert_eq!(parsed.slots[3].aliases, vec!["?"]);
        assert!(parsed
            .normalized_sql
            .starts_with("SELECT ?1,?1,?2,?3,?4,?2,?1,'$skip'"));
        assert!(parsed
            .normalized_sql
            .contains("foo$bar /* :skip */ -- ?999"));
        let parsed = layout(
            "SELECT ?2,?01,:名,$ns::name(some;value),$ns::name(some;value),:名",
            256,
        )
        .unwrap();
        assert_eq!(parsed.slots.len(), 4);
        assert_eq!(parsed.slots[0].aliases, vec!["?01"]);
        assert_eq!(parsed.slots[3].aliases, vec!["$ns::name(some;value)"]);
        for sql in [
            "SELECT ?0",
            "SELECT ?2",
            "SELECT ?257",
            "SELECT :",
            "SELECT $x(a b)",
            "SELECT $x(a",
            "SELECT 'unfinished",
            "SELECT #name",
            "SELECT \0",
        ] {
            assert!(layout(sql, 256).is_err(), "{sql:?}");
        }
    }
    #[test]
    fn templates_reserve_native_slots_and_cannot_rewrite_atomic_names() {
        let sql = "SELECT :value + {{value}} + {{ident:table}}";
        let spans = [
            sql.find("{{value}}").unwrap()..sql.find("{{value}}").unwrap() + 9,
            sql.find("{{ident:table}}").unwrap()..sql.len(),
        ];
        assert_eq!(template_layout(sql, &spans, 256).unwrap().slots.len(), 1);
        let sql = "SELECT $name({{value}})";
        let start = sql.find("{{").unwrap();
        assert!(template_layout(sql, std::slice::from_ref(&(start..start + 9)), 256).is_err());
    }
}
