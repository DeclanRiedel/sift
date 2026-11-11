//! Real room proposal commits, atomic refusal and original observer loss.
use sha2::{Digest, Sha256};
use sift_metadata::{DocumentId, MetadataStore, PrincipalId, TenantId};
use sift_protocol::*;
use sift_server::document_actor;
use sift_server::document_actor::CollaborationLimits;
use sift_server::{
    http::{app, AppState, AuthState},
    DriverRegistry, RoomRuntime, SessionStore,
};
use std::sync::Arc;
use uuid::Uuid;

struct Fixture {
    directory: tempfile::TempDir,
    state: AppState,
    metadata: MetadataStore,
    document: DocumentId,
    chat: Uuid,
    proposal: Uuid,
    request: CommitAiRoomQueryProposalRequest,
}
impl Fixture {
    async fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let metadata = MetadataStore::open(
            &directory.path().join("metadata.sqlite"),
            Arc::new(sift_metadata::MemorySecretStore::new()),
        )
        .unwrap();
        metadata.apply_migrations(false).unwrap();
        metadata.bootstrap_local("room proposal").unwrap();
        let actor = PrincipalId(1);
        let room = metadata
            .create_room(
                TenantId(1),
                actor,
                sift_metadata::NewRoom {
                    name: "query".into(),
                    kind: sift_metadata::RoomKind::Shared,
                },
            )
            .unwrap();
        let replica = sift_doc::TextReplica::new(sift_doc::random_peer_id()).unwrap();
        replica.insert(0, "SELECT 1").unwrap();
        let document = metadata
            .create_document(
                room.id,
                sift_metadata::NewDocument {
                    kind: "sql".into(),
                    title: "query.sql".into(),
                    crdt_state: replica.export_snapshot().unwrap(),
                    snapshot_version: replica.version_vector(),
                    position: 0,
                    connection_profile_id: None,
                },
            )
            .unwrap()
            .id;
        let chat = metadata
            .create_ai_chat(
                TenantId(1),
                None,
                actor,
                AiVisibility::Private,
                "query".into(),
            )
            .await
            .unwrap();
        let before = Sha256::digest(b"SELECT 1");
        let revision = u64::from_le_bytes(before[..8].try_into().unwrap()) & i64::MAX as u64;
        let context: AiTurnContext = serde_json::from_value(serde_json::json!({
            "target":{"tenant_id":1,"room_id":room.id.0,"document_id":document.0.to_string()},
            "sql":{"text":"SELECT 1","document_revision":revision,"room_document_id":document.0},"staged_change_count":0,
        })).unwrap();
        let run = metadata
            .start_ai_run(
                chat.id,
                actor,
                StartAiTurnRequest {
                    client_request_id: Uuid::new_v4(),
                    desktop_id: Uuid::new_v4(),
                    prompt: "edit query text".into(),
                    provider: AiProvider::Codex,
                    model: None,
                    mode: AiMode::Propose,
                    context: context.clone(),
                    attachment_previews: vec![],
                },
            )
            .await
            .unwrap();
        let proposal = metadata
            .stage_ai_query_proposal(
                run.run.id,
                actor,
                StageAiQueryProposalRequest {
                    client_request_id: Uuid::new_v4(),
                    lease_token: run.lease_token,
                    target: context.target,
                    base_revision: revision,
                    proposed_sql: "SELECT 2".into(),
                },
            )
            .await
            .unwrap();
        let mut auth = AuthState::default();
        auth.ai.enabled = true;
        let state = AppState {
            sessions: SessionStore::new(crate::DriverRegistry::new()),
            rooms: crate::RoomRuntime::default(),
            auth,
            metadata: Some(metadata.clone()),
            shutdown: Default::default(),
        };
        Self {
            directory,
            state,
            metadata,
            document,
            chat: chat.id,
            proposal: proposal.proposal.id,
            request: CommitAiRoomQueryProposalRequest {
                expected_revision: revision,
                expected_content_sha256: proposal.proposal.content_sha256,
            },
        }
    }
    fn caller(&self) -> sift_protocol::handshake::ExtensionCallerIdentity {
        sift_protocol::handshake::ExtensionCallerIdentity {
            instance_id: self.state.auth.instance_id.clone(),
            daemon_generation: self.state.auth.daemon_generation.clone(),
            principal_id: 1,
            auth_session_id: None,
        }
    }
    async fn prepared(&self, session: Option<String>) -> sift_metadata::PreparedAiRoomQueryCommit {
        self.metadata
            .prepare_ai_room_query_commit(
                self.chat,
                self.proposal,
                PrincipalId(1),
                session,
                self.request.clone(),
            )
            .await
            .unwrap()
    }
}

#[tokio::test]
async fn room_proposal_commit_http_replays_after_compaction_without_reediting() {
    let fixture = Fixture::new().await;
    let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let client = sift_client_sdk::Client::new(base.clone());
    let router = app(fixture.state.clone());
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let scoped = client.capture_ai_caller(&fixture.caller()).await.unwrap();
    let url = format!(
        "{base}/v1/scoped/ai/chats/{}/query-proposals/{}/room-commit",
        fixture.chat, fixture.proposal
    );
    let raw = reqwest::Client::new()
        .post(&url)
        .header("X-Sift-Protocol-Version", sift_protocol::PROTOCOL_VERSION)
        .json(&fixture.request)
        .send()
        .await
        .unwrap();
    assert_eq!(raw.status(), reqwest::StatusCode::BAD_REQUEST);
    assert!(fixture
        .metadata
        .ai_room_query_commit_receipt_now(&fixture.prepared(None).await)
        .unwrap()
        .is_none());
    let wrong = reqwest::Client::new()
        .post(&url)
        .header("X-Sift-Protocol-Version", sift_protocol::PROTOCOL_VERSION)
        .header(
            sift_protocol::handshake::EXPECTED_INSTANCE_HEADER,
            &fixture.state.auth.instance_id,
        )
        .header(
            sift_protocol::handshake::EXPECTED_DAEMON_HEADER,
            &fixture.state.auth.daemon_generation,
        )
        .header(sift_protocol::handshake::EXPECTED_PRINCIPAL_HEADER, "2")
        .header(sift_protocol::handshake::EXPECTED_AUTH_SESSION_HEADER, "-")
        .json(&fixture.request)
        .send()
        .await
        .unwrap();
    assert_eq!(wrong.status(), reqwest::StatusCode::FORBIDDEN);
    let committed = scoped
        .commit_room_ai_query_proposal(fixture.chat, fixture.proposal, &fixture.request)
        .await
        .unwrap();
    assert_eq!(
        committed.proposal.proposal.status,
        AiProposalStatus::Applied
    );
    assert_eq!(
        committed.receipt.before_sha256,
        format!("{:x}", Sha256::digest(b"SELECT 1"))
    );
    assert_eq!(
        committed.receipt.after_sha256,
        format!("{:x}", Sha256::digest(b"SELECT 2"))
    );
    let shared = fixture
        .state
        .rooms
        .documents()
        .get_or_load(&fixture.metadata, fixture.document)
        .unwrap();
    {
        let mut native = shared.lock().unwrap();
        assert_eq!(native.text(), "SELECT 2");
        native
            .author_replacement(
                &fixture.metadata,
                PrincipalId(1),
                "human",
                "later",
                "SELECT 3",
            )
            .unwrap();
        native.compact(&fixture.metadata).unwrap();
    }
    assert!(fixture
        .metadata
        .list_document_updates_since(fixture.document, 0)
        .unwrap()
        .is_empty());
    let replay = scoped
        .commit_room_ai_query_proposal(fixture.chat, fixture.proposal, &fixture.request)
        .await
        .unwrap();
    assert_eq!(replay.receipt, committed.receipt);
    assert_eq!(
        scoped
            .room_ai_query_commit(fixture.chat, fixture.proposal)
            .await
            .unwrap()
            .receipt,
        committed.receipt
    );
    assert_eq!(shared.lock().unwrap().text(), "SELECT 3");
    let mut changed = fixture.request.clone();
    changed.expected_revision += 1;
    assert!(scoped
        .commit_room_ai_query_proposal(fixture.chat, fixture.proposal, &changed)
        .await
        .is_err());
    assert_eq!(shared.lock().unwrap().text(), "SELECT 3");
    server.abort();
}

#[tokio::test]
async fn room_proposal_commit_refuses_stale_permission_session_and_atomic_tail_failure() {
    for mode in ["stale", "permission", "session", "expired", "kind", "tail"] {
        let fixture = Fixture::new().await;
        let prepared = fixture
            .prepared((mode == "session").then(|| "revoked-original-session".into()))
            .await;
        let prepared = if mode == "expired" {
            prepared.with_deadline(std::time::Instant::now())
        } else {
            prepared
        };
        let mut native = crate::document_actor::DocumentActor::load(
            &fixture.metadata,
            fixture.document,
            CollaborationLimits::default(),
        )
        .unwrap();
        if mode == "stale" {
            native
                .author_replacement(
                    &fixture.metadata,
                    PrincipalId(1),
                    "human",
                    "changed",
                    "SELECT 3",
                )
                .unwrap();
        }
        let sql =
            rusqlite::Connection::open(fixture.directory.path().join("metadata.sqlite")).unwrap();
        if mode == "permission" {
            sql.execute(
                "UPDATE room_member SET role='viewer' WHERE principal_id=1",
                [],
            )
            .unwrap();
        }
        if mode == "kind" {
            sql.execute("UPDATE document SET kind='text'", []).unwrap();
        }
        if mode == "tail" {
            sql.execute("UPDATE ai_run SET next_sequence=1025", [])
                .unwrap();
        }
        assert!(
            native
                .author_ai_room_query_proposal(&fixture.metadata, &prepared)
                .is_err(),
            "{mode}"
        );
        assert_eq!(
            native.text(),
            if mode == "stale" {
                "SELECT 3"
            } else {
                "SELECT 1"
            }
        );
        assert!(fixture
            .metadata
            .ai_room_query_commit_receipt_now(&prepared)
            .unwrap()
            .is_none());
        assert_eq!(
            sql.query_row(
                "SELECT status FROM ai_proposal WHERE id=?1",
                [fixture.proposal.to_string()],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
            "staged"
        );
        assert_eq!(
            fixture
                .metadata
                .list_document_updates_since(fixture.document, 0)
                .unwrap()
                .len(),
            usize::from(mode == "stale")
        );
    }
}

#[tokio::test]
async fn room_proposal_commit_owner_outlives_the_original_observer() {
    let fixture = Fixture::new().await;
    let shared = fixture
        .state
        .rooms
        .documents()
        .get_or_load(&fixture.metadata, fixture.document)
        .unwrap();
    let (release, released) = std::sync::mpsc::channel();
    let (acquired, ready) = tokio::sync::oneshot::channel();
    let blocker = tokio::task::spawn_blocking(move || {
        let _native = shared.lock().unwrap();
        acquired.send(()).unwrap();
        let _ = released.recv();
    });
    ready.await.unwrap();
    let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap();
    let client = sift_client_sdk::Client::new(format!("http://{}", listener.local_addr().unwrap()));
    let router = app(fixture.state.clone());
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let client = client.capture_ai_caller(&fixture.caller()).await.unwrap();
    let chat = fixture.chat;
    let proposal = fixture.proposal;
    let request = fixture.request.clone();
    let observer = tokio::spawn(async move {
        client
            .commit_room_ai_query_proposal(chat, proposal, &request)
            .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while fixture.state.shutdown.in_flight() == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    observer.abort();
    assert!(observer.await.unwrap_err().is_cancelled());
    let prepared = fixture.prepared(None).await;
    assert!(fixture
        .metadata
        .ai_room_query_commit_receipt_now(&prepared)
        .unwrap()
        .is_none());
    assert!(fixture.state.shutdown.in_flight() > 0);
    release.send(()).unwrap();
    blocker.await.unwrap();
    assert_eq!(
        fixture
            .state
            .shutdown
            .await_drain(std::time::Duration::from_secs(2))
            .await,
        0
    );
    let receipt = fixture
        .metadata
        .ai_room_query_commit_receipt_now(&prepared)
        .unwrap()
        .unwrap();
    assert_eq!(receipt.applied_by, 1);
    assert_eq!(
        crate::document_actor::DocumentActor::load(
            &fixture.metadata,
            fixture.document,
            CollaborationLimits::default()
        )
        .unwrap()
        .text(),
        "SELECT 2"
    );
    server.abort();
}
