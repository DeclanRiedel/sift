use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use sift_driver_api::mock::MockDriver;
use sift_protocol::{
    CsvConflictPolicy, CsvImportRequest, CsvImportResponse, Engine, Page, ServerInfo,
};
use sift_server::http::{app, AppState, AuthState};
use sift_server::{DriverRegistry, RoomRuntime, SessionStore, Shutdown};
use tower::ServiceExt;

fn state() -> AppState {
    let inserted = vec![Page::Done {
        affected_rows: Some(1),
        warnings: vec![],
    }];
    let skipped = vec![Page::Done {
        affected_rows: Some(0),
        warnings: vec![],
    }];
    let driver = MockDriver::builder()
        .engine(Engine::Postgres)
        .ping_ok(ServerInfo {
            engine: Engine::Postgres,
            server_version: "mock".into(),
            current_database: "app".into(),
            current_user: "alice".into(),
            pool_warm_slots: None,
        })
        .execute_ok(inserted)
        .execute_ok(skipped)
        .build();
    AppState {
        sessions: SessionStore::new(DriverRegistry::builder().register(driver).build()),
        rooms: RoomRuntime::default(),
        auth: AuthState::default(),
        metadata: None,
        shutdown: Shutdown::default(),
    }
}

async fn json<T: serde::de::DeserializeOwned>(body: Body) -> T {
    serde_json::from_slice(&to_bytes(body, 1024 * 1024).await.unwrap()).unwrap()
}

#[tokio::test]
async fn csv_import_skip_reports_inserted_and_duplicate_rows() {
    let router = app(state());
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
                        r#"{"engine":"postgres","host":"mock","user":"alice"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap()
            .into_body(),
    )
    .await;

    let request = CsvImportRequest {
        table: "public.people".into(),
        data: b"id,name\n1,Alice\n1,Alice again\n".to_vec(),
        header: true,
        delimiter: ',',
        null_value: Some("NULL".into()),
        create_table: false,
        conflict_policy: CsvConflictPolicy::Skip,
    };
    let response = router
        .clone()
        .oneshot(
            Request::post(format!(
                "/v1/sessions/{}/connections/{}/import/csv",
                session.id, connection.id
            ))
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(&request).unwrap()))
            .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let response: CsvImportResponse = json(response.into_body()).await;
    assert_eq!(response.rows_inserted, 1);
    assert_eq!(response.rows_skipped, 1);
    assert_eq!(
        response.columns[0].inferred_type,
        sift_protocol::InferredCsvType::Int64
    );

    let operations: Vec<sift_protocol::OperationAuditEntry> = json(
        router
            .oneshot(Request::get("/v1/operations").body(Body::empty()).unwrap())
            .await
            .unwrap()
            .into_body(),
    )
    .await;
    assert!(operations.iter().any(|entry| matches!(
        &entry.operation,
        sift_protocol::Operation::ImportCsv { table, .. } if table == "public.people"
    )));
}
