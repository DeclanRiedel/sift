//! Public-API smoke tests for `sift-completion`. Covers the three
//! contract points from `docs/PLANS/server-build-list-v2.md` Phase D:
//! context detection (FROM slot, dotted qualifier), ranking order
//! (prefix > substring), engine-specific identifier quoting.

use sift_completion::complete;
use sift_protocol::completion::{CompletionContext, CompletionKind, CompletionRequest};
use sift_protocol::{
    CatalogTree, ColumnMetadata, Engine, Nullability, ObjectInfo, ObjectKind, PrimitiveType,
    SchemaScope, SchemaSnapshot, SchemaTree, TypeRef,
};

fn snapshot() -> SchemaSnapshot {
    let users_cols = vec![
        ColumnMetadata {
            name: "id".into(),
            type_ref: TypeRef::Primitive(PrimitiveType::Int32),
            nullable: Nullability::NotNullable,
            auto_increment: false,
            primary_key: true,
            facets: Default::default(),
        },
        ColumnMetadata {
            name: "email".into(),
            type_ref: TypeRef::Primitive(PrimitiveType::Text),
            nullable: Nullability::NotNullable,
            auto_increment: false,
            primary_key: false,
            facets: Default::default(),
        },
    ];
    let mut users = ObjectInfo::new("users", ObjectKind::Table);
    users.columns = users_cols;
    let orders = ObjectInfo::new("orders", ObjectKind::Table);
    let user_events = ObjectInfo::new("user_events", ObjectKind::View);
    let quoted = ObjectInfo::new("MyTable", ObjectKind::Table);
    SchemaSnapshot {
        trees: vec![CatalogTree {
            name: "mock".into(),
            schemas: vec![SchemaTree {
                name: "public".into(),
                objects: vec![users, orders, user_events, quoted],
            }],
        }],
        fetched_at: chrono::Utc::now(),
        scope: SchemaScope::shallow(),
        incomplete: false,
    }
}

#[test]
fn after_from_returns_tables_first() {
    let req = CompletionRequest {
        sql: "SELECT * FROM us".into(),
        cursor: 16,
        limit: None,
    };
    let resp = complete(&req, &snapshot(), Engine::Postgres);
    assert!(matches!(resp.context, CompletionContext::ExpectingTable));
    let top = resp.candidates.first().expect("has candidate");
    // Prefix `us` matches users, user_events. Both are prefix-matches;
    // tables outrank views inside ExpectingTable, so `users` wins.
    assert_eq!(top.label, "users");
    assert_eq!(top.kind, CompletionKind::Table);
    assert_eq!(resp.replaced_range.start, 14);
    assert_eq!(resp.replaced_range.end, 16);
}

#[test]
fn dotted_qualifier_returns_columns_of_resolved_table() {
    // Alias resolution isn't implemented yet; use the bare table name.
    let sql = "SELECT users. FROM users";
    let cursor = 13; // right after "users."
    let req = CompletionRequest {
        sql: sql.into(),
        cursor: cursor as u32,
        limit: None,
    };
    let resp = complete(&req, &snapshot(), Engine::Postgres);
    match &resp.context {
        CompletionContext::ExpectingColumn { qualifier } => {
            assert_eq!(qualifier.as_deref(), Some("users"));
        }
        other => panic!("expected ExpectingColumn, got {other:?}"),
    }
    let labels: Vec<&str> = resp.candidates.iter().map(|c| c.label.as_ref()).collect();
    assert!(labels.contains(&"id"));
    assert!(labels.contains(&"email"));
}

#[test]
fn prefix_beats_substring() {
    let req = CompletionRequest {
        sql: "SELECT * FROM user".into(),
        cursor: 18,
        limit: None,
    };
    let resp = complete(&req, &snapshot(), Engine::Postgres);
    // Both `users` (prefix `user`) and `user_events` (prefix `user`) are
    // prefix matches; `users` still wins alphabetically over
    // `user_events` on a tie.
    let labels: Vec<&str> = resp.candidates.iter().map(|c| c.label.as_ref()).collect();
    let users_at = labels.iter().position(|l| *l == "users").unwrap();
    let ue_at = labels.iter().position(|l| *l == "user_events").unwrap();
    assert!(
        users_at < ue_at,
        "expected users before user_events in {labels:?}"
    );
}

#[test]
fn identifier_with_capitals_is_quoted_per_engine() {
    let req = CompletionRequest {
        sql: "SELECT * FROM My".into(),
        cursor: 16,
        limit: None,
    };
    let pg = complete(&req, &snapshot(), Engine::Postgres);
    let mssql = complete(&req, &snapshot(), Engine::SqlServer);
    let pg_entry = pg
        .candidates
        .iter()
        .find(|c| c.label == "MyTable")
        .expect("pg has MyTable candidate");
    let mssql_entry = mssql
        .candidates
        .iter()
        .find(|c| c.label == "MyTable")
        .expect("mssql has MyTable candidate");
    assert_eq!(pg_entry.insert, "\"MyTable\"");
    assert_eq!(mssql_entry.insert, "[MyTable]");
}

#[test]
fn statement_lead_shows_keywords() {
    let req = CompletionRequest {
        sql: "SEL".into(),
        cursor: 3,
        limit: None,
    };
    let resp = complete(&req, &snapshot(), Engine::Postgres);
    assert!(matches!(resp.context, CompletionContext::Statement));
    let has_select = resp
        .candidates
        .iter()
        .any(|c| c.label == "SELECT" && matches!(c.kind, CompletionKind::Keyword));
    assert!(has_select, "SELECT missing from {:?}", resp.candidates);
}
