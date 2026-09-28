use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use sift_driver_api::mock::MockDriver;
use sift_protocol::{
    Code, ColumnMetadata, ConnectionSpec, Engine, Page, PrimitiveType, ProfileRequest,
    ProfileResponse, Row, ServerInfo, TypeRef, Value,
};
use sift_server::http::{app, AppState, AuthState};
use sift_server::{DriverRegistry, RoomRuntime, SessionStore, Shutdown};
use tower::ServiceExt;

async fn json<T: serde::de::DeserializeOwned>(body: Body) -> T {
    serde_json::from_slice(&to_bytes(body, 1024 * 1024).await.unwrap()).unwrap()
}

#[tokio::test]
async fn profile_streams_actual_plan_without_retaining_query_rows() {
    let info = ServerInfo {
        provider: Engine::SqlServer.provider_ref("test"),
        server_version: "mock".into(),
        current_database: "app".into(),
        current_user: "reader".into(),
        pool_warm_slots: None,
    };
    let done = || Page::Done {
        affected_rows: None,
        warnings: vec![],
    };
    let xml = r#"<ShowPlanXML xmlns="http://schemas.microsoft.com/sqlserver/2004/07/showplan"><BatchSequence><Batch><Statements><StmtSimple><QueryPlan><QueryTimeStats CpuTime="2" ElapsedTime="3"/><RelOp PhysicalOp="Index Seek" EstimateRows="1"><RunTimeInformation><RunTimeCountersPerThread ActualRows="1" ActualElapsedms="3" ActualLogicalReads="4"/></RunTimeInformation></RelOp></QueryPlan></StmtSimple></Statements></Batch></BatchSequence></ShowPlanXML>"#;
    let driver = MockDriver::builder()
        .engine(Engine::SqlServer)
        .ping_ok(info.clone())
        .ping_ok(info)
        .execute_ok(vec![done()])
        .execute_ok(vec![
            Page::NextResult {
                columns: vec![ColumnMetadata::new(
                    "n",
                    TypeRef::Primitive(PrimitiveType::Int32),
                )],
            },
            Page::Rows {
                rows: vec![Row::new(vec![Value::Int32(1)])],
            },
            Page::NextResult {
                columns: vec![ColumnMetadata::new(
                    "Microsoft SQL Server 2005 XML Showplan",
                    TypeRef::Primitive(PrimitiveType::Text),
                )],
            },
            Page::Rows {
                rows: vec![Row::new(vec![Value::Text(xml.into())])],
            },
            done(),
        ])
        .execute_ok(vec![done()])
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
                        r#"{"provider_id":"sift/sql-server","host":"mock","user":"reader"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap()
            .into_body(),
    )
    .await;
    let request = ProfileRequest {
        connection: connection.id,
        run_id: uuid::Uuid::new_v4(),
        sql: "SELECT 1".into(),
        params: vec![],
        timeout_ms: 10_000,
        workload_confirmed: true,
    };
    let response = router
        .oneshot(
            Request::post(format!(
                "/v1/sessions/{}/connections/{}/profile",
                session.id, connection.id
            ))
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(&request).unwrap()))
            .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let profile: ProfileResponse = json(response.into_body()).await;
    assert_eq!(profile.plan.engine, Engine::SqlServer);
    assert!(profile.plan.analyzed);
    assert_eq!(profile.plan.root.actual_rows, Some(1.0));
    assert_eq!(profile.plan.root.extra["ActualLogicalReads"], 4);
    assert_eq!(profile.execution_ms, Some(3.0));
}

#[tokio::test]
async fn profile_timeout_discards_delayed_sqlserver_execution() {
    let info = ServerInfo {
        provider: Engine::SqlServer.provider_ref("test"),
        server_version: "mock".into(),
        current_database: "app".into(),
        current_user: "reader".into(),
        pool_warm_slots: None,
    };
    let driver = MockDriver::builder()
        .engine(Engine::SqlServer)
        .ping_ok(info.clone())
        .ping_ok(info)
        .execute_delay(std::time::Duration::ZERO)
        .execute_delay(std::time::Duration::from_millis(100))
        .execute_ok(vec![Page::Done {
            affected_rows: None,
            warnings: vec![],
        }])
        .build();
    let store = SessionStore::new(DriverRegistry::builder().register(driver).build());
    let session = store.open_session(sift_protocol::OpenSessionRequest {
        tag: None,
        tenant_id: None,
    });
    let connection = store
        .open_connection(
            session.id,
            Engine::SqlServer,
            ConnectionSpec {
                host: "mock".into(),
                port: None,
                database: None,
                user: "reader".into(),
                password: None,
                ssl_mode: None,
                engine_specific: None,
            },
        )
        .await
        .unwrap();
    let error = store
        .profile(
            session.id,
            connection.id,
            ProfileRequest {
                connection: connection.id,
                run_id: uuid::Uuid::new_v4(),
                sql: "SELECT 1".into(),
                params: vec![],
                timeout_ms: 1,
                workload_confirmed: true,
            },
        )
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        sift_server::error::ApiError::Driver(sift_protocol::DriverError {
            code: Code::QueryTimedOut,
            ..
        })
    ));
}
