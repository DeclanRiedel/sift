//! Revision-correlated semantic worker and its server document ownership.

use super::*;

/// One unit of semantic work, addressed to the exact buffer revision it
/// describes. The text travels with the job so the server document can be
/// resynchronized without relying on the order two channels happened to be
/// drained in.
pub(super) struct SemanticJob {
    pub(super) item_id: u64,
    pub(super) text_revision: u64,
    pub(super) text: String,
    pub(super) request: SemanticRequestKind,
}

pub(super) enum SemanticControl {
    Run(SemanticJob),
    Close(u64),
}

/// Server-side identity of one editor's semantic document.
struct SemanticDocument {
    id: sift_protocol::SemanticDocumentId,
    /// Server document revision, required verbatim by every read operation.
    revision: u64,
    /// Client text revision the server text currently matches.
    text_revision: u64,
}

/// Owns every server semantic document for one connection.
///
/// Runs on its own task so a slow analysis never delays query execution, and
/// processes jobs sequentially because the server document is stateful: two
/// concurrent updates would race on `base_revision`. Each pass drains
/// everything already queued first, which is where revision cancellation
/// happens — superseded work is discarded before it costs a round trip.
pub(super) async fn run_semantic_service(
    client: Client,
    session: SessionId,
    connection: ConnectionId,
    mut controls: tokio::sync::mpsc::UnboundedReceiver<SemanticControl>,
    events: tokio::sync::mpsc::UnboundedSender<ExecutorEvent>,
) {
    let mut documents: HashMap<u64, SemanticDocument> = HashMap::new();
    let mut catalog_revision: Option<sift_protocol::CatalogRevision> = None;
    while let Some(first) = controls.recv().await {
        let mut batch = vec![first];
        while let Ok(next) = controls.try_recv() {
            batch.push(next);
        }
        let closed = batch
            .iter()
            .filter_map(|control| match control {
                SemanticControl::Close(item_id) => Some(*item_id),
                SemanticControl::Run(_) => None,
            })
            .collect::<HashSet<_>>();
        for item_id in &closed {
            if let Some(document) = documents.remove(item_id) {
                let _ = client
                    .close_semantic_document(session, connection, document.id)
                    .await;
            }
        }
        let jobs = admissible_jobs(batch, &closed, &events);
        for job in jobs {
            if run_semantic_job(
                &client,
                session,
                connection,
                &mut documents,
                &mut catalog_revision,
                job,
                &events,
            )
            .await
            .is_err()
            {
                return;
            }
        }
    }
    for (_, document) in documents.drain() {
        let _ = client
            .close_semantic_document(session, connection, document.id)
            .await;
    }
}

/// Reduce one drained batch to the work still worth doing: newest revision per
/// item, and at most one analysis, hover, and completion for it. A superseded interactive request is
/// reported back rather than dropped silently, so the editor never waits on an
/// answer that will never arrive.
fn admissible_jobs(
    batch: Vec<SemanticControl>,
    closed: &HashSet<u64>,
    events: &tokio::sync::mpsc::UnboundedSender<ExecutorEvent>,
) -> Vec<SemanticJob> {
    let mut jobs = batch
        .into_iter()
        .filter_map(|control| match control {
            SemanticControl::Run(job) if !closed.contains(&job.item_id) => Some(job),
            _ => None,
        })
        .collect::<Vec<_>>();
    let mut newest: HashMap<u64, u64> = HashMap::new();
    for job in &jobs {
        let entry = newest.entry(job.item_id).or_insert(job.text_revision);
        *entry = (*entry).max(job.text_revision);
    }
    // Walking backwards keeps the last Analyze of a burst, which is the one
    // whose answer matches what the user is now looking at.
    let mut analyzed: HashSet<u64> = HashSet::new();
    let mut hovered: HashSet<u64> = HashSet::new();
    let mut completed: HashSet<u64> = HashSet::new();
    let mut kept = Vec::with_capacity(jobs.len());
    for job in jobs.drain(..).rev() {
        let current = newest.get(&job.item_id).copied() == Some(job.text_revision);
        let duplicate_analyze =
            job.request == SemanticRequestKind::Analyze && !analyzed.insert(job.item_id);
        let duplicate_hover = matches!(job.request, SemanticRequestKind::Hover { .. })
            && !hovered.insert(job.item_id);
        let duplicate_completion = matches!(
            job.request,
            SemanticRequestKind::Complete { .. } | SemanticRequestKind::AutoComplete { .. }
        ) && !completed.insert(job.item_id);
        if current && !duplicate_analyze && !duplicate_hover && !duplicate_completion {
            kept.push(job);
            continue;
        }
        if job.request != SemanticRequestKind::Analyze && !duplicate_hover && !duplicate_completion
        {
            let outcome = if matches!(job.request, SemanticRequestKind::Outline { .. }) {
                SemanticOutcome::OutlineFailed("Buffer changed before the request ran.".into())
            } else {
                SemanticOutcome::Failed("Buffer changed before the request ran.".into())
            };
            let _ = events.send(ExecutorEvent::Semantic {
                item_id: job.item_id,
                text_revision: job.text_revision,
                outcome: Box::new(outcome),
            });
        }
    }
    kept.reverse();
    kept
}

/// Bring the server document in line with `job.text`, returning the server
/// revision to quote in the request. Opening a fresh document is the recovery
/// path for any update failure: an out-of-date `base_revision` is not
/// something the client can reconcile, and the text is authoritative here.
async fn sync_semantic_document(
    client: &Client,
    session: SessionId,
    connection: ConnectionId,
    documents: &mut HashMap<u64, SemanticDocument>,
    job: &SemanticJob,
) -> Result<(sift_protocol::SemanticDocumentId, u64), String> {
    if let Some(existing) = documents.get(&job.item_id) {
        if existing.text_revision == job.text_revision {
            return Ok((existing.id, existing.revision));
        }
        let updated = client
            .update_semantic_document(
                session,
                connection,
                existing.id,
                sift_protocol::UpdateSemanticDocumentRequest {
                    base_revision: existing.revision,
                    text: job.text.clone(),
                },
            )
            .await;
        match updated {
            Ok(state) => {
                documents.insert(
                    job.item_id,
                    SemanticDocument {
                        id: state.document_id,
                        revision: state.revision,
                        text_revision: job.text_revision,
                    },
                );
                return Ok((state.document_id, state.revision));
            }
            Err(_) => {
                documents.remove(&job.item_id);
            }
        }
    }
    let state = client
        .open_semantic_document(
            session,
            connection,
            sift_protocol::CreateSemanticDocumentRequest {
                text: job.text.clone(),
                source: Some(sift_protocol::SemanticSource::Scratch),
            },
        )
        .await
        .map_err(|error| format!("SQL analysis is unavailable: {error}"))?;
    documents.insert(
        job.item_id,
        SemanticDocument {
            id: state.document_id,
            revision: state.revision,
            text_revision: job.text_revision,
        },
    );
    Ok((state.document_id, state.revision))
}

/// The catalog revision the semantic endpoints will accept. Both the catalog
/// diagnostics and quick-fix routes rebuild the graph from the *default*
/// request and reject anything but its current revision, so this must ask for
/// exactly that shape.
async fn current_catalog_revision(
    client: &Client,
    session: SessionId,
    connection: ConnectionId,
    cached: &mut Option<sift_protocol::CatalogRevision>,
) -> Option<sift_protocol::CatalogRevision> {
    if let Some(revision) = *cached {
        return Some(revision);
    }
    let graph = client
        .catalog_graph(
            session,
            connection,
            sift_protocol::CatalogGraphRequest::default(),
        )
        .await
        .ok()?;
    *cached = Some(graph.revision);
    *cached
}

/// Run one job. `Err(())` means the UI channel closed and the service should
/// stop; every other failure is reported to the editor as an outcome.
#[allow(clippy::result_unit_err)]
async fn run_semantic_job(
    client: &Client,
    session: SessionId,
    connection: ConnectionId,
    documents: &mut HashMap<u64, SemanticDocument>,
    catalog_revision: &mut Option<sift_protocol::CatalogRevision>,
    job: SemanticJob,
    events: &tokio::sync::mpsc::UnboundedSender<ExecutorEvent>,
) -> Result<(), ()> {
    let item_id = job.item_id;
    let text_revision = job.text_revision;
    let outcome = match sync_semantic_document(client, session, connection, documents, &job).await {
        Ok((document, revision)) => {
            semantic_outcome(
                client,
                session,
                connection,
                document,
                revision,
                catalog_revision,
                job.request,
            )
            .await
        }
        Err(message) => SemanticOutcome::Failed(message),
    };
    events
        .send(ExecutorEvent::Semantic {
            item_id,
            text_revision,
            outcome: Box::new(outcome),
        })
        .map_err(|_| ())
}

async fn semantic_outcome(
    client: &Client,
    session: SessionId,
    connection: ConnectionId,
    document: sift_protocol::SemanticDocumentId,
    revision: u64,
    catalog_revision: &mut Option<sift_protocol::CatalogRevision>,
    request: SemanticRequestKind,
) -> SemanticOutcome {
    match request {
        SemanticRequestKind::Analyze => {
            // Catalog-bound diagnostics are strictly better but need a live
            // catalog revision. When it is stale or unavailable, fall back to
            // syntax-only diagnostics instead of showing the user nothing.
            if let Some(catalog) =
                current_catalog_revision(client, session, connection, catalog_revision).await
            {
                match client
                    .semantic_diagnostics_with_catalog(
                        session, connection, document, revision, catalog,
                    )
                    .await
                {
                    Ok(response) => {
                        return SemanticOutcome::Diagnostics {
                            diagnostics: response.diagnostics,
                            incomplete: response.incomplete,
                        }
                    }
                    Err(_) => *catalog_revision = None,
                }
            }
            match client
                .semantic_diagnostics(session, connection, document, revision)
                .await
            {
                Ok(response) => SemanticOutcome::Diagnostics {
                    diagnostics: response.diagnostics,
                    incomplete: true,
                },
                Err(error) => SemanticOutcome::Failed(format!("diagnostics failed: {error}")),
            }
        }
        SemanticRequestKind::Complete { cursor } | SemanticRequestKind::AutoComplete { cursor } => {
            match client
                .complete_semantic_document(
                    session,
                    connection,
                    document,
                    sift_protocol::SemanticCompletionRequest {
                        revision,
                        cursor,
                        limit: Some(SEMANTIC_COMPLETION_LIMIT),
                    },
                )
                .await
            {
                Ok(response) => SemanticOutcome::Completions {
                    cursor,
                    replaced: sift_protocol::TextRange {
                        start: response.replaced_range.start,
                        end: response.replaced_range.end,
                    },
                    candidates: response.candidates,
                },
                Err(error) => SemanticOutcome::Failed(format!("completion failed: {error}")),
            }
        }
        SemanticRequestKind::Hover { position } => {
            let catalog =
                current_catalog_revision(client, session, connection, catalog_revision).await;
            match client
                .hover_semantic_document(
                    session,
                    connection,
                    document,
                    sift_protocol::SemanticHoverRequest {
                        revision,
                        position,
                        catalog_revision: catalog,
                    },
                )
                .await
            {
                Ok(response) => SemanticOutcome::Hover(response),
                Err(error) => {
                    *catalog_revision = None;
                    SemanticOutcome::Failed(format!("hover failed: {error}"))
                }
            }
        }
        SemanticRequestKind::ExpandStar { position } => {
            let Some(catalog) =
                current_catalog_revision(client, session, connection, catalog_revision).await
            else {
                return SemanticOutcome::Failed(
                    "Star expansion needs complete catalog metadata.".into(),
                );
            };
            match client
                .prepare_star_expansion(
                    session,
                    connection,
                    document,
                    sift_protocol::PrepareStarExpansionRequest {
                        revision,
                        position,
                        catalog_revision: catalog,
                    },
                )
                .await
            {
                Ok(preview) => SemanticOutcome::StarExpansion(preview),
                Err(error) => {
                    *catalog_revision = None;
                    SemanticOutcome::Failed(format!("star expansion failed: {error}"))
                }
            }
        }
        SemanticRequestKind::Format { range } => {
            match client
                .format_semantic_document(
                    session,
                    connection,
                    document,
                    sift_protocol::FormatSqlRequest {
                        revision,
                        range,
                        options: sift_protocol::FormatOptions::default(),
                    },
                )
                .await
            {
                Ok(edit) => workspace_edit_outcome(edit, document),
                Err(error) => SemanticOutcome::Failed(format!("formatting failed: {error}")),
            }
        }
        SemanticRequestKind::QuickFix { fix_id } => {
            let Some(catalog) =
                current_catalog_revision(client, session, connection, catalog_revision).await
            else {
                return SemanticOutcome::Failed(
                    "Quick fixes need catalog metadata this connection cannot provide.".into(),
                );
            };
            match client
                .prepare_semantic_quick_fix(
                    session,
                    connection,
                    document,
                    &fix_id,
                    sift_protocol::SqlQuickFixRequest {
                        revision,
                        catalog_revision: catalog,
                    },
                )
                .await
            {
                Ok(edit) => workspace_edit_outcome(edit, document),
                Err(error) => {
                    *catalog_revision = None;
                    SemanticOutcome::Failed(format!("quick fix failed: {error}"))
                }
            }
        }
        SemanticRequestKind::Usages { position } => {
            let catalog =
                current_catalog_revision(client, session, connection, catalog_revision).await;
            match client
                .find_semantic_usages(
                    session,
                    connection,
                    document,
                    sift_protocol::FindSqlUsagesRequest {
                        revision,
                        catalog_revision: catalog,
                        target: sift_protocol::SqlSymbolTarget::AtPosition { position },
                        cursor: None,
                        limit: Some(SEMANTIC_USAGE_LIMIT),
                    },
                )
                .await
            {
                Ok(page) => SemanticOutcome::Usages {
                    usages: page.usages,
                    is_complete: page.is_complete,
                },
                Err(error) => {
                    *catalog_revision = None;
                    SemanticOutcome::Failed(format!("finding usages failed: {error}"))
                }
            }
        }
        SemanticRequestKind::Rename { position, new_name } => {
            let catalog =
                current_catalog_revision(client, session, connection, catalog_revision).await;
            match client
                .prepare_semantic_refactor(
                    session,
                    connection,
                    document,
                    sift_protocol::PrepareSqlRefactorRequest {
                        revision,
                        catalog_revision: catalog,
                        refactor: sift_protocol::SqlRefactor::RenameSymbol { position, new_name },
                    },
                )
                .await
            {
                Ok(edit) => match workspace_edit_outcome(edit, document) {
                    SemanticOutcome::Edits { edits, warnings } => {
                        SemanticOutcome::RenamePreview { edits, warnings }
                    }
                    outcome => outcome,
                },
                Err(error) => SemanticOutcome::Failed(format!("preparing rename failed: {error}")),
            }
        }
        SemanticRequestKind::Outline { end } => {
            if end == 0 {
                return SemanticOutcome::Outline {
                    statements: Vec::new(),
                    symbols: Vec::new(),
                };
            }
            match client
                .select_semantic_statement(
                    session,
                    connection,
                    document,
                    sift_protocol::SelectStatementRequest {
                        revision,
                        cursor: 0,
                        selection: Some(sift_protocol::TextRange { start: 0, end }),
                    },
                )
                .await
            {
                Ok(selection) => SemanticOutcome::Outline {
                    statements: selection.statements,
                    symbols: selection.symbols,
                },
                Err(error) => {
                    SemanticOutcome::OutlineFailed(format!("loading query outline failed: {error}"))
                }
            }
        }
    }
}

/// Keep only the edits aimed at this editor's own document. A multi-document
/// `WorkspaceEdit` is reported rather than partially applied, because the
/// desktop has no way to edit another session's document on the user's behalf.
fn workspace_edit_outcome(
    edit: sift_protocol::WorkspaceEdit,
    document: sift_protocol::SemanticDocumentId,
) -> SemanticOutcome {
    let mut warnings = edit.warnings;
    let foreign = edit
        .documents
        .iter()
        .any(|target| target.document_id != document);
    if foreign {
        warnings.push("Edits for other documents were not applied.".into());
    }
    if !edit.is_complete {
        warnings.push("The server returned a partial edit.".into());
    }
    let edits = edit
        .documents
        .into_iter()
        .filter(|target| target.document_id == document)
        .flat_map(|target| target.edits)
        .collect();
    SemanticOutcome::Edits { edits, warnings }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn job(item_id: u64, text_revision: u64, request: SemanticRequestKind) -> SemanticControl {
        SemanticControl::Run(SemanticJob {
            item_id,
            text_revision,
            text: format!("select {text_revision}"),
            request,
        })
    }

    #[test]
    fn a_keystroke_burst_collapses_to_one_analysis_of_the_newest_text() {
        let (events, mut received) = tokio::sync::mpsc::unbounded_channel();
        let kept = admissible_jobs(
            vec![
                job(1, 1, SemanticRequestKind::Analyze),
                job(1, 2, SemanticRequestKind::Analyze),
                job(1, 3, SemanticRequestKind::Analyze),
                job(2, 9, SemanticRequestKind::Analyze),
            ],
            &HashSet::new(),
            &events,
        );

        assert_eq!(kept.len(), 2);
        assert_eq!((kept[0].item_id, kept[0].text_revision), (1, 3));
        assert_eq!((kept[1].item_id, kept[1].text_revision), (2, 9));
        // Superseded analysis is silent; nothing was waiting on it.
        assert!(received.try_recv().is_err());
    }

    #[test]
    fn a_superseded_interactive_request_is_reported_rather_than_dropped() {
        let (events, mut received) = tokio::sync::mpsc::unbounded_channel();
        let kept = admissible_jobs(
            vec![
                job(1, 1, SemanticRequestKind::Complete { cursor: 3 }),
                job(1, 2, SemanticRequestKind::Analyze),
            ],
            &HashSet::new(),
            &events,
        );

        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].request, SemanticRequestKind::Analyze);
        assert!(matches!(
            received.try_recv(),
            Ok(ExecutorEvent::Semantic {
                item_id: 1,
                text_revision: 1,
                ..
            })
        ));
    }

    #[test]
    fn caret_completion_bursts_keep_only_the_latest_position() {
        let (events, mut received) = tokio::sync::mpsc::unbounded_channel();
        let kept = admissible_jobs(
            vec![
                job(1, 1, SemanticRequestKind::Complete { cursor: 3 }),
                job(1, 1, SemanticRequestKind::Analyze),
                job(1, 1, SemanticRequestKind::Complete { cursor: 7 }),
                job(2, 1, SemanticRequestKind::Complete { cursor: 9 }),
            ],
            &HashSet::new(),
            &events,
        );
        assert_eq!(kept.len(), 3);
        assert_eq!(kept[1].request, SemanticRequestKind::Complete { cursor: 7 });
        assert_eq!(kept[2].item_id, 2);
        assert!(received.try_recv().is_err());
    }

    #[test]
    fn work_for_a_closed_tab_is_abandoned() {
        let (events, mut received) = tokio::sync::mpsc::unbounded_channel();
        let kept = admissible_jobs(
            vec![
                job(1, 1, SemanticRequestKind::Format { range: None }),
                SemanticControl::Close(1),
            ],
            &HashSet::from([1]),
            &events,
        );

        assert!(kept.is_empty());
        assert!(received.try_recv().is_err());
    }

    #[test]
    fn foreign_documents_in_a_workspace_edit_are_not_applied() {
        let mine = sift_protocol::SemanticDocumentId(uuid::Uuid::from_u128(1));
        let theirs = sift_protocol::SemanticDocumentId(uuid::Uuid::from_u128(2));
        let outcome = workspace_edit_outcome(
            sift_protocol::WorkspaceEdit {
                documents: vec![
                    sift_protocol::DocumentEdit {
                        document_id: mine,
                        expected_revision: 1,
                        source_digest: "d".into(),
                        edits: vec![sift_protocol::TextEdit {
                            range: sift_protocol::TextRange { start: 0, end: 1 },
                            new_text: "A".into(),
                        }],
                    },
                    sift_protocol::DocumentEdit {
                        document_id: theirs,
                        expected_revision: 1,
                        source_digest: "d".into(),
                        edits: vec![sift_protocol::TextEdit {
                            range: sift_protocol::TextRange { start: 0, end: 1 },
                            new_text: "B".into(),
                        }],
                    },
                ],
                warnings: Vec::new(),
                is_complete: true,
                actual_range: None,
            },
            mine,
        );

        let SemanticOutcome::Edits { edits, warnings } = outcome else {
            panic!("formatting produces edits");
        };
        assert_eq!(edits.len(), 1);
        assert_eq!(edits[0].new_text, "A");
        assert_eq!(warnings.len(), 1);
    }
}
