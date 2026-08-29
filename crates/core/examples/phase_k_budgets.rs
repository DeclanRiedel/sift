//! Repeatable synthetic CPU/retained-byte budget harness.
//!
//! Run with:
//! `cargo run -p sift-core --release --example phase_k_budgets`

use std::time::Instant;

use sift_core::catalog::{graph_from_trees, validate_graph};
use sift_core::comparison::{compare, ComparisonDataset, ComparisonInput};
use sift_core::schema_diff::diff_catalogs;
use sift_protocol::{
    CatalogCoverage, CatalogGraph, CatalogGraphOptions, CatalogRevision, CatalogSourceRef,
    CatalogTree, ColumnMetadata, CompareColumnPair, Nullability, ObjectInfo, ObjectKind,
    PrimitiveType, ProviderRef, ResolvedCompareKey, Row, SchemaTree, TypeRef, Value,
};

const TABLES: usize = 10_000;
const COLUMNS_PER_TABLE: usize = 8;
const COMPARISON_ROWS: usize = 50_000;

fn main() {
    let started = Instant::now();
    let objects = (0..TABLES)
        .map(|table_index| {
            let mut table = ObjectInfo::new(format!("table_{table_index:05}"), ObjectKind::Table);
            table.columns = (0..COLUMNS_PER_TABLE)
                .map(|column_index| {
                    ColumnMetadata::new(
                        format!("column_{column_index:02}"),
                        TypeRef::Primitive(if column_index == 0 {
                            PrimitiveType::Int64
                        } else {
                            PrimitiveType::Text
                        }),
                    )
                })
                .collect();
            table
        })
        .collect();
    let trees = vec![CatalogTree {
        name: "budget".into(),
        schemas: vec![SchemaTree {
            name: "public".into(),
            objects,
        }],
    }];
    let build_started = Instant::now();
    let data = graph_from_trees(&trees, CatalogCoverage::complete(), "budget:postgres");
    let graph_build_ms = build_started.elapsed().as_millis();
    let validation_started = Instant::now();
    validate_graph(&data, 100_000, 500_000).expect("synthetic graph is valid");
    let graph_validate_ms = validation_started.elapsed().as_millis();
    let graph_bytes = serde_json::to_vec(&data).expect("graph serializes").len();
    let graph = CatalogGraph {
        revision: CatalogRevision(1),
        content_digest: "catfp:budget".into(),
        invalidation_epoch: 1,
        captured_at: chrono::Utc::now(),
        provider: ProviderRef {
            provider_id: sift_protocol::ProviderId::new("sift/postgres").unwrap(),
            dialect_id: sift_protocol::DialectId::new("sift/postgres").unwrap(),
            provider_version: "budget".into(),
        },
        database_identity: "budget".into(),
        data,
    };

    let mut desired = graph.clone();
    for node in desired
        .data
        .nodes
        .iter_mut()
        .filter(|node| node.kind == sift_protocol::CatalogNodeKind::Column)
        .take(100)
    {
        if let sift_protocol::CatalogNodeDetails::Column { column } = &mut node.details {
            column.nullable = Nullability::NotNullable;
        }
    }
    let source = CatalogSourceRef::Live {
        expected_revision: CatalogRevision(1),
        options: CatalogGraphOptions::default(),
    };
    let diff_started = Instant::now();
    let diff = diff_catalogs(source.clone(), &graph, source, &desired, &[], Some(1_000))
        .expect("synthetic diff succeeds");
    let diff_ms = diff_started.elapsed().as_millis();

    let comparison_columns = vec![
        ColumnMetadata::new("id", TypeRef::Primitive(PrimitiveType::Int64)),
        ColumnMetadata::new("value", TypeRef::Primitive(PrimitiveType::Text)),
    ];
    let left_rows = (0..COMPARISON_ROWS)
        .map(|index| {
            Row::new(vec![
                Value::Int64(index as i64),
                Value::Text(format!("value-{index}")),
            ])
        })
        .collect::<Vec<_>>();
    let mut right_rows = left_rows.clone();
    for (index, row) in right_rows.iter_mut().enumerate().step_by(1_000) {
        row.values[1] = Value::Text(format!("changed-{index}"));
    }
    let left_bytes = serde_json::to_vec(&left_rows)
        .expect("left rows serialize")
        .len();
    let right_bytes = serde_json::to_vec(&right_rows)
        .expect("right rows serialize")
        .len();
    let comparison_started = Instant::now();
    let comparison = compare(ComparisonInput {
        left: ComparisonDataset {
            columns: comparison_columns.clone(),
            rows: left_rows,
            immutable_order: true,
        },
        right: ComparisonDataset {
            columns: comparison_columns,
            rows: right_rows,
            immutable_order: true,
        },
        mappings: Vec::new(),
        key: ResolvedCompareKey {
            columns: vec![CompareColumnPair {
                left: "id".into(),
                right: "id".into(),
            }],
            inferred_constraint: None,
            row_ordinal: false,
        },
        tolerances: Vec::new(),
        max_diff_rows: 20_000,
        max_duplicate_group: 1_024,
        cancel: None,
    })
    .expect("synthetic comparison succeeds");
    let comparison_ms = comparison_started.elapsed().as_millis();

    println!(
        "{}",
        serde_json::json!({
            "catalog": {
                "tables": TABLES,
                "columns_per_table": COLUMNS_PER_TABLE,
                "nodes": graph.data.nodes.len(),
                "edges": graph.data.edges.len(),
                "serialized_bytes": graph_bytes,
                "build_ms": graph_build_ms,
                "validate_ms": graph_validate_ms
            },
            "diff": {
                "changes": diff.changes.len(),
                "elapsed_ms": diff_ms
            },
            "comparison": {
                "rows_per_side": COMPARISON_ROWS,
                "left_serialized_bytes": left_bytes,
                "right_serialized_bytes": right_bytes,
                "changed_rows": comparison.changed_rows,
                "elapsed_ms": comparison_ms
            },
            "total_elapsed_ms": started.elapsed().as_millis()
        })
    );
}
