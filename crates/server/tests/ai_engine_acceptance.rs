//! Real-driver acceptance of the governed AI read/stage/human-apply boundary.
#![cfg(unix)]

use anyhow::{ensure, Context, Result};
use serde_json::json;
use sift_api_types::OpenConnectionFromProfileRequest;
use sift_client_sdk::Client;
use sift_driver_sqlite::{FilePolicy, SqliteDriver};
use sift_metadata::{
    CredentialMode, MemorySecretStore, MetadataStore, NewConnectionProfile, PrincipalId, TenantId,
};
use sift_protocol::*;
use sift_server::{
    http::{app, AppState, AuthState},
    room_runtime::RoomRuntime,
    DriverRegistry, SessionStore,
};
use std::{collections::BTreeMap, sync::Arc, time::Duration};
use uuid::Uuid;

#[path = "support/ai_native_faults.rs"]
mod ai_native_faults;

async fn native_ai_catalog(
    client: &Client,
    session: SessionId,
    connection: ConnectionId,
) -> Result<CatalogGraph> {
    Ok(client
        .catalog_graph(
            session,
            connection,
            CatalogGraphRequest {
                options: CatalogGraphOptions {
                    max_nodes: Some(2000),
                    ..Default::default()
                },
                refresh: true,
            },
        )
        .await?)
}

async fn acceptance(engine: Engine) -> Result<()> {
    let root = tempfile::tempdir()?;
    let native: Arc<dyn sift_driver_api::Driver> = match engine {
        Engine::Sqlite => Arc::new(SqliteDriver::with_files(FilePolicy {
            config: SqliteDriverConfig {
                roots: BTreeMap::from([(
                    "test".into(),
                    SqliteRootConfig {
                        path: root.path().to_string_lossy().into(),
                        allowed_tenants: vec![1],
                        read_only: false,
                    },
                )]),
                max_connections: 8,
            },
            protected: vec![],
        })),
        Engine::Postgres => Arc::new(sift_driver_postgres::PgDriver::new()),
        Engine::SqlServer => Arc::new(sift_driver_sqlserver::MssqlDriver::new()),
    };
    let faults = Arc::new(ai_native_faults::NativeFaults::new(native));
    let registry = DriverRegistry::new();
    registry.providers().replace(
        [Arc::new(sift_server::registry::BuiltinProviderAdapter::new(
            faults.clone(),
        ))
            as Arc<dyn sift_server::registry::DatabaseProvider>],
    )?;
    let metadata = MetadataStore::open_in_memory(Arc::new(MemorySecretStore::new()))?;
    metadata.bootstrap_local("AI real-engine acceptance")?;
    let (configuration, credentials) = if engine == Engine::Sqlite {
        (
            {
                rusqlite::Connection::open(root.path().join("acceptance.db"))?;
                json!({"root_id":"test","path":"acceptance.db","mode":"read_write"})
            },
            None,
        )
    } else {
        let pg = engine == Engine::Postgres;
        let prefix = if pg { "SIFT_PG" } else { "SIFT_MSSQL" };
        let env = |key: &str| std::env::var(format!("{prefix}_{key}"));
        let mut configuration = json!({"host":env("HOST")?, "port":env("PORT")?.parse::<u16>()?,
            "database":env("DB")?, "user":env("USER")?, "ssl_mode":"disable"});
        if !pg {
            configuration["engine_specific"] =
                json!({"engine":"sql_server","mars":false,"trust_server_certificate":true});
        }
        (
            configuration,
            env("PASSWORD")
                .ok()
                .map(|password| json!({"password":password})),
        )
    };
    let profile = metadata
        .upsert_connection_profile(
            TenantId(1),
            PrincipalId(1),
            NewConnectionProfile {
                name: "AI acceptance".into(),
                provider_id: engine.provider_id(),
                semantic_engine: Some(engine),
                configuration,
                credentials,
                credential_mode: CredentialMode::Shared,
                tags: vec![],
            },
        )
        .await?;
    let mut auth = AuthState::default();
    auth.ai.enabled = true;
    auth.ai.execution_enabled = true;
    // Shared demo catalogs exceed the default result bound; retain the supported 1 MiB ceiling.
    auth.ai.max_tool_result_bytes = 1024 * 1024;
    auth.ai.max_tool_calls_per_run = 40;
    let router = app(AppState {
        sessions: SessionStore::new(registry),
        rooms: RoomRuntime::default(),
        shutdown: Default::default(),
        auth,
        metadata: Some(metadata.clone()),
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let client = Client::new(format!("http://{}", listener.local_addr()?));
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let session = client.open_session(None).await?.id;
    let connection = client
        .open_connection_from_profile(
            session,
            OpenConnectionFromProfileRequest {
                tenant_id: 1,
                profile_id: profile.id.0,
            },
        )
        .await?
        .id;
    let schema = match engine {
        Engine::Sqlite => "main",
        Engine::Postgres => "public",
        Engine::SqlServer => "dbo",
    };
    let name = format!("sift_ai_{}", Uuid::new_v4().simple());
    let table = format!("{schema}.{name}");
    let migration_table = format!("{schema}.{name}_new");
    let outcome = async {
        client.execute(session, connection, format!("CREATE TABLE {table} (id INTEGER NOT NULL PRIMARY KEY, label varchar(80) NOT NULL)")).await?;
        client.execute(session, connection, format!("INSERT INTO {table} VALUES (1,'original')")).await?;
        // Exercise the shared HTTP control owner against actual engine state,
        // independently of the AI proposal's internal transaction helpers.
        let control_mode = TxMode {
            isolation: if engine == Engine::Sqlite { IsolationLevel::Serializable } else { IsolationLevel::ReadCommitted },
            access: TxAccessMode::ReadWrite,
        };
        let rolled_back = client.begin_transaction(session, connection, control_mode).await?;
        client.execute_in_tx(session, &rolled_back, format!("UPDATE {table} SET label='temporary' WHERE id=1")).await?;
        client.rollback_transaction(session, connection, rolled_back.tx_id).await?;
        ensure!(client.execute(session, connection, format!("SELECT label FROM {table} WHERE id=1")).await?.rows[0].values == vec![Value::Text("original".into())]);
        let committed = client.begin_transaction(session, connection, control_mode).await?;
        client.execute_in_tx(session, &committed, format!("UPDATE {table} SET label='committed' WHERE id=1")).await?;
        client.commit_transaction(session, connection, committed.tx_id).await?;
        ensure!(client.execute(session, connection, format!("SELECT label FROM {table} WHERE id=1")).await?.rows[0].values == vec![Value::Text("committed".into())]);
        let witness = client.open_connection_from_profile(session,
            OpenConnectionFromProfileRequest { tenant_id: 1, profile_id: profile.id.0 }).await?.id;
        ensure!(client.execute(session, witness, format!("SELECT label FROM {table} WHERE id=1")).await?.rows[0].values == vec![Value::Text("committed".into())],
            "a separate connection must observe the committed value");
        client.close_connection(session, witness).await?;
        ensure!(client.list_transactions(session).await?.is_empty());
        client.execute(session, connection, format!("UPDATE {table} SET label='original' WHERE id=1")).await?;
        let chat = client.create_ai_chat(&CreateAiChatRequest { tenant_id: 1, room_id: None, title: "Engine acceptance".into() }).await?;
        let context: AiTurnContext = serde_json::from_value(json!({"target":{"tenant_id":1,"profile_id":profile.id.0,
            "connection_id":format!("{}:{}",session.0,connection.0)}, "staged_change_count":0, "attachments":[]}))?;
        let start = |mode| StartAiTurnRequest { client_request_id: Uuid::new_v4(), desktop_id: Uuid::new_v4(),
            prompt: "Review fixture only".into(), provider: AiProvider::Codex, model: None, mode, context: context.clone(), attachment_previews: vec![] };
        let lease = client.start_ai_turn(chat.id, &start(AiMode::Propose)).await?;
        let tool = |kind, sql| InvokeAiToolRequest { call_id: Uuid::new_v4(), lease_token: lease.lease_token, tool: kind, sql, parameters: None };
        let read = tool(AiToolKind::Select, Some(format!("SELECT id, label FROM {table}")));
        let rows = client.invoke_ai_tool(lease.run.id, &read).await?;
        ensure!(rows.result.to_string().contains("original"), "governed SELECT must reach the actual fixture");
        // A call ID is one immutable invocation, never a second database read.
        ensure!(client.invoke_ai_tool(lease.run.id, &read).await.is_err());
        ensure!(client.invoke_ai_tool(lease.run.id, &tool(AiToolKind::Select, Some(format!("DELETE FROM {table}")))).await.is_err());
        let mut report = client.benchmark(session, connection, BenchmarkRequest {
            run_id:Uuid::new_v4(),sql:format!("SELECT id, label FROM {table}"),params:vec![],warmups:0,iterations:2,
            query_timeout_ms:1000,total_budget_ms:5000,delay_ms:0,workload_confirmed:true,
        }).await?;
        ensure!(report.completed);
        let attached_profile: BenchmarkProfileAttachment=serde_json::from_value(serde_json::json!({"captured_at":chrono::Utc::now(),"sql":report.sql,"parameter_count":0,"response":{"run_id":Uuid::new_v4(),"plan":{"engine":report.engine,"analyzed":report.engine!=Engine::Sqlite,"raw":"private-ai-plan-sentinel","root":{"op":"Fixture"}},"server_elapsed_ns":100}}))?;
        let saved = client.save_benchmark_run(sift_api_types::TenantId(1), &SaveBenchmarkRunRequest { profile: Some(attached_profile), source_context: Some(BenchmarkSourceContext {captured_at:chrono::Utc::now(),instance_id:"private-ai-context-sentinel".into(),workspace_id:WorkspaceId(1),workspace_revision:WorkspaceRevision(1),path:WorkspacePath("queries/private-ai-path-sentinel.sql".into()),git:None}), name:"Measured fixture".into(),report:report.clone() }).await?;
        client.update_benchmark_run_annotations(sift_api_types::TenantId(1),saved.id,&UpdateBenchmarkRunAnnotationsRequest { expected_revision:0,notes:"private-ai-note-sentinel".into(),tags:vec!["private-ai-tag-sentinel".into()] }).await?;
        report.run_id=Uuid::new_v4();
        let other = client.save_benchmark_run(sift_api_types::TenantId(1), &SaveBenchmarkRunRequest { profile: None, source_context: None, name:"Comparison fixture".into(),report }).await?;
        let invoke_performance = |kind, parameters| InvokeAiToolRequest { parameters:Some(parameters), ..tool(kind,None) };
        let measurement = client.invoke_ai_tool(lease.run.id, &invoke_performance(AiToolKind::BenchmarkRun,AiToolParameters::BenchmarkRun { run_id:saved.id })).await?;
        ensure!(measurement.result["statistics"]["successful"] == 2);
        ensure!(measurement.result.get("sql").is_none());
        ensure!(!measurement.result.to_string().contains("private-ai-plan-sentinel"));
        ensure!(!measurement.result.to_string().contains("private-ai-context-sentinel"));
        let artifact=sift_core::performance::artifact::export(&saved.report,false);
        ensure!(!serde_json::to_string(&artifact)?.contains("private-ai-context-sentinel"));
        let compared = client.invoke_ai_tool(lease.run.id, &invoke_performance(AiToolKind::BenchmarkCompare,AiToolParameters::BenchmarkCompare { baseline_id:saved.id,candidate_id:other.id })).await?;
        ensure!(compared.result["comparison"]["delta_percent"] == 0.0);
        ensure!(client.invoke_ai_tool(lease.run.id, &tool(AiToolKind::BenchmarkRuns,None)).await?.result["items"].as_array().is_some_and(|items| items.len()==2));
        let annotation_projection = client.invoke_ai_tool(lease.run.id,&tool(AiToolKind::BenchmarkRuns,None)).await?.result;
        let projection_text=serde_json::to_string(&annotation_projection)?;
        ensure!(!projection_text.contains("private-ai-note-sentinel") && !projection_text.contains("private-ai-tag-sentinel") && !projection_text.contains("private-ai-context-sentinel"));
        ensure!(annotation_projection["items"][0].get("tags").is_none());
        let peer = metadata.create_principal("benchmark-peer","Peer",None)?.id;
        metadata.upsert_tenant_membership(TenantId(1),peer,sift_metadata::MembershipRole::Member)?;
        let private = metadata.save_benchmark_run(TenantId(1),peer,SavedBenchmarkRun { profile: None, source_context: None, annotations: Default::default(),
            id:Uuid::new_v4(),saved_at:chrono::Utc::now(),name:"Peer-only measurement".into(),report:saved.report.clone(),
        }).await?;
        ensure!(client.invoke_ai_tool(lease.run.id, &invoke_performance(AiToolKind::BenchmarkRun,AiToolParameters::BenchmarkRun { run_id:private.id })).await.is_err());
        ensure!(client.invoke_ai_tool(lease.run.id, &invoke_performance(AiToolKind::BenchmarkCompare,AiToolParameters::BenchmarkCompare { baseline_id:saved.id,candidate_id:private.id })).await.is_err());
        ensure!(client.invoke_ai_tool(lease.run.id, &InvokeAiToolRequest { parameters:Some(AiToolParameters::BenchmarkRun {run_id:saved.id}), ..tool(AiToolKind::BenchmarkCompare,None) }).await.is_err());
        let graph: CatalogGraph = serde_json::from_value(client.invoke_ai_tool(lease.run.id, &tool(AiToolKind::Catalog, None)).await?.result)?;
        let object = graph.data.nodes.iter().find(|node| node.kind == CatalogNodeKind::Table && node.name == name).ok_or_else(|| anyhow::anyhow!("fixture table missing from catalog"))?;
        let ddl = InvokeAiToolRequest { parameters: Some(AiToolParameters::ObjectDdl { object_id: object.id.clone(), expected_catalog_revision: graph.revision }), ..tool(AiToolKind::ObjectDdl, None) };
        ensure!(client.invoke_ai_tool(lease.run.id, &ddl).await?.result["object"]["ddl"].as_str().is_some_and(|text| text.contains(&name)));
        let cell = |column: &str, value| CellEdit { column: column.into(), value };
        let draft = AiDatabaseDraft::RowEditSet { expected_catalog_revision: graph.revision, edit_set: EditSet {
            table: ObjectPath { schema: Some(schema.into()), kind: Some(ObjectKind::Table), ..ObjectPath::new(&name) },
            edits: vec![RowEdit::Update { key: RowKey { columns: vec![cell("id", if engine == Engine::Sqlite { Value::Int64(1) } else { Value::Int32(1) })] },
                changes: vec![cell("label", Value::Text("reviewed".into()))], expected: vec![cell("label", Value::Text("original".into()))] }],
        }};
        let stage = StageAiDatabaseProposalRequest { client_request_id: Uuid::new_v4(), lease_token: lease.lease_token, draft };
        let proposal = client.stage_ai_database_proposal(lease.run.id, &stage).await?;
        ensure!(client.stage_ai_database_proposal(lease.run.id, &stage).await?.proposal.id == proposal.proposal.id);
        let reviewer = client.open_connection_from_profile(session, OpenConnectionFromProfileRequest { tenant_id:1, profile_id:profile.id.0 }).await?.id;
        let review_request = || ReviewAiDatabaseProposalRequest { client_request_id: Uuid::new_v4(), session, connection: reviewer };
        let review = client.review_ai_database_proposal(proposal.proposal.id, &review_request()).await.context("row proposal review")?;
        ensure!(client.execute(session, connection, format!("SELECT label FROM {table}")).await?.rows[0].values == vec![Value::Text("original".into())], "staging/review must not write");
        let apply = ApplyAiDatabaseProposalRequest { client_request_id:Uuid::new_v4(), review_id:review.id,
            review_digest:review.review_digest, production_confirmation:None, acknowledgements:vec![] };
        let applied = client.apply_ai_database_proposal(proposal.proposal.id, &apply).await.context("row proposal apply")?;
        ensure!(applied.state == AiDatabaseApplyState::Applied, "row apply failed: {applied:?}");
        ensure!(client.apply_ai_database_proposal(proposal.proposal.id, &apply).await?.state == AiDatabaseApplyState::Applied);
        let row_ledger = metadata.change_ledger(&ChangeLedgerFilter {
            connection_profile_id: Some(profile.id.0), ..Default::default()
        })?;
        let row_entries = row_ledger.entries.iter().filter(|entry|
            entry.source_workflow == "ai_row_proposal"
                && entry.correlation_id.as_deref() == Some(format!("ai:{}:{}", lease.run.id, proposal.proposal.id).as_str())
        ).collect::<Vec<_>>();
        ensure!(row_entries.len() == 1, "exact AI replay must retain one native row ledger entry");
        let row_entry = row_entries[0];
        ensure!(row_entry.operation == ChangeLedgerOperation::GridUpdate);
        ensure!(row_entry.row_count == Some(1) && row_entry.outcome == ChangeLedgerOutcome::Committed);
        ensure!(row_entry.transaction_id.as_deref().is_some_and(|id| serde_json::from_str::<TxId>(id).is_ok()),
            "AI row ledger must retain the actual managed transaction witness");
        ensure!(client.execute(session, connection, format!("SELECT label FROM {table}")).await?.rows[0].values == vec![Value::Text("reviewed".into())]);
        // The original expected value is now stale. Actual native conflict must
        // retain its owned rollback and exact replay without another update.
        let mut conflict_request = stage.clone();
        conflict_request.client_request_id = Uuid::new_v4();
        if let AiDatabaseDraft::RowEditSet { expected_catalog_revision, .. } = &mut conflict_request.draft {
            *expected_catalog_revision = client.catalog_graph(session, reviewer,
                CatalogGraphRequest { refresh: true, ..Default::default() }).await?.revision;
        }
        let conflict = client.stage_ai_database_proposal(lease.run.id, &conflict_request).await?;
        let conflict_review = client.review_ai_database_proposal(conflict.proposal.id, &review_request()).await?;
        let conflict_apply = ApplyAiDatabaseProposalRequest { client_request_id: Uuid::new_v4(),
            review_id: conflict_review.id, review_digest: conflict_review.review_digest,
            production_confirmation: None, acknowledgements: vec![] };
        let rolled_back = client.apply_ai_database_proposal(conflict.proposal.id, &conflict_apply).await?;
        ensure!(rolled_back.state == AiDatabaseApplyState::Failed && rolled_back.message.contains("confirmed rolled back"));
        ensure!(client.apply_ai_database_proposal(conflict.proposal.id, &conflict_apply).await?.state == AiDatabaseApplyState::Failed);
        let conflict_ledger = metadata.change_ledger(&ChangeLedgerFilter {
            connection_profile_id: Some(profile.id.0), ..Default::default()
        })?;
        let conflict_correlation = format!("ai:{}:{}", lease.run.id, conflict.proposal.id);
        let conflicts = conflict_ledger.entries.iter().filter(|entry|
            entry.source_workflow == "ai_row_proposal" && entry.correlation_id.as_deref() == Some(conflict_correlation.as_str())
        ).collect::<Vec<_>>();
        ensure!(conflicts.len() == 1 && conflicts[0].outcome == ChangeLedgerOutcome::RolledBack);
        ensure!(conflicts[0].transaction_id.is_some() && conflicts[0].transaction_id != row_entry.transaction_id);
        ensure!(conflicts[0].row_count.is_none(), "a conflict does not invent an affected count");
        ensure!(client.execute(session, connection, format!("SELECT label FROM {table}")).await?.rows[0].values == vec![Value::Text("reviewed".into())]);
        // Absence of a native count must not be replaced with returned rows or 1.
        // This UPDATE really runs; the decorator suppresses or oversizes only its count receipt.
        let mut no_count = stage.clone();
        no_count.client_request_id = Uuid::new_v4();
        if let AiDatabaseDraft::RowEditSet {expected_catalog_revision,edit_set} = &mut no_count.draft {
            *expected_catalog_revision=native_ai_catalog(&client,session,connection).await?.revision;
            if let RowEdit::Update {expected,changes,..}=&mut edit_set.edits[0] {
                expected[0].value=Value::Text("reviewed".into()); changes[0].value=Value::Text("must rollback".into());
            }
        }
        for observed_count in [None,Some(u64::MAX)] {
        no_count.client_request_id=Uuid::new_v4();
        let no_count_proposal=client.stage_ai_database_proposal(lease.run.id,&no_count).await.context("missing-count proposal staging")?;
        let no_count_review=client.review_ai_database_proposal(no_count_proposal.proposal.id,&review_request()).await?;
        let no_count_apply=ApplyAiDatabaseProposalRequest {client_request_id:Uuid::new_v4(),review_id:no_count_review.id,review_digest:no_count_review.review_digest,production_confirmation:None,acknowledgements:vec![]};
        let before=faults.counters();
        faults.replace_next_update_count(&name,observed_count);
        let no_count_receipt=client.apply_ai_database_proposal(no_count_proposal.proposal.id,&no_count_apply).await?;
        ensure!(faults.fault_consumed() && faults.native_update_observed(),"count fault must hide a real native one-row UPDATE acknowledgement");
        ensure!(no_count_receipt.state==AiDatabaseApplyState::Failed && no_count_receipt.message.contains("confirmed rolled back"));
        ensure!(faults.counters().2==before.2+1 && faults.counters().1==before.1);
        ensure!(faults.rollback_error().is_none(),"missing/oversized count rollback must receive native acknowledgement");
        let count_ledger=metadata.change_ledger(&ChangeLedgerFilter {connection_profile_id:Some(profile.id.0),..Default::default()})?;
        let correlation=format!("ai:{}:{}",lease.run.id,no_count_proposal.proposal.id);
        let entries=count_ledger.entries.iter().filter(|entry|entry.correlation_id.as_deref()==Some(correlation.as_str())).collect::<Vec<_>>();
        ensure!(entries.len()==1 && entries[0].outcome==ChangeLedgerOutcome::RolledBack && entries[0].row_count.is_none());
        let before_replay=faults.counters();
        ensure!(client.apply_ai_database_proposal(no_count_proposal.proposal.id,&no_count_apply).await?.state==AiDatabaseApplyState::Failed);
        ensure!(faults.counters()==before_replay,"rollback replay must not redispatch native work");
        ensure!(client.execute(session,connection,format!("SELECT label FROM {table} WHERE id=1")).await?.rows[0].values==vec![Value::Text("reviewed".into())]);

        }

        // A real COMMIT may succeed while its acknowledgement never reaches the host.
        // Use a separate review connection for each case; uncertain sources stay fenced.
        for migration in [false,true] {
            let fault_connection=client.open_connection_from_profile(session,OpenConnectionFromProfileRequest {tenant_id:1,profile_id:profile.id.0}).await?.id;
            let fault_draft=if migration {
                client.execute(session,connection,format!("CREATE TABLE {migration_table} (id INTEGER NOT NULL)")).await?;
                let desired:CatalogGraph=native_ai_catalog(&client,session,connection).await?;
                client.execute(session,connection,format!("DROP TABLE {migration_table}")).await?;
                let fresh=native_ai_catalog(&client,session,connection).await?;
                AiDatabaseDraft::MigrationDraft {expected_catalog_revision:fresh.revision,desired_catalog:desired,options:MigrationOptions {prefer_transactional:true,..Default::default()}}
            } else {
                let mut draft=no_count.draft.clone();
                if let AiDatabaseDraft::RowEditSet {expected_catalog_revision,edit_set}=&mut draft {
                    *expected_catalog_revision=native_ai_catalog(&client,session,connection).await?.revision;
                    if let RowEdit::Update {changes,..}=&mut edit_set.edits[0] { changes[0].value=Value::Text("ack missing".into()); }
                }
                draft
            };
            let proposal=client.stage_ai_database_proposal(lease.run.id,&StageAiDatabaseProposalRequest {client_request_id:Uuid::new_v4(),lease_token:lease.lease_token,draft:fault_draft}).await.context("lost-ack proposal staging")?;
            let review=client.review_ai_database_proposal(proposal.proposal.id,&ReviewAiDatabaseProposalRequest {client_request_id:Uuid::new_v4(),session,connection:fault_connection}).await?;
            let acknowledgements=match &review.preview {AiDatabasePreview::MigrationDraft {plan}=>plan.required_acknowledgements.clone(),_=>vec![]};
            let apply=ApplyAiDatabaseProposalRequest {client_request_id:Uuid::new_v4(),review_id:review.id,review_digest:review.review_digest.clone(),production_confirmation:None,acknowledgements};
            let before=faults.counters();
            faults.lose_next_commit_ack();
            let receipt=client.apply_ai_database_proposal(proposal.proposal.id,&apply).await?;
            ensure!(faults.fault_consumed());
            ensure!(receipt.state==AiDatabaseApplyState::OutcomeUnknown,"lost native commit ack must stay unknown: {receipt:?}");
            ensure!(faults.counters().1==before.1+1 && faults.counters().2==before.2,"never issue a compensating rollback for an uncertain commit");
            ensure!(client.execute(session,fault_connection,"SELECT 1").await.is_err(),"uncertain source must remain fenced");
            let native_ledger=metadata.change_ledger(&ChangeLedgerFilter {connection_profile_id:Some(profile.id.0),..Default::default()})?;
            let correlation=format!("ai:{}:{}",lease.run.id,proposal.proposal.id);
            let entries=native_ledger.entries.iter().filter(|entry|entry.correlation_id.as_deref()==Some(correlation.as_str())).collect::<Vec<_>>();
            ensure!(entries.len()==1 && entries[0].outcome==ChangeLedgerOutcome::OutcomeUnknown && entries[0].transaction_id.is_some());
            if let AiDatabasePreview::MigrationDraft {plan}=&review.preview {
                let journal=client.migration_native_outcomes(sift_api_types::TenantId(1),plan.run_id,&MigrationNativePageRequest::default()).await?;
                ensure!(journal.closed && journal.observations.iter().any(|item|item.group.commit_intent && item.group.outcome==Some(ChangeLedgerOutcome::OutcomeUnknown)));
                ensure!(journal.observations.iter().all(|item|item.group.outcome!=Some(ChangeLedgerOutcome::RolledBack)),"cleanup intent cannot prove a rollback after a lost COMMIT acknowledgement");
            }
            let before_replay=faults.counters();
            ensure!(client.apply_ai_database_proposal(proposal.proposal.id,&apply).await?.state==AiDatabaseApplyState::OutcomeUnknown);
            let status=client.ai_database_apply_status(proposal.proposal.id).await?;
            ensure!(status.reconciled_at.is_none());
            let proof=ReconcileAiDatabaseApply {review_id:apply.review_id,client_request_id:apply.client_request_id,review_digest:apply.review_digest.clone()};
            ensure!(client.reconcile_ai_database_apply(proposal.proposal.id,&proof).await?.state==AiDatabaseApplyState::OutcomeUnknown);
            ensure!(client.apply_ai_database_proposal(proposal.proposal.id,&apply).await?.state==AiDatabaseApplyState::OutcomeUnknown);
            ensure!(faults.counters()==before_replay,"acknowledgement/replay cannot redispatch or end a native transaction");
            ensure!(!serde_json::to_string(&receipt)?.contains("PRIVATE-NATIVE-COMMIT-ACK-LOST") && !serde_json::to_string(&native_ledger)?.contains("PRIVATE-NATIVE-COMMIT-ACK-LOST"));
            // This independent source proves actual DB state, separately from host evidence.
            if migration {
                ensure!(client.execute(session,connection,format!("SELECT * FROM {migration_table}")).await?.rows.is_empty());
                client.execute(session,connection,format!("DROP TABLE {migration_table}")).await?;
            } else {
                ensure!(client.execute(session,connection,format!("SELECT label FROM {table} WHERE id=1")).await?.rows[0].values==vec![Value::Text("ack missing".into())]);
                client.execute(session,connection,format!("UPDATE {table} SET label='reviewed' WHERE id=1")).await?;
            }
            let after_ack=client.change_ledger(&ChangeLedgerFilter::default()).await?;
            let retained=after_ack.entries.iter().filter(|entry|entry.correlation_id.as_deref()==Some(correlation.as_str())).collect::<Vec<_>>();
            ensure!(retained.len()==1 && retained[0].outcome==ChangeLedgerOutcome::OutcomeUnknown);
            client.close_connection(session,fault_connection).await?;
        }
        let mut stale_request = stage.clone();
        stale_request.client_request_id = Uuid::new_v4();
        if let AiDatabaseDraft::RowEditSet { edit_set, expected_catalog_revision } = &mut stale_request.draft {
            *expected_catalog_revision=native_ai_catalog(&client,session,connection).await?.revision;
            if let RowEdit::Update { expected, .. } = &mut edit_set.edits[0] {
                expected[0].value = Value::Text("reviewed".into());
            }
        }
        let stale = client.stage_ai_database_proposal(lease.run.id, &stale_request).await?;
        client.execute(session, reviewer, format!("ALTER TABLE {table} ADD changed int")).await?;
        ensure!(client.review_ai_database_proposal(stale.proposal.id, &review_request()).await.is_err(), "independent-connection DDL must invalidate the draft");
        let stale_ddl = InvokeAiToolRequest { call_id:Uuid::new_v4(), ..ddl };
        ensure!(client.invoke_ai_tool(lease.run.id, &stale_ddl).await.is_err(), "old object revision must not read new DDL");
        for prefer_transactional in [true, false] {
        // Capture a lossless native simple-table shape, then propose recreating only that table.
        // Existing native tables/keys and SQLite ALTER/DROP are deliberately conservative.
        client.execute(session, reviewer, format!("CREATE TABLE {migration_table} (id INTEGER NOT NULL)")).await?;
        client.catalog_graph(session, reviewer, CatalogGraphRequest { refresh:true, ..Default::default() }).await?;
        let desired: CatalogGraph = serde_json::from_value(client.invoke_ai_tool(lease.run.id, &tool(AiToolKind::Catalog, None)).await?.result)?;
        client.execute(session, reviewer, format!("DROP TABLE {migration_table}")).await?;
        client.catalog_graph(session, reviewer, CatalogGraphRequest { refresh:true, ..Default::default() }).await?;
        let fresh: CatalogGraph = serde_json::from_value(client.invoke_ai_tool(lease.run.id, &tool(AiToolKind::Catalog, None)).await?.result)?;
        let migration = client.stage_ai_database_proposal(lease.run.id, &StageAiDatabaseProposalRequest {
            client_request_id:Uuid::new_v4(), lease_token:lease.lease_token,
            draft:AiDatabaseDraft::MigrationDraft { expected_catalog_revision:fresh.revision, desired_catalog:desired, options:MigrationOptions { prefer_transactional, ..Default::default() } },
        }).await?;
        let review = client.review_ai_database_proposal(migration.proposal.id, &review_request()).await.context("migration proposal review")?;
        let AiDatabasePreview::MigrationDraft { plan } = &review.preview else { anyhow::bail!("expected typed migration preview") };
        ensure!(!plan.groups.is_empty());
        let apply = ApplyAiDatabaseProposalRequest { client_request_id:Uuid::new_v4(), review_id:review.id,
            review_digest:review.review_digest.clone(), production_confirmation:None, acknowledgements:plan.required_acknowledgements.clone() };
        let migration_applied = client.apply_ai_database_proposal(migration.proposal.id, &apply).await.context("migration proposal apply")?;
        ensure!(migration_applied.state == AiDatabaseApplyState::Applied);
        ensure!(client.apply_ai_database_proposal(migration.proposal.id, &apply).await.context("migration proposal apply")?.state == AiDatabaseApplyState::Applied);
        let migration_run = migration_applied.migration_result.as_ref().ok_or_else(||anyhow::anyhow!("migration receipt missing its workflow"))?;
        let native = client.migration_native_outcomes(sift_api_types::TenantId(1),migration_run.id,&MigrationNativePageRequest::default()).await?;
        ensure!(native.run_id == migration_run.id && native.plan_id == plan.id && native.plan_digest == plan.digest);
        ensure!(native.journal_present && native.closed && native.source.is_some());
        ensure!(!native.observations.is_empty());
        for witness in &native.observations {
            let statement = witness.statement.as_ref().ok_or_else(||anyhow::anyhow!("missing native statement"))?;
            ensure!(statement.native_admitted == Some(true) && statement.native_completed == Some(true));
            if witness.group.transactional {
                ensure!(witness.group.transaction_id.is_some() && witness.group.outcome == Some(ChangeLedgerOutcome::Committed));
                ensure!(!statement.autocommit_known);
            } else {
                ensure!(witness.group.transaction_id.is_none() && witness.group.outcome.is_none());
                ensure!(statement.autocommit_known);
            }
        }
        let ledger = client.change_ledger(&ChangeLedgerFilter::default()).await?;
        let correlation = format!("ai:{}:{}", lease.run.id, migration.proposal.id);
        let entries = ledger.entries.iter().filter(|entry| entry.source_workflow == "ai_migration_proposal"
            && entry.correlation_id.as_deref() == Some(correlation.as_str())).collect::<Vec<_>>();
        ensure!(!entries.is_empty());
        for entry in entries {
            ensure!(entry.outcome == ChangeLedgerOutcome::Committed);
            ensure!(entry.transaction_id != Some(migration_run.id.to_string()));
            ensure!(native.observations.iter().any(|witness|
                entry.transaction_id == witness.group.transaction_id.map(|tx|tx.0.to_string())));
        }
        // This second native connection can see the DDL only after native commit/autocommit.
        ensure!(client.execute(session, reviewer, format!("SELECT * FROM {migration_table}")).await?.rows.is_empty());
        ensure!(client.execute(session, connection, format!("SELECT * FROM {migration_table}")).await?.rows.is_empty());
        client.execute(session, reviewer, format!("DROP TABLE {migration_table}")).await?;
        }
        let policy = |revision, blocked_ops| UpdateConnectionPolicyRequest { expected_revision:Some(revision), minimum_tenant_role:TenantRole::Member,
            read_only:false, allowed_ops:None, blocked_ops, allowed_schemas:None };
        client.update_connection_policy(sift_api_types::ConnectionProfileId(profile.id.0), policy(0, vec![OperationKind::ExecuteQuery])).await?;
        ensure!(client.invoke_ai_tool(lease.run.id, &tool(AiToolKind::Select, Some("SELECT 1".into()))).await.is_err());
        client.update_connection_policy(sift_api_types::ConnectionProfileId(profile.id.0), policy(1, vec![])).await?;
        client.finish_ai_run(lease.run.id, &FinishAiRunRequest { lease_token:lease.lease_token, status:AiRunStatus::Completed }).await?;
        // Execute is separate from read/staging authority, and staging cannot dispatch SQL.
        let read_only = client.start_ai_turn(chat.id, &start(AiMode::Read)).await?;
        let sql_request = |token| RequestAiSqlExecution {call_id:Uuid::new_v4(),lease_token:token,
            sql:format!("UPDATE {table} SET id=id+1, label={}",match engine {Engine::Postgres=>"$1",Engine::SqlServer=>"@P1",Engine::Sqlite=>"?1"}),params:vec![Value::Text("executed".into())]};
        ensure!(client.request_ai_sql_execution(read_only.run.id,&sql_request(read_only.lease_token)).await.is_err());
        client.finish_ai_run(read_only.run.id,&FinishAiRunRequest {lease_token:read_only.lease_token,status:AiRunStatus::Completed}).await?;
        let execute = client.start_ai_turn(chat.id, &start(AiMode::Execute)).await?;
        ensure!(execute.run.mode==AiMode::Execute);
        ensure!(client.ai_run(chat.id,execute.run.id).await?.run.mode==AiMode::Execute);
        let request=sql_request(execute.lease_token);
        let action=client.request_ai_sql_execution(execute.run.id,&request).await?;
        ensure!(action.state==AiExecutionState::AwaitingApproval);
        ensure!(client.execute(session,connection,format!("SELECT id FROM {table} WHERE id=1")).await?.rows.len()==1,"requesting SQL must not dispatch it");
        ensure!(client.request_ai_sql_execution(execute.run.id,&request).await?.id==action.id);
        ensure!(client.request_ai_sql_execution(execute.run.id,&RequestAiSqlExecution {sql:format!("DELETE FROM {table}"),..request.clone()}).await.is_err(),"changed call ID input must refuse");
        ensure!(client.request_ai_sql_execution(execute.run.id,&RequestAiSqlExecution {call_id:Uuid::new_v4(),sql:"BEGIN".into(),..request.clone()}).await.is_err(),"transaction controls require their own execution family");
        let review=client.ai_sql_execution(action.id).await?;
        ensure!(review.input.sql==request.sql && review.input.source.profile_id==profile.id.0);
        let approval=ApproveAiSqlExecution {fingerprint:action.fingerprint.clone(),production_confirmation:None};
        ensure!(client.approve_ai_sql_execution(action.id,&ApproveAiSqlExecution {fingerprint:"changed".into(),..approval.clone()}).await.is_err());
        let (first,second)=tokio::join!(client.approve_ai_sql_execution(action.id,&approval),client.approve_ai_sql_execution(action.id,&approval));
        ensure!(first?.id==action.id && second?.id==action.id);
        let settled=tokio::time::timeout(Duration::from_secs(10),async {
            loop {
                let state=client.ai_sql_execution_status(action.id).await?;
                if state.state!=AiExecutionState::Applying {return Ok::<_,anyhow::Error>(state);}
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }).await??;
        ensure!(settled.state==AiExecutionState::Completed,"SQL failed: {settled:?}");
        ensure!(settled.receipt.as_ref().is_some_and(|r|r.native_outcome==Some(ChangeLedgerOutcome::Committed) && r.affected_rows==Some(1)));
        ensure!(client.execute(session,connection,format!("SELECT id FROM {table} WHERE id=2 AND label='executed'")).await?.rows.len()==1,"concurrent approval must dispatch once");
        ensure!(client.request_ai_sql_execution(execute.run.id,&request).await?.state==AiExecutionState::Completed);
        ensure!(client.approve_ai_sql_execution(action.id,&approval).await?.state==AiExecutionState::Completed);
        let ledger=metadata.change_ledger(&ChangeLedgerFilter {connection_profile_id:Some(profile.id.0),..Default::default()})?;
        let executed=ledger.entries.iter().filter(|e|e.source_workflow=="ai_execute").collect::<Vec<_>>();
        ensure!(executed.len()==1 && executed[0].outcome==ChangeLedgerOutcome::Committed && executed[0].row_count==Some(1));
        ensure!(executed[0].authored_by==Some(1) && executed[0].approved_by==Some(1) && executed[0].executed_by==1);
        let denied=client.request_ai_sql_execution(execute.run.id,&sql_request(execute.lease_token)).await?;
        let deny=ApproveAiSqlExecution {fingerprint:denied.fingerprint,production_confirmation:None};
        ensure!(client.deny_ai_sql_execution(denied.id,&deny).await?.state==AiExecutionState::Denied);
        ensure!(client.approve_ai_sql_execution(denied.id,&deny).await?.state==AiExecutionState::Denied);
        ensure!(client.execute(session,connection,format!("SELECT id FROM {table} WHERE id=2")).await?.rows.len()==1);
        let read_request=RequestAiSqlExecution {call_id:Uuid::new_v4(),lease_token:execute.lease_token,sql:format!("SELECT label FROM {table} WHERE id=2"),params:vec![]};
        let read_action=client.request_ai_sql_execution(execute.run.id,&read_request).await?;
        let read_approval=ApproveAiSqlExecution {fingerprint:read_action.fingerprint.clone(),production_confirmation:None};
        client.approve_ai_sql_execution(read_action.id,&read_approval).await?;
        let read_receipt=tokio::time::timeout(Duration::from_secs(10),async {
            loop {
                let receipt=client.ai_sql_execution_status(read_action.id).await?;
                if receipt.state!=AiExecutionState::Applying {return Ok::<_,anyhow::Error>(receipt);}
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }).await??;
        ensure!(read_receipt.state==AiExecutionState::Completed);
        ensure!(read_receipt.receipt.as_ref().and_then(|r|r.result.as_ref()).is_some_and(|r|r.rows[0].values==vec![Value::Text("executed".into())]));
        let stale=client.request_ai_sql_execution(execute.run.id,&sql_request(execute.lease_token)).await?;
        client.update_connection_policy(sift_api_types::ConnectionProfileId(profile.id.0),policy(2,vec![OperationKind::ExecuteQuery])).await?;
        let hidden=|action:&AiSqlExecutionSummary|action.receipt.as_ref().is_some_and(|r|r.result.is_none());
        ensure!(hidden(&client.ai_sql_execution_status(read_action.id).await?));
        ensure!(hidden(&client.ai_sql_execution(read_action.id).await?.summary));
        ensure!(hidden(&client.request_ai_sql_execution(execute.run.id,&read_request).await?));
        ensure!(hidden(&client.approve_ai_sql_execution(read_action.id,&read_approval).await?),"approval replay must not disclose revoked-source rows");
        let prior_write=client.approve_ai_sql_execution(action.id,&approval).await?;
        ensure!(hidden(&prior_write) && prior_write.receipt.as_ref().is_some_and(|r|r.native_outcome==Some(ChangeLedgerOutcome::Committed)),"delivery refusal must preserve known native writes");
        ensure!(client.approve_ai_sql_execution(stale.id,&ApproveAiSqlExecution {fingerprint:stale.fingerprint.clone(),production_confirmation:None}).await.is_err(),"policy changes must refuse pending dispatch");
        client.update_connection_policy(sift_api_types::ConnectionProfileId(profile.id.0),policy(3,vec![])).await?;
        ensure!(client.approve_ai_sql_execution(stale.id,&ApproveAiSqlExecution {fingerprint:stale.fingerprint,production_confirmation:None}).await.is_err(),"restoring permissions must not silently refresh the source proof");
        client.finish_ai_run(execute.run.id,&FinishAiRunRequest {lease_token:execute.lease_token,status:AiRunStatus::Completed}).await?;
        let canceled = client.start_ai_turn(chat.id, &start(AiMode::Read)).await?;
        let slow = match engine {
            Engine::Postgres => "SELECT pg_sleep(10)",
            Engine::SqlServer => "SELECT COUNT_BIG(*) FROM sys.all_objects a CROSS JOIN sys.all_objects b CROSS JOIN sys.all_objects c",
            Engine::Sqlite => "WITH RECURSIVE n(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM n WHERE x<100000000) SELECT sum(x) FROM n",
        };
        let call_id = Uuid::new_v4();
        let slow_request = InvokeAiToolRequest { call_id, lease_token:canceled.lease_token, tool:AiToolKind::Select, sql:Some(slow.into()), parameters:None };
        let pending = client.invoke_ai_tool(canceled.run.id, &slow_request);
        tokio::pin!(pending);
        tokio::select! {
            result = &mut pending => anyhow::bail!("slow read ended before Stop: {}", if result.is_ok() { "succeeded" } else { "denied" }),
            result = tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    if client.ai_events(canceled.run.id, 0).await?.iter().any(|event| event.tool_call_id == Some(call_id) && event.kind == AiEventKind::ToolRequested) { return Ok::<_, anyhow::Error>(()); }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            }) => { result??; }
        }
        client.finish_ai_run(canceled.run.id, &FinishAiRunRequest { lease_token:canceled.lease_token, status:AiRunStatus::Canceled }).await?;
        ensure!(tokio::time::timeout(Duration::from_secs(5), pending).await?.is_err(), "Stop must cancel the driver read");
        ensure!(client.finish_ai_run(canceled.run.id, &FinishAiRunRequest { lease_token:canceled.lease_token, status:AiRunStatus::Completed }).await.is_err());
        ensure!(client.ai_run(chat.id, canceled.run.id).await?.run.status == AiRunStatus::Canceled);
        ensure!(client.ai_events(canceled.run.id, 0).await?.iter().any(|event| event.tool_call_id == Some(call_id) && event.kind == AiEventKind::ToolDenied));
        Ok::<_, anyhow::Error>(())
    }.await;
    // Stop can invalidate its native source; cleanup uses a fresh managed connection.
    let cleanup_connection = client
        .open_connection_from_profile(
            session,
            OpenConnectionFromProfileRequest {
                tenant_id: 1,
                profile_id: profile.id.0,
            },
        )
        .await
        .ok()
        .map(|c| c.id)
        .unwrap_or(connection);
    // Only our UUID-named table is disposable; clean up even when acceptance fails.
    let _ = client
        .execute(
            session,
            cleanup_connection,
            format!("DROP TABLE IF EXISTS {table}"),
        )
        .await;
    let _ = client
        .execute(
            session,
            cleanup_connection,
            format!("DROP TABLE IF EXISTS {migration_table}"),
        )
        .await;
    let _ = client.close_session(session).await;
    server.abort();
    outcome
}

#[tokio::test]
async fn sqlite_ai_read_propose_apply_and_stop() {
    acceptance(Engine::Sqlite).await.unwrap();
}

#[cfg(feature = "live-pg")]
#[tokio::test]
async fn postgres_ai_read_propose_apply_and_stop() {
    acceptance(Engine::Postgres).await.unwrap();
}

#[cfg(feature = "live-mssql")]
#[tokio::test]
async fn sqlserver_ai_read_propose_apply_and_stop() {
    acceptance(Engine::SqlServer).await.unwrap();
}
