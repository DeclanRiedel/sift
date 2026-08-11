use std::sync::Arc;

use axum::body::{to_bytes, Body};
use axum::http::{Method, Request, StatusCode};
use sift_metadata::{MemorySecretStore, MetadataStore, NewRoom, PrincipalId, RoomKind, TenantId};
use sift_protocol::{
    DdlSource, DdlSourceModel, ProjectionBinding, ReconcilePlan, ReconcileResolution, Workspace,
    WorkspaceCheckpoint, WorkspaceNodeKind,
};
use sift_server::config::{VcsConfig, WorkspaceProjectionConfig, WorkspaceRootConfig};
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

#[tokio::test]
async fn projection_conflicts_and_offline_ddl_are_explicit_and_deterministic() {
    let metadata = MetadataStore::open_in_memory(Arc::new(MemorySecretStore::new())).unwrap();
    metadata.bootstrap_local("local user").unwrap();
    let room = metadata
        .create_room(
            TenantId(1),
            PrincipalId(1),
            NewRoom {
                name: "projection room".into(),
                kind: RoomKind::Shared,
            },
        )
        .unwrap();
    let checkout = tempfile::tempdir().unwrap();
    let rooms = RoomRuntime::with_workspace_config(&WorkspaceProjectionConfig {
        enabled: true,
        roots: vec![WorkspaceRootConfig {
            handle: "checkout".into(),
            path: checkout.path().display().to_string(),
            read_only: false,
        }],
    })
    .unwrap();
    let router = app(AppState {
        sessions: SessionStore::new(DriverRegistry::builder().build()),
        rooms: rooms.clone(),
        auth: AuthState::default(),
        metadata: Some(metadata),
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
    let workspace: Workspace = json(response.into_body()).await;
    assert!(workspace.capabilities.filesystem_projection);
    let response = router
        .clone()
        .oneshot(json_request(
            Method::POST,
            &format!("/v1/metadata/workspaces/{}/nodes", workspace.id.0),
            serde_json::json!({
                "expected_workspace_revision": 1,
                "parent_id": null,
                "path": "schema.sql",
                "kind": "sql_document",
                "initial_text": "CREATE TABLE public.users (id bigint primary key);"
            }),
        ))
        .await
        .unwrap();
    let tree: sift_metadata::http::WorkspaceTreeResponse = json(response.into_body()).await;
    let node = tree.nodes[0].clone();

    let response = router
        .clone()
        .oneshot(json_request(
            Method::POST,
            &format!("/v1/metadata/workspaces/{}/projection", workspace.id.0),
            serde_json::json!({"root_handle": "checkout", "mode": "read_write"}),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let binding: ProjectionBinding = json(response.into_body()).await;
    let response = router
        .clone()
        .oneshot(json_request(
            Method::GET,
            &format!(
                "/v1/metadata/workspace-projections/{}/reconcile",
                binding.id.0
            ),
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    let plan: ReconcilePlan = json(response.into_body()).await;
    assert_eq!(plan.entries.len(), 1);
    let response = router
        .clone()
        .oneshot(json_request(
            Method::POST,
            &format!(
                "/v1/metadata/workspace-projections/{}/reconcile",
                binding.id.0
            ),
            serde_json::json!({
                "binding_revision": plan.binding_revision,
                "workspace_revision": plan.workspace_revision,
                "resolutions": [{
                    "observed": plan.entries[0],
                    "resolution": ReconcileResolution::MaterializeWorkspace
                }]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(checkout.path().join("schema.sql").is_file());

    std::fs::write(
        checkout.path().join("schema.sql"),
        "CREATE TABLE public.accounts (id bigint primary key);",
    )
    .unwrap();
    let response = router
        .clone()
        .oneshot(json_request(
            Method::GET,
            &format!(
                "/v1/metadata/workspace-projections/{}/reconcile",
                binding.id.0
            ),
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    let plan: ReconcilePlan = json(response.into_body()).await;
    let response = router
        .clone()
        .oneshot(json_request(
            Method::POST,
            &format!(
                "/v1/metadata/workspace-projections/{}/reconcile",
                binding.id.0
            ),
            serde_json::json!({
                "binding_revision": plan.binding_revision,
                "workspace_revision": plan.workspace_revision,
                "resolutions": [{
                    "observed": plan.entries[0],
                    "resolution": ReconcileResolution::ImportProjection
                }]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let response = router
        .clone()
        .oneshot(json_request(
            Method::POST,
            &format!("/v1/metadata/workspaces/{}/ddl-sources", workspace.id.0),
            serde_json::json!({
                "name": "desired",
                "dialect_id": "sift/postgres",
                "roots": [node.id]
            }),
        ))
        .await
        .unwrap();
    let source: DdlSource = json(response.into_body()).await;
    let response = router
        .clone()
        .oneshot(json_request(
            Method::POST,
            &format!("/v1/metadata/ddl-sources/{}/refresh", source.id.0),
            serde_json::json!({"expected_revision": source.revision}),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let model: DdlSourceModel = json(response.into_body()).await;
    let graph = model.graph.unwrap();
    assert!(graph
        .data
        .nodes
        .iter()
        .any(|catalog_node| catalog_node.name == "accounts"));
    let rebuilt = sift_server::ddl_source::build_model(
        "sift/postgres",
        graph.revision.0,
        &[sift_server::ddl_source::DdlInput {
            path: sift_protocol::WorkspacePath("schema.sql".into()),
            text: "CREATE TABLE public.accounts (id bigint primary key);".into(),
        }],
    );
    assert_eq!(
        graph.content_digest,
        rebuilt.graph.unwrap().content_digest,
        "local and remote-topology builders share the same canonical graph"
    );
}

#[tokio::test]
async fn git_commit_is_tied_to_a_checkpoint_and_later_virtual_edits_stay_uncommitted() {
    let metadata = MetadataStore::open_in_memory(Arc::new(MemorySecretStore::new())).unwrap();
    metadata.bootstrap_local("local user").unwrap();
    let room = metadata
        .create_room(
            TenantId(1),
            PrincipalId(1),
            NewRoom {
                name: "Git room".into(),
                kind: RoomKind::Shared,
            },
        )
        .unwrap();
    let checkout = tempfile::tempdir().unwrap();
    let rooms = RoomRuntime::with_integrations(
        &WorkspaceProjectionConfig {
            enabled: true,
            roots: vec![WorkspaceRootConfig {
                handle: "checkout".into(),
                path: checkout.path().display().to_string(),
                read_only: false,
            }],
        },
        &VcsConfig {
            enabled: true,
            network_enabled: false,
            ..VcsConfig::default()
        },
    )
    .await
    .unwrap();
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
    let workspace: Workspace = json(response.into_body()).await;
    assert!(workspace.capabilities.git);
    assert!(!workspace.capabilities.git_network);
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
                "initial_text": "select 1;\n"
            }),
        ))
        .await
        .unwrap();
    let tree: sift_metadata::http::WorkspaceTreeResponse = json(response.into_body()).await;
    let node = tree.nodes[0].clone();
    let response = router
        .clone()
        .oneshot(json_request(
            Method::POST,
            &format!("/v1/metadata/workspaces/{}/projection", workspace.id.0),
            serde_json::json!({"root_handle": "checkout", "mode": "read_write"}),
        ))
        .await
        .unwrap();
    let projection: ProjectionBinding = json(response.into_body()).await;
    let response = router
        .clone()
        .oneshot(json_request(
            Method::GET,
            &format!(
                "/v1/metadata/workspace-projections/{}/reconcile",
                projection.id.0
            ),
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    let plan: ReconcilePlan = json(response.into_body()).await;
    let response = router
        .clone()
        .oneshot(json_request(
            Method::POST,
            &format!(
                "/v1/metadata/workspace-projections/{}/reconcile",
                projection.id.0
            ),
            serde_json::json!({
                "binding_revision": plan.binding_revision,
                "workspace_revision": plan.workspace_revision,
                "resolutions": [{
                    "observed": plan.entries[0],
                    "resolution": "materialize_workspace"
                }]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let response = router
        .clone()
        .oneshot(json_request(
            Method::POST,
            &format!("/v1/metadata/workspaces/{}/repository", workspace.id.0),
            serde_json::json!({"projection_id": projection.id, "initialize": true}),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let binding: sift_protocol::RepositoryBinding = json(response.into_body()).await;
    let response = router
        .clone()
        .oneshot(json_request(
            Method::POST,
            &format!("/v1/metadata/repositories/{}/commit", binding.id.0),
            serde_json::json!({
                "expected_revision": binding.revision,
                "message": "checkpoint",
                "author_name": "Sift Test",
                "author_email": "sift@example.invalid"
            }),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let committed: sift_protocol::VcsCommitResult = json(response.into_body()).await;
    assert_eq!(committed.workspace_revision.0, 2);

    let document = sift_metadata::DocumentId(node.document_id.unwrap());
    let actor = rooms.documents().get_or_load(&metadata, document).unwrap();
    actor
        .lock()
        .unwrap()
        .author_replacement(
            &metadata,
            PrincipalId(1),
            "other-client",
            "after-git-capture",
            "select 2;\n",
        )
        .unwrap();
    let response = router
        .oneshot(json_request(
            Method::GET,
            &format!("/v1/metadata/repositories/{}/status", binding.id.0),
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    let status: sift_protocol::VcsStatus = json(response.into_body()).await;
    assert!(status.entries.is_empty());
    assert_eq!(
        std::fs::read_to_string(checkout.path().join("query.sql")).unwrap(),
        "select 1;\n"
    );
}
