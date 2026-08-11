use std::sync::Arc;

use axum::body::{to_bytes, Body};
use axum::http::{Method, Request, StatusCode};
use sift_metadata::{MemorySecretStore, MetadataStore, NewRoom, PrincipalId, RoomKind, TenantId};
use sift_protocol::{Workspace, WorkspaceCheckpoint, WorkspaceNodeKind};
use sift_server::http::{app, AppState, AuthState};
use sift_server::registry::DriverRegistry;
use sift_server::room_runtime::RoomRuntime;
use sift_server::session::SessionStore;
use sift_server::shutdown::Shutdown;
use tower::ServiceExt;

fn json_request(method: Method, uri: &str, body: serde_json::Value) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap()
}

async fn json<T: serde::de::DeserializeOwned>(body: Body) -> T {
    let bytes = to_bytes(body, 4 * 1024 * 1024).await.unwrap();
    serde_json::from_slice(&bytes).unwrap_or_else(|error| {
        panic!(
            "decode response: {error}: {}",
            String::from_utf8_lossy(&bytes)
        )
    })
}

#[tokio::test]
async fn concurrent_clients_checkpoint_and_restore_one_collaborative_document() {
    let metadata = MetadataStore::open_in_memory(Arc::new(MemorySecretStore::new())).unwrap();
    metadata.bootstrap_local("local user").unwrap();
    let room = metadata
        .create_room(
            TenantId(1),
            PrincipalId(1),
            NewRoom {
                name: "workspace room".into(),
                kind: RoomKind::Shared,
            },
        )
        .unwrap();
    let rooms = RoomRuntime::default();
    let router = app(AppState {
        sessions: SessionStore::new(DriverRegistry::builder().build()),
        rooms: rooms.clone(),
        auth: AuthState::default(),
        metadata: Some(metadata.clone()),
        shutdown: Shutdown::default(),
    });

    let response = router
        .clone()
        .oneshot(json_request(
            Method::POST,
            &format!("/v1/metadata/rooms/{}/workspaces", room.id.0),
            serde_json::json!({"name": "database"}),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let workspace: Workspace = json(response.into_body()).await;

    let response = router
        .clone()
        .oneshot(json_request(
            Method::POST,
            &format!("/v1/metadata/workspaces/{}/nodes", workspace.id.0),
            serde_json::json!({
                "expected_workspace_revision": 1,
                "parent_id": null,
                "path": "query.sql",
                "kind": "sql_document",
                "initial_text": "select 1"
            }),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let tree: sift_metadata::http::WorkspaceTreeResponse = json(response.into_body()).await;
    let query = tree.nodes[0].clone();
    assert_eq!(query.kind, WorkspaceNodeKind::SqlDocument);

    // A second client observed revision 1 before the first mutation and is
    // rejected instead of silently overwriting the new tree head.
    let stale = router
        .clone()
        .oneshot(json_request(
            Method::POST,
            &format!("/v1/metadata/workspaces/{}/nodes", workspace.id.0),
            serde_json::json!({
                "expected_workspace_revision": 1,
                "parent_id": null,
                "path": "stale",
                "kind": "folder"
            }),
        ))
        .await
        .unwrap();
    assert_eq!(stale.status(), StatusCode::CONFLICT);

    let response = router
        .clone()
        .oneshot(json_request(
            Method::POST,
            &format!("/v1/metadata/workspaces/{}/checkpoints", workspace.id.0),
            serde_json::json!({
                "expected_workspace_revision": 2,
                "reason": "named",
                "name": "baseline"
            }),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let checkpoint: WorkspaceCheckpoint = json(response.into_body()).await;

    let document = sift_metadata::DocumentId(query.document_id.unwrap());
    let actor = rooms.documents().get_or_load(&metadata, document).unwrap();
    {
        let mut actor = actor.lock().unwrap();
        actor
            .author_replacement(
                &metadata,
                PrincipalId(1),
                "test-client",
                "edit-after-checkpoint",
                "select 2",
            )
            .unwrap();
        assert_eq!(actor.text(), "select 2");
    }

    let response = router
        .clone()
        .oneshot(json_request(
            Method::PUT,
            &format!("/v1/metadata/workspace-nodes/{}", query.id.0),
            serde_json::json!({
                "expected_workspace_revision": 2,
                "parent_id": null,
                "path": "renamed.sql"
            }),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let response = router
        .clone()
        .oneshot(json_request(
            Method::POST,
            &format!(
                "/v1/metadata/workspace-checkpoints/{}/restore",
                checkpoint.id.0
            ),
            serde_json::json!({"expected_workspace_revision": 3}),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let restored: sift_metadata::http::WorkspaceTreeResponse = json(response.into_body()).await;
    assert_eq!(restored.workspace.revision.0, 4);
    assert_eq!(restored.nodes[0].id, query.id);
    assert_eq!(restored.nodes[0].path.0, "query.sql");
    assert_eq!(actor.lock().unwrap().text(), "select 1");
}
