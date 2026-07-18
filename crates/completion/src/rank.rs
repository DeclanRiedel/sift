//! Candidate generation + ranking.
//!
//! Given a detected [`CompletionContext`] and a [`Dictionary`], produce
//! a bounded, ordered list of [`CompletionCandidate`]s.
//!
//! Ranking is intentionally simple:
//! 1. Exact case-sensitive prefix match on `label`  — score 1000
//! 2. Case-insensitive prefix match                  — score 800
//! 3. Case-insensitive substring contains            — score 300
//! 4. No match on prefix                             — score 0 (dropped)
//!
//! On top of the match-quality score we add a small kind bonus that
//! reflects the current context — inside `ExpectingTable`, tables win
//! over keywords; inside `ExpectingColumn`, columns win. Ties break
//! alphabetically for stable output.

use sift_protocol::completion::{CompletionCandidate, CompletionContext, CompletionKind};
use sift_protocol::{Engine, ObjectKind};

use crate::context::ContextResult;
use crate::dictionary::{ColumnEntry, Dictionary, ObjectEntry};
use crate::keywords::{functions_for, keyword_groups_for};

pub fn rank(
    ctx: &ContextResult,
    dict: &Dictionary,
    engine: Engine,
    limit: usize,
) -> Vec<CompletionCandidate> {
    let prefix = ctx.prefix_lower.as_str();
    let mut out: Vec<CompletionCandidate> = Vec::new();

    match &ctx.context {
        CompletionContext::Statement => {
            push_keywords(&mut out, engine, prefix, /*context_bonus=*/ 40);
            push_tables_and_views(&mut out, dict, prefix, engine, /*bonus=*/ 10);
        }
        CompletionContext::ExpectingTable => {
            push_tables_and_views(&mut out, dict, prefix, engine, /*bonus=*/ 60);
            push_schemas(&mut out, dict, prefix, /*bonus=*/ 30);
            push_keywords(&mut out, engine, prefix, /*bonus=*/ 5);
        }
        CompletionContext::ExpectingColumn { qualifier } => {
            match qualifier {
                Some(q) => {
                    // Resolve the qualifier against declared aliases /
                    // direct table names. Alias tracking is a future
                    // extension; today `resolve_by_name` catches the
                    // "select from a table without an alias" common case.
                    if let Some(obj) = dict.resolve_by_name(q) {
                        push_columns(&mut out, obj, prefix, /*bonus=*/ 80);
                    }
                }
                None => {
                    push_all_columns(&mut out, dict, prefix, /*bonus=*/ 40);
                    push_functions(&mut out, engine, prefix, /*bonus=*/ 30);
                    push_keywords(&mut out, engine, prefix, /*bonus=*/ 5);
                }
            }
        }
        CompletionContext::ExpectingObjectInSchema { schema } => {
            let schema_l = schema.to_ascii_lowercase();
            for obj in &dict.objects {
                if obj
                    .schema
                    .as_deref()
                    .is_some_and(|obj_schema| obj_schema.eq_ignore_ascii_case(&schema_l))
                {
                    if let Some(cand) = object_candidate(obj, prefix, engine, 80) {
                        out.push(cand);
                    }
                }
            }
        }
        CompletionContext::Unknown => {
            push_keywords(&mut out, engine, prefix, /*bonus=*/ 20);
            push_tables_and_views(&mut out, dict, prefix, engine, /*bonus=*/ 20);
            push_all_columns(&mut out, dict, prefix, /*bonus=*/ 20);
        }
    }

    // Sort: score desc, then label alpha for stable order.
    out.sort_by(|a, b| b.score.cmp(&a.score).then_with(|| a.label.cmp(&b.label)));
    out.truncate(limit);
    out
}

// ----------------------------------------------------------------------------
// Producers
// ----------------------------------------------------------------------------

fn push_keywords(out: &mut Vec<CompletionCandidate>, engine: Engine, prefix: &str, bonus: i32) {
    for group in keyword_groups_for(engine) {
        for kw in group {
            let Some(match_score) = score_match(kw, prefix) else {
                continue;
            };
            out.push(CompletionCandidate {
                label: kw.to_string(),
                insert: kw.to_string(),
                kind: CompletionKind::Keyword,
                detail: None,
                qualified_name: None,
                score: match_score + bonus,
            });
        }
    }
}

fn push_functions(out: &mut Vec<CompletionCandidate>, engine: Engine, prefix: &str, bonus: i32) {
    for f in functions_for(engine) {
        let Some(match_score) = score_match(f, prefix) else {
            continue;
        };
        out.push(CompletionCandidate {
            label: f.to_string(),
            insert: format!("{f}("),
            kind: CompletionKind::Function,
            detail: Some("built-in".into()),
            qualified_name: None,
            score: match_score + bonus,
        });
    }
}

fn push_schemas(out: &mut Vec<CompletionCandidate>, dict: &Dictionary, prefix: &str, bonus: i32) {
    for s in &dict.schemas {
        let Some(match_score) = score_match(s, prefix) else {
            continue;
        };
        out.push(CompletionCandidate {
            label: s.clone(),
            insert: s.clone(),
            kind: CompletionKind::Schema,
            detail: None,
            qualified_name: None,
            score: match_score + bonus,
        });
    }
}

fn push_tables_and_views(
    out: &mut Vec<CompletionCandidate>,
    dict: &Dictionary,
    prefix: &str,
    engine: Engine,
    bonus: i32,
) {
    for obj in table_view_candidates(dict, prefix) {
        if !matches!(
            obj.kind,
            ObjectKind::Table
                | ObjectKind::View
                | ObjectKind::MaterializedView
                | ObjectKind::PartitionedTable
                | ObjectKind::ForeignTable
        ) {
            continue;
        }
        if let Some(cand) = object_candidate(obj, prefix, engine, bonus) {
            out.push(cand);
        }
    }
}

fn push_columns(out: &mut Vec<CompletionCandidate>, obj: &ObjectEntry, prefix: &str, bonus: i32) {
    for c in &obj.columns {
        let Some(match_score) = score_match_with_lower(&c.name, &c.name_lower, prefix) else {
            continue;
        };
        out.push(column_candidate(c, obj, match_score + bonus));
    }
}

fn push_all_columns(
    out: &mut Vec<CompletionCandidate>,
    dict: &Dictionary,
    prefix: &str,
    bonus: i32,
) {
    for obj in &dict.objects {
        push_columns(out, obj, prefix, bonus);
    }
}

fn column_candidate(c: &ColumnEntry, owner: &ObjectEntry, score: i32) -> CompletionCandidate {
    let detail = if c.not_null {
        format!("{} NOT NULL", c.type_display)
    } else {
        c.type_display.clone()
    };
    CompletionCandidate {
        label: c.name.clone(),
        insert: c.name.clone(),
        kind: CompletionKind::Column,
        detail: Some(detail),
        qualified_name: qualified_name(owner),
        score,
    }
}

fn object_candidate(
    obj: &ObjectEntry,
    prefix: &str,
    engine: Engine,
    bonus: i32,
) -> Option<CompletionCandidate> {
    let match_score = score_match_with_lower(&obj.name, &obj.name_lower, prefix)?;
    let kind = match obj.kind {
        ObjectKind::Table | ObjectKind::PartitionedTable | ObjectKind::ForeignTable => {
            CompletionKind::Table
        }
        ObjectKind::View => CompletionKind::View,
        ObjectKind::MaterializedView => CompletionKind::MaterializedView,
        ObjectKind::Procedure => CompletionKind::Procedure,
        ObjectKind::ScalarFunction | ObjectKind::TableValuedFunction => CompletionKind::Function,
        ObjectKind::Type => CompletionKind::Type,
        _ => CompletionKind::Table,
    };
    // Small kind-based nudge: tables > views > materialized views. Same
    // magnitude as an alphabetic tie so a strong prefix match still wins
    // regardless of kind.
    let kind_bonus = match kind {
        CompletionKind::Table => 5,
        CompletionKind::View => 3,
        CompletionKind::MaterializedView => 2,
        _ => 0,
    };
    Some(CompletionCandidate {
        label: obj.name.clone(),
        insert: quote_ident_if_needed(&obj.name, engine),
        kind,
        detail: obj.schema.clone(),
        qualified_name: qualified_name(obj),
        score: match_score + bonus + kind_bonus,
    })
}

fn table_view_candidates<'a>(
    dict: &'a Dictionary,
    prefix: &str,
) -> Box<dyn Iterator<Item = &'a ObjectEntry> + 'a> {
    if prefix.is_empty() {
        return Box::new(dict.objects.iter());
    }
    let start = dict
        .objects_by_name
        .partition_point(|idx| dict.objects[*idx].name_lower.as_str() < prefix);
    let end = dict.objects_by_name[start..]
        .partition_point(|idx| dict.objects[*idx].name_lower.starts_with(prefix));
    Box::new(
        dict.objects_by_name[start..start + end]
            .iter()
            .map(|idx| &dict.objects[*idx]),
    )
}

fn qualified_name(obj: &ObjectEntry) -> Option<String> {
    obj.schema.as_ref().map(|s| format!("{}.{}", s, obj.name))
}

// ----------------------------------------------------------------------------
// Match scoring
// ----------------------------------------------------------------------------

fn score_match(candidate: &str, prefix: &str) -> Option<i32> {
    if prefix.is_empty() {
        return Some(500);
    }
    if candidate.starts_with(prefix) {
        return Some(1000);
    }
    if starts_with_ignore_ascii_case(candidate, prefix) {
        return Some(800);
    }
    if contains_ignore_ascii_case(candidate, prefix) {
        return Some(300);
    }
    None
}

fn score_match_with_lower(candidate: &str, candidate_lower: &str, prefix: &str) -> Option<i32> {
    if prefix.is_empty() {
        return Some(500);
    }
    if candidate.starts_with(prefix) {
        return Some(1000);
    }
    if candidate_lower.starts_with(prefix) {
        return Some(800);
    }
    if candidate_lower.contains(prefix) {
        return Some(300);
    }
    None
}

fn starts_with_ignore_ascii_case(candidate: &str, prefix: &str) -> bool {
    candidate
        .as_bytes()
        .get(..prefix.len())
        .is_some_and(|head| head.eq_ignore_ascii_case(prefix.as_bytes()))
}

fn contains_ignore_ascii_case(candidate: &str, needle: &str) -> bool {
    let needle = needle.as_bytes();
    !needle.is_empty()
        && candidate
            .as_bytes()
            .windows(needle.len())
            .any(|window| window.eq_ignore_ascii_case(needle))
}

/// Quote an identifier if it isn't already a simple lowercase word.
/// The heuristic is deliberately conservative — over-quoting is a
/// rendering choice, not a correctness issue.
fn quote_ident_if_needed(name: &str, engine: Engine) -> String {
    let simple = !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        && !name.starts_with(|c: char| c.is_ascii_digit());
    if simple {
        return name.to_string();
    }
    match engine {
        Engine::Postgres => format!("\"{}\"", name.replace('"', "\"\"")),
        Engine::SqlServer => format!("[{}]", name.replace(']', "]]")),
    }
}
