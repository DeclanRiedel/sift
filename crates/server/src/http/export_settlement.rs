//! Native export effects settle separately from body/artifact production.
use super::*;
use crate::session::export_execution::{ExportExecution, ExportObservation};

pub(super) struct ExportLedger {
    pub scope: Option<DatabaseActorScope>,
    pub workspace: Option<WorkspaceId>,
    pub effect: Option<(sift_protocol::ChangeLedgerOperation, bool)>,
    pub fingerprint: String,
    pub workflow: &'static str,
}
impl ExportLedger {
    pub fn spawn(
        self,
        state: AppState,
        observation: Arc<ExportExecution>,
        permit: Arc<tokio::sync::OwnedSemaphorePermit>,
    ) {
        let correlation =
            crate::correlation::current().unwrap_or_else(crate::correlation::generate);
        let guard = crate::shutdown::current_query_guard();
        tokio::spawn(crate::correlation::scope(
            correlation,
            crate::shutdown::scope_query(guard, async move {
                let _permit = permit;
                let terminal = observation.terminal().await;
                self.finish(&state, terminal).await;
            }),
        ));
    }
    async fn finish(self, state: &AppState, terminal: ExportObservation) {
        let (Some(scope), Some((operation, autocommit))) = (self.scope, self.effect) else {
            return;
        };
        if !terminal.attempted {
            return;
        }
        let outcome = if terminal.completed && autocommit && !terminal.transaction_unknown {
            sift_protocol::ChangeLedgerOutcome::Committed
        } else {
            sift_protocol::ChangeLedgerOutcome::OutcomeUnknown
        };
        if record_database_change(
            state,
            scope.actor,
            DatabaseChangeRecord {
                workspace_id: self.workspace,
                tenant_id: scope.tenant_id,
                room_id: None,
                connection_profile_id: scope.connection_profile_id,
                database_target: scope.database_target,
                operation,
                affected_object: None,
                row_count: if terminal.completed && autocommit {
                    terminal.rows.and_then(|rows| i64::try_from(rows).ok())
                } else {
                    None
                },
                sql_fingerprint: Some(self.fingerprint),
                row_identity_fingerprint: None,
                transaction_id: None,
                source_workflow: self.workflow,
                approved_by: None,
                outcome,
                result_code: terminal.code,
                versioned_source: None,
            },
        )
        .await
        .is_err()
        {
            tracing::error!("failed to append native export change receipt");
        }
    }
}
