//! Public-API smoke tests for `sift-completion`. Covers the three
//! contract points: context detection (FROM slot, dotted qualifier), ranking order
//! (prefix > substring), engine-specific identifier quoting.

use sift_completion::{complete, detect_context, Dictionary};
use sift_protocol::completion::{CompletionContext, CompletionKind, CompletionRequest};
use sift_protocol::{
    CatalogTree, ColumnMetadata, Engine, Nullability, ObjectInfo, ObjectKind, PrimitiveType,
    SchemaScope, SchemaSnapshot, SchemaTree, TypeRef,
};

#[derive(serde::Deserialize)]
struct GoldenCompletionCase {
    name: String,
    engine: String,
    dialect_id: String,
    connection: String,
    database: String,
    catalog_revision: u64,
    sql_with_cursor: String,
    cursor: usize,
    expected_context: String,
    expected_range: [u32; 2],
    ordered_top_candidates: Vec<String>,
    forbidden_candidates: Vec<String>,
}

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
    let mut calculate_one = ObjectInfo::new("calculate", ObjectKind::ScalarFunction);
    calculate_one.routine_args = Some(vec!["int".into()]);
    let mut calculate_two = ObjectInfo::new("calculate", ObjectKind::ScalarFunction);
    calculate_two.routine_args = Some(vec!["int".into(), "int".into()]);
    SchemaSnapshot {
        trees: vec![CatalogTree {
            name: "mock".into(),
            schemas: vec![SchemaTree {
                name: "public".into(),
                objects: vec![
                    users,
                    orders,
                    user_events,
                    quoted,
                    routine,
                    calculate_one,
                    calculate_two,
                ],
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
    assert_eq!(top.insert, "public.users");
    assert_eq!(top.qualified_name.as_deref(), Some("mock.public.users"));
    assert_eq!(top.kind, CompletionKind::Table);
    assert_eq!(resp.replaced_range.start, 14);
    assert_eq!(resp.replaced_range.end, 16);
}

#[test]
fn table_slot_category_stays_ahead_of_keywords() {
    let mut catalog = snapshot();
    catalog.trees[0].schemas[0]
        .objects
        .push(ObjectInfo::new("server_events", ObjectKind::Table));
    let response = complete(
        &CompletionRequest {
            sql: "SELECT * FROM se".into(),
            cursor: 16,
            limit: None,
        },
        &catalog,
        Engine::Postgres,
    );
    let table = response
        .candidates
        .iter()
        .position(|candidate| candidate.label == "server_events")
        .expect("prefix-matched table");
    let keyword = response
        .candidates
        .iter()
        .position(|candidate| candidate.label == "SELECT")
        .expect("prefix-matched keyword");
    assert!(table < keyword, "FROM context must rank tables first");
}

#[test]
fn source_schema_stays_ahead_of_tables_in_from_slots() {
    let mut catalog = snapshot();
    catalog.trees[0].schemas[0]
        .objects
        .push(ObjectInfo::new("projects", ObjectKind::Table));
    let response = complete(
        &CompletionRequest {
            sql: "SELECT * FROM p".into(),
            cursor: 15,
            limit: None,
        },
        &catalog,
        Engine::Postgres,
    );
    let source = response
        .candidates
        .iter()
        .position(|candidate| candidate.kind == CompletionKind::Schema)
        .expect("source schema");
    let table = response
        .candidates
        .iter()
        .position(|candidate| candidate.label == "projects")
        .expect("table");
    assert!(source < table, "source must rank before table");
    assert_eq!(response.candidates[table].insert, "public.projects");
}

#[test]
fn qualified_from_slot_filters_out_non_relation_objects() {
    let mut catalog = snapshot();
    catalog.trees[0].schemas[0]
        .objects
        .push(ObjectInfo::new("account_status", ObjectKind::Type));
    let sql = "SELECT * FROM public.";
    let response = complete(
        &CompletionRequest {
            sql: sql.into(),
            cursor: sql.len() as u32,
            limit: None,
        },
        &catalog,
        Engine::Postgres,
    );
    assert!(matches!(
        response.context,
        CompletionContext::ExpectingObjectInSchema { .. }
    ));
    assert!(response
        .candidates
        .iter()
        .any(|candidate| candidate.label == "users"));
    assert!(response
        .candidates
        .iter()
        .any(|candidate| candidate.label == "find_users"));
    assert!(!response
        .candidates
        .iter()
        .any(|candidate| candidate.label == "calculate"));
    assert!(!response
        .candidates
        .iter()
        .any(|candidate| candidate.label == "account_status"));
}

#[test]
fn duplicate_table_names_insert_the_minimum_safe_qualifier() {
    let mut catalog = snapshot();
    catalog.trees[0].schemas.push(SchemaTree {
        name: "archive".into(),
        objects: vec![ObjectInfo::new("users", ObjectKind::Table)],
    });
    let response = complete(
        &CompletionRequest {
            sql: "SELECT * FROM us".into(),
            cursor: 16,
            limit: None,
        },
        &catalog,
        Engine::Postgres,
    );
    let inserts = response
        .candidates
        .iter()
        .filter(|candidate| candidate.label == "users")
        .map(|candidate| candidate.insert.as_ref())
        .collect::<Vec<_>>();
    assert!(inserts.contains(&"public.users"));
    assert!(inserts.contains(&"archive.users"));

    let qualified = complete(
        &CompletionRequest {
            sql: "SELECT * FROM archive.us".into(),
            cursor: 24,
            limit: None,
        },
        &catalog,
        Engine::Postgres,
    );
    let users = qualified
        .candidates
        .iter()
        .find(|candidate| candidate.label == "users")
        .expect("archive users completion");
    assert_eq!(users.insert, "users", "the qualifier is already in the SQL");
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
    assert_eq!(pg_entry.insert, "public.\"MyTable\"");
    assert_eq!(mssql_entry.insert, "public.[MyTable]");
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
fn temp_and_tsql_pseudo_relations_offer_only_their_bound_columns() {
    let cases = [
        (
            Engine::Postgres,
            "CREATE TEMP TABLE scratch (local_id int, note text); SELECT scratch. FROM scratch",
            &["local_id", "note"][..],
        ),
        (
            Engine::SqlServer,
            "UPDATE users SET email = 'x' OUTPUT inserted.",
            &["id", "email"][..],
        ),
    ];
    for (engine, sql, expected) in cases {
        let qualifier = if engine == Engine::Postgres {
            "scratch."
        } else {
            "inserted."
        };
        let cursor = sql.find(qualifier).unwrap() + qualifier.len();
        let response = complete(
            &CompletionRequest {
                sql: sql.into(),
                cursor: cursor as u32,
                limit: None,
            },
            &snapshot(),
            engine,
        );
        let labels = response
            .candidates
            .iter()
            .map(|candidate| candidate.label.as_ref())
            .collect::<Vec<_>>();
        for expected in expected {
            assert!(
                labels.contains(expected),
                "{expected} missing from {labels:?}"
            );
        }
    }
}

#[test]
fn routine_overloads_remain_distinct_completion_candidates() {
    let sql = "cal";
    let response = complete(
        &CompletionRequest {
            sql: sql.into(),
            cursor: sql.len() as u32,
            limit: None,
        },
        &snapshot(),
        Engine::Postgres,
    );
    let overloads = response
        .candidates
        .iter()
        .filter(|candidate| candidate.label == "calculate")
        .collect::<Vec<_>>();
    assert_eq!(overloads.len(), 2);
    assert_ne!(overloads[0].detail, overloads[1].detail);
}

#[test]
fn golden_dialect_completion_corpus() {
    let cases: Vec<GoldenCompletionCase> =
        serde_json::from_str(include_str!("fixtures/dialect-completion-corpus.json")).unwrap();
    for case in cases {
        let engine = match case.engine.as_str() {
            "postgres" => Engine::Postgres,
            "sql_server" => Engine::SqlServer,
            other => panic!("unknown engine {other}"),
        };
        assert_eq!(case.dialect_id, engine.dialect_id().as_str());
        assert!(!case.connection.is_empty() && !case.database.is_empty());
        assert_eq!(case.catalog_revision, 73);
        assert_eq!(case.sql_with_cursor.matches('|').count(), 1);
        let marker = case.sql_with_cursor.find('|').unwrap();
        assert_eq!(marker, case.cursor, "{} cursor drifted", case.name);
        let sql = case.sql_with_cursor.replace('|', "");
        let response = complete(
            &CompletionRequest {
                sql,
                cursor: case.cursor as u32,
                limit: Some(50),
            },
            &snapshot(),
            engine,
        );
        let context = match response.context {
            CompletionContext::Statement => "statement",
            CompletionContext::ExpectingTable => "table",
            CompletionContext::ExpectingColumn { .. } => "column",
            CompletionContext::ExpectingObjectInSchema { .. } => "object_in_schema",
            CompletionContext::Unknown => "unknown",
        };
        assert_eq!(context, case.expected_context, "{} context", case.name);
        assert_eq!(
            [response.replaced_range.start, response.replaced_range.end],
            case.expected_range,
            "{} replacement range",
            case.name
        );
        let labels = response
            .candidates
            .iter()
            .map(|candidate| candidate.label.as_ref())
            .collect::<Vec<_>>();
        for (index, expected) in case.ordered_top_candidates.iter().enumerate() {
            assert_eq!(
                labels.get(index),
                Some(&expected.as_str()),
                "{} rank",
                case.name
            );
        }
        for forbidden in &case.forbidden_candidates {
            assert!(
                !labels.contains(&forbidden.as_str()),
                "{} leaked {forbidden}",
                case.name
            );
        }
    }
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
