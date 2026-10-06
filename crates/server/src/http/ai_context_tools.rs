//! Bounded historical reads and revision-bound object DDL (ADR-104).
use super::*;
use sha2::{Digest, Sha256};
use sift_protocol::{AiToolKind, AiToolParameters, InvokeAiToolRequest};

pub(super) fn validate_ai_tool_input(request: &InvokeAiToolRequest, max_sql: u64) -> ApiResult<()> {
    let valid = match request.tool {
        AiToolKind::Diagnostics | AiToolKind::Explain | AiToolKind::Select => {
            request.parameters.is_none()
                && request
                    .sql
                    .as_ref()
                    .is_some_and(|sql| !sql.trim().is_empty() && sql.len() as u64 <= max_sql)
        }
        AiToolKind::Schema
        | AiToolKind::Catalog
        | AiToolKind::QueryHistory
        | AiToolKind::PlanCaptures => request.sql.is_none() && request.parameters.is_none(),
        AiToolKind::ObjectDdl => {
            request.sql.is_none()
                && matches!(&request.parameters,Some(AiToolParameters::ObjectDdl {object_id,..}) if !object_id.0.is_empty() && object_id.0.len()<=4096)
        }
        AiToolKind::PlanCapture => {
            request.sql.is_none()
                && matches!(
                    request.parameters,
                    Some(AiToolParameters::PlanCapture { .. })
                )
        }
    };
    if !valid {
        return Err(ApiError::BadRequest(
            "AI tool arguments do not match the bounded tool contract".into(),
        ));
    }
    Ok(())
}

pub(super) async fn historical_ai_tool(
    state: &AppState,
    auth: &AuthContext,
    run: &sift_metadata::AiAuthorizedToolRun,
    session: sift_protocol::SessionId,
    connection: sift_protocol::ConnectionId,
    request: &InvokeAiToolRequest,
) -> ApiResult<serde_json::Value> {
    let (_, tenant, profile, _) = state.sessions.managed_catalog_scope(
        session,
        connection,
        sift_protocol::OperationKind::ReadCatalogGraph,
    )?;
    if run.context.target.tenant_id != Some(tenant.0)
        || run.context.target.profile_id != Some(profile.0)
    {
        return Err(ApiError::Forbidden(
            "AI historical resource scope changed".into(),
        ));
    }
    let metadata = metadata_store_cloned(state)?;
    let actor = auth.principal_id;
    let public = run.visibility == sift_protocol::AiVisibility::RoomPublic;
    let limit = state.auth.ai.max_tool_result_bytes.min(512 * 1024) as usize;
    match request.tool {
        AiToolKind::QueryHistory => {
            let room = public
                .then(|| run.context.target.room_id.map(RoomId))
                .flatten();
            if public && room.is_none() {
                return Err(ApiError::Forbidden(
                    "AI room history requires a room".into(),
                ));
            }
            let mut history = metadata_blocking(move || {
                metadata
                    .ai_query_history(tenant, profile, actor, room)
                    .map_err(Into::into)
            })
            .await?;
            let mut truncated = history.len() > 20;
            history.truncate(20);
            let mut sql_excerpts = Vec::new();
            for entry in &mut history {
                let sql_changed = shorten(&mut entry.sql_text, (limit / 8).min(4096));
                if sql_changed {
                    sql_excerpts.push(entry.id.0);
                }
                truncated |= sql_changed;
                if let Some(error) = &mut entry.error_message {
                    truncated |= shorten(error, 1024);
                }
            }
            let mut value = json!({"items":history,"truncated":truncated,"sql_excerpt_ids":sql_excerpts,"scope":if public {"current_room_and_profile"} else {"initiator_and_current_profile"}});
            bound_items(&mut value, limit)?;
            Ok(value)
        }
        AiToolKind::PlanCaptures => {
            // Captures have no room-public label. Only explicit published
            // attachment snapshots may enter a public turn in the next stage.
            if public {
                return Ok(
                    json!({"items":[],"truncated":false,"notice":"Saved plans require an explicitly published attachment in room-public chats."}),
                );
            }
            let mut captures = metadata_blocking(move || {
                metadata
                    .ai_plan_captures(tenant, profile, actor)
                    .map_err(Into::into)
            })
            .await?;
            let truncated = captures.len() > 20;
            captures.truncate(20);
            let mut value = json!({"items":captures,"truncated":truncated,"scope":"initiator_and_current_profile"});
            bound_items(&mut value, limit)?;
            Ok(value)
        }
        AiToolKind::PlanCapture => {
            if public {
                return Err(ApiError::Forbidden(
                    "This saved plan has no room-public attachment proof".into(),
                ));
            }
            let Some(AiToolParameters::PlanCapture { capture_id }) = request.parameters else {
                return Err(ApiError::BadRequest(
                    "Saved plan identity is required".into(),
                ));
            };
            let mut capture = metadata_blocking(move || {
                metadata
                    .ai_plan_capture(tenant, profile, actor, capture_id)
                    .map_err(Into::into)
            })
            .await?;
            capture.raw_response = None;
            // Reserve half the envelope for warnings and capture provenance.
            let mut nodes = (limit / 4096).clamp(1, 128);
            let mut truncated = trim_plan(&mut capture.root, &mut nodes, 0);
            if capture.warnings.len() > 16 {
                capture.warnings.truncate(16);
                truncated = true;
            }
            for warning in &mut capture.warnings {
                truncated |= shorten(&mut warning.message, 512);
            }
            Ok(
                json!({"capture":capture,"truncated":truncated,"notice":"This is a saved historical plan. No statement was executed to read it."}),
            )
        }
        _ => Err(ApiError::BadRequest("Not a historical AI tool".into())),
    }
}

fn bound_items(value: &mut serde_json::Value, limit: usize) -> ApiResult<()> {
    while serde_json::to_vec(value)
        .map_err(|_| ApiError::Internal("Cannot encode historical AI resource".into()))?
        .len()
        > limit
    {
        let items = value
            .get_mut("items")
            .and_then(serde_json::Value::as_array_mut)
            .ok_or_else(|| ApiError::Internal("Invalid historical AI envelope".into()))?;
        if items.pop().is_none() {
            return Err(ApiError::BadRequest(
                "AI resource envelope exceeds the instance limit".into(),
            ));
        }
        value["truncated"] = json!(true);
    }
    Ok(())
}
fn shorten(text: &mut String, limit: usize) -> bool {
    if text.len() <= limit {
        return false;
    }
    let mut end = limit;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text.truncate(end);
    true
}
fn trim_plan(node: &mut sift_protocol::PlanNode, remaining: &mut usize, depth: usize) -> bool {
    *remaining = remaining.saturating_sub(1);
    let mut truncated = shorten(&mut node.op, 256);
    if let Some(relation) = &mut node.relation {
        truncated |= shorten(relation, 512);
    }
    if serde_json::to_vec(&node.extra).map_or(true, |bytes| bytes.len() > 1024) {
        node.extra.clear();
        truncated = true;
    }
    if depth >= 8 || *remaining == 0 {
        truncated |= !node.children.is_empty();
        node.children.clear();
        return truncated;
    }
    let mut retained = 0;
    for child in &mut node.children {
        if *remaining == 0 {
            break;
        }
        truncated |= trim_plan(child, remaining, depth + 1);
        retained += 1;
    }
    if retained < node.children.len() {
        node.children.truncate(retained);
        truncated = true;
    }
    truncated
}

pub(super) async fn ai_object_ddl(
    state: &AppState,
    session: sift_protocol::SessionId,
    connection: sift_protocol::ConnectionId,
    request: &InvokeAiToolRequest,
) -> ApiResult<serde_json::Value> {
    let Some(AiToolParameters::ObjectDdl {
        expected_catalog_revision,
        object_id,
    }) = &request.parameters
    else {
        return Err(ApiError::BadRequest(
            "Catalog object identity and revision are required".into(),
        ));
    };
    let graph = state
        .sessions
        .catalog_graph(
            session,
            connection,
            sift_protocol::CatalogGraphRequest {
                options: super::ai::ai_catalog_options(),
                refresh: true,
            },
        )
        .await?;
    if graph.revision != *expected_catalog_revision {
        return Err(ApiError::BadRequest(
            "Catalog changed; call sift_catalog again before reading object DDL".into(),
        ));
    }
    let path = object_path(&graph, object_id)?;
    let ddl = state.sessions.ddl_for(session, connection, path).await?;
    let current = state
        .sessions
        .catalog_graph(
            session,
            connection,
            sift_protocol::CatalogGraphRequest {
                options: super::ai::ai_catalog_options(),
                refresh: true,
            },
        )
        .await?;
    if current.revision != graph.revision || current.database_identity != graph.database_identity {
        return Err(ApiError::Forbidden(
            "Catalog or database changed while reading object DDL".into(),
        ));
    }
    let ddl_digest = format!("sha256:{:x}", Sha256::digest(ddl.ddl.as_bytes()));
    Ok(
        json!({"object_id":object_id,"catalog_revision":graph.revision,"database_identity":graph.database_identity,"ddl_digest":ddl_digest,"object":ddl,"truncated":false}),
    )
}
fn object_path(
    graph: &sift_protocol::CatalogGraph,
    id: &sift_protocol::CatalogObjectId,
) -> ApiResult<sift_protocol::ObjectPath> {
    use sift_protocol::{CatalogNodeKind as N, ObjectKind as O};
    let node = graph
        .data
        .nodes
        .iter()
        .find(|node| &node.id == id)
        .ok_or_else(|| {
            ApiError::BadRequest("Object is absent from the current authorized catalog".into())
        })?;
    let kind = match node.kind {
        N::Table => O::Table,
        N::View => O::View,
        N::MaterializedView => O::MaterializedView,
        N::ForeignTable => O::ForeignTable,
        N::PartitionedTable => O::PartitionedTable,
        N::TableValuedFunction => O::TableValuedFunction,
        N::ScalarFunction => O::ScalarFunction,
        N::Procedure => O::Procedure,
        N::Synonym => O::Synonym,
        N::Sequence => O::Sequence,
        N::Trigger => O::Trigger,
        N::Type => O::Type,
        N::Extension => O::Extension,
        N::Index => O::Index,
        _ => {
            return Err(ApiError::BadRequest(
                "Select a DDL-bearing object from sift_catalog".into(),
            ))
        }
    };
    let mut parent = node.parent_id.as_ref();
    let mut schema = None;
    for _ in 0..32 {
        let Some(next) = parent.and_then(|id| graph.data.nodes.iter().find(|node| &node.id == id))
        else {
            break;
        };
        if next.kind == N::Schema {
            schema = Some(next.name.clone());
            break;
        }
        parent = next.parent_id.as_ref();
    }
    let routine_args = match &node.details {
        sift_protocol::CatalogNodeDetails::Object { routine_args } => routine_args.clone(),
        _ => None,
    };
    Ok(sift_protocol::ObjectPath {
        catalog: None,
        schema,
        name: node.name.clone(),
        kind: Some(kind),
        routine_args,
    })
}
