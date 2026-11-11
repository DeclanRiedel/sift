//! Route registration in bounded functions keeps compiler IR smaller.
//! Every group consumes the same router; registration order and final middleware
//! remain owned by `app`.
use super::*;

pub(super) fn register(router: ApiRouter<AppState>) -> ApiRouter<AppState> {
    let router = ai(router, false);
    let router = ai(router, true);
    let router = scoped_ai_setup(router);
    let router = delegation(router);
    let router = instance(router);
    let router = accounts(router);
    let router = workspaces(router);
    let router = automation(router);
    let router = connections(router);
    let router = saved_performance(router);
    let router = queries(router);
    let router = analysis(router);
    let router = workbench(router);
    transactions(router)
}

#[inline(never)]
fn ai(router: ApiRouter<AppState>, scoped: bool) -> ApiRouter<AppState> {
    router
        .api_route(&ai_path(scoped,"/v1/ai/runs/:id/external-tools"),
            post_with(ai_external_read::inventory, ai_doc(scoped,"invokeAiExternalInventory", "Discover only selected reviewed source tools under the canonical AI quota")))
        .api_route(&ai_path(scoped,"/v1/ai/runs/:id/external-stage"),
            post_with(ai_external_stage::stage, ai_doc(scoped,"stageAiExternalProposal", "Stage a reviewed external intent through local human-review contracts")))
        .api_route(&ai_path(scoped,"/v1/ai/runs/:id/external-read"),
            post_with(ai_external_read::read, ai_doc(scoped,"invokeAiExternalRead", "Invoke a selected reviewed read with the canonical AI quota and receipt")))
        .api_route(&ai_path(scoped,"/v1/ai/external-sources"),
            get_with(ai_external::list, ai_doc(scoped,"listAiExternalSources", "List accessible explicitly registered AI sources"))
                .post_with(ai_external::discover, ai_doc(scoped,"discoverAiExternalSource", "Discover a draft source for explicit tool and credential review")))
        .api_route(&ai_path(scoped,"/v1/ai/external-sources/:id"),
            get_with(ai_external::get, ai_doc(scoped,"getAiExternalSource", "Read an authorized registered source"))
                .delete_with(ai_external::delete, ai_doc(scoped,"deleteAiExternalSource", "Delete a registered source at its expected revision")))
        .api_route(&ai_path(scoped,"/v1/ai/external-sources/:id/refresh"),
            post_with(ai_external::refresh, ai_doc(scoped,"refreshAiExternalSource", "Rediscover an owned source with explicit endpoint-bound credential retention or rotation")))
        .api_route(&ai_path(scoped,"/v1/ai/external-sources/:id/activate"),
            post_with(ai_external::activate, ai_doc(scoped,"activateAiExternalSource", "Approve exact discovered tools and credential scope")))
        .api_route(&ai_path(scoped,"/v1/ai/external-sources/:id/disable"),
            post_with(ai_external::disable, ai_doc(scoped,"disableAiExternalSource", "Disable future source use at its expected revision")))
        .api_route(&ai_path(scoped,"/v1/ai/rooms/:id/external-sources/review"),
            get_with(ai_external::grant_review, ai_doc(scoped,"reviewAiExternalRoomGrants", "List owned safe room grant headers for explicit replacement or revocation after source changes")))
        .api_route(&ai_path(scoped,"/v1/ai/rooms/:id/external-sources"),
            get_with(ai_external::room_sources, ai_doc(scoped,"listAiExternalRoomSources", "Read independently granted room sources without private endpoints"))
                .post_with(ai_external::publish, ai_doc(scoped,"publishAiExternalSource", "Publish an independent reviewed source grant to a room")))
        .api_route(&ai_path(scoped,"/v1/ai/rooms/:room/external-sources/:id"),
            delete_with(ai_external::revoke, ai_doc(scoped,"revokeAiExternalSource", "Revoke future room source access")))
        .api_route(
            &ai_path(scoped,"/v1/ai/policy"),
            get_with(get_ai_policy, ai_doc(scoped,"getAiPolicy", "Disclose AI chat visibility and limits before sending")),
        )
        .api_route(
            &ai_path(scoped,"/v1/ai/tenants/:id/retention"),
            get_with(get_ai_retention,ai_doc(scoped,"getAiRetention","Read tenant AI retention and effective instance ceiling"))
                .put_with(set_ai_retention,ai_doc(scoped,"setAiRetention","Set tenant-admin AI chat retention within instance limits")),
        )
        .api_route(
            &ai_path(scoped,"/v1/ai/tenants/:id/content-key/rotate"),
            post_with(rotate_ai_content_key, ai_doc(scoped,"rotateAiContentKey", "Rotate tenant AI content keys with resumable encrypted blob migration")),
        )
        .api_route(
            &ai_path(scoped,"/v1/ai/rooms/:id/publication/preview"),
            get_with(preview_ai_publication, ai_doc(scoped,"previewAiRoomPublication", "Review the shared database scope before publication")),
        )
        .api_route(
            &ai_path(scoped,"/v1/ai/rooms/:id/publication"),
            get_with(get_ai_publication, ai_doc(scoped,"getAiRoomPublication", "Read the current explicit room database publication"))
                .post_with(create_ai_publication, ai_doc(scoped,"createAiRoomPublication", "Publish reviewed shared database context to room AI chats"))
                .delete_with(revoke_ai_publication, ai_doc(scoped,"revokeAiRoomPublication", "Revoke future room AI database reads")),
        )
        .api_route(
            &ai_path(scoped,"/v1/ai/chats"),
            get_with(list_ai_chats, ai_doc(scoped,"listAiChats", "List accessible AI chats"))
                .post_with(create_ai_chat, ai_doc(scoped,"createAiChat", "Create a private or room-public AI chat from instance policy")),
        )
        .api_route(
            &ai_path(scoped,"/v1/ai/chats/:id"),
            get_with(get_ai_chat, ai_doc(scoped,"getAiChat", "Read an accessible AI chat"))
                .put_with(rename_ai_chat, ai_doc(scoped,"renameAiChat", "Rename an owned or tenant-admin AI chat with revision checking"))
                .delete_with(delete_ai_chat, ai_doc(scoped,"deleteAiChat", "Delete an owned or tenant-admin AI chat")),
        )
        .api_route(
            &ai_path(scoped,"/v1/ai/chats/:id/attachments/preview"),
            post_with(ai_attachments::preview, ai_doc(scoped,"previewAiAttachment", "Review an exact server-resolved AI attachment")),
        )
        .api_route(&ai_path(scoped,"/v1/ai/chats/:id/runs/cancel-pending"),
            post_with(ai::cancel_ai_pending_turn, ai_doc(scoped,"cancelAiPendingTurn", "Settle the exact original startup request without reading chat content")))
        .api_route(
            &ai_path(scoped,"/v1/ai/chats/:id/runs"),
            get_with(list_ai_runs, ai_doc(scoped,"listAiRuns", "Read turns in an AI chat"))
                .post_with(start_ai_turn, ai_doc(scoped,"startAiTurn", "Start a desktop-owned AI turn")),
        )
        .api_route(
            &ai_path(scoped,"/v1/ai/chats/:chat_id/runs/:run_id"),
            get_with(get_ai_run, ai_doc(scoped,"getAiRun", "Read an exact authorized AI turn")),
        )
        .api_route(
            &ai_path(scoped,"/v1/ai/runs/:id/events"),
            get_with(list_ai_events, ai_doc(scoped,"listAiEvents", "Replay AI run events"))
                .post_with(append_ai_event, ai_doc(scoped,"appendAiEvent", "Append a desktop provider event")),
        )
        .api_route(
            &ai_path(scoped,"/v1/ai/runs/:id/tools"),
            post_with(invoke_ai_tool, ai_doc(scoped,"invokeAiTool", "Invoke a Sift-governed AI read tool")),
        )
        .api_route(
            &ai_path(scoped,"/v1/ai/runs/:id/finish"),
            post_with(finish_ai_run, ai_doc(scoped,"finishAiRun", "Finish a desktop-owned AI run")),
        )
        .api_route(&ai_path(scoped,"/v1/ai/runs/:id/extension-executions"),post_with(ai_extension_execution::request,ai_doc(scoped,"requestAiExtensionExecution","Request an immutable governed extension action for human review")))
        .api_route(&ai_path(scoped,"/v1/ai/chats/:id/extension-executions"),get_with(ai_extension_execution::list,ai_doc(scoped,"listAiExtensionExecutions","List own reviewed extension actions")))
        .api_route(&ai_path(scoped,"/v1/ai/extension-executions/:id"),get_with(ai_extension_execution::get,ai_doc(scoped,"getAiExtensionExecution","Review the exact encrypted extension input and RPC observations")))
        .api_route(&ai_path(scoped,"/v1/ai/extension-executions/:id/status"),get_with(ai_extension_execution::status,ai_doc(scoped,"getAiExtensionExecutionStatus","Inspect redacted extension execution status")))
        .api_route(&ai_path(scoped,"/v1/ai/extension-executions/:id/approve"),post_with(ai_extension_execution::approve,ai_doc(scoped,"approveAiExtensionExecution","Approve the exact reviewed extension action")))
        .api_route(&ai_path(scoped,"/v1/ai/extension-executions/:id/deny"),post_with(ai_extension_execution::deny,ai_doc(scoped,"denyAiExtensionExecution","Deny an exact pending extension request")))
        .api_route(&ai_path(scoped,"/v1/ai/extension-executions/:id/acknowledge"),post_with(ai_extension_execution::acknowledge,ai_doc(scoped,"acknowledgeAiExtensionExecution","Acknowledge an inspected unknown RPC outcome without redispatch")))
        .api_route(&ai_path(scoped,"/v1/ai/runs/:id/query-executions"),post_with(ai_query_execution::request,ai_doc(scoped,"requestAiQueryExecution","Request an immutable same-run query text edit for human review")))
        .api_route(&ai_path(scoped,"/v1/ai/chats/:id/query-executions"),get_with(ai_query_execution::list,ai_doc(scoped,"listAiQueryExecutions","List own reviewed query text actions")))
        .api_route(&ai_path(scoped,"/v1/ai/query-executions/:id"),get_with(ai_query_execution::get,ai_doc(scoped,"getAiQueryExecution","Inspect the original query target and complete before/after SQL")))
        .api_route(&ai_path(scoped,"/v1/ai/query-executions/:id/status"),get_with(ai_query_execution::status,ai_doc(scoped,"getAiQueryExecutionStatus","Inspect a redacted room commit or companion acknowledgement")))
        .api_route(&ai_path(scoped,"/v1/ai/query-executions/:id/approve"),post_with(ai_query_execution::approve,ai_doc(scoped,"approveAiQueryExecution","Approve the exact reviewed text edit on its original target")))
        .api_route(&ai_path(scoped,"/v1/ai/query-executions/:id/deny"),post_with(ai_query_execution::deny,ai_doc(scoped,"denyAiQueryExecution","Deny a pending query text edit")))
        .api_route(&ai_path(scoped,"/v1/ai/query-executions/:id/reconcile"),post_with(ai_query_execution::reconcile,ai_doc(scoped,"reconcileAiQueryExecution","Acknowledge an inspected unknown query edit without redispatch")))
        .api_route(&ai_path(scoped,"/v1/ai/query-executions/:id/acknowledge"),post_with(ai_query_execution::acknowledge,ai_doc(scoped,"acknowledgeAiQueryEditorExecution","Record the exact issued companion observation without redispatch")))
        .api_route(&ai_path(scoped,"/v1/ai/runs/:id/database-executions"),post_with(ai_database_execution::request,ai_doc(scoped,"requestAiDatabaseExecution","Request exact typed database review on the original Execute source")))
        .api_route(&ai_path(scoped,"/v1/ai/chats/:id/database-executions"),get_with(ai_database_execution::list,ai_doc(scoped,"listAiDatabaseExecutions","List own typed execution requests")))
        .api_route(&ai_path(scoped,"/v1/ai/database-executions/:id"),get_with(ai_database_execution::get,ai_doc(scoped,"getAiDatabaseExecution","Inspect exact typed execution review")))
        .api_route(&ai_path(scoped,"/v1/ai/database-executions/:id/status"),get_with(ai_database_execution::status,ai_doc(scoped,"getAiDatabaseExecutionStatus","Inspect native typed outcome with current disclosure checks")))
        .api_route(&ai_path(scoped,"/v1/ai/database-executions/:id/approve"),post_with(ai_database_execution::approve,ai_doc(scoped,"approveAiDatabaseExecution","Approve exact typed fingerprint and human risk confirmations")))
        .api_route(&ai_path(scoped,"/v1/ai/database-executions/:id/deny"),post_with(ai_database_execution::deny,ai_doc(scoped,"denyAiDatabaseExecution","Deny a pending typed execution")))
        .api_route(&ai_path(scoped,"/v1/ai/database-executions/:id/reconcile"),post_with(ai_database_execution::reconcile,ai_doc(scoped,"reconcileAiDatabaseExecution","Acknowledge an exact unknown typed outcome without redispatch")))
        .api_route(&ai_path(scoped,"/v1/ai/runs/:id/sql-executions"), post_with(ai_execution::request_sql, ai_doc(scoped,"requestAiSqlExecution", "Request immutable SQL for human approval")))
        .api_route(&ai_path(scoped,"/v1/ai/chats/:id/sql-executions"), get_with(ai_execution::list_sql, ai_doc(scoped,"listAiSqlExecutions", "List own SQL execution actions")))
        .api_route(&ai_path(scoped,"/v1/ai/sql-executions/:id"), get_with(ai_execution::get_sql, ai_doc(scoped,"getAiSqlExecution", "Review exact SQL execution input and receipt")))
        .api_route(&ai_path(scoped,"/v1/ai/sql-executions/:id/status"), get_with(ai_execution::status_sql, ai_doc(scoped,"getAiSqlExecutionStatus", "Read SQL execution status and bounded result")))
        .api_route(&ai_path(scoped,"/v1/ai/sql-executions/:id/approve"), post_with(ai_execution::approve_sql, ai_doc(scoped,"approveAiSqlExecution", "Approve the exact reviewed SQL action")))
        .api_route(&ai_path(scoped,"/v1/ai/sql-executions/:id/deny"),post_with(ai_execution::deny_sql,ai_doc(scoped,"denyAiSqlExecution","Deny an exact pending SQL request without dispatch")))
        .api_route(&ai_path(scoped,"/v1/ai/sql-executions/:id/reconcile"), post_with(ai_execution::reconcile_sql, ai_doc(scoped,"reconcileAiSqlExecution", "Acknowledge an unknown SQL outcome without retrying")))
        .api_route(
            &ai_path(scoped,"/v1/ai/runs/:id/query-proposals"),
            post_with(stage_ai_query_proposal, ai_doc(scoped,"stageAiQueryProposal", "Stage an AI SQL draft for review")),
        )
        .api_route(
            &ai_path(scoped,"/v1/ai/chats/:id/query-proposals"),
            get_with(list_ai_query_proposals, ai_doc(scoped,"listAiQueryProposals", "Review staged AI SQL drafts")),
        )
        .api_route(
            &ai_path(scoped,"/v1/ai/chats/:chat_id/query-proposals/:proposal_id/discard"),
            post_with(discard_ai_query_proposal, ai_doc(scoped,"discardAiQueryProposal", "Discard a staged AI SQL draft")),
        )
        .api_route(
            &ai_path(scoped,"/v1/ai/chats/:chat_id/query-proposals/:proposal_id/apply"),
            post_with(apply_ai_query_proposal, ai_doc(scoped,"applyAiQueryProposal", "Record a human-applied AI SQL draft after revision check")),
        )
        .api_route(
            &ai_path(scoped,"/v1/ai/runs/:id/database-proposals"),
            post_with(stage_ai_database_change,ai_doc(scoped,"stageAiDatabaseProposal","Stage a typed row or desired catalog draft without applying it")),
        )
        .api_route(
            &ai_path(scoped,"/v1/ai/chats/:id/database-proposals"),
            get_with(list_ai_database_changes,ai_doc(scoped,"listAiDatabaseProposals","Read authorized typed database proposals and outcomes")),
        )
        .api_route(
            &ai_path(scoped,"/v1/ai/database-proposals/:id/review"),
            post_with(review_ai_database_change,ai_doc(scoped,"reviewAiDatabaseProposal","Preview typed changes on the human reviewer's managed connection")),
        )
        .api_route(
            &ai_path(scoped,"/v1/ai/database-proposals/:id/apply"),
            get_with(inspect_ai_database_apply,ai_doc(scoped,"getAiDatabaseApplyStatus","Inspect redacted apply bindings and acknowledgement without retrying native work"))
                .post_with(apply_ai_database_change,ai_doc(scoped,"applyAiDatabaseProposal","Apply a human-confirmed preview under a durable single-dispatch claim")),
        )
        .api_route(
            &ai_path(scoped,"/v1/ai/database-proposals/:id/apply/reconcile"),
            post_with(reconcile_ai_database_apply,ai_doc(scoped,"reconcileAiDatabaseApply","Acknowledge an exact unknown apply without changing its receipt or retrying")),
        )
        .api_route(
            &ai_path(scoped,"/v1/ai/database-proposals/:id/discard"),
            post_with(discard_ai_database_change,ai_doc(scoped,"discardAiDatabaseProposal","Discard an unclaimed typed database draft")),
        )
}

fn ai_path(scoped: bool, path: &str) -> std::borrow::Cow<'_, str> {
    if scoped {
        std::borrow::Cow::Owned(path.replacen("/v1/ai/", "/v1/scoped/ai/", 1))
    } else {
        std::borrow::Cow::Borrowed(path)
    }
}
fn ai_doc(
    scoped: bool,
    id: &'static str,
    summary: &'static str,
) -> impl FnOnce(TransformOperation) -> TransformOperation {
    let id = if scoped {
        id.replacen("Ai", "ScopedAi", 1)
    } else {
        id.into()
    };
    move |op| op.id(&id).summary(summary)
}
#[inline(never)]
fn scoped_ai_setup(router: ApiRouter<AppState>) -> ApiRouter<AppState> {
    router
        .api_route(
            "/v1/scoped/ai/chats/:chat_id/query-proposals/:proposal_id/room-commit",
            post_with(
                ai_room_query::commit,
                doc(
                    "commitScopedAiRoomQueryProposal",
                    "Commit an exact human-reviewed room query through its retained server owner",
                ),
            )
            .get_with(
                ai_room_query::inspect,
                doc(
                    "getScopedAiRoomQueryCommit",
                    "Observe a durable server room query commit without replaying the edit",
                ),
            ),
        )
        .api_route(
            "/v1/scoped/ai/database-proposals/:id/managed-review",
            post_with(
                managed_review_ai_database_change,
                doc(
                    "managedReviewScopedAiDatabaseProposal",
                    "Prepare a caller-bound server-owned database review source",
                ),
            ),
        )
        .api_route(
            "/v1/scoped/metadata/vaults",
            get_with(
                list_metadata_vaults,
                doc(
                    "listScopedAiVaults",
                    "List caller-visible vaults under captured identity and tenant permissions",
                ),
            ),
        )
        .api_route(
            "/v1/scoped/metadata/rooms/:id/results",
            get_with(
                list_room_results,
                doc(
                    "listScopedAiRoomResults",
                    "List shared result references under captured caller and room permissions",
                ),
            ),
        )
        .api_route(
            "/v1/scoped/auth/whoami",
            get_with(
                whoami,
                doc("whoAmIScopedAi", "Read the exact captured caller"),
            ),
        )
        .api_route(
            "/v1/scoped/sessions",
            post_with(
                create_session,
                doc(
                    "createScopedAiSession",
                    "Create a session under captured caller assertions",
                ),
            ),
        )
        .api_route(
            "/v1/scoped/sessions/:id",
            delete_with(
                close_session,
                doc(
                    "closeScopedAiSession",
                    "Close an owned captured-caller session",
                ),
            ),
        )
        .api_route(
            "/v1/scoped/sessions/:id/connections/from-profile",
            post_with(
                open_connection_from_profile,
                doc(
                    "openScopedAiConnectionFromProfile",
                    "Connect an owned managed source under captured caller assertions",
                ),
            ),
        )
        .api_route(
            "/v1/scoped/sessions/:id/connections/:conn_id",
            delete_with(
                close_connection,
                doc(
                    "closeScopedAiConnection",
                    "Close an owned captured-caller connection",
                ),
            ),
        )
        .api_route(
            "/v1/scoped/sessions/:id/connections/:conn_id/ping",
            get_with(
                ping_connection,
                doc(
                    "pingScopedAiConnection",
                    "Observe an owned captured-caller connection",
                ),
            ),
        )
}

#[inline(never)]
fn instance(router: ApiRouter<AppState>) -> ApiRouter<AppState> {
    router
        .api_route("/v1/metrics", get_with(read_metrics, doc("readMetrics", "Administrator-only Prometheus metrics")))
        .api_route(
            "/v1/handshake",
            post_with(
                handshake,
                doc("handshake", "Negotiate the application protocol version"),
            ),
        )
        .api_route(
            "/v1/health",
            get_with(health, doc("health", "Liveness and registered engines")),
        )
        .api_route(
            "/v1/ready",
            get_with(ready, |op| {
                doc(
                    "ready",
                    "Readiness: 200 when ready, 503 while draining/unhealthy",
                )(op)
                .response::<200, Json<sift_protocol::Readiness>>()
                .response::<503, Json<sift_protocol::Readiness>>()
            }),
        )
        .api_route(
            "/v1/audit",
            get_with(list_audit, doc("listAudit", "List in-memory operation audit rows")),
        )
        .api_route(
            "/v1/admin/instance/configuration",
            get_with(get_instance_configuration, doc("getInstanceConfiguration", "Read the current instance desired-state TOML"))
                .put_with(update_instance_configuration, doc("updateInstanceConfiguration", "Validate and replace instance desired-state TOML using an optimistic source revision")),
        )
        .api_route(
            "/v1/admin/instance/vcs-diagnostics",
            get_with(get_vcs_diagnostics, doc("getVcsDiagnostics", "Probe the fixed Git adapter and report its effective safety policy")),
        )
        .api_route(
            "/v1/operations",
            get_with(list_operations, doc("listOperations", "List replayable operation audit rows")),
        )
        .api_route(
            "/v1/operations/available",
            get_with(list_available_operations, doc("listAvailableOperations", "List contextual operation capabilities")),
        )
        .api_route(
            "/v1/operations/audit",
            get_with(list_operation_audit_log, doc("listOperationAudit", "List durable operation audit rows (actor, target, result, rows)")),
        )
        .api_route(
            "/v1/operations/audit/pages",
            get_with(list_operation_audit_pages, doc("pageOperationAudit", "Keyset-page durable operation audit rows")),
        )
        .api_route(
            "/v1/change-ledger",
            get_with(list_change_ledger, doc("listChangeLedger", "Filter the append-only database change ledger")),
        )
        .api_route(
            "/v1/change-ledger/export",
            get_with(export_change_ledger, doc("exportChangeLedger", "Export a permission-scoped database change ledger report")),
        )
        .api_route(
            "/v1/change-ledger/external",
            post_with(import_external_change_ledger_event, doc("importExternalChangeLedger", "Append a labeled database-native audit or CDC event")),
        )
        .api_route(
            "/v1/change-ledger/policy/:tenant",
            get_with(get_change_ledger_policy, doc("getChangeLedgerPolicy", "Read database change ledger retention and sink policy"))
                .put_with(update_change_ledger_policy, doc("updateChangeLedgerPolicy", "Configure database change ledger retention and external sink label")),
        )
        .api_route(
            "/v1/providers",
            get_with(list_providers, doc("listProviders", "List provider-neutral database capabilities")),
        )
        .api_route(
            "/v1/extensions",
            get_with(list_extensions, doc("listExtensions", "List installed extension descriptors")),
        )
        .api_route(
            "/v1/extensions/:publisher/:name",
            get_with(get_extension, doc("getExtension", "Inspect one installed extension"))
                .delete_with(uninstall_extension, doc("uninstallExtension", "Uninstall an extension while retaining orphaned data")),
        )
        .api_route(
            "/v1/extensions/validate",
            post_with(validate_extension, doc("validateExtension", "Validate a bounded local extension archive")),
        )
        .api_route(
            "/v1/extensions/install",
            post_with(install_extension, doc("installExtension", "Validate and install a bounded local extension archive")),
        )
        .api_route(
            "/v1/extensions/:publisher/:name/selection",
            put_with(update_extension_selection, doc("updateExtensionSelection", "Enable or disable an extension with revision checking")),
        )
        .api_route(
            "/v1/extensions/:publisher/:name/grants",
            put_with(update_extension_grants, doc("updateExtensionGrants", "Replace extension grants with revision checking")),
        )
        .api_route(
            "/v1/extensions/:publisher/:name/tenants/:tenant_id",
            put_with(update_extension_tenant, doc("updateExtensionTenant", "Allow or deny an extension for one tenant")),
        )
        .api_route(
            "/v1/extensions/:publisher/:name/rollback",
            post_with(rollback_extension, doc("rollbackExtension", "Select the previous installed package")),
        )
        .api_route(
            "/v1/extensions/:publisher/:name/purge",
            post_with(purge_extension, doc("purgeExtension", "Purge orphaned extension data")),
        )
        .api_route(
            "/v1/extensions/:publisher/:name/diagnostics",
            get_with(extension_diagnostics, doc("extensionDiagnostics", "Inspect bounded extension lifecycle diagnostics")),
        )
        .api_route("/v1/scoped/extensions",get_with(list_extensions,doc("listScopedExtensions","List extensions on the captured server/account")))
        .api_route("/v1/scoped/tools",get_with(list_governed_tools,doc("listScopedGovernedTools","List tools on the captured server/account")))
        .api_route("/v1/scoped/tools/invoke",post_with(invoke_governed_tool,doc("invokeScopedGovernedTool","Invoke under receiving server/account assertions")))
        .api_route("/v1/scoped/extension-actions/invoke",post_with(invoke_extension_action,doc("invokeScopedExtensionAction","Invoke an extension under receiving server/account assertions")))
        .api_route("/v1/scoped/operation-approvals",post_with(create_operation_approval,doc("createScopedOperationApproval","Create an approval on the captured server/account")))
        .api_route("/v1/scoped/operation-approvals/:approval_id/approve",post_with(approve_operation,doc("approveScopedOperation","Approve on the captured server/account")))
        .api_route("/v1/scoped/extension-executions",get_with(extension_actions::list,doc("listScopedExtensionExecutions","Read own receipts on the captured server/account")))
        .api_route("/v1/extension-executions",get_with(extension_actions::list,doc("listExtensionExecutions","Inspect own redacted extension receipts")))
        .api_route("/v1/extension-executions/:id",get_with(extension_actions::get,doc("getExtensionExecution","Inspect one own redacted extension receipt")))
        .api_route(
            "/v1/extension-actions/invoke",
            post_with(invoke_extension_action, doc("invokeExtensionAction", "Invoke a schema-validated governed extension action")),
        )
        .api_route(
            "/v1/operation-approvals",
            get_with(list_operation_approvals, doc("listOperationApprovals", "List the current principal's operation approvals")).post_with(create_operation_approval, doc("createOperationApproval", "Create a narrowly bound one-use approval request")),
        )
        .api_route(
            "/v1/operation-approvals/:approval_id/approve",
            post_with(approve_operation, doc("approveOperation", "Approve a pending operation request")),
        )
        .api_route(
            "/v1/tools",
            get_with(list_governed_tools, doc("listGovernedTools", "List governed tools available in context")),
        )
        .api_route(
            "/v1/tools/invoke",
            post_with(invoke_governed_tool, doc("invokeGovernedTool", "Invoke a governed tool through policy and approval admission")),
        )
        .api_route(
            "/v1/openapi.json",
            get_with(openapi, doc("openapi", "OpenAPI document")),
        )
        .api_route(
            "/v1/auth/login",
            post_with(password_login, doc("passwordLogin", "Authenticate an instance-owned password identity")),
        )
        .api_route(
            "/v1/auth/refresh",
            post_with(refresh_auth, doc("refreshAuth", "Atomically rotate an interactive refresh credential")),
        )
        .api_route(
            "/v1/auth/logout",
            post_with(logout_auth, doc("logoutAuth", "Revoke the current interactive auth session")),
        )
        .api_route(
            "/v1/auth/logout-all",
            post_with(logout_all_auth, doc("logoutAllAuth", "Revoke every interactive auth session for the principal")),
        )
        .api_route(
            "/v1/auth/whoami",
            get_with(whoami, doc("whoAmI", "Return the authenticated principal and memberships")),
        )
        .api_route(
            "/v1/auth/password",
            put_with(change_password, doc("changePassword", "Replace the current principal password and revoke interactive sessions")),
        )
        .api_route(
            "/v1/auth/password/reset",
            post_with(reset_password, doc("resetPassword", "Consume an administrator-issued one-use password reset token")),
        )
        .api_route("/v1/auth/methods", get_with(auth_methods, doc("authMethods", "Available instance-owned GitHub authentication methods")))
        .api_route("/v1/auth/github/device/start", post_with(github_owner_device_start, doc("githubOwnerDeviceStart", "Start owner linking with verified local administrator ownership")))
        .api_route("/v1/auth/github/device/poll", post_with(github_owner_device_poll, doc("githubOwnerDevicePoll", "Poll and complete a principal-bound local owner setup")))
        .api_route("/v1/auth/github/device/cancel", post_with(github_owner_device_cancel, doc("githubOwnerDeviceCancel", "Cancel local owner device authorization")))
        .api_route(
            "/v1/auth/github/start",
            get_with(github_start, doc("githubAuthStart", "Start the instance GitHub OAuth flow with state and S256 PKCE")),
        )
        .api_route(
            "/v1/auth/github/callback",
            get_with(github_callback, doc("githubAuthCallback", "Complete GitHub OAuth, enforce the allowlist, and set browser cookies")),
        )
        .api_route(
            "/v1/auth/github/exchange",
            post_with(github_native_exchange, doc("githubNativeAuthExchange", "Exchange a completed one-use native GitHub handoff for Sift tokens")),
        )
        .api_route(
            "/v1/admin/auth/github-allowlist",
            get_with(list_github_allowlist, doc("listGithubAllowlist", "List GitHub allowlist entries (instance admin)")).post_with(create_github_allowlist, doc("createGithubAllowlist", "Allow a GitHub login, optionally linked to an existing principal")),
        )
        .api_route(
            "/v1/admin/auth/github-allowlist/:id",
            delete_with(revoke_github_allowlist, doc("revokeGithubAllowlist", "Revoke a pending GitHub allowlist entry")),
        )
}

#[inline(never)]
fn accounts(router: ApiRouter<AppState>) -> ApiRouter<AppState> {
    router
        .api_route(
            "/v1/admin/principals",
            post_with(admin_create_principal, doc("adminCreatePasswordPrincipal", "")),
        )
        .api_route(
            "/v1/admin/principals/:id/disabled",
            put_with(admin_set_principal_disabled, doc("adminSetPrincipalDisabled", "")),
        )
        .api_route(
            "/v1/admin/principals/:id/identities",
            get_with(admin_list_principal_identities, doc("adminListPrincipalIdentities", "")),
        )
        .api_route(
            "/v1/admin/principals/:id/identities/password",
            post_with(admin_link_password_identity, doc("adminLinkPasswordIdentity", "")),
        )
        .api_route(
            "/v1/admin/principals/:principal_id/identities/:identity_id",
            delete_with(admin_unlink_identity, doc("adminUnlinkIdentity", "")),
        )
        .api_route(
            "/v1/admin/principals/:id/auth-sessions",
            get_with(admin_list_auth_sessions, doc("adminListAuthSessions", "")),
        )
        .api_route(
            "/v1/admin/principals/:principal_id/auth-sessions/:session_id",
            delete_with(admin_revoke_auth_session, doc("adminRevokeAuthSession", "")),
        )
        .api_route(
            "/v1/admin/principals/:principal_id/identities/:identity_id/password-reset",
            post_with(admin_issue_password_reset, doc("adminIssuePasswordReset", "")),
        )
        .api_route(
            "/v1/metadata/tenants/:id/invitations",
            get_with(list_tenant_invitations, doc("listTenantInvitations", "")).post_with(create_tenant_invitation, doc("createTenantInvitation", "")),
        )
        .api_route(
            "/v1/metadata/tenants/:tenant_id/invitations/:id",
            delete_with(revoke_tenant_invitation, doc("revokeTenantInvitation", "Revoke an unconsumed tenant invitation")),
        )
        .api_route(
            "/v1/auth/invitations/accept",
            post_with(accept_tenant_invitation, doc("acceptTenantInvitation", "")),
        )
        .api_route(
            "/v1/auth/keys",
            get_with(list_principal_keys, doc("listPrincipalKeys", "")).post_with(register_principal_key, doc("registerPrincipalKey", "")),
        )
        .api_route(
            "/v1/auth/keys/:id",
            delete_with(revoke_principal_key, doc("revokePrincipalKey", "Revoke a registered principal key")),
        )
        .api_route(
            "/v1/auth/keys/challenge",
            post_with(issue_key_challenge, doc("issueKeyChallenge", "")),
        )
        .api_route(
            "/v1/auth/keys/authenticate",
            post_with(authenticate_key, doc("authenticateKey", "")),
        )
        .api_route(
            "/v1/auth/ssh-proxy/exchange",
            post_with(
                exchange_ssh_proxy_capability,
                doc(
                    "exchangeSshProxyCapability",
                    "Atomically exchange a one-use SSH bootstrap capability",
                ),
            ),
        )
        .api_route(
            "/v1/metadata/tenants",
            get_with(list_metadata_tenants, doc("listMetadataTenants", "List current principal tenant memberships")),
        )
        .api_route(
            "/v1/metadata/tenants/:tenant_id/members/:principal_id",
            delete_with(remove_metadata_tenant_member, doc("removeMetadataTenantMember", "Remove a tenant member and immediately revoke inherited access")),
        )
        .api_route(
            "/v1/metadata/rooms",
            get_with(list_metadata_rooms, doc("listMetadataRooms", "List rooms for current principal in a tenant")).post_with(create_metadata_room, doc("createMetadataRoom", "Create room")),
        )
        .api_route(
            "/v1/metadata/rooms/:id",
            delete_with(delete_metadata_room, doc("deleteMetadataRoom", "Delete room")),
        )
        .api_route(
            "/v1/metadata/rooms/:id/members",
            get_with(list_metadata_room_members, doc("listMetadataRoomMembers", "List room members")).post_with(add_metadata_room_member, doc("addMetadataRoomMember", "Add or update room member")),
        )
        .api_route(
            "/v1/metadata/rooms/:id/members/:principal_id",
            delete_with(remove_metadata_room_member, doc("removeMetadataRoomMember", "Remove room member")),
        )
        .api_route(
            "/v1/metadata/rooms/:id/connection",
            put_with(bind_metadata_room_connection, doc("bindMetadataRoomConnection", "Bind a connection profile to a room"))
                .delete_with(unbind_metadata_room_connection, doc("unbindMetadataRoomConnection", "Unbind the room's connection")),
        )
        .api_route(
            "/v1/metadata/rooms/:id/join",
            post_with(join_metadata_room, doc("joinMetadataRoom", "Join room as current principal")),
        )
        .api_route(
            "/v1/metadata/rooms/:id/leave",
            post_with(leave_metadata_room, doc("leaveMetadataRoom", "Leave room as current principal")),
        )
        .api_route(
            "/v1/metadata/rooms/:id/ws",
            get_with(ws_room, doc("roomWebSocket", "WebSocket room presence and document operations; protocol uses RoomClientMessage/RoomServerMessage")),
        )
        .api_route(
            "/v1/metadata/rooms/:id/results",
            get_with(list_room_results, doc("listRoomResults", "List transient shared results visible to current room members")),
        )
        .api_route(
            "/v1/metadata/rooms/:id/results/:result_id",
            get_with(get_room_result, doc("getRoomResult", "Get one transient shared-result reference")),
        )
        .api_route(
            "/v1/metadata/rooms/:id/results/:result_id/pages",
            get_with(get_room_result_pages, doc("getRoomResultPages", "Independently page a transient shared result; query params: from_seq and limit")),
        )
        .api_route(
            "/v1/metadata/rooms/:id/documents",
            get_with(list_metadata_documents, doc("listMetadataDocuments", "List room documents")).post_with(create_metadata_document, doc("createMetadataDocument", "Create room document")),
        )
        .api_route(
            "/v1/metadata/documents/:id",
            put_with(update_metadata_document, doc("updateMetadataDocument", "Update document CRDT snapshot")).delete_with(delete_metadata_document, doc("deleteMetadataDocument", "Delete document")),
        )
}

#[inline(never)]
fn workspaces(router: ApiRouter<AppState>) -> ApiRouter<AppState> {
    router
        .api_route(
            "/v1/metadata/rooms/:id/workspaces",
            get_with(
                list_room_workspaces,
                doc("listRoomWorkspaces", "List virtual workspaces in a room"),
            )
            .post_with(
                create_room_workspace,
                doc(
                    "createRoomWorkspace",
                    "Create a room-owned virtual workspace",
                ),
            ),
        )
        .api_route(
            "/v1/metadata/workspaces/:id",
            get_with(
                get_workspace,
                doc("getWorkspace", "Get a virtual workspace"),
            )
            .put_with(
                update_workspace,
                doc(
                    "updateWorkspace",
                    "Rename a virtual workspace using its expected revision",
                ),
            )
            .delete_with(
                delete_workspace,
                doc(
                    "deleteWorkspace",
                    "Delete a virtual workspace using its expected revision",
                ),
            ),
        )
        .api_route(
            "/v1/metadata/workspaces/:id/nodes",
            get_with(
                list_workspace_nodes,
                doc(
                    "listWorkspaceNodes",
                    "List the authoritative workspace tree",
                ),
            )
            .post_with(
                create_workspace_node,
                doc(
                    "createWorkspaceNode",
                    "Create a folder or collaborative SQL file",
                ),
            ),
        )
        .api_route(
            "/v1/metadata/workspaces/:id/nodes/batch",
            post_with(
                mutate_workspace_batch,
                doc(
                    "mutateWorkspaceBatch",
                    "Atomically apply a bounded workspace tree mutation batch",
                ),
            ),
        )
        .api_route(
            "/v1/metadata/workspace-nodes/:id",
            put_with(
                move_workspace_node,
                doc("moveWorkspaceNode", "Move or rename a workspace subtree"),
            )
            .delete_with(
                delete_workspace_node,
                doc("deleteWorkspaceNode", "Delete a workspace subtree"),
            ),
        )
        .api_route(
            "/v1/metadata/workspaces/:id/checkpoints",
            get_with(
                list_workspace_checkpoints,
                doc(
                    "listWorkspaceCheckpoints",
                    "Keyset-page immutable workspace checkpoints",
                ),
            )
            .post_with(
                create_workspace_checkpoint,
                doc(
                    "createWorkspaceCheckpoint",
                    "Capture an immutable workspace checkpoint",
                ),
            ),
        )
        .api_route(
            "/v1/metadata/workspace-checkpoints/:id/restore",
            post_with(
                restore_workspace_checkpoint,
                doc(
                    "restoreWorkspaceCheckpoint",
                    "Restore a checkpoint as a new workspace head revision",
                ),
            ),
        )
        .api_route(
            "/v1/metadata/workspaces/:id/projection",
            get_with(
                get_workspace_projection,
                doc(
                    "getWorkspaceProjection",
                    "Get the optional filesystem projection binding",
                ),
            )
            .post_with(
                bind_workspace_projection,
                doc(
                    "bindWorkspaceProjection",
                    "Bind an operator-configured filesystem root",
                ),
            ),
        )
        .api_route(
            "/v1/metadata/workspace-projections/:id",
            delete_with(
                delete_workspace_projection,
                doc(
                    "deleteWorkspaceProjection",
                    "Remove a filesystem projection binding",
                ),
            ),
        )
        .api_route(
            "/v1/metadata/workspace-projections/:id/reconcile",
            get_with(
                plan_workspace_projection,
                doc(
                    "planWorkspaceProjection",
                    "Read-only deterministic workspace projection plan",
                ),
            )
            .post_with(
                apply_workspace_projection,
                doc(
                    "applyWorkspaceProjection",
                    "Apply explicit projection conflict resolutions",
                ),
            ),
        )
        .api_route(
            "/v1/metadata/workspaces/:id/repository",
            get_with(
                get_workspace_repository,
                doc(
                    "getWorkspaceRepository",
                    "Get the optional repository binding",
                ),
            )
            .post_with(
                bind_workspace_repository,
                doc(
                    "bindWorkspaceRepository",
                    "Bind or initialize Git for a filesystem projection",
                ),
            ),
        )
        .api_route(
            "/v1/metadata/workspaces/:id/repository/clone",
            post_with(
                clone_workspace_repository,
                doc(
                    "cloneWorkspaceRepository",
                    "Clone an HTTPS repository into an empty configured projection",
                ),
            ),
        )
        .api_route(
            "/v1/metadata/repositories/:id",
            delete_with(
                delete_workspace_repository,
                doc("deleteWorkspaceRepository", "Remove a repository binding"),
            ),
        )
        .api_route(
            "/v1/metadata/repositories/:id/status",
            get_with(
                get_repository_status,
                doc(
                    "getRepositoryStatus",
                    "Read bounded typed repository status",
                ),
            ),
        )
        .api_route(
            "/v1/metadata/repositories/:id/diff",
            get_with(
                get_repository_diff,
                doc(
                    "getRepositoryDiff",
                    "Read bounded typed repository diff statistics",
                ),
            ),
        )
        .api_route(
            "/v1/metadata/repositories/:id/branches",
            get_with(
                list_repository_branches,
                doc(
                    "listRepositoryBranches",
                    "List typed local and remote branches",
                ),
            )
            .post_with(
                create_repository_branch,
                doc(
                    "createRepositoryBranch",
                    "Create a local branch from HEAD or a revision",
                ),
            ),
        )
        .api_route(
            "/v1/metadata/repositories/:id/branches/switch",
            post_with(
                switch_repository_branch,
                doc(
                    "switchRepositoryBranch",
                    "Checkpoint and switch a clean shared worktree",
                ),
            ),
        )
        .api_route(
            "/v1/metadata/repositories/:id/branches/rename",
            post_with(
                rename_repository_branch,
                doc("renameRepositoryBranch", "Rename a local branch"),
            ),
        )
        .api_route(
            "/v1/metadata/repositories/:id/branches/delete",
            post_with(
                delete_repository_branch,
                doc(
                    "deleteRepositoryBranch",
                    "Delete a local branch with merged-state protection",
                ),
            ),
        )
        .api_route(
            "/v1/metadata/repositories/:id/branches/upstream",
            post_with(
                set_repository_upstream,
                doc(
                    "setRepositoryUpstream",
                    "Set or clear a local branch upstream",
                ),
            ),
        )
        .api_route(
            "/v1/metadata/repositories/:id/history",
            get_with(
                get_repository_history,
                doc(
                    "getRepositoryHistory",
                    "Keyset-page bounded repository history",
                ),
            ),
        )
        .api_route(
            "/v1/metadata/repositories/:id/history/compare",
            get_with(
                compare_repository_commits,
                doc("compareRepositoryCommits", "Compare two immutable commits"),
            ),
        )
        .api_route(
            "/v1/metadata/repositories/:id/history/:oid",
            get_with(
                get_repository_commit,
                doc(
                    "getRepositoryCommit",
                    "Read bounded commit details and file statistics",
                ),
            ),
        )
        .api_route(
            "/v1/metadata/repositories/:id/history/:oid/file",
            get_with(
                get_repository_historical_file,
                doc(
                    "getRepositoryHistoricalFile",
                    "Read a bounded historical text file",
                ),
            ),
        )
        .api_route(
            "/v1/metadata/repositories/:id/history/restore-file",
            post_with(
                restore_repository_historical_file,
                doc(
                    "restoreRepositoryHistoricalFile",
                    "Checkpoint and restore a historical file",
                ),
            ),
        )
        .api_route(
            "/v1/metadata/repositories/:id/history/revert",
            post_with(
                revert_repository_commit,
                doc(
                    "revertRepositoryCommit",
                    "Checkpoint and preview a commit revert in the worktree",
                ),
            ),
        )
        .api_route(
            "/v1/metadata/repositories/:id/stage",
            post_with(
                stage_repository_paths,
                doc(
                    "stageRepositoryPaths",
                    "Stage root-confined workspace paths",
                ),
            ),
        )
        .api_route(
            "/v1/metadata/repositories/:id/unstage",
            post_with(
                unstage_repository_paths,
                doc(
                    "unstageRepositoryPaths",
                    "Unstage root-confined workspace paths",
                ),
            ),
        )
        .api_route(
            "/v1/metadata/repositories/:id/stage-hunk",
            post_with(
                stage_repository_hunk,
                doc(
                    "stageRepositoryHunk",
                    "Stage one revision-guarded typed diff hunk",
                ),
            ),
        )
        .api_route(
            "/v1/metadata/repositories/:id/unstage-hunk",
            post_with(
                unstage_repository_hunk,
                doc(
                    "unstageRepositoryHunk",
                    "Unstage one revision-guarded typed diff hunk",
                ),
            ),
        )
        .api_route(
            "/v1/metadata/repositories/:id/discard",
            post_with(
                discard_repository_path,
                doc(
                    "discardRepositoryPath",
                    "Checkpoint and discard one tracked worktree path into the canonical workspace",
                ),
            ),
        )
        .api_route(
            "/v1/metadata/repositories/:id/revert-hunk",
            post_with(
                revert_repository_hunk,
                doc(
                    "revertRepositoryHunk",
                    "Checkpoint and revert one worktree diff hunk into the canonical workspace",
                ),
            ),
        )
        .api_route(
            "/v1/metadata/repositories/:id/conflicts",
            get_with(
                get_repository_conflict,
                doc(
                    "getRepositoryConflict",
                    "Read conflict stages as bounded typed regions",
                ),
            ),
        )
        .api_route(
            "/v1/metadata/repositories/:id/conflicts/begin",
            post_with(
                begin_repository_conflict_resolution,
                doc(
                    "beginRepositoryConflictResolution",
                    "Checkpoint before manual conflict resolution",
                ),
            ),
        )
        .api_route(
            "/v1/metadata/repositories/:id/conflicts/resolve",
            post_with(
                resolve_repository_conflict,
                doc(
                    "resolveRepositoryConflict",
                    "Apply one typed conflict resolution",
                ),
            ),
        )
        .api_route(
            "/v1/metadata/repositories/:id/conflicts/mark-resolved",
            post_with(
                mark_repository_conflict_resolved,
                doc(
                    "markRepositoryConflictResolved",
                    "Stage a manually resolved conflict",
                ),
            ),
        )
        .api_route(
            "/v1/metadata/repositories/:id/operation/continue",
            post_with(
                continue_repository_operation,
                doc(
                    "continueRepositoryOperation",
                    "Continue a supported repository operation",
                ),
            ),
        )
        .api_route(
            "/v1/metadata/repositories/:id/operation/abort",
            post_with(
                abort_repository_operation,
                doc(
                    "abortRepositoryOperation",
                    "Checkpoint and abort a repository operation",
                ),
            ),
        )
        .api_route(
            "/v1/metadata/repositories/:id/repair",
            post_with(
                repair_repository_binding,
                doc(
                    "repairRepositoryBinding",
                    "Re-observe a moved or repaired repository projection",
                ),
            ),
        )
        .api_route(
            "/v1/metadata/repositories/:id/commit",
            post_with(
                commit_repository,
                doc(
                    "commitRepository",
                    "Commit one immutable workspace checkpoint",
                ),
            ),
        )
        .api_route(
            "/v1/metadata/repositories/:id/amend",
            post_with(
                amend_repository,
                doc(
                    "amendRepository",
                    "Amend guarded shared HEAD after a workspace checkpoint",
                ),
            ),
        )
        .api_route(
            "/v1/metadata/repositories/:id/uncommit",
            post_with(
                uncommit_repository,
                doc(
                    "uncommitRepository",
                    "Soft-reset guarded shared HEAD after a workspace checkpoint",
                ),
            ),
        )
        .api_route(
            "/v1/metadata/repositories/:id/credential",
            post_with(
                set_repository_credential,
                doc(
                    "setRepositoryCredential",
                    "Set an opaque repository credential",
                ),
            )
            .delete_with(
                delete_repository_credential,
                doc(
                    "deleteRepositoryCredential",
                    "Remove the repository credential",
                ),
            ),
        )
        .api_route(
            "/v1/metadata/repositories/:id/credential/test",
            post_with(
                test_repository_credential,
                doc(
                    "testRepositoryCredential",
                    "Test the stored credential without mutating refs",
                ),
            ),
        )
        .api_route(
            "/v1/metadata/repositories/:id/remotes",
            get_with(
                list_repository_remotes,
                doc("listRepositoryRemotes", "List typed repository remotes"),
            )
            .post_with(
                add_repository_remote,
                doc("addRepositoryRemote", "Add a validated repository remote"),
            ),
        )
        .api_route(
            "/v1/metadata/repositories/:id/remotes/rename",
            post_with(
                rename_repository_remote,
                doc("renameRepositoryRemote", "Rename a repository remote"),
            ),
        )
        .api_route(
            "/v1/metadata/repositories/:id/remotes/update",
            post_with(
                update_repository_remote,
                doc("updateRepositoryRemote", "Replace a repository remote URL"),
            ),
        )
        .api_route(
            "/v1/metadata/repositories/:id/remotes/remove",
            post_with(
                remove_repository_remote,
                doc("removeRepositoryRemote", "Remove a repository remote"),
            ),
        )
        .api_route(
            "/v1/metadata/repositories/:id/fetch",
            post_with(
                fetch_repository,
                doc(
                    "fetchRepository",
                    "Fetch with the bound one-operation credential helper",
                ),
            ),
        )
        .api_route(
            "/v1/metadata/repositories/:id/push",
            post_with(
                push_repository,
                doc(
                    "pushRepository",
                    "Push with the bound one-operation credential helper",
                ),
            ),
        )
        .api_route(
            "/v1/metadata/repositories/:id/hosting",
            get_with(
                get_repository_hosting,
                doc(
                    "getRepositoryHosting",
                    "Detect hosting and load safe links, pull requests, and checks",
                ),
            ),
        )
        .api_route(
            "/v1/metadata/repositories/:id/hosting/repositories",
            get_with(
                list_hosting_repositories,
                doc(
                    "listHostingRepositories",
                    "List repositories visible to the separate hosting credential",
                ),
            ),
        )
        .api_route(
            "/v1/metadata/repositories/:id/hosting/credential",
            post_with(
                set_hosting_credential,
                doc(
                    "setHostingCredential",
                    "Store a per-principal hosting API credential separately from Git transport",
                ),
            )
            .delete_with(
                delete_hosting_credential,
                doc(
                    "deleteHostingCredential",
                    "Remove the current principal hosting credential",
                ),
            ),
        )
        .api_route(
            "/v1/metadata/repositories/:id/hosting/pull-requests",
            post_with(
                create_hosting_pull_request,
                doc(
                    "createHostingPullRequest",
                    "Create a pull request from the current pushed branch",
                ),
            ),
        )
        .api_route(
            "/v1/metadata/workspaces/:id/ddl-sources",
            get_with(
                list_ddl_sources,
                doc("listDdlSources", "List offline DDL sources"),
            )
            .post_with(
                create_ddl_source,
                doc(
                    "createDdlSource",
                    "Create an offline DDL source over workspace roots",
                ),
            ),
        )
        .api_route(
            "/v1/metadata/ddl-sources/:id",
            get_with(
                get_ddl_source,
                doc("getDdlSource", "Get an offline DDL model and mappings"),
            )
            .put_with(
                update_ddl_source,
                doc("updateDdlSource", "Update DDL roots and live mappings"),
            )
            .delete_with(
                delete_ddl_source,
                doc("deleteDdlSource", "Delete an offline DDL source"),
            ),
        )
        .api_route(
            "/v1/metadata/ddl-sources/:id/refresh",
            post_with(
                refresh_ddl_source,
                doc(
                    "refreshDdlSource",
                    "Rebuild the deterministic offline catalog graph",
                ),
            ),
        )
}

#[inline(never)]
fn automation(router: ApiRouter<AppState>) -> ApiRouter<AppState> {
    router
        .api_route(
            "/v1/metadata/workspaces/:id/run-configurations",
            get_with(list_run_configurations, doc("listRunConfigurations", "List workspace run configurations")).post_with(create_run_configuration, doc("createRunConfiguration", "Create a revisioned foreground run configuration")),
        )
        .api_route(
            "/v1/metadata/workspaces/:id/runs/latest-success",
            get_with(latest_successful_run_for_commit, doc("getLatestSuccessfulRunForCommit", "Get the latest successful immutable run for a Git commit")),
        )
        .api_route(
            "/v1/metadata/run-configurations/:id",
            get_with(get_run_configuration, doc("getRunConfiguration", "Get a run configuration")).put_with(update_run_configuration, doc("updateRunConfiguration", "Update a run configuration by revision")).delete_with(delete_run_configuration, doc("deleteRunConfiguration", "Delete an unused run configuration by revision")),
        )
        .api_route(
            "/v1/metadata/run-configurations/:id/validate",
            post_with(validate_run_configuration, doc("validateRunConfiguration", "Validate current scripts, variables, and target profile")),
        )
        .api_route(
            "/v1/metadata/run-configurations/:id/runs",
            post_with(start_run, doc("startRun", "Capture and start an immutable foreground run")),
        )
        .api_route(
            "/v1/metadata/run-configurations/:id/schedules",
            get_with(list_run_schedules, doc("listRunSchedules", "List durable schedules for a run configuration")).post_with(create_run_schedule, doc("createRunSchedule", "Create an owner-bound durable run schedule")),
        )
        .api_route(
            "/v1/metadata/schedules/:id",
            get_with(get_run_schedule, doc("getRunSchedule", "Get a durable run schedule")).put_with(update_run_schedule, doc("updateRunSchedule", "Update a run schedule by revision")).delete_with(delete_run_schedule, doc("deleteRunSchedule", "Delete a run schedule by revision")),
        )
        .api_route(
            "/v1/metadata/schedules/:id/enable",
            post_with(enable_run_schedule, doc("enableRunSchedule", "Enable a run schedule by revision")),
        )
        .api_route(
            "/v1/metadata/schedules/:id/disable",
            post_with(disable_run_schedule, doc("disableRunSchedule", "Disable a run schedule by revision")),
        )
        .api_route(
            "/v1/metadata/schedules/:id/occurrences",
            get_with(list_schedule_occurrences, doc("listScheduleOccurrences", "Inspect durable schedule occurrences")),
        )
        .api_route(
            "/v1/metadata/schedule-occurrences/:id/resume",
            post_with(resume_schedule_occurrence, doc("resumeScheduleOccurrence", "Requeue a blocked occurrence after reauthorization")),
        )
        .api_route(
            "/v1/metadata/workspaces/:id/transfer-recipes",
            get_with(list_transfer_recipes, doc("listTransferRecipes", "List workspace transfer recipes")).post_with(create_transfer_recipe, doc("createTransferRecipe", "Create a revisioned transfer recipe")),
        )
        .api_route(
            "/v1/metadata/workspaces/:id/quarantine-artifacts",
            get_with(list_quarantine_artifacts, doc("listQuarantineArtifacts", "List retained CSV quarantine reports")),
        )
        .api_route(
            "/v1/metadata/transfer-recipes/:id",
            get_with(get_transfer_recipe, doc("getTransferRecipe", "Get a transfer recipe")).put_with(update_transfer_recipe, doc("updateTransferRecipe", "Update a transfer recipe by revision")).delete_with(delete_transfer_recipe, doc("deleteTransferRecipe", "Delete a transfer recipe by revision")),
        )
        .api_route(
            "/v1/metadata/transfer-recipes/:id/validate",
            post_with(validate_transfer_recipe, doc("validateTransferRecipe", "Validate a recipe and bundled format")),
        )
        .api_route(
            "/v1/metadata/transfer-recipes/:id/preview-source",
            post_with(preview_transfer_source,doc("previewTransferSource","Review bounded XLSX/Parquet source schema and mapped samples without database access"))
                .layer(axum::extract::DefaultBodyLimit::max(crate::transfer::source_preview::MAX_WIRE_BYTES))
                .layer(axum::middleware::from_fn(crate::transfer::source_preview::admit_request)),
        )
        .api_route(
            "/v1/metadata/transfer-recipes/:id/execute",
            post_with(execute_transfer_recipe, doc("executeTransferRecipe", "Execute a bounded query-to-artifact recipe")),
        )
        .api_route(
            "/v1/metadata/artifacts/:id",
            get_with(get_workspace_artifact, doc("getWorkspaceArtifact", "Download an immutable workspace artifact")),
        )
        .api_route(
            "/v1/metadata/runs/:id",
            get_with(get_run, doc("getRun", "Get durable foreground run state")),
        )
        .api_route(
            "/v1/metadata/runs/:id/steps",
            get_with(get_run_steps, doc("getRunSteps", "Get ordered run step outcomes")),
        )
        .api_route(
            "/v1/metadata/runs/:id/logs",
            get_with(get_run_logs, doc("getRunLogs", "Page bounded redacted run logs")),
        )
        .api_route(
            "/v1/metadata/runs/:id/cancel",
            post_with(cancel_run, doc("cancelRun", "Request cancellation of a foreground run")),
        )
        .api_route(
            "/v1/metadata/runs/:id/rerun",
            post_with(rerun, doc("rerun", "Create a new run from an immutable prior manifest")),
        )
}

#[inline(never)]
fn connections(router: ApiRouter<AppState>) -> ApiRouter<AppState> {
    router
        .api_route(
            "/v1/metadata/connections",
            get_with(list_metadata_connections, doc("listMetadataConnectionProfiles", "List connection profiles")).post_with(upsert_metadata_connection, doc("upsertMetadataConnectionProfile", "Create or replace connection profile")),
        )
        .api_route(
            "/v1/tailnet/status",
            get_with(tailnet_status, doc("tailnetStatus", "Discover devices visible to the Sift backend")),
        )
        .api_route(
            "/v1/metadata/connections/validate",
            post_with(validate_connection_candidate, doc("validateConnectionCandidate", "Test candidate settings without saving; reuse existing credentials when omitted")),
        )
        .api_route(
            "/v1/tailnet/probe",
            post_with(tailnet_probe, doc("tailnetProbe", "Diagnose tailnet database connectivity")),
        )
        .api_route(
            "/v1/tailnet/serve",
            post_with(tailnet_serve, doc("tailnetServe", "Preview and manage explicitly approved remote Serve forwarding")),
        )
        .api_route(
            "/v1/tailnet/host-key",
            post_with(tailnet_host_key, doc("tailnetHostKey", "Read an untrusted SSH fingerprint for explicit verification")),
        )
        .api_route(
            "/v1/metadata/connections/:id",
            delete_with(delete_metadata_connection, doc("deleteMetadataConnectionProfile", "Delete connection profile")),
        )
        .api_route(
            "/v1/metadata/connections/:id/credential",
            post_with(set_metadata_connection_credential, doc("setMetadataConnectionCredential", "Set per-user credential for connection profile")),
        )
        .api_route(
            "/v1/metadata/connections/:id/policy",
            get_with(get_metadata_connection_policy, doc("getMetadataConnectionPolicy", "Read the effective connection profile policy")).put_with(update_metadata_connection_policy, doc("updateMetadataConnectionPolicy", "Replace a connection profile policy with optimistic revision checking")),
        )
        .api_route(
            "/v1/metadata/connections/:id/disconnect",
            post_with(disconnect_metadata_connection, doc("disconnectMetadataConnectionProfile", "Immediately close all active connections using a managed profile")),
        )
        .api_route(
            "/v1/metadata/tenants/:id/usage",
            get_with(get_tenant_usage, doc("getTenantUsage", "Read effective tenant limits and current resource usage")),
        )
        .api_route(
            "/v1/admin/tenants/:id/limits",
            put_with(set_tenant_limits, doc("setTenantLimits", "Set an operator-bounded tenant resource limit override")).delete_with(clear_tenant_limits, doc("clearTenantLimits", "Clear a tenant resource limit override")),
        )
        .api_route(
            "/v1/metadata/history",
            get_with(list_metadata_history, doc("listMetadataHistory", "List query history by room or current principal")),
        )
        .api_route(
            "/v1/metadata/history/pages",
            get_with(page_metadata_history, doc("pageMetadataHistory", "Keyset-page query history by room or current principal")),
        )
        .api_route(
            "/v1/metadata/history/performance",
            get_with(get_query_performance, doc("getQueryPerformance", "Summarize the current principal's query timings without SQL payloads")),
        )
        .api_route(
            "/v1/metadata/saved-queries",
            get_with(list_metadata_saved_queries, doc("listMetadataSavedQueries", "List visible personal and tenant-shared saved queries")).post_with(create_metadata_saved_query, doc("createMetadataSavedQuery", "Create a personal or tenant-shared saved query")),
        )
        .api_route(
            "/v1/metadata/saved-queries/:id",
            get_with(get_metadata_saved_query, doc("getMetadataSavedQuery", "")).put_with(update_metadata_saved_query, doc("updateMetadataSavedQuery", "")).delete_with(delete_metadata_saved_query, doc("deleteMetadataSavedQuery", "")),
        )
        .api_route(
            "/v1/metadata/sql-snippets",
            get_with(list_sql_snippets, doc("listSqlSnippets", "List dialect-filterable built-in and visible saved SQL snippets")).post_with(create_sql_snippet, doc("createSqlSnippet", "Create a versioned personal, workspace, or tenant SQL snippet")),
        )
        .api_route(
            "/v1/metadata/sql-snippets/:id",
            put_with(update_sql_snippet, doc("updateSqlSnippet", "Update an owned SQL snippet using optimistic revision checking")).delete_with(delete_sql_snippet, doc("deleteSqlSnippet", "Delete an owned SQL snippet using optimistic revision checking")),
        )
        .api_route(
            "/v1/metadata/vaults",
            get_with(list_metadata_vaults, doc("listMetadataVaults", "List personal and shared vaults visible to the caller")).post_with(create_metadata_vault, doc("createMetadataVault", "Create a personal or team vault")),
        )
        .api_route(
            "/v1/metadata/vaults/:id",
            get_with(get_metadata_vault, doc("getMetadataVault", "Read one visible vault")).put_with(update_metadata_vault, doc("updateMetadataVault", "Rename a vault with optimistic revision checking")).delete_with(delete_metadata_vault, doc("deleteMetadataVault", "Delete a team vault and its stored items")),
        )
        .api_route(
            "/v1/metadata/vaults/:id/items",
            get_with(list_metadata_vault_items, doc("listMetadataVaultItems", "List redacted vault item metadata")).post_with(create_metadata_vault_item, doc("createMetadataVaultItem", "Store a versioned vault item")),
        )
        .api_route(
            "/v1/metadata/vaults/:id/grants",
            get_with(list_metadata_vault_grants, doc("listMetadataVaultGrants", "List explicit team-vault grants")),
        )
        .api_route(
            "/v1/metadata/vaults/:id/grants/:principal",
            put_with(set_metadata_vault_grant, doc("setMetadataVaultGrant", "Create or replace a team-vault grant")).delete_with(delete_metadata_vault_grant, doc("deleteMetadataVaultGrant", "Remove a team-vault grant")),
        )
        .api_route(
            "/v1/metadata/vault-items/:id",
            get_with(get_metadata_vault_item, doc("getMetadataVaultItem", "Read redacted vault item metadata")).put_with(update_metadata_vault_item, doc("updateMetadataVaultItem", "Update metadata and optionally rotate a vault item secret")).delete_with(delete_metadata_vault_item, doc("deleteMetadataVaultItem", "Delete a vault item and retire its secret versions")),
        )
        .api_route(
            "/v1/metadata/vault-items/:id/secret",
            post_with(set_metadata_vault_item_secret, doc("setMetadataVaultItemSecret", "Replace a vault item secret without revealing the prior value")).delete_with(clear_metadata_vault_item_secret, doc("clearMetadataVaultItemSecret", "Clear a vault item secret")),
        )
        .api_route(
            "/v1/metadata/vault-items/:id/versions",
            get_with(list_metadata_vault_item_versions, doc("listMetadataVaultItemVersions", "List redacted vault item history")),
        )
        .api_route(
            "/v1/metadata/vault-items/:id/versions/:version",
            get_with(get_metadata_vault_item_version, doc("getMetadataVaultItemVersion", "Read one redacted vault item version")),
        )
        .api_route(
            "/v1/metadata/vault-items/:id/diff",
            get_with(diff_metadata_vault_item_versions, doc("diffMetadataVaultItemVersions", "Compare redacted vault item versions")),
        )
        .api_route(
            "/v1/metadata/vault-items/:id/restore",
            post_with(restore_metadata_vault_item, doc("restoreMetadataVaultItem", "Restore a retained vault item version as a new head")),
        )
        .api_route(
            "/v1/metadata/vault-items/:id/test",
            post_with(test_metadata_vault_item, doc("testMetadataVaultItem", "Test a vault-backed connection without revealing credentials")),
        )
        .api_route(
            "/v1/metadata/vault-items/:id/reveal",
            post_with(reveal_metadata_vault_item, doc("revealMetadataVaultItem", "Reveal a non-connection secret after local trust or a single-use step-up lease")),
        )
        .api_route(
            "/v1/metadata/vault-items/:id/reveal-step-up",
            post_with(step_up_metadata_vault_reveal, doc("stepUpMetadataVaultReveal", "Verify the interactive account password and issue a short single-use reveal lease")),
        )
}

#[inline(never)]
fn saved_performance(router: ApiRouter<AppState>) -> ApiRouter<AppState> {
    router
        .api_route(
            "/v1/metadata/tenants/:tenant/catalog-snapshots",
            get_with(
                list_catalog_snapshots,
                doc(
                    "listCatalogSnapshots",
                    "List immutable catalog snapshots in a tenant",
                ),
            ),
        )
        .api_route(
            "/v1/metadata/tenants/:tenant/catalog-snapshots/:snapshot",
            get_with(
                get_catalog_snapshot,
                doc("getCatalogSnapshot", "Get an immutable catalog snapshot"),
            )
            .delete_with(
                delete_catalog_snapshot,
                doc(
                    "deleteCatalogSnapshot",
                    "Delete a catalog snapshot using its metadata revision",
                ),
            ),
        )
        .api_route(
            "/v1/metadata/tenants/:tenant/migration-runs/:run",
            get_with(
                get_durable_migration_run,
                doc(
                    "getDurableMigrationRun",
                    "Get a durable redacted migration run outcome",
                ),
            ),
        )
        .api_route(
            "/v1/metadata/tenants/:tenant/migration-runs/:run/native-outcomes",
            get_with(
                migration_native::get,
                doc(
                    "getMigrationNativeOutcomes",
                    "Read revision-bound redacted native migration observations",
                ),
            ),
        )
        .api_route(
            "/v1/metadata/tenants/:tenant/benchmark-retention",
            get_with(
                get_benchmark_retention,
                doc(
                    "getBenchmarkRetention",
                    "Read your private saved-benchmark retention policy",
                ),
            )
            .put_with(
                update_benchmark_retention,
                doc(
                    "updateBenchmarkRetention",
                    "Activate reviewed automatic removal of owned benchmark snapshots",
                ),
            ),
        )
        .api_route(
            "/v1/metadata/tenants/:tenant/benchmark-retention/preview",
            post_with(
                preview_benchmark_retention,
                doc(
                    "previewBenchmarkRetention",
                    "Review exact current cleanup and ongoing owner-private retention criteria",
                ),
            ),
        )
        .api_route(
            "/v1/metadata/tenants/:tenant/benchmark-runs",
            get_with(
                list_benchmark_runs,
                doc(
                    "listBenchmarkRuns",
                    "List your private saved benchmark runs",
                ),
            )
            .post_with(
                save_benchmark_run,
                doc(
                    "saveBenchmarkRun",
                    "Save an immutable private benchmark snapshot",
                ),
            )
            .layer(axum::extract::DefaultBodyLimit::max(4 * 1024 * 1024)),
        )
        .api_route(
            "/v1/metadata/tenants/:tenant/benchmark-runs/:run",
            get_with(
                get_benchmark_run,
                doc("getBenchmarkRun", "Get your private benchmark snapshot"),
            )
            .delete_with(
                delete_benchmark_run,
                doc(
                    "deleteBenchmarkRun",
                    "Delete your private benchmark snapshot",
                ),
            ),
        )
        .api_route(
            "/v1/metadata/tenants/:tenant/benchmark-runs/:run/annotations",
            put_with(
                update_benchmark_run_annotations,
                doc(
                    "updateBenchmarkRunAnnotations",
                    "Update private notes and tags without changing saved measurements",
                ),
            )
            .layer(axum::extract::DefaultBodyLimit::max(64 * 1024)),
        )
        .api_route(
            "/v1/metadata/tenants/:tenant/benchmark-definitions",
            get_with(
                list_benchmark_definitions,
                doc(
                    "listBenchmarkDefinitions",
                    "List private reusable benchmark definitions",
                ),
            )
            .post_with(
                save_benchmark_definition,
                doc(
                    "saveBenchmarkDefinition",
                    "Save a private benchmark definition",
                ),
            )
            .layer(axum::extract::DefaultBodyLimit::max(1024 * 1024 + 4096)),
        )
        .api_route(
            "/v1/metadata/tenants/:tenant/benchmark-definitions/:definition",
            get_with(
                get_benchmark_definition,
                doc(
                    "getBenchmarkDefinition",
                    "Get a private benchmark definition",
                ),
            )
            .put_with(
                update_benchmark_definition,
                doc(
                    "updateBenchmarkDefinition",
                    "Update a private benchmark definition by revision",
                ),
            )
            .delete_with(
                delete_benchmark_definition,
                doc(
                    "deleteBenchmarkDefinition",
                    "Delete a private benchmark definition",
                ),
            )
            .layer(axum::extract::DefaultBodyLimit::max(1024 * 1024 + 4096)),
        )
        .api_route(
            "/v1/metadata/tenants/:tenant/plan-captures",
            get_with(
                list_plan_captures,
                doc(
                    "listPlanCaptures",
                    "Keyset-page durable normalized plan captures",
                ),
            ),
        )
        .api_route(
            "/v1/metadata/tenants/:tenant/plan-captures/compare",
            post_with(
                compare_plan_captures,
                doc(
                    "comparePlanCaptures",
                    "Compare two same-engine normalized plan captures",
                ),
            ),
        )
        .api_route(
            "/v1/metadata/tenants/:tenant/plan-captures/:capture",
            get_with(
                get_plan_capture,
                doc("getPlanCapture", "Get a durable normalized plan capture"),
            )
            .delete_with(
                delete_plan_capture,
                doc(
                    "deletePlanCapture",
                    "Delete a plan capture using its metadata revision",
                ),
            ),
        )
        .api_route(
            "/v1/auth/tokens",
            get_with(
                list_auth_tokens,
                doc("listAuthTokens", "List current principal API tokens"),
            )
            .post_with(
                issue_auth_token,
                doc("issueAuthToken", "Issue API token; plaintext returned once"),
            ),
        )
        .api_route(
            "/v1/auth/tokens/:id",
            delete_with(
                revoke_auth_token,
                doc("revokeAuthToken", "Revoke API token"),
            ),
        )
}

#[inline(never)]
fn queries(router: ApiRouter<AppState>) -> ApiRouter<AppState> {
    router
        .api_route(
            "/v1/sessions",
            post_with(create_session, doc("createSession", "Create session")).get_with(list_sessions, doc("listSessions", "List sessions")),
        )
        .api_route(
            "/v1/sessions/:id",
            get_with(get_session, doc("getSession", "Get session")).delete_with(close_session, doc("closeSession", "Close session")),
        )
        .api_route(
            "/v1/sessions/:id/connections",
            post_with(open_connection, doc("openConnection", "Open connection")).get_with(list_connections, doc("listConnections", "List connections")),
        )
        .api_route(
            "/v1/sessions/:id/connections/from-profile",
            post_with(open_connection_from_profile, doc("openConnectionFromProfile", "Open session connection from metadata profile")),
        )
        .api_route(
            "/v1/sessions/:id/connections/:conn_id",
            delete_with(close_connection, doc("closeConnection", "Close connection")),
        )
        .api_route(
            "/v1/sessions/:id/connections/:conn_id/ping",
            post_with(ping_connection, doc("pingConnection", "Ping connection")),
        )
        .api_route(
            "/v1/sessions/:id/connections/:conn_id/bulk-insert",
            post_with(bulk_insert, doc("bulkInsert", "Bulk insert CSV into SQL Server or preview/apply typed SQLite rows"))
                .layer(axum::extract::DefaultBodyLimit::max(10 * 1024 * 1024)),
        )
        .api_route(
            "/v1/sessions/:id/connections/:conn_id/import/csv",
            post_with(import_csv, doc("importCsv", "Import CSV into a table")),
        )
        .api_route(
            "/v1/sessions/:id/connections/:conn_id/schema",
            get_with(get_schema, doc("getSchema", "Fetch schema")),
        )
        .api_route(
            "/v1/sessions/:id/connections/:conn_id/catalog/graph",
            post_with(post_catalog_graph, doc("getCatalogGraph", "Fetch a revisioned, dependency-aware catalog graph")),
        )
        .api_route(
            "/v1/sessions/:id/connections/:conn_id/catalog/snippets/prepare",
            post_with(prepare_catalog_snippet, doc("prepareCatalogSnippet", "Prepare an object template against an exact active catalog revision")),
        )
        .api_route(
            "/v1/sessions/:id/connections/:conn_id/catalog/diagram",
            post_with(post_catalog_diagram, doc("projectCatalogDiagram", "Project a deterministic diagram from an exact catalog revision")),
        )
        .api_route(
            "/v1/sessions/:id/connections/:conn_id/catalog/diagram/mutations/preview",
            post_with(preview_catalog_diagram_mutation, doc("previewCatalogDiagramMutation", "Translate a declarative diagram mutation into a normal migration plan")),
        )
        .api_route(
            "/v1/sessions/:id/connections/:conn_id/catalog/snapshots",
            post_with(create_catalog_snapshot, doc("createCatalogSnapshot", "Persist an immutable tenant-scoped catalog graph snapshot")),
        )
        .api_route(
            "/v1/sessions/:id/connections/:conn_id/catalog/diffs",
            post_with(compare_catalog_schemas, doc("compareCatalogSchemas", "Compare two authorized catalog sources using normalized dependency-aware changes")),
        )
        .api_route(
            "/v1/sessions/:id/connections/:conn_id/catalog/migrations/preview",
            post_with(preview_migration, doc("previewMigration", "Create a short-lived scope-bound migration plan from a revalidated schema diff")),
        )
        .api_route(
            "/v1/sessions/:id/connections/:conn_id/catalog/migrations/apply",
            post_with(apply_migration, doc("applyMigration", "Apply a one-use migration plan with revision and risk checks")),
        )
        .api_route(
            "/v1/sessions/:id/connections/:conn_id/catalog/migrations/validate",
            post_with(validate_migration, doc("validateMigration", "Execute a migration on an explicitly confirmed test database and roll it back")),
        )
        .api_route(
            "/v1/sessions/:id/connections/:conn_id/catalog/migrations/runs/:run",
            get_with(get_migration_run, doc("getMigrationRun", "Get a process-local migration run outcome")),
        )
        .api_route(
            "/v1/sessions/:id/connections/:conn_id/catalog/migrations/runs/:run/cancel",
            post_with(cancel_migration, doc("cancelMigration", "Request cancellation at the next safe migration statement boundary")),
        )
        .api_route(
            "/v1/sessions/:id/comparisons",
            post_with(start_comparison, doc("startComparison", "Start a bounded comparison between exact live-table or immutable retained-result sources")),
        )
        .api_route(
            "/v1/sessions/:id/comparisons/:comparison",
            get_with(get_comparison, doc("getComparison", "Get an immutable comparison summary or current running state")),
        )
        .api_route(
            "/v1/sessions/:id/comparisons/:comparison/pages",
            post_with(page_comparison, doc("pageComparison", "Keyset-page retained comparison row differences")),
        )
        .api_route(
            "/v1/sessions/:id/comparisons/:comparison/cancel",
            post_with(cancel_comparison, doc("cancelComparison", "Cancel a running comparison")),
        )
        .api_route(
            "/v1/sessions/:id/comparisons/:comparison/patch",
            post_with(prepare_comparison_patch, doc("prepareComparisonPatch", "Prepare an optimistic edit plan for an eligible complete comparison")),
        )
        .api_route(
            "/v1/sessions/:id/connections/:conn_id/plan-captures",
            post_with(capture_semantic_plan, doc("captureSemanticPlan", "Capture and durably persist a revision-bound normalized semantic statement plan")),
        )
        .api_route(
            "/v1/sessions/:id/connections/:conn_id/ddl",
            get_with(get_object_ddl, doc("getObjectDdl", "Generate DDL (CREATE statement) for a database object. Query params: `name` (required), `schema`, `kind` (defaults to `table`).")),
        )
        .api_route(
            "/v1/sessions/:id/connections/:conn_id/complete",
            post_with(post_completion, doc("postCompletion", "Compute ranked autocomplete candidates for a SQL text + cursor position on the connection's engine.")),
        )
        .api_route(
            "/v1/sessions/:id/connections/:conn_id/semantic-documents",
            post_with(open_semantic_document, doc("openSemanticDocument", "Create bounded parsed SQL document state for this connection's dialect")),
        )
        .api_route(
            "/v1/sessions/:id/connections/:conn_id/semantic-documents/:document",
            put_with(update_semantic_document, doc("updateSemanticDocument", "Replace SQL text using an optimistic semantic revision")).delete_with(close_semantic_document, doc("closeSemanticDocument", "Release process-local semantic document state")),
        )
}

#[inline(never)]
fn analysis(router: ApiRouter<AppState>) -> ApiRouter<AppState> {
    router
        .api_route(
            "/v1/sessions/:id/connections/:conn_id/semantic-documents/:document/statements/select",
            post_with(select_semantic_statement, doc("selectSemanticStatement", "Select the active top-level SQL statement using UTF-8 byte offsets")),
        )
        .api_route(
            "/v1/sessions/:id/connections/:conn_id/semantic-documents/:document/diagnostics",
            post_with(semantic_diagnostics, doc("diagnoseSemanticDocument", "Read syntax diagnostics for an exact semantic document revision")),
        )
        .api_route(
            "/v1/sessions/:id/connections/:conn_id/semantic-documents/:document/format",
            post_with(format_semantic_document, doc("formatSemanticDocument", "Prepare deterministic formatting edits for an exact semantic document revision")),
        )
        .api_route(
            "/v1/sessions/:id/connections/:conn_id/semantic-documents/:document/quick-fixes/:fix",
            post_with(prepare_semantic_quick_fix, doc("prepareSemanticQuickFix", "Prepare a catalog-revision-bound semantic quick fix")),
        )
        .api_route(
            "/v1/sessions/:id/connections/:conn_id/semantic-documents/:document/usages",
            post_with(find_semantic_usages, doc("findSemanticUsages", "Find bounded usages in an exact semantic document revision")),
        )
        .api_route(
            "/v1/sessions/:id/connections/:conn_id/semantic-documents/:document/refactors/prepare",
            post_with(prepare_semantic_refactor, doc("prepareSemanticRefactor", "Prepare revision-bound semantic refactor edits without applying them")),
        )
        .api_route(
            "/v1/sessions/:id/connections/:conn_id/semantic-documents/:document/complete",
            post_with(complete_semantic_document, doc("completeSemanticDocument", "Complete SQL from an exact shared semantic document revision")),
        )
        .api_route(
            "/v1/sessions/:id/connections/:conn_id/semantic-documents/:document/hover",
            post_with(hover_semantic_document, doc("hoverSemanticDocument", "Describe the typed symbol at a UTF-8 byte position in an exact semantic document revision")),
        )
        .api_route(
            "/v1/sessions/:id/connections/:conn_id/semantic-documents/:document/star-expansions/prepare",
            post_with(prepare_star_expansion, doc("prepareStarExpansion", "Prepare an exact catalog-revision-bound wildcard replacement without applying it")),
        )
        .api_route(
            "/v1/sessions/:id/connections/:conn_id/export",
            post_with(export_query, doc("exportQuery", "Stream a query result as CSV / TSV / JSON Lines / JSON Array. Response is chunked; Content-Type depends on the requested format.")),
        )
        .api_route(
            "/v1/sessions/:id/connections/:conn_id/edits/preview",
            post_with(post_edits_preview, doc("previewEdits", "Preview parameterized inline-edit DML")),
        )
        .api_route(
            "/v1/sessions/:id/connections/:conn_id/edits/apply",
            post_with(post_edits_apply, doc("applyEdits", "Apply inline edits transactionally")),
        )
        .api_route(
            "/v1/sessions/:id/connections/:conn_id/search/schema",
            post_with(post_search_schema, doc("searchSchema", "Search schema objects and columns")),
        )
        .api_route(
            "/v1/sessions/:id/connections/:conn_id/search/data",
            post_with(post_search_data, doc("searchData", "Search table data with bounded fan-out")),
        )
        .api_route(
            "/v1/sessions/:id/connections/:conn_id/explain",
            post_with(post_explain, doc("explainQuery", "Capture a typed execution plan")),
        )
        .api_route(
            "/v1/sessions/:id/connections/:conn_id/profile",
            post_with(post_profile, doc("profileQuery", "Capture a bounded PostgreSQL or SQL Server actual plan")),
        )
        .api_route(
            "/v1/sessions/:id/connections/:conn_id/profile/:run_id/progress",
            get_with(get_profile_progress, doc("readProfileProgress", "Read row-free active Profile stages")),
        )
        .api_route(
            "/v1/sessions/:id/connections/:conn_id/benchmark/:run_id/progress",
            get_with(get_benchmark_progress, doc("readBenchmarkProgress", "Read bounded active benchmark attempt progress")),
        )
        .api_route(
            "/v1/sessions/:id/connections/:conn_id/profile/:run_id/cancel",
            post_with(post_cancel_profile, doc("cancelProfile", "Cancel an active Profile run")),
        )
        .api_route(
            "/v1/sessions/:id/connections/:conn_id/benchmark/suite",
            post_with(post_benchmark_suite, doc("benchmarkSuite", "Run a bounded serial query and parameter-case suite")),
        )
        .api_route(
            "/v1/sessions/:id/connections/:conn_id/benchmark/pair",
            post_with(post_benchmark_pair, doc("benchmarkPair", "Run controlled serial A/B workloads with a recorded order")),
        )
        .api_route(
            "/v1/sessions/:id/connections/:conn_id/benchmark",
            post_with(post_benchmark, doc("benchmarkQuery", "Run a bounded read-query benchmark on a dedicated connection")),
        )
        .api_route(
            "/v1/sessions/:id/connections/:conn_id/benchmark-definitions/:definition/run",
            post_with(run_benchmark_definition, doc("runBenchmarkDefinition", "Run a reviewed private definition revision with current bind values and confirmation")),
        )
        .api_route(
            "/v1/sessions/:id/connections/:conn_id/benchmark/:run_id/cancel",
            post_with(post_cancel_benchmark, doc("cancelBenchmark", "Cancel a benchmark and retain partial samples")),
        )
        .api_route(
            "/v1/sessions/:id/connections/:conn_id/dashboard",
            get_with(read_server_dashboard, doc("readServerDashboard", "Read a bounded database server overview")),
        )
        .api_route(
            "/v1/sessions/:id/connections/:conn_id/processes",
            get_with(list_processes, doc("listProcesses", "List database processes")),
        )
        .api_route(
            "/v1/sessions/:id/connections/:conn_id/deadlocks",
            get_with(list_deadlocks, doc("listDeadlocks", "List retained native deadlock events")),
        )
        .api_route(
            "/v1/sessions/:id/connections/:conn_id/settings/postgres",
            get_with(list_postgres_settings, doc("listPostgresSettings", "Browse PostgreSQL server settings")),
        )
        .api_route(
            "/v1/sessions/:id/connections/:conn_id/postgres/extensions",
            get_with(list_postgres_extensions, doc("listPostgresExtensions", "Browse PostgreSQL database extensions")),
        )
        .api_route(
            "/v1/sessions/:id/connections/:conn_id/postgres/partitions",
            get_with(list_postgres_partitions, doc("listPostgresPartitions", "Browse PostgreSQL partitions")),
        )
}

#[inline(never)]
fn workbench(router: ApiRouter<AppState>) -> ApiRouter<AppState> {
    router
        .api_route(
            "/v1/sessions/:id/connections/:conn_id/postgres/policies",
            get_with(
                list_postgres_policies,
                doc(
                    "listPostgresPolicies",
                    "Browse bounded PostgreSQL row security policies",
                ),
            ),
        )
        .api_route(
            "/v1/sessions/:id/connections/:conn_id/imports/:run_id/progress",
            get_with(
                get_import_progress,
                doc("readImportProgress", "Read active reviewed import progress"),
            ),
        )
        .api_route(
            "/v1/sessions/:id/connections/:conn_id/imports/:run_id/cancel",
            post_with(
                post_cancel_import,
                doc(
                    "cancelImport",
                    "Request cancellation of the original import",
                ),
            ),
        )
        .api_route(
            "/v1/sessions/:id/connections/:conn_id/postgres/roles",
            get_with(
                list_postgres_roles,
                doc(
                    "listPostgresRoles",
                    "Browse PostgreSQL roles without secrets",
                ),
            ),
        )
        .api_route(
            "/v1/sessions/:id/connections/:conn_id/postgres/role-memberships",
            get_with(
                list_postgres_role_memberships,
                doc(
                    "listPostgresRoleMemberships",
                    "Browse explicit PostgreSQL role memberships",
                ),
            ),
        )
        .api_route(
            "/v1/sessions/:id/connections/:conn_id/postgres/owners",
            get_with(
                list_postgres_owners,
                doc(
                    "listPostgresOwners",
                    "Browse PostgreSQL database and schema owners",
                ),
            ),
        )
        .api_route(
            "/v1/sessions/:id/connections/:conn_id/postgres/schema-grants",
            get_with(
                list_postgres_schema_grants,
                doc(
                    "listPostgresSchemaGrants",
                    "Browse explicit PostgreSQL schema grants",
                ),
            ),
        )
        .api_route(
            "/v1/sessions/:id/connections/:conn_id/postgres/replication",
            get_with(
                read_postgres_replication,
                doc(
                    "readPostgresReplication",
                    "Inspect bounded PostgreSQL replication state",
                ),
            ),
        )
        .api_route(
            "/v1/sessions/:id/connections/:conn_id/postgres/statistics",
            get_with(
                read_postgres_statistics,
                doc(
                    "readPostgresStatistics",
                    "Inspect bounded PostgreSQL statistics",
                ),
            ),
        )
        .api_route(
            "/v1/sessions/:id/connections/:conn_id/postgres/objects/preview",
            post_with(
                preview_postgres_object,
                doc(
                    "previewPostgresObject",
                    "Preview a guarded PostgreSQL object change",
                ),
            ),
        )
        .api_route(
            "/v1/sessions/:id/connections/:conn_id/postgres/objects/apply",
            post_with(
                apply_postgres_object,
                doc(
                    "applyPostgresObject",
                    "Apply a confirmed PostgreSQL object change",
                ),
            ),
        )
        .api_route(
            "/v1/sessions/:id/connections/:conn_id/query-store",
            get_with(
                read_query_store,
                doc(
                    "readQueryStore",
                    "Inspect the current SQL Server database's Query Store",
                ),
            ),
        )
        .api_route(
            "/v1/sessions/:id/connections/:conn_id/agent/jobs",
            get_with(
                read_agent_jobs,
                doc(
                    "readAgentJobs",
                    "Inspect SQL Server Agent jobs visible to the login",
                ),
            ),
        )
        .api_route(
            "/v1/sessions/:id/connections/:conn_id/settings/sqlserver",
            get_with(
                read_sqlserver_settings,
                doc(
                    "readSqlServerSettings",
                    "Inspect SQL Server instance settings",
                ),
            ),
        )
        .api_route(
            "/v1/sessions/:id/connections/:conn_id/sqlserver/security",
            get_with(
                read_sqlserver_security,
                doc(
                    "readSqlServerSecurity",
                    "Inspect bounded SQL Server security catalogs",
                ),
            ),
        )
        .api_route(
            "/v1/sessions/:id/connections/:conn_id/sqlserver/security/preview",
            post_with(
                preview_sqlserver_security,
                doc(
                    "previewSqlServerSecurity",
                    "Preview a typed SQL Server security change",
                ),
            ),
        )
        .api_route(
            "/v1/sessions/:id/connections/:conn_id/sqlserver/security/apply",
            post_with(
                apply_sqlserver_security,
                doc(
                    "applySqlServerSecurity",
                    "Apply a confirmed SQL Server security change",
                ),
            ),
        )
        .api_route(
            "/v1/sessions/:id/connections/:conn_id/processes/kill",
            post_with(
                kill_process,
                doc("killProcess", "Terminate a database process"),
            ),
        )
        .api_route(
            "/v1/sessions/:id/connections/:conn_id/processes/alerts",
            get_with(
                watch_process_alerts,
                doc(
                    "watchProcessAlerts",
                    "Watch bounded, deduplicated process alerts as NDJSON",
                ),
            ),
        )
        .api_route(
            "/v1/sessions/:id/connections/:conn_id/maintenance/postgres",
            post_with(
                postgres_maintenance,
                doc(
                    "postgresMaintenance",
                    "Preview or apply explicit-target PostgreSQL maintenance",
                ),
            ),
        )
        .api_route(
            "/v1/sessions/:id/connections/:conn_id/maintenance/sqlite",
            post_with(
                sqlite_maintenance,
                doc(
                    "sqliteMaintenance",
                    "Preview or apply scoped SQLite creation, backup, restore or VACUUM",
                ),
            ),
        )
        .api_route(
            "/v1/sessions/:id/connections/:conn_id/integrity",
            post_with(
                check_integrity,
                doc(
                    "checkIntegrity",
                    "Run a scoped native integrity check without repair",
                ),
            ),
        )
        .api_route(
            "/v1/sessions/:id/connections/:conn_id/recovery/sql-server",
            post_with(
                sql_server_recovery,
                doc(
                    "sqlServerRecovery",
                    "Preview or apply copy-only backup or restore to a new database name",
                ),
            ),
        )
        .api_route(
            "/v1/sessions/:id/connections/:conn_id/sqlserver/privileges",
            get_with(
                sqlserver_privileges,
                doc(
                    "readSqlServerPrivileges",
                    "Read native effective SQL Server permissions",
                ),
            ),
        )
        .api_route(
            "/v1/sessions/:id/connections/:conn_id/postgres/privileges",
            get_with(
                postgres_privileges,
                doc(
                    "readPostgresPrivileges",
                    "Read native effective PostgreSQL privileges",
                ),
            ),
        )
        .api_route(
            "/v1/sessions/:id/connections/:conn_id/maintenance/sql-server-index",
            post_with(
                sqlserver_index_maintenance,
                doc(
                    "sqlServerIndexMaintenance",
                    "Preview or apply one reviewed rowstore index maintenance action",
                ),
            ),
        )
}

#[inline(never)]
fn transactions(router: ApiRouter<AppState>) -> ApiRouter<AppState> {
    router
        .api_route(
            "/v1/sessions/:id/queries",
            post_with(execute_query, doc("executeQuery", "Execute query over synchronous HTTP")),
        )
        .api_route(
            "/v1/sessions/:id/transactions",
            get_with(list_transactions, doc("listTransactions", "List open transactions")).post_with(begin_transaction, doc("beginTransaction", "Begin transaction")),
        )
        .api_route(
            "/v1/sessions/:id/transactions/:tx_id/commit",
            post_with(commit_transaction, doc("commitTransaction", "Commit transaction")),
        )
        .api_route(
            "/v1/sessions/:id/transactions/:tx_id/rollback",
            post_with(rollback_transaction, doc("rollbackTransaction", "Rollback transaction")),
        )
        .api_route(
            "/v1/sessions/:id/transactions/:tx_id/preview",
            post_with(preview_transaction, doc("previewTransaction", "Preview commit or rollback consequences")),
        )
        .api_route(
            "/v1/sessions/:id/transactions/:tx_id/savepoints",
            post_with(create_savepoint, doc("createSavepoint", "Create transaction savepoint")),
        )
        .api_route(
            "/v1/sessions/:id/transactions/:tx_id/savepoints/rollback",
            post_with(rollback_to_savepoint, doc("rollbackToSavepoint", "Rollback to transaction savepoint")),
        )
        .api_route(
            "/v1/sessions/:id/transactions/:tx_id/savepoints/release",
            post_with(release_savepoint, doc("releaseSavepoint", "Release transaction savepoint")),
        )
        .api_route(
            "/v1/sessions/:id/ws",
            get_with(ws_session, doc("sessionWebSocket", "WebSocket query stream; protocol uses WsClientMessage/WsServerMessage")),
        )
        .api_route(
            "/v1/sessions/:id/queries/:cursor_id/cancel",
            post_with(cancel_query, doc("cancelQuery", "Cancel query")),
        )
        .api_route(
            "/v1/cursors/:cursor_id/pages",
            get_with(read_spill_pages, doc("readSpilledCursorPages", "Read pages from a spilled (evicted) cursor. Query params: `from_seq` (optional, must equal current pages_read), `limit` (default 32, max 256).")),
        )
        .api_route(
            "/v1/cursors/:cursor_id",
            delete_with(delete_spilled_cursor, doc("deleteSpilledCursor", "Delete a spilled cursor's file explicitly (idempotent). Reaper handles this on TTL too.")),
        )
}

#[inline(never)]
fn delegation(router: ApiRouter<AppState>) -> ApiRouter<AppState> {
    let router = delegation_catalog(router);
    let router = delegation_changes(router);
    delegation_policy(router)
}

#[inline(never)]
fn delegation_catalog(router: ApiRouter<AppState>) -> ApiRouter<AppState> {
    router
        .api_route(
            "/v1/scoped/ai/chats/:chat_id/delegation-reviews",
            get_with(
                ai_delegation::list,
                doc(
                    "listScopedAiDelegationReviews",
                    "List original human delegation reviews",
                ),
            )
            .post_with(
                ai_delegation::create,
                doc(
                    "createScopedAiDelegationReview",
                    "Create an immutable human run review from native material",
                ),
            ),
        )
        .api_route(
            "/v1/scoped/ai/chats/:chat_id/delegation-materials",
            post_with(
                ai_delegation::preview,
                doc(
                    "previewScopedAiDelegationMaterials",
                    "Resolve original native material and exact fingerprints for human review",
                ),
            ),
        )
        .api_route(
            "/v1/scoped/ai/delegation-reviews/:id",
            get_with(
                ai_delegation::inspect,
                doc(
                    "getScopedAiDelegationReview",
                    "Read private human run review content and original grant state",
                ),
            ),
        )
}

#[inline(never)]
fn delegation_changes(router: ApiRouter<AppState>) -> ApiRouter<AppState> {
    router
        .api_route(
            "/v1/scoped/ai/delegation-reviews/:id/approve",
            post_with(
                ai_delegation::approve,
                doc(
                    "approveScopedAiDelegationReview",
                    "Approve the exact human review without activating a run",
                ),
            ),
        )
        .api_route(
            "/v1/scoped/ai/delegation-reviews/:id/deny",
            post_with(
                ai_delegation::deny,
                doc(
                    "denyScopedAiDelegationReview",
                    "Deny an unused human run review",
                ),
            ),
        )
        .api_route(
            "/v1/scoped/ai/delegation-reviews/:id/revoke",
            post_with(
                ai_delegation::revoke,
                doc(
                    "revokeScopedAiDelegationReview",
                    "Withdraw original run authority and notify its retained owner",
                ),
            ),
        )
}

#[inline(never)]
fn delegation_policy(router: ApiRouter<AppState>) -> ApiRouter<AppState> {
    router.api_route(
        "/v1/scoped/ai/tenants/:tenant_id/delegation-policy",
        get_with(
            ai_delegation::tenant_policy,
            doc(
                "getScopedAiDelegationTenantPolicy",
                "Read current tenant delegation opt-ins",
            ),
        )
        .put_with(
            ai_delegation::set_tenant_policy,
            doc(
                "setScopedAiDelegationTenantPolicy",
                "Set admin opt-ins at the expected revision and revoke old authority",
            ),
        ),
    )
}
