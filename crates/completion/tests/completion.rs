//! Public-API smoke tests for `sift-completion`. Covers the three
//! contract points: context detection (FROM slot, dotted qualifier), ranking order
//! (prefix > substring), engine-specific identifier quoting.

use sift_completion::{complete, detect_context, Dictionary};
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
    let mut orders = ObjectInfo::new("orders", ObjectKind::Table);
    orders.columns = vec![
        ColumnMetadata {
            name: "id".into(),
            type_ref: TypeRef::Primitive(PrimitiveType::Int64),
            nullable: Nullability::NotNullable,
            auto_increment: false,
            primary_key: true,
            facets: Default::default(),
        },
        ColumnMetadata {
            name: "total".into(),
            type_ref: TypeRef::Primitive(PrimitiveType::Decimal),
            nullable: Nullability::Nullable,
            auto_increment: false,
            primary_key: false,
            facets: Default::default(),
        },
    ];
    let user_events = ObjectInfo::new("user_events", ObjectKind::View);
    let quoted = ObjectInfo::new("MyTable", ObjectKind::Table);
    let routine = ObjectInfo::new("find_users", ObjectKind::TableValuedFunction);
    SchemaSnapshot {
        trees: vec![CatalogTree {
            name: "mock".into(),
            schemas: vec![SchemaTree {
                name: "public".into(),
                objects: vec![users, orders, user_events, quoted, routine],
            }],
        }],
        fetched_at: chrono::Utc::now(),
        scope: SchemaScope::shallow(),
        incomplete: false,
        graph: None,
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
fn dotted_alias_returns_columns_of_bound_table() {
    let sql = "SELECT u. FROM public.users AS u";
    let cursor = 9; // right after `u.`
    let response = complete(
        &CompletionRequest {
            sql: sql.into(),
            cursor,
            limit: None,
        },
        &snapshot(),
        Engine::Postgres,
    );
    let labels: Vec<&str> = response
        .candidates
        .iter()
        .map(|candidate| candidate.label.as_ref())
        .collect();
    assert!(labels.contains(&"id"));
    assert!(labels.contains(&"email"));
}

#[test]
fn unqualified_columns_are_limited_to_relations_in_scope() {
    let sql = "SELECT e FROM users";
    let response = complete(
        &CompletionRequest {
            sql: sql.into(),
            cursor: 8,
            limit: None,
        },
        &snapshot(),
        Engine::Postgres,
    );
    assert!(response
        .candidates
        .iter()
        .any(|candidate| candidate.label == "email"));
    assert!(!response
        .candidates
        .iter()
        .any(|candidate| candidate.label == "total"));
}

#[test]
fn ambiguous_columns_keep_both_qualified_owners() {
    let sql = "SELECT i FROM users u JOIN orders o ON u.id = o.id";
    let response = complete(
        &CompletionRequest {
            sql: sql.into(),
            cursor: 8,
            limit: None,
        },
        &snapshot(),
        Engine::Postgres,
    );
    let owners = response
        .candidates
        .iter()
        .filter(|candidate| candidate.label == "id")
        .filter_map(|candidate| candidate.qualified_name.as_deref())
        .collect::<Vec<_>>();
    assert_eq!(owners.len(), 2, "expected both owners, got {owners:?}");
    assert!(owners.iter().any(|owner| owner.ends_with(".users")));
    assert!(owners.iter().any(|owner| owner.ends_with(".orders")));
}

#[test]
fn tsql_three_part_alias_resolves_fields() {
    let sql = "SELECT u. FROM mock.public.users u";
    let response = complete(
        &CompletionRequest {
            sql: sql.into(),
            cursor: 9,
            limit: None,
        },
        &snapshot(),
        Engine::SqlServer,
    );
    assert!(response
        .candidates
        .iter()
        .any(|candidate| candidate.label == "email"));
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

#[test]
fn local_ctes_and_temp_tables_share_the_object_pipeline() {
    for sql in [
        "WITH recent AS (SELECT 1) SELECT * FROM rec",
        "CREATE TEMP TABLE scratch (id int); SELECT * FROM scr",
    ] {
        let response = complete(
            &CompletionRequest {
                sql: sql.into(),
                cursor: sql.len() as u32,
                limit: None,
            },
            &snapshot(),
            Engine::Postgres,
        );
        assert!(response.candidates.first().is_some_and(|candidate| {
            candidate.detail.as_deref() == Some("document-local relation")
        }));
    }
}

#[test]
fn table_valued_functions_are_available_in_from_slots() {
    let sql = "SELECT * FROM find";
    let response = complete(
        &CompletionRequest {
            sql: sql.into(),
            cursor: sql.len() as u32,
            limit: None,
        },
        &snapshot(),
        Engine::SqlServer,
    );
    assert!(response.candidates.iter().any(|candidate| {
        candidate.label == "find_users" && candidate.kind == CompletionKind::Function
    }));
}

#[test]
fn unresolved_cte_qualifier_falls_back_to_available_columns() {
    let sql = "WITH recent AS (SELECT 1) SELECT recent.e";
    let response = complete(
        &CompletionRequest {
            sql: sql.into(),
            cursor: sql.len() as u32,
            limit: None,
        },
        &snapshot(),
        Engine::Postgres,
    );
    assert!(response
        .candidates
        .iter()
        .any(|candidate| candidate.label == "email"));
}

#[test]
fn dictionary_deduplicates_schema_names_case_insensitively() {
    let mut snapshot = snapshot();
    snapshot.trees.push(CatalogTree {
        name: "other".into(),
        schemas: vec![SchemaTree {
            name: "PUBLIC".into(),
            objects: vec![ObjectInfo::new("users", ObjectKind::Table)],
        }],
    });
    let dictionary = Dictionary::from_snapshot(&snapshot);
    assert_eq!(dictionary.schemas, vec!["public"]);
    assert!(dictionary.resolve_qualified("public", "users").is_none());
    assert!(dictionary.resolve_reference("mock.public.users").is_some());
    assert!(dictionary.resolve_reference("other.public.users").is_some());
}

#[test]
fn mssql_array_subscript_does_not_become_a_bracketed_identifier_prefix() {
    let sql = "SELECT arr[0]";
    let cursor = sql.len() - 1;
    let analysis = detect_context(sql, cursor, Engine::SqlServer);
    assert_eq!(&sql[analysis.prefix_start..cursor], "0");
}
