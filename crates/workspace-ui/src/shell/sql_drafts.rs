//! Explicitly partial catalog previews and adapters for reviewable SQL templates.
//! Full definitions are loaded through the audited server DDL operation.

use super::*;

pub(super) fn quote_identifier(identifier: &str) -> String {
    format!("\"{}\"", identifier.replace('"', "\"\""))
}

pub(super) fn ddl_quote_identifier(
    provider_id: &sift_protocol::ProviderId,
    identifier: &str,
) -> String {
    if provider_id.as_str() == "sift/sql-server" {
        format!("[{}]", identifier.replace(']', "]]"))
    } else {
        quote_identifier(identifier)
    }
}

pub(super) fn object_designer_sql(source: &DatabaseObjectSource) -> Option<String> {
    sift_snippets::object_designer_sql(
        &source.provider_id,
        &source.schema,
        &source.object,
        source.object_kind,
    )
}

pub(super) fn table_preview_sql(
    provider_id: &sift_protocol::ProviderId,
    schema: &str,
    object: &str,
) -> String {
    sift_snippets::table_preview_sql(provider_id, schema, object)
        .unwrap_or_else(|| "-- Preview SQL is not available for this provider.".into())
}

pub(super) fn type_ref_label(type_ref: &sift_protocol::TypeRef) -> String {
    match type_ref {
        sift_protocol::TypeRef::Native { name, .. } => name.clone(),
        sift_protocol::TypeRef::Primitive(primitive) => match primitive {
            sift_protocol::PrimitiveType::Int16 => "smallint",
            sift_protocol::PrimitiveType::Int32 => "integer",
            sift_protocol::PrimitiveType::Int64 => "bigint",
            sift_protocol::PrimitiveType::Float32 => "real",
            sift_protocol::PrimitiveType::Float64 => "double precision",
            sift_protocol::PrimitiveType::Decimal => "decimal",
            sift_protocol::PrimitiveType::Bool => "boolean",
            sift_protocol::PrimitiveType::Text => "text",
            sift_protocol::PrimitiveType::Blob => "binary",
            sift_protocol::PrimitiveType::Date => "date",
            sift_protocol::PrimitiveType::Time => "time",
            sift_protocol::PrimitiveType::Timestamp => "timestamp",
            sift_protocol::PrimitiveType::TimestampTz => "timestamp with time zone",
            sift_protocol::PrimitiveType::Interval => "interval",
            sift_protocol::PrimitiveType::Uuid => "uuid",
            sift_protocol::PrimitiveType::Json => "json",
            sift_protocol::PrimitiveType::Jsonb => "jsonb",
        }
        .into(),
    }
}

pub(super) fn catalog_detail_ddl(
    source: &DatabaseObjectSource,
    node: &sift_protocol::CatalogNode,
) -> Option<String> {
    let qualified_table = format!(
        "{}.{}",
        ddl_quote_identifier(&source.provider_id, &source.schema),
        ddl_quote_identifier(&source.provider_id, &source.object)
    );
    match &node.details {
        sift_protocol::CatalogNodeDetails::Index { index } => {
            let columns = index
                .columns
                .iter()
                .map(|column| {
                    if column
                        .chars()
                        .all(|character| character.is_alphanumeric() || character == '_')
                    {
                        ddl_quote_identifier(&source.provider_id, column)
                    } else {
                        column.clone()
                    }
                })
                .collect::<Vec<_>>()
                .join(", ");
            let mut ddl = format!(
                "-- Catalog-derived index DDL preview\nCREATE {}INDEX {} ON {} ({})",
                if index.unique { "UNIQUE " } else { "" },
                ddl_quote_identifier(&source.provider_id, &index.name),
                qualified_table,
                columns
            );
            if let Some(predicate) = &index.partial_predicate {
                ddl.push_str(" WHERE ");
                ddl.push_str(predicate);
            }
            ddl.push(';');
            Some(ddl)
        }
        sift_protocol::CatalogNodeDetails::Constraint { constraint } => Some(
            constraint
                .definition
                .as_ref()
                .map(|definition| {
                    format!(
                        "-- Catalog relation definition\nALTER TABLE {} ADD CONSTRAINT {} {};",
                        qualified_table,
                        ddl_quote_identifier(&source.provider_id, &constraint.name),
                        definition.trim().trim_end_matches(';')
                    )
                })
                .unwrap_or_else(|| {
                    format!(
                        "-- DDL definition is unavailable for relation {}.",
                        constraint.name
                    )
                }),
        ),
        sift_protocol::CatalogNodeDetails::Trigger { trigger } => {
            Some(trigger.definition.clone().unwrap_or_else(|| {
                format!(
                    "-- DDL definition is unavailable for trigger {}.",
                    trigger.name
                )
            }))
        }
        _ => None,
    }
}

pub(super) fn catalog_columns_ddl(
    source: &DatabaseObjectSource,
    graph: &sift_protocol::CatalogGraph,
    table_id: &sift_protocol::CatalogObjectId,
) -> String {
    let qualified_relation = format!(
        "{}.{}",
        ddl_quote_identifier(&source.provider_id, &source.schema),
        ddl_quote_identifier(&source.provider_id, &source.object)
    );
    let columns = table_columns(graph, table_id)
        .into_iter()
        .filter_map(|node| {
            let sift_protocol::CatalogNodeDetails::Column { column } = &node.details else {
                return None;
            };
            Some(format!(
                "    {} {}{}",
                ddl_quote_identifier(&source.provider_id, &node.name),
                type_ref_label(&column.type_ref),
                if column.nullable == sift_protocol::Nullability::NotNullable {
                    " NOT NULL"
                } else {
                    ""
                }
            ))
        })
        .collect::<Vec<_>>();
    if matches!(
        source.object_kind,
        sift_protocol::ObjectKind::Table
            | sift_protocol::ObjectKind::ForeignTable
            | sift_protocol::ObjectKind::PartitionedTable
    ) {
        let scope =
            "-- Columns only; indexes, constraints, triggers, storage, and grants are omitted.";
        format!(
            "-- Catalog-derived relation DDL preview\n{scope}\nCREATE TABLE {qualified_relation} (\n{}\n);",
            columns.join(",\n")
        )
    } else {
        format!(
            "-- Catalog-derived column metadata for {qualified_relation}\n-- Use Full DDL to request the exact server definition.\n{}",
            columns.join("\n")
        )
    }
}
