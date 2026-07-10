//! Autocomplete orchestration (Phase D).
//!
//! Server-side composition on top of the existing `Driver::schema()` +
//! `SchemaCache` — no new `Driver` trait method (ADR-017), no protocol
//! bump. The heavy lifting (SQL context detection, ranking, keyword
//! tables) lives in `sift-completion`; this file resolves the schema
//! snapshot(s) the ranker consumes.
//!
//! Strategy:
//!   1. Fetch the shallow snapshot for the connection (through the
//!      cache).
//!   2. Detect SQL context once and rank candidates from the cached dictionary.
//!   3. If the detected context is `ExpectingColumn { qualifier }` and
//!      the qualifier resolves to a known table in the shallow snapshot,
//!      re-fetch a deep snapshot for that specific object and re-run
//!      the ranker so column candidates carry types.
//!
//! Step 3 is best-effort — if the object can't be located or the deep
//! fetch fails, the shallow result still ships. That mirrors the
//! `SchemaCache` philosophy: never let a slow / failed metadata query
//! stall a completion.

use std::collections::HashMap;

use sift_protocol::completion::{CompletionContext, CompletionRequest, CompletionResponse};
use sift_protocol::{ConnectionId, Engine, SchemaDepth, SchemaScope, SchemaSnapshot, SessionId};

use crate::error::ApiResult;
use crate::session::SessionStore;

pub async fn generate_completion(
    registry: &SessionStore,
    session_id: SessionId,
    conn_id: ConnectionId,
    engine: Engine,
    req: CompletionRequest,
) -> ApiResult<CompletionResponse> {
    let shallow = registry
        .schema_cached(session_id, conn_id, SchemaScope::shallow())
        .await?;
    let cursor = usize::min(req.cursor as usize, req.sql.len());
    let ctx = sift_completion::detect_context(&req.sql, cursor, engine);
    let shallow_response =
        sift_completion::complete_with_context(&req, &ctx, &shallow.dictionary, engine);

    // If we're expecting a column and the qualifier resolves to a
    // shallow-known table, upgrade to a deep snapshot for that object.
    if let CompletionContext::ExpectingColumn { qualifier: Some(q) } = &shallow_response.context {
        if let Some(path) = shallow.dictionary.resolve_object_path(q) {
            let deep_scope = SchemaScope {
                depth: SchemaDepth::Deep { object: path },
                filter: None,
            };
            if let Ok(deep) = registry
                .schema_cached(session_id, conn_id, deep_scope)
                .await
            {
                let merged =
                    merge_deep_into_shallow((*shallow.snapshot).clone(), (*deep.snapshot).clone());
                let dict = sift_completion::Dictionary::from_snapshot(&merged);
                return Ok(sift_completion::complete_with_context(
                    &req, &ctx, &dict, engine,
                ));
            }
        }
    }
    Ok(shallow_response)
}

/// Fold the columns from a deep snapshot back into a shallow snapshot's
/// object entry so the completion ranker sees both.
fn merge_deep_into_shallow(mut shallow: SchemaSnapshot, deep: SchemaSnapshot) -> SchemaSnapshot {
    let mut deep_objects = HashMap::new();
    for catalog in deep.trees {
        for schema in catalog.schemas {
            for obj in schema.objects {
                deep_objects.insert(
                    object_key(Some(&catalog.name), Some(&schema.name), &obj.name),
                    obj,
                );
            }
        }
    }
    for tree in &mut shallow.trees {
        for schema in &mut tree.schemas {
            for obj in &mut schema.objects {
                let key = object_key(Some(&tree.name), Some(&schema.name), &obj.name);
                if let Some(d) = deep_objects.get(&key) {
                    obj.columns = d.columns.clone();
                    obj.indexes = d.indexes.clone();
                    obj.constraints = d.constraints.clone();
                    obj.triggers = d.triggers.clone();
                    obj.routine_args = d.routine_args.clone();
                }
            }
        }
    }
    shallow
}

fn object_key(
    catalog: Option<&str>,
    schema: Option<&str>,
    name: &str,
) -> (Option<String>, Option<String>, String) {
    (
        catalog.map(str::to_ascii_lowercase),
        schema.map(str::to_ascii_lowercase),
        name.to_ascii_lowercase(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use sift_protocol::{
        CatalogTree, ColumnMetadata, Nullability, ObjectInfo, ObjectKind, PrimitiveType,
        SchemaTree, TypeRef,
    };

    fn col(name: &str) -> ColumnMetadata {
        ColumnMetadata {
            name: name.into(),
            type_ref: TypeRef::Primitive(PrimitiveType::Text),
            nullable: Nullability::Nullable,
            auto_increment: false,
            primary_key: false,
            facets: Default::default(),
        }
    }

    fn snapshot(public_orders: ObjectInfo, sales_orders: ObjectInfo) -> SchemaSnapshot {
        SchemaSnapshot {
            trees: vec![CatalogTree {
                name: "mock".into(),
                schemas: vec![
                    SchemaTree {
                        name: "public".into(),
                        objects: vec![public_orders],
                    },
                    SchemaTree {
                        name: "sales".into(),
                        objects: vec![sales_orders],
                    },
                ],
            }],
            fetched_at: chrono::Utc::now(),
            scope: SchemaScope::shallow(),
            incomplete: false,
        }
    }

    #[test]
    fn deep_merge_matches_catalog_schema_and_name() {
        let public_orders = ObjectInfo::new("orders", ObjectKind::Table);
        let sales_orders = ObjectInfo::new("orders", ObjectKind::Table);
        let shallow = snapshot(public_orders, sales_orders);

        let mut public_deep = ObjectInfo::new("orders", ObjectKind::Table);
        public_deep.columns = vec![col("public_only")];
        let sales_deep = ObjectInfo::new("orders", ObjectKind::Table);
        let deep = snapshot(public_deep, sales_deep);

        let merged = merge_deep_into_shallow(shallow, deep);
        let public = &merged.trees[0].schemas[0].objects[0];
        let sales = &merged.trees[0].schemas[1].objects[0];
        assert_eq!(public.columns[0].name, "public_only");
        assert!(sales.columns.is_empty());
    }
}
