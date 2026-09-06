//! Semantic HTTP handlers; shared admission and audit stay at the router boundary.

use super::*;

pub(super) async fn open_semantic_document(
    State(state): State<AppState>,
    Path((session, connection)): Path<(sift_protocol::SessionId, sift_protocol::ConnectionId)>,
    Json(request): Json<sift_protocol::CreateSemanticDocumentRequest>,
) -> ApiResult<(StatusCode, Json<sift_protocol::SemanticDocumentState>)> {
    let source_bytes = request.text.len() as u64;
    let result = state
        .sessions
        .open_semantic_document(session, connection, request)
        .await;
    let document = finish_operation(
        &state.sessions,
        Operation::OpenSemanticDocument {
            session,
            connection,
            source_bytes,
        },
        result,
        |_| None,
    )?;
    Ok((StatusCode::CREATED, Json(document)))
}

pub(super) async fn update_semantic_document(
    State(state): State<AppState>,
    Path((session, connection, document)): Path<(
        sift_protocol::SessionId,
        sift_protocol::ConnectionId,
        sift_protocol::SemanticDocumentId,
    )>,
    Json(request): Json<sift_protocol::UpdateSemanticDocumentRequest>,
) -> ApiResult<Json<sift_protocol::SemanticDocumentState>> {
    let operation = Operation::UpdateSemanticDocument {
        session,
        connection,
        document,
        base_revision: request.base_revision,
        source_bytes: request.text.len() as u64,
    };
    let result = state
        .sessions
        .update_semantic_document(session, connection, document, request)
        .await;
    Ok(Json(finish_operation(
        &state.sessions,
        operation,
        result,
        |_| None,
    )?))
}

pub(super) async fn close_semantic_document(
    State(state): State<AppState>,
    Path((session, connection, document)): Path<(
        sift_protocol::SessionId,
        sift_protocol::ConnectionId,
        sift_protocol::SemanticDocumentId,
    )>,
) -> ApiResult<StatusCode> {
    finish_operation(
        &state.sessions,
        Operation::CloseSemanticDocument {
            session,
            connection,
            document,
        },
        state
            .sessions
            .close_semantic_document(session, connection, document),
        |_| None,
    )?;
    Ok(StatusCode::NO_CONTENT)
}

pub(super) async fn select_semantic_statement(
    State(state): State<AppState>,
    Path((session, connection, document)): Path<(
        sift_protocol::SessionId,
        sift_protocol::ConnectionId,
        sift_protocol::SemanticDocumentId,
    )>,
    Json(request): Json<sift_protocol::SelectStatementRequest>,
) -> ApiResult<Json<sift_protocol::StatementSelection>> {
    let operation = Operation::SelectStatement {
        session,
        connection,
        document,
        revision: request.revision,
    };
    let result = state
        .sessions
        .select_semantic_statement(session, connection, document, request);
    Ok(Json(finish_operation(
        &state.sessions,
        operation,
        result,
        |selection| Some(selection.statements.len() as i64),
    )?))
}

pub(super) async fn semantic_diagnostics(
    State(state): State<AppState>,
    Path((session, connection, document)): Path<(
        sift_protocol::SessionId,
        sift_protocol::ConnectionId,
        sift_protocol::SemanticDocumentId,
    )>,
    Json(request): Json<sift_protocol::SemanticRevisionRequest>,
) -> ApiResult<Json<sift_protocol::DiagnosticsResponse>> {
    let revision = request.revision;
    let result = state
        .sessions
        .semantic_diagnostics(session, connection, document, request)
        .await;
    Ok(Json(finish_operation(
        &state.sessions,
        Operation::DiagnoseSql {
            session,
            connection,
            document,
            revision,
        },
        result,
        |diagnostics| Some(diagnostics.diagnostics.len() as i64),
    )?))
}

pub(super) async fn format_semantic_document(
    State(state): State<AppState>,
    Path((session, connection, document)): Path<(
        sift_protocol::SessionId,
        sift_protocol::ConnectionId,
        sift_protocol::SemanticDocumentId,
    )>,
    Json(request): Json<sift_protocol::FormatSqlRequest>,
) -> ApiResult<Json<sift_protocol::WorkspaceEdit>> {
    let operation = Operation::FormatSql {
        session,
        connection,
        document,
        revision: request.revision,
        range_requested: request.range.is_some(),
    };
    let result = state
        .sessions
        .format_semantic_document(session, connection, document, request)
        .await;
    Ok(Json(finish_operation(
        &state.sessions,
        operation,
        result,
        |edit| {
            Some(
                edit.documents
                    .iter()
                    .map(|document| document.edits.len() as i64)
                    .sum(),
            )
        },
    )?))
}

pub(super) async fn prepare_semantic_quick_fix(
    State(state): State<AppState>,
    Path((session, connection, document, fix)): Path<(
        sift_protocol::SessionId,
        sift_protocol::ConnectionId,
        sift_protocol::SemanticDocumentId,
        String,
    )>,
    Json(request): Json<sift_protocol::SqlQuickFixRequest>,
) -> ApiResult<Json<sift_protocol::WorkspaceEdit>> {
    let operation = Operation::SqlQuickFix {
        session,
        connection,
        document,
        revision: request.revision,
        catalog_revision: request.catalog_revision,
    };
    let result = state
        .sessions
        .prepare_semantic_quick_fix(session, connection, document, fix, request)
        .await;
    Ok(Json(finish_operation(
        &state.sessions,
        operation,
        result,
        |edit| {
            Some(
                edit.documents
                    .iter()
                    .map(|document| document.edits.len() as i64)
                    .sum(),
            )
        },
    )?))
}

pub(super) async fn find_semantic_usages(
    State(state): State<AppState>,
    Path((session, connection, document)): Path<(
        sift_protocol::SessionId,
        sift_protocol::ConnectionId,
        sift_protocol::SemanticDocumentId,
    )>,
    Json(request): Json<sift_protocol::FindSqlUsagesRequest>,
) -> ApiResult<Json<sift_protocol::SqlUsagePage>> {
    let operation = Operation::FindSqlUsages {
        session,
        connection,
        document,
        revision: request.revision,
        catalog_bound: request.catalog_revision.is_some(),
        limit: request.limit,
    };
    let result = state
        .sessions
        .find_semantic_usages(session, connection, document, request)
        .await;
    Ok(Json(finish_operation(
        &state.sessions,
        operation,
        result,
        |page| Some(page.usages.len() as i64),
    )?))
}

pub(super) async fn prepare_semantic_refactor(
    State(state): State<AppState>,
    Path((session, connection, document)): Path<(
        sift_protocol::SessionId,
        sift_protocol::ConnectionId,
        sift_protocol::SemanticDocumentId,
    )>,
    Json(request): Json<sift_protocol::PrepareSqlRefactorRequest>,
) -> ApiResult<Json<sift_protocol::WorkspaceEdit>> {
    let operation = Operation::PrepareSqlRefactor {
        session,
        connection,
        document,
        revision: request.revision,
        catalog_bound: request.catalog_revision.is_some(),
        rename: matches!(
            &request.refactor,
            sift_protocol::SqlRefactor::RenameSymbol { .. }
        ),
    };
    let result = state
        .sessions
        .prepare_semantic_refactor(session, connection, document, request)
        .await;
    Ok(Json(finish_operation(
        &state.sessions,
        operation,
        result,
        |edit| {
            Some(
                edit.documents
                    .iter()
                    .map(|document| document.edits.len() as i64)
                    .sum(),
            )
        },
    )?))
}

pub(super) async fn complete_semantic_document(
    State(state): State<AppState>,
    Path((session, connection, document)): Path<(
        sift_protocol::SessionId,
        sift_protocol::ConnectionId,
        sift_protocol::SemanticDocumentId,
    )>,
    Json(request): Json<sift_protocol::SemanticCompletionRequest>,
) -> ApiResult<Json<sift_protocol::completion::CompletionResponse>> {
    let operation = Operation::CompleteSemanticDocument {
        session,
        connection,
        document,
        revision: request.revision,
        cursor: request.cursor,
        limit: request.limit,
    };
    let result = state
        .sessions
        .complete_semantic_document(session, connection, document, request)
        .await;
    Ok(Json(finish_operation(
        &state.sessions,
        operation,
        result,
        |response| Some(response.candidates.len() as i64),
    )?))
}

pub(super) async fn hover_semantic_document(
    State(state): State<AppState>,
    Path((session, connection, document)): Path<(
        sift_protocol::SessionId,
        sift_protocol::ConnectionId,
        sift_protocol::SemanticDocumentId,
    )>,
    Json(request): Json<sift_protocol::SemanticHoverRequest>,
) -> ApiResult<Json<sift_protocol::SemanticHoverResponse>> {
    let operation = Operation::HoverSemanticDocument {
        session,
        connection,
        document,
        revision: request.revision,
        position: request.position,
        catalog_bound: request.catalog_revision.is_some(),
    };
    let result = state
        .sessions
        .hover_semantic_document(session, connection, document, request)
        .await;
    Ok(Json(finish_operation(
        &state.sessions,
        operation,
        result,
        |_| Some(1),
    )?))
}

pub(super) async fn prepare_star_expansion(
    State(state): State<AppState>,
    Path((session, connection, document)): Path<(
        sift_protocol::SessionId,
        sift_protocol::ConnectionId,
        sift_protocol::SemanticDocumentId,
    )>,
    Json(request): Json<sift_protocol::PrepareStarExpansionRequest>,
) -> ApiResult<Json<sift_protocol::StarExpansionPreview>> {
    let operation = Operation::PrepareStarExpansion {
        session,
        connection,
        document,
        revision: request.revision,
        position: request.position,
        catalog_revision: request.catalog_revision,
    };
    let result = state
        .sessions
        .prepare_star_expansion(session, connection, document, request)
        .await;
    Ok(Json(finish_operation(
        &state.sessions,
        operation,
        result,
        |preview| Some(preview.columns.len() as i64),
    )?))
}

pub(super) async fn prepare_catalog_snippet(
    State(state): State<AppState>,
    Path((session, connection)): Path<(sift_protocol::SessionId, sift_protocol::ConnectionId)>,
    Json(request): Json<sift_protocol::PrepareCatalogSnippetRequest>,
) -> ApiResult<Json<sift_protocol::PreparedCatalogSnippet>> {
    let graph = state
        .sessions
        .catalog_graph(
            session,
            connection,
            sift_protocol::CatalogGraphRequest {
                options: sift_protocol::CatalogGraphOptions {
                    include_definitions: false,
                    ..Default::default()
                },
                refresh: false,
            },
        )
        .await?;
    if graph.revision.0 != request.catalog_revision {
        return Err(ApiError::BadRequest(format!(
            "stale catalog revision: expected {}, current {}",
            request.catalog_revision, graph.revision.0
        )));
    }
    if graph.data.coverage.state != sift_protocol::CatalogCoverageState::Complete {
        return Err(ApiError::BadRequest(
            "catalog template requires complete catalog coverage".into(),
        ));
    }
    let object = graph
        .data
        .nodes
        .iter()
        .find(|node| {
            matches!(
                node.kind,
                sift_protocol::CatalogNodeKind::Table
                    | sift_protocol::CatalogNodeKind::View
                    | sift_protocol::CatalogNodeKind::MaterializedView
                    | sift_protocol::CatalogNodeKind::ForeignTable
                    | sift_protocol::CatalogNodeKind::PartitionedTable
                    | sift_protocol::CatalogNodeKind::TableValuedFunction
            ) && node.name == request.object.name
                && request.object.schema.as_ref().map_or(true, |schema| {
                    node.qualified_name
                        .split('.')
                        .rev()
                        .nth(1)
                        .is_some_and(|part| part.trim_matches(['"', '[', ']']) == schema)
                })
        })
        .ok_or_else(|| ApiError::BadRequest("catalog object was not found".into()))?;
    if object.completeness != sift_protocol::CatalogCompleteness::Complete {
        return Err(ApiError::BadRequest(
            "catalog object metadata is incomplete".into(),
        ));
    }
    let mut columns = graph
        .data
        .nodes
        .iter()
        .filter(|node| {
            node.parent_id.as_ref() == Some(&object.id)
                && matches!(node.kind, sift_protocol::CatalogNodeKind::Column)
                && node.completeness == sift_protocol::CatalogCompleteness::Complete
        })
        .collect::<Vec<_>>();
    columns.sort_by_key(|column| column.ordinal.unwrap_or(u32::MAX));
    if columns.is_empty() || columns.len() > 1_000 {
        return Err(ApiError::BadRequest(
            "catalog template requires between 1 and 1000 ordered columns".into(),
        ));
    }
    let engine = match graph.provider.dialect_id.as_str() {
        "sift/tsql" => sift_protocol::Engine::SqlServer,
        "sift/sqlite" => sift_protocol::Engine::Sqlite,
        _ => sift_protocol::Engine::Postgres,
    };
    let quote = |name: &str| crate::ddl::quote_ident(name, engine);
    let qualified = request.object.schema.as_ref().map_or_else(
        || quote(&request.object.name),
        |schema| format!("{}.{}", quote(schema), quote(&request.object.name)),
    );
    let names = columns
        .iter()
        .map(|column| quote(&column.name))
        .collect::<Vec<_>>();
    let body = match request.kind {
        sift_protocol::CatalogSnippetKind::Select => format!(
            "SELECT {}\nFROM {qualified}\nWHERE ${{1:condition}};$0",
            names.join(", ")
        ),
        sift_protocol::CatalogSnippetKind::Insert => format!(
            "INSERT INTO {qualified} ({})\nVALUES ({});$0",
            names.join(", "),
            (1..=names.len())
                .map(|index| format!("${{{index}:value}}"))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        sift_protocol::CatalogSnippetKind::Update => format!(
            "UPDATE {qualified}\nSET {} = ${{1:value}}\nWHERE ${{2:condition}};$0",
            names[0]
        ),
    };
    let object_trigger = request
        .object
        .name
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '_' | '-') {
                character.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .take(48)
        .collect::<String>();
    let snippet = sift_protocol::SqlSnippet {
        id: None,
        tenant_id: None,
        workspace_id: None,
        owner_principal_id: None,
        trigger: format!("{:?}_{object_trigger}", request.kind).to_ascii_lowercase(),
        title: format!("{:?} {}", request.kind, request.object.name),
        description: format!("Generated from catalog revision {}", graph.revision.0),
        body,
        dialects: vec![graph.provider.dialect_id],
        scope: sift_protocol::SnippetScope::Catalog,
        revision: graph.revision.0,
    };
    sift_snippets::validate(&snippet).map_err(|error| ApiError::BadRequest(error.to_string()))?;
    let prepared = sift_protocol::PreparedCatalogSnippet {
        snippet,
        catalog_revision: graph.revision.0,
    };
    state.sessions.push_operation(
        Operation::Snippet {
            action: sift_protocol::SnippetAction::PrepareCatalog,
            snippet_id: None,
        },
        OperationStatus::Succeeded,
    );
    Ok(Json(prepared))
}
