use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use sift_driver_api::mock::MockDriver;
use sift_protocol::{
    AgentJobOutcome, AgentJobsReport, AgentJobsState, ColumnMetadata, Engine, Page, PrimitiveType,
    Row, ServerInfo, TypeRef, Value,
};
use sift_server::http::{app, AppState, AuthState};
use sift_server::{DriverRegistry, RoomRuntime, SessionStore, Shutdown};
use tower::ServiceExt;

async fn json<T: serde::de::DeserializeOwned>(body: Body) -> T {
    serde_json::from_slice(&to_bytes(body, 1024 * 1024).await.unwrap()).unwrap()
}

#[tokio::test]
async fn agent_jobs_route_caps_rows_and_omits_job_commands() {
    let rows = (0..101)
        .map(|index| {
            Row::new(vec![
                Value::Text(format!("00000000-0000-0000-0000-{index:012}")),
                Value::Text(format!("job-{index:03}")),
                Value::Bool(true),
                Value::Text("operator".into()),
                Value::Int32(1),
                Value::Int32(20260928),
                Value::Int32(93000),
                Value::Int32(5),
            ])
        })
        .collect();
    let driver = MockDriver::builder()
        .engine(Engine::SqlServer)
        .ping_ok(ServerInfo {
            provider: Engine::SqlServer.provider_ref("test"),
            server_version: "mock".into(),
            current_database: "app".into(),
            current_user: "operator".into(),
            pool_warm_slots: None,
        })
        .execute_ok(vec![
            Page::NextResult {
                columns: (0..8)
                    .map(|index| {
                        ColumnMetadata::new(
                            format!("c{index}"),
                            TypeRef::Primitive(PrimitiveType::Text),
                        )
                    })
                    .collect(),
            },
            Page::Rows { rows },
            Page::Done {
                affected_rows: None,
                warnings: vec![],
            },
        ])
        .build();
    let router = app(AppState {
        sessions: SessionStore::new(DriverRegistry::builder().register(driver).build()),
        rooms: RoomRuntime::default(),
        auth: AuthState::default(),
        metadata: None,
        shutdown: Shutdown::default(),
    });
    let session: sift_protocol::SessionInfo = json(
        router
            .clone()
            .oneshot(
                Request::post("/v1/sessions")
                    .header("content-type", "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap()
            .into_body(),
    )
    .await;
    let connection: sift_protocol::ConnectionInfo = json(
        router
            .clone()
            .oneshot(
                Request::post(format!("/v1/sessions/{}/connections", session.id))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"provider_id":"sift/sql-server","host":"mock","user":"operator"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap()
            .into_body(),
    )
    .await;
    let response = router
        .oneshot(
            Request::get(format!(
                "/v1/sessions/{}/connections/{}/agent/jobs",
                session.id, connection.id
            ))
            .body(Body::empty())
            .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let payload = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    let report: AgentJobsReport = serde_json::from_slice(&payload).unwrap();
    assert_eq!(report.state, AgentJobsState::Available);
    assert_eq!(report.jobs.len(), 100);
    assert!(report.truncated);
    assert_eq!(
        report.jobs[0].last_outcome,
        Some(AgentJobOutcome::Succeeded)
    );
    assert!(!String::from_utf8_lossy(&payload).contains("command"));
}

#[cfg(feature = "live-mssql")]
mod native {
    use super::*;
    use sift_driver_api::{ConnHandle, Driver};
    use sift_driver_sqlserver::MssqlDriver;
    use sift_protocol::{
        ConnectionSpec, EngineConnectionSpec, ExecuteRequest, ExecuteRequestHttp,
        MssqlConnectionSpec, OpenSessionRequest, SqlServerSettingsReport, SqlServerSettingsState,
        SslMode,
    };

    fn spec() -> ConnectionSpec {
        ConnectionSpec {
            host: std::env::var("SIFT_MSSQL_HOST").unwrap_or_else(|_| "127.0.0.1".into()),
            port: Some(
                std::env::var("SIFT_MSSQL_PORT")
                    .ok()
                    .and_then(|value| value.parse().ok())
                    .unwrap_or(1433),
            ),
            database: Some(std::env::var("SIFT_MSSQL_DB").unwrap_or_else(|_| "master".into())),
            user: std::env::var("SIFT_MSSQL_USER").unwrap_or_else(|_| "sa".into()),
            password: Some(
                std::env::var("SIFT_MSSQL_PASSWORD").expect("SIFT_MSSQL_PASSWORD required"),
            ),
            ssl_mode: Some(SslMode::Require),
            engine_specific: Some(EngineConnectionSpec::SqlServer(MssqlConnectionSpec {
                trust_server_certificate: Some(true),
                ..Default::default()
            })),
        }
    }
    fn require(condition: bool, message: &str) -> Result<(), String> {
        if condition {
            Ok(())
        } else {
            Err(message.into())
        }
    }
    async fn sql(
        driver: &MssqlDriver,
        connection: &ConnHandle,
        sql: String,
    ) -> Result<Vec<Row>, String> {
        let mut stream = driver
            .execute(connection.clone(), ExecuteRequest::new(sql))
            .await
            .map_err(|error| format!("fixture execute failed ({:?})", error.native_code))?;
        let mut rows = Vec::new();
        while let Some(page) = stream.rows.recv().await {
            match page {
                Page::Rows { rows: page } => rows.extend(page),
                Page::Done { .. } => return Ok(rows),
                Page::Error { error } => {
                    return Err(format!("fixture SQL failed ({:?})", error.native_code))
                }
                Page::NextResult { .. } => {}
            }
        }
        Err("fixture SQL did not complete".into())
    }
    async fn get<T: serde::de::DeserializeOwned>(
        router: &axum::Router,
        path: String,
    ) -> Result<T, String> {
        let response = router
            .clone()
            .oneshot(Request::get(path).body(Body::empty()).unwrap())
            .await
            .map_err(|_| "inspection request failed")?;
        require(
            response.status() == StatusCode::OK,
            "inspection HTTP status was not OK",
        )?;
        require(
            response
                .headers()
                .get("cache-control")
                .is_some_and(|value| value == "no-store"),
            "inspection must not be cached",
        )?;
        let bytes = to_bytes(response.into_body(), 1024 * 1024)
            .await
            .map_err(|_| "inspection body too large")?;
        serde_json::from_slice(&bytes).map_err(|_| "inspection result shape was invalid".into())
    }
    fn request(connection: sift_protocol::ConnectionId, sql: &str) -> ExecuteRequestHttp {
        ExecuteRequestHttp {
            connection,
            sql: sql.into(),
            params: vec![],
            tx: None,
            room_id: None,
            connection_profile_id: None,
            transform: None,
            source: None,
        }
    }

    #[tokio::test]
    async fn agent_roles_own_history_and_configuration_permissions_are_enforced_natively() {
        let suffix = uuid::Uuid::new_v4().simple().to_string();
        let login = format!("sift_inspect_{suffix}");
        let prefix = format!("000_sift_inspect_{suffix}");
        let own = format!("{prefix}_owned");
        let empty = format!("{prefix}_empty");
        let peer = format!("{prefix}_peer");
        let cap = (0..99)
            .map(|index| format!("{prefix}_cap_{index:03}"))
            .collect::<Vec<_>>();
        // Generated only in memory; errors and assertions never print setup SQL.
        let password = format!("Sift!{}aA9", uuid::Uuid::new_v4().simple());
        let counter = format!("sift_inspect_counter_{suffix}");
        let admin_spec = spec();
        let database = admin_spec.database.as_deref().unwrap().replace(']', "]]");
        let driver = MssqlDriver::new();
        let admin = driver.open(&admin_spec).await.expect("native admin opens");
        let store = SessionStore::new(
            DriverRegistry::builder()
                .register(MssqlDriver::new())
                .build(),
        );
        let router = app(AppState {
            sessions: store.clone(),
            rooms: RoomRuntime::default(),
            auth: AuthState::default(),
            metadata: None,
            shutdown: Shutdown::default(),
        });
        let mut connections = Vec::new();
        let outcome:Result<(),String>=async {
            sql(&driver,&admin,format!("CREATE LOGIN [{login}] WITH PASSWORD='{password}'; USE [{database}]; CREATE USER [{login}] FOR LOGIN [{login}]; USE msdb; IF USER_ID(N'{login}') IS NULL CREATE USER [{login}] FOR LOGIN [{login}];")).await?;
            sql(&driver,&admin,format!("USE msdb; DECLARE @job uniqueidentifier; EXEC dbo.sp_add_job @job_name=N'{own}',@enabled=0,@description=N'private-description-sentinel',@owner_login_name=N'{login}',@job_id=@job OUTPUT; EXEC dbo.sp_add_jobserver @job_id=@job; EXEC dbo.sp_add_jobstep @job_id=@job,@step_name=N'never executed',@subsystem=N'TSQL',@command=N'SELECT 1 /* private-command-sentinel */'; EXEC dbo.sp_sqlagent_log_jobhistory @job_id=@job,@step_id=0,@run_status=0,@run_date=20261104,@run_time=80000,@run_duration=2,@retries_attempted=0,@message=N'private-history-message-sentinel'; EXEC dbo.sp_sqlagent_log_jobhistory @job_id=@job,@step_id=0,@run_status=1,@run_date=20261104,@run_time=90102,@run_duration=10123,@retries_attempted=0,@message=N'private-history-message-sentinel'; EXEC dbo.sp_sqlagent_log_jobhistory @job_id=@job,@step_id=1,@run_status=0,@run_date=20261104,@run_time=100000,@run_duration=5,@retries_attempted=0,@message=N'private-history-message-sentinel'; EXEC dbo.sp_add_job @job_name=N'{empty}',@enabled=0,@owner_login_name=N'{login}'; EXEC dbo.sp_add_job @job_name=N'{peer}',@enabled=0;")).await?;
            let session=store.open_session(OpenSessionRequest{tag:Some("native-inspection-acceptance".into()),tenant_id:None}).id;
            let mut restricted=admin_spec.clone(); restricted.user=login.clone(); restricted.password=Some(password.clone());
            if let Some(EngineConnectionSpec::SqlServer(config))=&mut restricted.engine_specific {
                config.startup_sql=vec!["SET ROWCOUNT 2".into(),"SET NOCOUNT ON".into()];
            }
            let source=store.open_connection(session,Engine::SqlServer,restricted).await.map_err(|_|"restricted source opens")?.id;
            connections.push((session,source));
            let jobs_path=format!("/v1/sessions/{session}/connections/{source}/agent/jobs");
            let settings_path=format!("/v1/sessions/{session}/connections/{source}/settings/sqlserver");
            let denied:AgentJobsReport=get(&router,jobs_path.clone()).await?;
            require(denied.state==AgentJobsState::PermissionRequired && denied.jobs.is_empty(),"public login must not inspect Agent jobs")?;
            for role in ["SQLAgentUserRole","SQLAgentReaderRole","SQLAgentOperatorRole"] {
                sql(&driver,&admin,format!("USE msdb; ALTER ROLE [{role}] ADD MEMBER [{login}];")).await?;
                let report:AgentJobsReport=get(&router,jobs_path.clone()).await?;
                require(report.state==AgentJobsState::Available && report.jobs.len()==2 && !report.truncated,"Agent role must see exactly its two owned jobs")?;
                require(!report.jobs.iter().any(|job|job.name==peer),"broader native roles must not bypass Sift ownership filtering")?;
                let history=report.jobs.iter().find(|job|job.name==own).ok_or("owned history job missing")?;
                require(!history.enabled && history.last_outcome==Some(AgentJobOutcome::Succeeded),"latest whole-job history must exclude later failed steps")?;
                require(history.last_run_local.as_deref()==Some("2026-11-04 09:01:02") && history.last_duration_seconds==Some(3683),"native history time/duration was misdecoded")?;
                let never=report.jobs.iter().find(|job|job.name==empty).ok_or("never-run job missing")?;
                require(!never.enabled && never.last_outcome.is_none() && never.last_run_local.is_none() && never.last_duration_seconds.is_none(),"missing history must remain unavailable")?;
                let encoded=serde_json::to_string(&report).map_err(|_|"report serialization failed")?;
                for sentinel in ["private-description-sentinel","private-command-sentinel","private-history-message-sentinel"] {
                    require(!encoded.contains(sentinel),"private job content escaped the projection")?;
                }
                sql(&driver,&admin,format!("USE msdb; ALTER ROLE [{role}] DROP MEMBER [{login}];")).await?;
                let revoked:AgentJobsReport=get(&router,jobs_path.clone()).await?;
                require(revoked.state==AgentJobsState::PermissionRequired && revoked.jobs.is_empty(),"Agent permission revocation must be visible")?;
            }
            let admin_session=store.open_session(OpenSessionRequest{tag:Some("native-inspection-admin".into()),tenant_id:None}).id;
            let admin_source=store.open_connection(admin_session,Engine::SqlServer,admin_spec.clone()).await.map_err(|_|"admin source opens")?.id;
            connections.push((admin_session,admin_source));
            let admin_report:AgentJobsReport=get(&router,format!("/v1/sessions/{admin_session}/connections/{admin_source}/agent/jobs")).await?;
            require(admin_report.state==AgentJobsState::Available && [&own,&empty,&peer].iter().all(|name|admin_report.jobs.iter().any(|job|&job.name==*name)),"sysadmin must retain cross-owner job visibility")?;
            let admin_history=admin_report.jobs.iter().find(|job|job.name==own).ok_or("sysadmin history job missing")?;
            require(admin_history.last_outcome==Some(AgentJobOutcome::Succeeded) && admin_history.last_duration_seconds==Some(3683),"sysadmin native whole-job history changed")?;
            sql(&driver,&admin,format!("USE msdb; ALTER ROLE SQLAgentUserRole ADD MEMBER [{login}];")).await?;
            let create=cap.iter().map(|name|format!("EXEC dbo.sp_add_job @job_name=N'{name}',@enabled=0,@owner_login_name=N'{login}';")).collect::<String>();
            sql(&driver,&admin,format!("USE msdb; {create}")).await?;
            store.execute_http(session,request(source,"SET ROWCOUNT 2")).await.map_err(|_|"set editor rowcount failed")?;
            let capped:AgentJobsReport=get(&router,jobs_path.clone()).await?;
            require(capped.state==AgentJobsState::Available && capped.jobs.len()==100 && capped.truncated,"native job truncation must ignore editor rowcount")?;
            let editor=store.execute_http(session,request(source,"SELECT n FROM (VALUES (1),(2),(3),(4),(5)) AS r(n)")).await.map_err(|_|"read editor rowcount failed")?;
            require(editor.rows.len()==2,"inspection changed editor ROWCOUNT")?;

            require(store.list_connections(session).map_err(|_|"list source connections failed")?.len()==1,"private inspection connections leaked")?;
            // A 2022 build can allow public catalog reads. Grant the documented
            // permission explicitly, then require native object denial to win.
            sql(&driver,&admin,format!("USE master; GRANT VIEW SERVER PERFORMANCE STATE TO [{login}];")).await?;
            let settings:SqlServerSettingsReport=get(&router,settings_path.clone()).await?;
            require(settings.state==SqlServerSettingsState::Available && settings.settings.len()>2 && settings.settings.len()<=200,"native settings must be available and bounded")?;
            let expected=sql(&driver,&admin,"SELECT CONVERT(bigint,value),CONVERT(bigint,value_in_use) FROM sys.configurations WHERE name=N'max degree of parallelism'".into()).await?;
            require(expected.len()==1 && expected[0].values.len()==2,"native configuration reference shape changed")?;
            let setting=settings.settings.iter().find(|setting|setting.name=="max degree of parallelism").ok_or("expected native setting missing")?;
            require(expected[0].values==vec![Value::Int64(setting.configured_value),Value::Int64(setting.effective_value)],"configured/effective values differ from the native catalog")?;
            sql(&driver,&admin,format!("USE [{database}]; DENY SELECT ON OBJECT::sys.configurations TO [{login}];")).await?;
            let denied:SqlServerSettingsReport=get(&router,settings_path.clone()).await?;
            require(denied.state==SqlServerSettingsState::PermissionRequired && denied.settings.is_empty() && !denied.truncated,"native configuration denial must not look like an empty available catalog")?;
            sql(&driver,&admin,format!("USE [{database}]; REVOKE SELECT ON OBJECT::sys.configurations FROM [{login}];")).await?;
            let restored:SqlServerSettingsReport=get(&router,settings_path).await?;
            require(restored.state==SqlServerSettingsState::Available,"configuration read did not recover after revocation of denial")?;
            let editor=store.execute_http(session,request(source,"SELECT n FROM (VALUES (1),(2),(3),(4),(5)) AS r(n)")).await.map_err(|_|"read editor rowcount after settings failed")?;
            require(editor.rows.len()==2,"settings inspection changed editor ROWCOUNT")?;
            store.execute_http(session,request(source,"SET ROWCOUNT 0")).await.map_err(|_|"restore editor rowcount failed")?;

            sql(&driver,&admin,format!("USE [{database}]; CREATE TABLE dbo.[{counter}] (v int NOT NULL); GRANT INSERT,SELECT ON OBJECT::dbo.[{counter}] TO [{login}];")).await?;
            let mut startup_spec=admin_spec.clone(); startup_spec.user=login.clone(); startup_spec.password=Some(password.clone());
            if let Some(EngineConnectionSpec::SqlServer(config))=&mut startup_spec.engine_specific {
                config.startup_sql=vec![format!("INSERT INTO dbo.[{counter}] VALUES(1)")];
            }
            let startup_source=store.open_connection(session,Engine::SqlServer,startup_spec).await.map_err(|_|"startup counter source opens")?.id;
            connections.push((session,startup_source));
            for endpoint in ["agent/jobs","settings/sqlserver"] {
                let response=router.clone().oneshot(Request::get(format!("/v1/sessions/{session}/connections/{startup_source}/{endpoint}")).body(Body::empty()).unwrap()).await.map_err(|_|"startup refusal request failed")?;
                require(response.status()==StatusCode::BAD_REQUEST,"inspection must refuse arbitrary startup replay")?;
            }
            let counts=sql(&driver,&admin,format!("SELECT COUNT_BIG(*) FROM [{database}].dbo.[{counter}]")).await?;
            require(counts.len()==1 && counts[0].values==vec![Value::Int64(1)],"inspection replayed a write-bearing startup batch")?;
            require(store.list_connections(session).map_err(|_|"list startup sources failed")?.len()==2,"startup refusal opened another connection")?;
            let audit=serde_json::to_string(&store.list_operations()).map_err(|_|"audit serialization failed")?;
            require(audit.contains("read_agent_jobs") && audit.contains("read_sql_server_settings"),"native inspection operations were not audited")?;
            require(!audit.contains("private-command-sentinel") && !audit.contains("private-history-message-sentinel") && !audit.contains(&password),"private fixture content escaped into operation audit")?;
            Ok(())
        }.await;
        for (session, connection) in connections {
            let _ = store.close_connection(session, connection).await;
        }
        let mut names = vec![own, empty, peer];
        names.extend(cap);
        let delete=names.iter().map(|name|format!("IF EXISTS(SELECT 1 FROM dbo.sysjobs WHERE name=N'{name}') EXEC dbo.sp_delete_job @job_name=N'{name}';")).collect::<String>();
        let cleanup=sql(&driver,&admin,format!("USE msdb; {delete} IF USER_ID(N'{login}') IS NOT NULL DROP USER [{login}]; USE [{database}]; IF OBJECT_ID(N'dbo.{counter}',N'U') IS NOT NULL DROP TABLE dbo.[{counter}]; IF USER_ID(N'{login}') IS NOT NULL DROP USER [{login}]; USE master; IF SUSER_ID(N'{login}') IS NOT NULL DROP LOGIN [{login}];")).await;
        let _ = driver.close(admin).await;
        assert!(cleanup.is_ok(), "native inspection fixture cleanup failed");
        assert!(outcome.is_ok(), "{}", outcome.unwrap_err());
    }
}
