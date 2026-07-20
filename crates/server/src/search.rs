//! Schema & data search (Phase D).
//!
//! Schema search runs over an in-memory per-connection [`SearchIndex`] of
//! object + column names using the `sift-completion` fuzzy matcher — no DB
//! round-trip on the hot path. Data search is a bounded, parameterized live
//! `LIKE` fan-out over a chosen scope. Composes over `Driver::schema` +
//! `SessionStore::execute_http`; no new `Driver` method (ADR-017 preserved).
//! See `docs/PLANS/schema-data-search.md` (ADR-024 candidate).

use sift_completion::fuzzy::fuzzy_match;
use sift_protocol::{
    DataSearchScope, Engine, ObjectInfo, ObjectKind, ObjectPath, SchemaSnapshot, SearchHit,
    SearchTarget, Value,
};

use crate::ddl::{qualified_name, quote_ident};

/// Hard ceilings applied regardless of request values.
pub const MAX_SCHEMA_HITS: u32 = 200;
pub const DEFAULT_SCHEMA_HITS: u32 = 50;
pub const MAX_PER_TABLE: u32 = 1000;
pub const DEFAULT_PER_TABLE: u32 = 100;
pub const MAX_TABLES: u32 = 200;
pub const DEFAULT_MAX_TABLES: u32 = 50;

/// One searchable name in the index: an object, or a column on a table.
#[derive(Debug, Clone)]
pub struct SearchEntry {
    pub target: SearchTarget,
    /// The object (for a column, its table).
    pub path: ObjectPath,
    pub column: Option<String>,
    /// Display string (`schema.table` or `schema.table.column`).
    pub display: String,
    /// Lowercased `display` — the fuzzy target, precomputed once.
    pub haystack_lower: Box<str>,
    /// Rendered column type (column entries only). Doubles as the text-ish
    /// classifier source for data search.
    pub type_display: Option<String>,
}

/// Per-connection denormalized search index. Cheap to fuzzy-scan.
#[derive(Debug, Clone, Default)]
pub struct SearchIndex {
    pub entries: Vec<SearchEntry>,
}

impl SearchIndex {
    /// Build from a shallow schema snapshot (objects) plus decoded column rows
    /// from the bulk catalog query (`(schema, table, column, type)`).
    pub fn build(snapshot: &SchemaSnapshot, columns: Vec<CatalogColumn>) -> Self {
        let mut entries = Vec::new();
        for tree in &snapshot.trees {
            for schema in &tree.schemas {
                for obj in &schema.objects {
                    entries.push(object_entry(obj, Some(&schema.name)));
                }
            }
        }
        for c in columns {
            let display = format!("{}.{}.{}", c.schema, c.table, c.column);
            let haystack_lower = display.to_lowercase().into_boxed_str();
            let mut path = ObjectPath::new(c.table);
            path.schema = Some(c.schema);
            path.kind = Some(ObjectKind::Table);
            entries.push(SearchEntry {
                target: SearchTarget::Column,
                path,
                column: Some(c.column),
                display,
                haystack_lower,
                type_display: Some(c.data_type),
            });
        }
        SearchIndex { entries }
    }
}

fn object_entry(obj: &ObjectInfo, schema: Option<&str>) -> SearchEntry {
    let display = match schema {
        Some(s) => format!("{s}.{}", obj.name),
        None => obj.name.clone(),
    };
    let haystack_lower = display.to_lowercase().into_boxed_str();
    let mut path = ObjectPath::new(&obj.name);
    path.schema = schema.map(str::to_string);
    path.kind = Some(obj.kind);
    SearchEntry {
        target: SearchTarget::Object {
            object_kind: obj.kind,
        },
        path,
        column: None,
        display,
        haystack_lower,
        type_display: None,
    }
}

/// A decoded row from the bulk column catalog query.
#[derive(Debug, Clone)]
pub struct CatalogColumn {
    pub schema: String,
    pub table: String,
    pub column: String,
    pub data_type: String,
}

/// Engine SQL that lists every user column as `(schema, table, column, type)`.
pub fn bulk_columns_sql(engine: Engine) -> &'static str {
    match engine {
        Engine::Postgres => {
            "SELECT table_schema, table_name, column_name, data_type \
             FROM information_schema.columns \
             WHERE table_schema NOT IN ('pg_catalog', 'information_schema') \
             ORDER BY table_schema, table_name, ordinal_position"
        }
        Engine::SqlServer => {
            "SELECT s.name, t.name, c.name, ty.name \
             FROM sys.columns c \
             JOIN sys.objects t ON c.object_id = t.object_id \
             JOIN sys.schemas s ON t.schema_id = s.schema_id \
             JOIN sys.types ty ON c.user_type_id = ty.user_type_id \
             WHERE t.type IN ('U', 'V') \
             ORDER BY s.name, t.name, c.column_id"
        }
    }
}

/// Decode bulk-column-query rows into [`CatalogColumn`]s. Rows whose first four
/// values aren't text are skipped.
pub fn decode_catalog_columns(rows: Vec<sift_protocol::Row>) -> Vec<CatalogColumn> {
    rows.into_iter()
        .filter_map(|r| {
            let v = r.values;
            if v.len() < 4 {
                return None;
            }
            Some(CatalogColumn {
                schema: as_text(&v[0])?,
                table: as_text(&v[1])?,
                column: as_text(&v[2])?,
                data_type: as_text(&v[3])?,
            })
        })
        .collect()
}

fn as_text(v: &Value) -> Option<String> {
    match v {
        Value::Text(s) => Some(s.clone()),
        _ => None,
    }
}

/// Rank the index against `query`. `kinds` (when set) restricts *object* hits;
/// column hits are always considered. Returns hits sorted best-first, capped.
pub fn rank(
    index: &SearchIndex,
    query: &str,
    kinds: Option<&[ObjectKind]>,
    limit: u32,
) -> Vec<SearchHit> {
    let cap = limit.clamp(1, MAX_SCHEMA_HITS) as usize;
    let mut hits: Vec<SearchHit> = index
        .entries
        .iter()
        .filter(|e| match (&e.target, kinds) {
            (SearchTarget::Object { object_kind }, Some(ks)) => ks.contains(object_kind),
            _ => true,
        })
        .filter_map(|e| {
            let m = fuzzy_match(query, &e.haystack_lower)?;
            Some(SearchHit {
                target: e.target.clone(),
                path: e.path.clone(),
                column: e.column.clone(),
                display: e.display.clone(),
                score: m.score,
                type_display: e.type_display.clone(),
                match_ranges: m.ranges,
            })
        })
        .collect();
    hits.sort_by(|a, b| {
        b.score
            .cmp(&a.score)
            .then_with(|| a.display.cmp(&b.display))
    });
    hits.truncate(cap);
    hits
}

/// True if `data_type` is a text-ish column worth an `ILIKE`/`LIKE` search.
pub fn is_text_type(data_type: &str) -> bool {
    let d = data_type.to_ascii_lowercase();
    d.contains("char") || d.contains("text") || d == "citext" || d == "name" || d == "sysname"
}

/// Escape `%`, `_`, and `\` for a `LIKE`/`ILIKE` pattern, then wrap in `%…%`.
pub fn like_pattern(query: &str) -> String {
    let mut out = String::with_capacity(query.len() + 2);
    out.push('%');
    for ch in query.chars() {
        if matches!(ch, '\\' | '%' | '_') {
            out.push('\\');
        }
        out.push(ch);
    }
    out.push('%');
    out
}

/// Build a parameterized data-search statement for one table over `text_cols`.
/// Returns `None` if there are no searchable columns. The statement's single
/// bind (`$1` / `@P1`) is the `LIKE` pattern the caller supplies via
/// [`like_pattern`]; `limit` is inlined (already clamped, non-negative).
pub fn data_search_sql(
    engine: Engine,
    table: &ObjectPath,
    text_cols: &[String],
    limit: u32,
) -> Option<String> {
    if text_cols.is_empty() {
        return None;
    }
    let table_sql = qualified_name(table, engine);
    let select: Vec<String> = text_cols.iter().map(|c| quote_ident(c, engine)).collect();
    let select = select.join(", ");
    let (op, param) = match engine {
        Engine::Postgres => ("ILIKE", "$1"),
        Engine::SqlServer => ("LIKE", "@P1"),
    };
    let preds: Vec<String> = text_cols
        .iter()
        .map(|c| format!("{} {op} {param} ESCAPE '\\'", quote_ident(c, engine)))
        .collect();
    let where_sql = preds.join(" OR ");
    let sql = match engine {
        Engine::Postgres => {
            format!("SELECT {select} FROM {table_sql} WHERE {where_sql} LIMIT {limit}")
        }
        Engine::SqlServer => {
            format!("SELECT TOP ({limit}) {select} FROM {table_sql} WHERE {where_sql}")
        }
    };
    Some(sql)
}

/// True if `kind` is a relation whose rows a data search can scan.
fn is_relation(kind: ObjectKind) -> bool {
    matches!(
        kind,
        ObjectKind::Table
            | ObjectKind::View
            | ObjectKind::MaterializedView
            | ObjectKind::PartitionedTable
            | ObjectKind::ForeignTable
    )
}

fn same_table(entry_path: &ObjectPath, table: &ObjectPath) -> bool {
    entry_path.name == table.name && (table.schema.is_none() || entry_path.schema == table.schema)
}

/// Resolve a [`DataSearchScope`] to concrete table paths using the index's
/// object entries (for the `Schema` case).
pub fn resolve_scope(index: &SearchIndex, scope: &DataSearchScope) -> Vec<ObjectPath> {
    match scope {
        DataSearchScope::Table { table } => vec![table.clone()],
        DataSearchScope::Tables { tables } => tables.clone(),
        DataSearchScope::Schema { schema } => index
            .entries
            .iter()
            .filter_map(|e| match &e.target {
                SearchTarget::Object { object_kind } if is_relation(*object_kind) => {
                    (e.path.schema.as_deref() == Some(schema.as_str())).then(|| e.path.clone())
                }
                _ => None,
            })
            .collect(),
    }
}

/// The text-ish columns of `table` per the index, optionally restricted to
/// `filter`. Preserves index (catalog) order.
pub fn text_columns_for(
    index: &SearchIndex,
    table: &ObjectPath,
    filter: Option<&[String]>,
) -> Vec<String> {
    index
        .entries
        .iter()
        .filter_map(|e| {
            if !matches!(e.target, SearchTarget::Column) {
                return None;
            }
            if !same_table(&e.path, table) {
                return None;
            }
            let col = e.column.as_ref()?;
            let is_text = e.type_display.as_deref().is_some_and(is_text_type);
            let allowed = filter.map_or(true, |f| f.iter().any(|c| c == col));
            (is_text && allowed).then(|| col.clone())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use sift_protocol::{CatalogTree, SchemaScope, SchemaTree};

    fn index_with(
        objects: Vec<(&str, ObjectKind)>,
        cols: Vec<(&str, &str, &str, &str)>,
    ) -> SearchIndex {
        let snap = SchemaSnapshot {
            trees: vec![CatalogTree {
                name: "db".into(),
                schemas: vec![SchemaTree {
                    name: "public".into(),
                    objects: objects
                        .into_iter()
                        .map(|(n, k)| ObjectInfo::new(n, k))
                        .collect(),
                }],
            }],
            fetched_at: chrono::Utc::now(),
            scope: SchemaScope::shallow(),
            incomplete: false,
        };
        let columns = cols
            .into_iter()
            .map(|(s, t, c, ty)| CatalogColumn {
                schema: s.into(),
                table: t.into(),
                column: c.into(),
                data_type: ty.into(),
            })
            .collect();
        SearchIndex::build(&snap, columns)
    }

    #[test]
    fn ranks_object_and_column_hits() {
        let idx = index_with(
            vec![("users", ObjectKind::Table), ("orders", ObjectKind::Table)],
            vec![
                ("public", "users", "email", "text"),
                ("public", "users", "id", "integer"),
            ],
        );
        let hits = rank(&idx, "email", None, 10);
        assert!(hits.iter().any(|h| h.display == "public.users.email"));
    }

    #[test]
    fn kinds_filter_restricts_objects_only() {
        let idx = index_with(
            vec![("users", ObjectKind::Table), ("uview", ObjectKind::View)],
            vec![("public", "users", "uu", "text")],
        );
        let hits = rank(&idx, "u", Some(&[ObjectKind::Table]), 50);
        // The view object must be filtered out...
        assert!(!hits.iter().any(|h| matches!(
            h.target,
            SearchTarget::Object {
                object_kind: ObjectKind::View
            }
        )));
        // ...but the column hit survives the object-kind filter.
        assert!(hits.iter().any(|h| h.column.as_deref() == Some("uu")));
    }

    #[test]
    fn like_pattern_escapes_wildcards() {
        assert_eq!(like_pattern("100%_off"), r"%100\%\_off%");
        assert_eq!(like_pattern(r"a\b"), r"%a\\b%");
    }

    #[test]
    fn data_search_sql_pg_and_mssql() {
        let mut t = ObjectPath::new("users");
        t.schema = Some("public".into());
        let cols = vec!["email".to_string(), "name".to_string()];
        let pg = data_search_sql(Engine::Postgres, &t, &cols, 100).unwrap();
        assert_eq!(
            pg,
            r#"SELECT "email", "name" FROM "public"."users" WHERE "email" ILIKE $1 ESCAPE '\' OR "name" ILIKE $1 ESCAPE '\' LIMIT 100"#
        );
        let ms = data_search_sql(Engine::SqlServer, &t, &cols, 50).unwrap();
        assert_eq!(
            ms,
            r#"SELECT TOP (50) [email], [name] FROM [public].[users] WHERE [email] LIKE @P1 ESCAPE '\' OR [name] LIKE @P1 ESCAPE '\'"#
        );
    }

    #[test]
    fn is_text_type_classifies() {
        assert!(is_text_type("character varying"));
        assert!(is_text_type("nvarchar"));
        assert!(is_text_type("text"));
        assert!(!is_text_type("integer"));
        assert!(!is_text_type("timestamp with time zone"));
    }
}
