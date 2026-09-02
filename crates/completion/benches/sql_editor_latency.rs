use std::sync::atomic::AtomicBool;

use criterion::{criterion_group, criterion_main, BatchSize, Criterion};
use sift_completion::{complete_with_dictionary, Dictionary};
use sift_protocol::completion::CompletionRequest;
use sift_protocol::{
    CatalogRevision, CatalogTree, ColumnMetadata, Engine, Nullability, ObjectInfo, ObjectKind,
    PrimitiveType, SchemaScope, SchemaSnapshot, SchemaTree, TypeRef,
};
use sift_semantic::{
    CatalogBindingColumn, CatalogBindingObject, CatalogBindingView, DocumentScope, SemanticRegistry,
};

fn catalog_snapshot(object_count: usize) -> SchemaSnapshot {
    SchemaSnapshot {
        trees: vec![CatalogTree {
            name: "bench".into(),
            schemas: vec![SchemaTree {
                name: "public".into(),
                objects: (0..object_count)
                    .map(|index| {
                        let mut object =
                            ObjectInfo::new(format!("relation_{index:06}"), ObjectKind::Table);
                        object.columns = vec![ColumnMetadata::new(
                            "id",
                            TypeRef::Primitive(PrimitiveType::Int64),
                        )];
                        object
                    })
                    .collect(),
            }],
        }],
        fetched_at: chrono::Utc::now(),
        scope: SchemaScope::shallow(),
        incomplete: false,
        graph: None,
    }
}

fn wide_catalog(columns: usize) -> CatalogBindingView {
    CatalogBindingView::new(
        CatalogRevision(1),
        true,
        vec![CatalogBindingObject {
            id: sift_protocol::CatalogObjectId("wide".into()),
            catalog: "bench".into(),
            schema: "public".into(),
            name: "wide".into(),
            qualified_name: "bench.public.wide".into(),
            kind: sift_protocol::CatalogNodeKind::Table,
            complete: true,
            comment: None,
            routine_args: None,
            return_type: None,
            columns: (0..columns)
                .map(|index| CatalogBindingColumn {
                    id: sift_protocol::CatalogObjectId(format!("wide-{index}")),
                    name: format!("column_{index:03}"),
                    type_ref: TypeRef::Primitive(PrimitiveType::Text),
                    nullable: Nullability::Nullable,
                    ordinal: Some(index as u32 + 1),
                })
                .collect(),
        }],
    )
}

fn semantic_document(lines: usize) -> String {
    (0..lines)
        .map(|line| format!("select {line} as id, 'payload-{line}' as payload;"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn benchmarks(criterion: &mut Criterion) {
    let snapshot = catalog_snapshot(100_000);
    let dictionary = Dictionary::from_snapshot(&snapshot);
    let request = CompletionRequest {
        sql: "select * from relation_099".into(),
        cursor: 26,
        limit: Some(50),
    };
    criterion.bench_function("warm_completion_100k_catalog", |bencher| {
        bencher.iter(|| complete_with_dictionary(&request, &dictionary, Engine::Postgres));
    });

    let source = semantic_document(8_000);
    criterion.bench_function("semantic_full_8000_lines", |bencher| {
        bencher.iter(|| {
            SemanticRegistry::default()
                .create(
                    DocumentScope {
                        session: 1,
                        connection: 1,
                    },
                    Engine::Postgres.dialect_id(),
                    source.clone(),
                    None,
                    &AtomicBool::new(false),
                )
                .unwrap()
        });
    });

    let incremental_source = semantic_document(100);
    criterion.bench_function("semantic_changed_statement_100_lines", |bencher| {
        bencher.iter_batched(
            || {
                let registry = SemanticRegistry::default();
                let scope = DocumentScope {
                    session: 2,
                    connection: 1,
                };
                let state = registry
                    .create(
                        scope,
                        Engine::Postgres.dialect_id(),
                        incremental_source.clone(),
                        None,
                        &AtomicBool::new(false),
                    )
                    .unwrap();
                (registry, scope, state)
            },
            |(registry, scope, state)| {
                let mut changed = incremental_source.clone();
                changed.push_str("\nselect 1;");
                registry
                    .update(
                        scope,
                        state.document_id,
                        state.revision,
                        changed,
                        &AtomicBool::new(false),
                    )
                    .unwrap()
            },
            BatchSize::SmallInput,
        );
    });

    let wide = wide_catalog(500);
    criterion.bench_function("star_expansion_500_columns", |bencher| {
        bencher.iter_batched(
            || {
                let registry = SemanticRegistry::default();
                let scope = DocumentScope {
                    session: 1,
                    connection: 1,
                };
                let state = registry
                    .create(
                        scope,
                        Engine::Postgres.dialect_id(),
                        "select * from wide".into(),
                        None,
                        &AtomicBool::new(false),
                    )
                    .unwrap();
                (registry, scope, state)
            },
            |(registry, scope, state)| {
                registry
                    .prepare_star_expansion(scope, state.document_id, state.revision, 7, &wide)
                    .unwrap()
            },
            BatchSize::SmallInput,
        );
    });

    let hover_registry = SemanticRegistry::default();
    let hover_scope = DocumentScope {
        session: 3,
        connection: 1,
    };
    let hover_state = hover_registry
        .create(
            hover_scope,
            Engine::Postgres.dialect_id(),
            "select w.column_499 from wide w".into(),
            None,
            &AtomicBool::new(false),
        )
        .unwrap();
    criterion.bench_function("warm_hover_500_columns", |bencher| {
        bencher.iter(|| {
            hover_registry
                .hover(
                    hover_scope,
                    hover_state.document_id,
                    hover_state.revision,
                    9,
                    Some(&wide),
                )
                .unwrap()
        });
    });

    let snippets =
        sift_snippets::SnippetIndex::build((0..2_000).map(|index| sift_protocol::SqlSnippet {
            id: None,
            tenant_id: None,
            workspace_id: None,
            owner_principal_id: None,
            trigger: format!("query_{index:04}"),
            title: format!("Query {index}"),
            description: String::new(),
            body: "select ${1:*} from ${2:table};$0".into(),
            dialects: vec![Engine::Postgres.dialect_id()],
            scope: sift_protocol::SnippetScope::Personal,
            revision: 1,
        }))
        .unwrap();
    criterion.bench_function("snippet_lookup_2000", |bencher| {
        bencher.iter(|| snippets.matching("query_19", &Engine::Postgres.dialect_id(), 50));
    });
}

criterion_group! {
    name = benches;
    config = Criterion::default().sample_size(10).warm_up_time(std::time::Duration::from_secs(1)).measurement_time(std::time::Duration::from_secs(2));
    targets = benchmarks
}
criterion_main!(benches);
