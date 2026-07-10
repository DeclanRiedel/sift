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
//!   2. Ask `sift_completion::complete` for candidates.
//!   3. If the detected context is `ExpectingColumn { qualifier }` and
//!      the qualifier resolves to a known table in the shallow snapshot,
//!      re-fetch a deep snapshot for that specific object and re-run
//!      the ranker so column candidates carry types.
//!
//! Step 3 is best-effort — if the object can't be located or the deep
//! fetch fails, the shallow result still ships. That mirrors the
//! `SchemaCache` philosophy: never let a slow / failed metadata query
//! stall a completion.

use sift_protocol::completion::{CompletionContext, CompletionRequest, CompletionResponse};
use sift_protocol::{
    ConnectionId, Engine, ObjectPath, SchemaDepth, SchemaScope, SchemaSnapshot, SessionId,
};

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
    let shallow_response =
        sift_completion::complete_with_dictionary(&req, &shallow.dictionary, engine);

    // If we're expecting a column and the qualifier resolves to a
    // shallow-known table, upgrade to a deep snapshot for that object.
    if let CompletionContext::ExpectingColumn { qualifier: Some(q) } = &shallow_response.context {
        if let Some(path) = resolve_object_path(&shallow.snapshot, q) {
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
                return Ok(sift_completion::complete(&req, &merged, engine));
            }
        }
    }
    Ok(shallow_response)
}

/// Locate an object in a shallow snapshot by unqualified name, returning
/// the fully-qualified [`ObjectPath`] needed for a deep fetch. Returns
/// `None` on ambiguity — same name in multiple schemas is left to a
/// future disambiguation pass.
fn resolve_object_path(snapshot: &SchemaSnapshot, name: &str) -> Option<ObjectPath> {
    let name_l = name.to_ascii_lowercase();
    let mut hit: Option<ObjectPath> = None;
    for catalog in &snapshot.trees {
        for schema in &catalog.schemas {
            for obj in &schema.objects {
                if obj.name.eq_ignore_ascii_case(&name_l) {
                    if hit.is_some() {
                        return None;
                    }
                    hit = Some(ObjectPath {
                        catalog: Some(catalog.name.clone()),
                        schema: Some(schema.name.clone()),
                        name: obj.name.clone(),
                        kind: Some(obj.kind),
                        routine_args: obj.routine_args.clone(),
                    });
                }
            }
        }
    }
    hit
}

/// Fold the columns from a deep snapshot back into a shallow snapshot's
/// object entry so the completion ranker sees both.
fn merge_deep_into_shallow(mut shallow: SchemaSnapshot, deep: SchemaSnapshot) -> SchemaSnapshot {
    let deep_objects: Vec<_> = deep
        .trees
        .into_iter()
        .flat_map(|c| c.schemas.into_iter())
        .flat_map(|s| s.objects.into_iter())
        .collect();
    for tree in &mut shallow.trees {
        for schema in &mut tree.schemas {
            for obj in &mut schema.objects {
                if let Some(d) = deep_objects
                    .iter()
                    .find(|d| d.name.eq_ignore_ascii_case(&obj.name))
                {
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
