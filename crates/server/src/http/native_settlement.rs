//! Native statement effects settle separately from result observation.
use super::*;
use crate::session::native_execution::{NativeExecution, NativeObservation};

pub(super) struct NativeLedger {
    pub scope: Option<DatabaseActorScope>,
    pub workspace: Option<WorkspaceId>,
    pub room: Option<RoomId>,
    pub transaction: Option<String>,
    pub source: Option<sift_metadata::ValidatedVersionedExecution>,
    pub effect: Option<(sift_protocol::ChangeLedgerOperation, bool)>,
    pub fingerprint: String,
    pub workflow: &'static str,
}
impl NativeLedger {
    pub fn spawn(
        self,
        state: AppState,
        observation: Arc<NativeExecution>,
        permit: Arc<tokio::sync::OwnedSemaphorePermit>,
        guard: Option<Arc<crate::shutdown::QueryGuard>>,
    ) {
        let correlation =
            crate::correlation::current().unwrap_or_else(crate::correlation::generate);
        tokio::spawn(crate::correlation::scope(
            correlation,
            crate::shutdown::scope_query(guard, async move {
                let _permit = permit;
                let terminal = observation.terminal().await;
                self.finish(&state, terminal).await;
            }),
        ));
    }
    async fn finish(self, state: &AppState, terminal: NativeObservation) {
        let (Some(scope), Some((operation, autocommit))) = (self.scope, self.effect) else {
            return;
        };
        if !terminal.attempted {
            return;
        }
        let outcome = if terminal.completed && autocommit && !terminal.transaction_unknown {
            if self.transaction.is_some() {
                sift_protocol::ChangeLedgerOutcome::Partial
            } else {
                sift_protocol::ChangeLedgerOutcome::Committed
            }
        } else {
            sift_protocol::ChangeLedgerOutcome::OutcomeUnknown
        };
        if record_database_change(
            state,
            scope.actor,
            DatabaseChangeRecord {
                workspace_id: self.workspace,
                tenant_id: scope.tenant_id,
                room_id: self.room,
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
                transaction_id: self.transaction,
                source_workflow: self.workflow,
                approved_by: None,
                outcome,
                result_code: terminal.code,
                versioned_source: self.source,
            },
        )
        .await
        .is_err()
        {
            tracing::error!("failed to append native statement change receipt");
        }
    }
}
