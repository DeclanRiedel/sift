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

use std::borrow::Cow;

use sift_protocol::completion::{CompletionCandidate, CompletionContext, CompletionKind};
use sift_protocol::{Engine, ObjectKind};

use crate::dictionary::{ColumnEntry, Dictionary, ObjectEntry};
use crate::keywords::{functions_for, keyword_groups_for};
use crate::ContextResult;

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
            push_routines(&mut out, dict, prefix, engine, /*bonus=*/ 10);
        }
        CompletionContext::ExpectingTable => {
            push_tables_and_views(&mut out, dict, prefix, engine, /*bonus=*/ 60);
            push_local_relations(&mut out, ctx, prefix, /*bonus=*/ 70);
            push_schemas(&mut out, dict, prefix, /*bonus=*/ 30);
            push_keywords(&mut out, engine, prefix, /*bonus=*/ 5);
        }
        CompletionContext::ExpectingColumn { qualifier } => {
            match qualifier {
                Some(q) => {
                    let relation = ctx
                        .relations
                        .iter()
                        .find(|relation| relation.name.eq_ignore_ascii_case(q));
                    let target = relation
                        .and_then(|relation| relation.target.as_deref())
                        .unwrap_or(q);
                    if let Some(relation) = relation.filter(|relation| !relation.columns.is_empty())
                    {
                        push_local_columns(&mut out, &relation.columns, prefix, 85);
                    } else if let Some(obj) = dict.resolve_reference(target) {
                        push_columns(&mut out, obj, prefix, /*bonus=*/ 80);
                    } else {
                        // CTEs, temporary relations, and incomplete shallow
                        // snapshots may not resolve to one catalog object.
                        // Useful unqualified candidates beat an empty popup.
                        push_all_columns(&mut out, dict, prefix, /*bonus=*/ 20);
                        push_functions(&mut out, engine, prefix, /*bonus=*/ 10);
                    }
                }
                None => {
                    if !push_relation_columns(&mut out, ctx, dict, prefix, /*bonus=*/ 70) {
                        push_all_columns(&mut out, dict, prefix, /*bonus=*/ 20);
                    }
                    push_functions(&mut out, engine, prefix, /*bonus=*/ 30);
                    push_keywords(&mut out, engine, prefix, /*bonus=*/ 5);
                }
            }
        }
        CompletionContext::ExpectingObjectInSchema { schema } => {
            for obj in &dict.objects {
                if obj
                    .schema
                    .as_deref()
                    .is_some_and(|obj_schema| obj_schema.eq_ignore_ascii_case(schema))
                    || obj
                        .catalog
                        .as_deref()
                        .is_some_and(|catalog| catalog.eq_ignore_ascii_case(schema))
                {
                    if let Some(cand) = object_candidate(obj, dict, prefix, engine, 80, true) {
                        out.push(cand);
                    }
                }
            }
        }
        CompletionContext::Unknown => {
            push_keywords(&mut out, engine, prefix, /*bonus=*/ 20);
            push_tables_and_views(&mut out, dict, prefix, engine, /*bonus=*/ 20);
            push_routines(&mut out, dict, prefix, engine, /*bonus=*/ 20);
            push_all_columns(&mut out, dict, prefix, /*bonus=*/ 20);
        }
    }

    // Sort: score desc, then label alpha for stable order.
    out.sort_by(|a, b| b.score.cmp(&a.score).then_with(|| a.label.cmp(&b.label)));
    out.dedup_by(|left, right| {
        left.kind == right.kind
            && left.insert == right.insert
            && left.qualified_name == right.qualified_name
    });
    out.truncate(limit);
    out
}

fn push_relation_columns(
    out: &mut Vec<CompletionCandidate>,
    ctx: &ContextResult,
    dict: &Dictionary,
    prefix: &str,
    bonus: i32,
) -> bool {
    let mut resolved = Vec::<&ObjectEntry>::new();
    let mut found = false;
    for relation in ctx.relations.iter().filter(|relation| !relation.is_alias) {
        if !relation.columns.is_empty() {
            push_local_columns(out, &relation.columns, prefix, bonus + 5);
            found = true;
            continue;
        }
        let reference = relation.target.as_deref().unwrap_or(&relation.name);
        let Some(object) = dict.resolve_reference(reference) else {
            continue;
        };
        if !resolved.iter().any(|candidate| {
            candidate.catalog == object.catalog
                && candidate.schema == object.schema
                && candidate.name.eq_ignore_ascii_case(&object.name)
        }) {
            resolved.push(object);
        }
    }
    for object in &resolved {
        push_columns(out, object, prefix, bonus);
    }
    found || !resolved.is_empty()
}

fn push_local_columns(
    out: &mut Vec<CompletionCandidate>,
    columns: &[String],
    prefix: &str,
    bonus: i32,
) {
    for column in columns {
        let Some(match_score) = score_match(column, prefix) else {
            continue;
        };
        out.push(CompletionCandidate {
            label: column.clone().into(),
            insert: column.clone().into(),
            kind: CompletionKind::Column,
            detail: Some("document-local column".into()),
            qualified_name: None,
            score: match_score + bonus,
        });
    }
}

fn push_local_relations(
    out: &mut Vec<CompletionCandidate>,
    ctx: &ContextResult,
    prefix: &str,
    bonus: i32,
) {
    for relation in ctx
        .relations
        .iter()
        .filter(|relation| !relation.is_alias && relation.target.is_none())
    {
        let Some(match_score) = score_match(&relation.name, prefix) else {
            continue;
        };
        out.push(CompletionCandidate {
            label: relation.name.clone().into(),
            insert: relation.name.clone().into(),
            kind: CompletionKind::Table,
            detail: Some("document-local relation".into()),
            qualified_name: None,
            score: match_score + bonus,
        });
    }
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
                label: Cow::Borrowed(*kw),
                insert: Cow::Borrowed(*kw),
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
            label: Cow::Borrowed(*f),
            insert: Cow::Owned(format!("{f}(")),
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
            label: s.clone().into(),
            insert: s.clone().into(),
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
                | ObjectKind::TableValuedFunction
        ) {
            continue;
        }
        if let Some(cand) = object_candidate(obj, dict, prefix, engine, bonus, false) {
            out.push(cand);
        }
    }
}

fn push_routines(
    out: &mut Vec<CompletionCandidate>,
    dict: &Dictionary,
    prefix: &str,
    engine: Engine,
    bonus: i32,
) {
    for object in &dict.objects {
        if matches!(
            object.kind,
            ObjectKind::Procedure | ObjectKind::ScalarFunction | ObjectKind::TableValuedFunction
        ) {
            if let Some(candidate) = object_candidate(object, dict, prefix, engine, bonus, false) {
                out.push(candidate);
            }
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
        label: c.name.clone().into(),
        insert: c.name.clone().into(),
        kind: CompletionKind::Column,
        detail: Some(detail),
        qualified_name: qualified_name(owner),
        score,
    }
}

fn object_candidate(
    obj: &ObjectEntry,
    dict: &Dictionary,
    prefix: &str,
    engine: Engine,
    bonus: i32,
    qualifier_already_typed: bool,
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
    let detail = if let Some(arguments) = &obj.routine_args {
        Some(format!(
            "({}) · {}",
            arguments.join(", "),
            object_location(obj, dict).as_deref().unwrap_or("routine")
        ))
    } else {
        let location = object_location(obj, dict);
        match (location, &obj.comment) {
            (Some(location), Some(comment)) => Some(format!("{location} — {comment}")),
            (Some(location), None) => Some(location),
            (None, Some(comment)) => Some(comment.clone()),
            (None, None) => None,
        }
    };
    let qualified_name = qualified_name(obj).map(|qualified| {
        obj.routine_args
            .as_ref()
            .map_or(qualified.clone(), |arguments| {
                format!("{qualified}({})", arguments.join(","))
            })
    });
    Some(CompletionCandidate {
        label: obj.name.clone().into(),
        insert: object_insert(obj, dict, engine, qualifier_already_typed).into(),
        kind,
        detail,
        qualified_name,
        score: match_score + bonus + kind_bonus,
    })
}

/// Insert the shortest name that is safe in the connected catalog. Most
/// objects are unique and stay pleasantly unqualified. Collisions receive a
/// schema prefix (or catalog + schema when even the schema collides). When the
/// user already typed `schema.`, only the final identifier is replaced.
fn object_insert(
    obj: &ObjectEntry,
    dict: &Dictionary,
    engine: Engine,
    qualifier_already_typed: bool,
) -> String {
    let name = quote_ident_if_needed(&obj.name, engine);
    if qualifier_already_typed {
        return name;
    }
    let Some(matches) = dict.by_name.get(&obj.name_lower) else {
        return name;
    };
    let is_other_location = |candidate: &ObjectEntry| !same_object_location(candidate, obj);
    let candidates = || matches.iter().map(|index| &dict.objects[*index]);
    if !candidates().any(is_other_location) {
        return name;
    }
    let Some(schema_name) = obj.schema.as_deref() else {
        return name;
    };
    let same_schema_in_another_catalog = candidates()
        .filter(|candidate| is_other_location(candidate))
        .any(|candidate| {
            candidate
                .schema
                .as_deref()
                .is_some_and(|other| other.eq_ignore_ascii_case(schema_name))
        });
    let schema = quote_ident_if_needed(schema_name, engine);
    if same_schema_in_another_catalog {
        if let Some(catalog) = obj.catalog.as_deref() {
            return format!(
                "{}.{}.{}",
                quote_ident_if_needed(catalog, engine),
                schema,
                name
            );
        }
    }
    format!("{schema}.{name}")
}

fn object_location(obj: &ObjectEntry, dict: &Dictionary) -> Option<String> {
    let schema = obj.schema.as_deref()?;
    let same_schema_in_another_catalog = dict
        .by_name
        .get(&obj.name_lower)
        .into_iter()
        .flatten()
        .map(|index| &dict.objects[*index])
        .filter(|candidate| !same_object_location(candidate, obj))
        .any(|candidate| {
            candidate
                .schema
                .as_deref()
                .is_some_and(|other| other.eq_ignore_ascii_case(schema))
        });
    if same_schema_in_another_catalog {
        obj.catalog
            .as_deref()
            .map(|catalog| format!("{catalog}.{schema}"))
            .or_else(|| Some(schema.to_string()))
    } else {
        Some(schema.to_string())
    }
}

fn same_object_location(left: &ObjectEntry, right: &ObjectEntry) -> bool {
    optional_identifier_eq(left.catalog.as_deref(), right.catalog.as_deref())
        && optional_identifier_eq(left.schema.as_deref(), right.schema.as_deref())
}

fn optional_identifier_eq(left: Option<&str>, right: Option<&str>) -> bool {
    match (left, right) {
        (Some(left), Some(right)) => left.eq_ignore_ascii_case(right),
        (None, None) => true,
        _ => false,
    }
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
    match (&obj.catalog, &obj.schema) {
        (Some(catalog), Some(schema)) => Some(format!("{catalog}.{schema}.{}", obj.name)),
        (None, Some(schema)) => Some(format!("{schema}.{}", obj.name)),
        _ => None,
    }
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
