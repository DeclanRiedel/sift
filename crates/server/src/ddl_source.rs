//! Offline DDL parsing into the same canonical catalog graph used by live providers.

use std::collections::BTreeMap;

use chrono::Utc;
use sha2::{Digest, Sha256};
use sift_protocol::{
    CatalogCoverage, CatalogCoverageFailure, CatalogCoverageState, CatalogGraph, CatalogRevision,
    CatalogTree, ColumnMetadata, DdlDiagnostic, DdlSourceCoverage, DialectId, ObjectInfo,
    ObjectKind, PrimitiveType, ProviderId, ProviderRef, SchemaTree, TypeRef, WorkspacePath,
};
use sqlparser::ast::{ColumnOption, Statement};
use sqlparser::dialect::{MsSqlDialect, PostgreSqlDialect};
use sqlparser::parser::Parser;

#[derive(Debug, Clone)]
pub struct DdlInput {
    pub path: WorkspacePath,
    pub text: String,
}

pub struct DdlBuild {
    pub graph: Option<CatalogGraph>,
    pub diagnostics: Vec<DdlDiagnostic>,
    pub coverage: DdlSourceCoverage,
}

pub fn build_model(dialect_id: &str, revision: u64, inputs: &[DdlInput]) -> DdlBuild {
    let mut diagnostics = Vec::new();
    let mut schemas = BTreeMap::<String, Vec<ObjectInfo>>::new();
    for input in inputs {
        let parsed = match dialect_id {
            "sift/postgres" => Parser::parse_sql(&PostgreSqlDialect {}, &input.text),
            "sift/sql-server" => Parser::parse_sql(&MsSqlDialect {}, &input.text),
            _ => {
                diagnostics.push(DdlDiagnostic {
                    path: input.path.clone(),
                    message: "unsupported DDL dialect".into(),
                    error: true,
                });
                continue;
            }
        };
        let statements = match parsed {
            Ok(statements) => statements,
            Err(error) => {
                diagnostics.push(DdlDiagnostic {
                    path: input.path.clone(),
                    message: error.to_string(),
                    error: true,
                });
                continue;
            }
        };
        for statement in statements {
            match statement {
                Statement::CreateTable(table) => {
                    let (schema, name) = object_name_parts(&table.name.to_string());
                    let mut object = ObjectInfo::new(name, ObjectKind::Table);
                    object.columns = table
                        .columns
                        .into_iter()
                        .map(|column| {
                            let mut metadata = ColumnMetadata::new(
                                unquote(&column.name.to_string()),
                                ddl_type(dialect_id, &column.data_type.to_string()),
                            );
                            for option in column.options {
                                match option.option {
                                    ColumnOption::NotNull => {
                                        metadata.nullable = sift_protocol::Nullability::NotNullable
                                    }
                                    ColumnOption::Null => {
                                        metadata.nullable = sift_protocol::Nullability::Nullable
                                    }
                                    ColumnOption::Unique { is_primary, .. } if is_primary => {
                                        metadata.primary_key = true;
                                        metadata.nullable = sift_protocol::Nullability::NotNullable;
                                    }
                                    _ => {}
                                }
                            }
                            metadata
                        })
                        .collect();
                    schemas.entry(schema).or_default().push(object);
                }
                Statement::CreateView { name, .. } => {
                    let (schema, name) = object_name_parts(&name.to_string());
                    schemas
                        .entry(schema)
                        .or_default()
                        .push(ObjectInfo::new(name, ObjectKind::View));
                }
                Statement::CreateSchema { schema_name, .. } => {
                    schemas
                        .entry(unquote(&schema_name.to_string()))
                        .or_default();
                }
                other => diagnostics.push(DdlDiagnostic {
                    path: input.path.clone(),
                    message: format!(
                        "statement is valid but not represented in the v1 offline graph: {}",
                        statement_name(&other)
                    ),
                    error: false,
                }),
            }
        }
    }
    for objects in schemas.values_mut() {
        objects.sort_by(|left, right| left.name.cmp(&right.name));
    }
    let has_errors = diagnostics.iter().any(|diagnostic| diagnostic.error);
    let coverage = if schemas.is_empty() && has_errors {
        DdlSourceCoverage::Invalid
    } else if has_errors || !diagnostics.is_empty() {
        DdlSourceCoverage::Partial
    } else {
        DdlSourceCoverage::Complete
    };
    let trees = vec![CatalogTree {
        name: "workspace".into(),
        schemas: schemas
            .into_iter()
            .map(|(name, objects)| SchemaTree { name, objects })
            .collect(),
    }];
    let graph = if coverage == DdlSourceCoverage::Invalid {
        None
    } else {
        let graph_coverage = if coverage == DdlSourceCoverage::Complete {
            CatalogCoverage::complete()
        } else {
            CatalogCoverage {
                state: CatalogCoverageState::Partial,
                requested_kinds: Vec::new(),
                covered_schemas: trees[0]
                    .schemas
                    .iter()
                    .map(|schema| schema.name.clone())
                    .collect(),
                omitted_schemas: Vec::new(),
                truncated_at_nodes: None,
                failures: diagnostics
                    .iter()
                    .filter(|diagnostic| diagnostic.error)
                    .map(|_| CatalogCoverageFailure {
                        stage: "ddl_parse".into(),
                        schema: None,
                        code: "invalid_ddl".into(),
                    })
                    .collect(),
            }
        };
        let data = sift_core::catalog::graph_from_trees(
            &trees,
            graph_coverage,
            &format!("ddl:{dialect_id}"),
        );
        let encoded = serde_json::to_vec(&data).expect("catalog graph data serializes");
        Some(CatalogGraph {
            revision: CatalogRevision(revision),
            content_digest: format!("{:x}", Sha256::digest(encoded)),
            invalidation_epoch: revision,
            captured_at: Utc::now(),
            provider: ProviderRef {
                provider_id: ProviderId::new(dialect_id).expect("locked dialect ids are valid"),
                dialect_id: DialectId::new(dialect_id).expect("locked dialect ids are valid"),
                provider_version: env!("CARGO_PKG_VERSION").into(),
            },
            database_identity: format!("workspace:{dialect_id}"),
            data,
        })
    };
    DdlBuild {
        graph,
        diagnostics,
        coverage,
    }
}

fn object_name_parts(name: &str) -> (String, String) {
    let parts = name.split('.').map(unquote).collect::<Vec<_>>();
    match parts.as_slice() {
        [name] => ("public".into(), name.clone()),
        [.., schema, name] => (schema.clone(), name.clone()),
        [] => ("public".into(), name.into()),
    }
}

fn unquote(value: &str) -> String {
    value
        .trim_matches(|character| matches!(character, '"' | '[' | ']' | '`'))
        .to_string()
}

fn ddl_type(dialect_id: &str, name: &str) -> TypeRef {
    let normalized = name.to_ascii_lowercase();
    let primitive = if normalized.contains("bigint") {
        Some(PrimitiveType::Int64)
    } else if normalized.contains("smallint") {
        Some(PrimitiveType::Int16)
    } else if normalized == "int" || normalized.contains("integer") {
        Some(PrimitiveType::Int32)
    } else if normalized.contains("bool") || normalized == "bit" {
        Some(PrimitiveType::Bool)
    } else if normalized.contains("timestamp") || normalized.contains("datetime") {
        Some(PrimitiveType::Timestamp)
    } else if normalized == "date" {
        Some(PrimitiveType::Date)
    } else if normalized.contains("jsonb") {
        Some(PrimitiveType::Jsonb)
    } else if normalized.contains("json") {
        Some(PrimitiveType::Json)
    } else if normalized.contains("char") || normalized.contains("text") {
        Some(PrimitiveType::Text)
    } else {
        None
    };
    primitive
        .map(TypeRef::Primitive)
        .unwrap_or_else(|| TypeRef::Native {
            provider_id: ProviderId::new(dialect_id).expect("locked dialect ids are valid"),
            name: name.into(),
            category: sift_protocol::TypeCategory::Other,
        })
}

fn statement_name(statement: &Statement) -> &'static str {
    match statement {
        Statement::CreateIndex(_) => "create_index",
        Statement::CreateSequence { .. } => "create_sequence",
        Statement::CreateFunction { .. } => "create_function",
        Statement::CreateProcedure { .. } => "create_procedure",
        Statement::CreateType { .. } => "create_type",
        Statement::AlterTable { .. } => "alter_table",
        _ => "other",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_ddl_builds_the_same_structural_graph() {
        let input = vec![DdlInput {
            path: WorkspacePath("schema.sql".into()),
            text: "CREATE TABLE public.users (id bigint primary key, name text not null);".into(),
        }];
        let left = build_model("sift/postgres", 1, &input);
        let right = build_model("sift/postgres", 1, &input);
        assert_eq!(
            left.graph.as_ref().unwrap().content_digest,
            right.graph.as_ref().unwrap().content_digest
        );
        assert_eq!(left.coverage, DdlSourceCoverage::Complete);
    }
}
