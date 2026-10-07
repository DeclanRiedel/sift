//! Real-driver acceptance of the governed AI read/stage/human-apply boundary.
#![cfg(unix)]

use anyhow::{ensure, Result};
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

async fn acceptance(engine: Engine) -> Result<()> {
    let root = tempfile::tempdir()?;
    let registry = match engine {
        Engine::Sqlite => DriverRegistry::builder()
            .register(SqliteDriver::with_files(FilePolicy {
                config: SqliteDriverConfig {
                    roots: BTreeMap::from([(
                        "test".into(),
                        SqliteRootConfig {
                            path: root.path().to_string_lossy().into(),
                            allowed_tenants: vec![1],
                            read_only: false,
                        },
                    )]),
                    max_connections: 4,
                },
                protected: vec![],
            }))
            .build(),
        Engine::Postgres => DriverRegistry::builder()
            .register(sift_driver_postgres::PgDriver::new())
            .build(),
        Engine::SqlServer => DriverRegistry::builder()
            .register(sift_driver_sqlserver::MssqlDriver::new())
            .build(),
    };
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
        let saved = client.save_benchmark_run(sift_api_types::TenantId(1), &SaveBenchmarkRunRequest { name:"Measured fixture".into(),report:report.clone() }).await?;
        report.run_id=Uuid::new_v4();
        let other = client.save_benchmark_run(sift_api_types::TenantId(1), &SaveBenchmarkRunRequest { name:"Comparison fixture".into(),report }).await?;
        let invoke_performance = |kind, parameters| InvokeAiToolRequest { parameters:Some(parameters), ..tool(kind,None) };
        let measurement = client.invoke_ai_tool(lease.run.id, &invoke_performance(AiToolKind::BenchmarkRun,AiToolParameters::BenchmarkRun { run_id:saved.id })).await?;
        ensure!(measurement.result["statistics"]["successful"] == 2);
        ensure!(measurement.result.get("sql").is_none());
        let compared = client.invoke_ai_tool(lease.run.id, &invoke_performance(AiToolKind::BenchmarkCompare,AiToolParameters::BenchmarkCompare { baseline_id:saved.id,candidate_id:other.id })).await?;
        ensure!(compared.result["comparison"]["delta_percent"] == 0.0);
        ensure!(client.invoke_ai_tool(lease.run.id, &tool(AiToolKind::BenchmarkRuns,None)).await?.result["items"].as_array().is_some_and(|items| items.len()==2));
        let peer = metadata.create_principal("benchmark-peer","Peer",None)?.id;
        metadata.upsert_tenant_membership(TenantId(1),peer,sift_metadata::MembershipRole::Member)?;
        let private = metadata.save_benchmark_run(TenantId(1),peer,SavedBenchmarkRun {
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
        let review = client.review_ai_database_proposal(proposal.proposal.id, &review_request()).await?;
        ensure!(client.execute(session, connection, format!("SELECT label FROM {table}")).await?.rows[0].values == vec![Value::Text("original".into())], "staging/review must not write");
        let apply = ApplyAiDatabaseProposalRequest { client_request_id:Uuid::new_v4(), review_id:review.id,
            review_digest:review.review_digest, production_confirmation:None, acknowledgements:vec![] };
        let applied = client.apply_ai_database_proposal(proposal.proposal.id, &apply).await?;
        ensure!(applied.state == AiDatabaseApplyState::Applied, "row apply failed: {applied:?}");
        ensure!(client.apply_ai_database_proposal(proposal.proposal.id, &apply).await?.state == AiDatabaseApplyState::Applied);
        ensure!(client.execute(session, connection, format!("SELECT label FROM {table}")).await?.rows[0].values == vec![Value::Text("reviewed".into())]);
        let mut stale_request = stage.clone();
        stale_request.client_request_id = Uuid::new_v4();
        if let AiDatabaseDraft::RowEditSet { edit_set, .. } = &mut stale_request.draft {
            if let RowEdit::Update { expected, .. } = &mut edit_set.edits[0] {
                expected[0].value = Value::Text("reviewed".into());
            }
        }
        let stale = client.stage_ai_database_proposal(lease.run.id, &stale_request).await?;
        client.execute(session, reviewer, format!("ALTER TABLE {table} ADD changed int")).await?;
        ensure!(client.review_ai_database_proposal(stale.proposal.id, &review_request()).await.is_err(), "independent-connection DDL must invalidate the draft");
        let stale_ddl = InvokeAiToolRequest { call_id:Uuid::new_v4(), ..ddl };
        ensure!(client.invoke_ai_tool(lease.run.id, &stale_ddl).await.is_err(), "old object revision must not read new DDL");
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
            draft:AiDatabaseDraft::MigrationDraft { expected_catalog_revision:fresh.revision, desired_catalog:desired, options:MigrationOptions::default() },
        }).await?;
        let review = client.review_ai_database_proposal(migration.proposal.id, &review_request()).await?;
        let AiDatabasePreview::MigrationDraft { plan } = &review.preview else { anyhow::bail!("expected typed migration preview") };
        ensure!(!plan.groups.is_empty());
        let apply = ApplyAiDatabaseProposalRequest { client_request_id:Uuid::new_v4(), review_id:review.id,
            review_digest:review.review_digest.clone(), production_confirmation:None, acknowledgements:plan.required_acknowledgements.clone() };
        ensure!(client.apply_ai_database_proposal(migration.proposal.id, &apply).await?.state == AiDatabaseApplyState::Applied);
        ensure!(client.apply_ai_database_proposal(migration.proposal.id, &apply).await?.state == AiDatabaseApplyState::Applied);
        ensure!(client.execute(session, connection, format!("SELECT * FROM {migration_table}")).await?.rows.is_empty());
        let policy = |revision, blocked_ops| UpdateConnectionPolicyRequest { expected_revision:Some(revision), minimum_tenant_role:TenantRole::Member,
            read_only:false, allowed_ops:None, blocked_ops, allowed_schemas:None };
        client.update_connection_policy(sift_api_types::ConnectionProfileId(profile.id.0), policy(0, vec![OperationKind::ExecuteQuery])).await?;
        ensure!(client.invoke_ai_tool(lease.run.id, &tool(AiToolKind::Select, Some("SELECT 1".into()))).await.is_err());
        client.update_connection_policy(sift_api_types::ConnectionProfileId(profile.id.0), policy(1, vec![])).await?;
        client.finish_ai_run(lease.run.id, &FinishAiRunRequest { lease_token:lease.lease_token, status:AiRunStatus::Completed }).await?;
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
    // Only our UUID-named table is disposable; clean up even when acceptance fails.
    let _ = client
        .execute(session, connection, format!("DROP TABLE IF EXISTS {table}"))
        .await;
    let _ = client
        .execute(
            session,
            connection,
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
