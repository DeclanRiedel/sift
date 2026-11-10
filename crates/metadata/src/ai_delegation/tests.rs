use super::*;
use std::sync::Arc;

async fn fixture() -> (
    MetadataStore,
    Uuid,
    CreateAiDelegationReview,
    AiDelegationReviewContent,
    AiDelegationAdmissionPolicy,
) {
    let store = MetadataStore::open_in_memory(Arc::new(crate::MemorySecretStore::new())).unwrap();
    store.bootstrap_local("owner").unwrap();
    let chat = store
        .create_ai_chat(
            TenantId(1),
            None,
            PrincipalId(1),
            AiVisibility::Private,
            "delegation".into(),
        )
        .await
        .unwrap();
    let policy = AiDelegationPolicy {
        budget: AiDelegationBudget {
            max_tool_calls: 3,
            max_input_bytes: 4096,
            max_output_bytes: 8192,
            max_duration_secs: 300,
        },
        rules: vec![AiDelegationRule {
            id: Uuid::new_v4(),
            max_uses: 2,
            action: AiDelegationActionRule::ExactSql {
                sql: "DELETE FROM private_delegate_sentinel WHERE id=1".into(),
                params: vec![],
            },
        }],
    };
    let desktop = Uuid::new_v4();
    let request = CreateAiDelegationReview {
        client_request_id: Uuid::new_v4(),
        desktop_id: desktop,
        target: ToolContext {
            tenant_id: Some(1),
            profile_id: Some(7),
            connection_id: Some("2:3".into()),
            room_id: None,
            document_id: None,
        },
        policy: policy.clone(),
        expires_at: Utc::now() + chrono::Duration::minutes(5),
        material_references: vec![],
    };
    let content = AiDelegationReviewContent {
        policy,
        target: request.target.clone(),
        source: AiExecutionSource {
            tenant_id: 1,
            profile_id: 7,
            session: SessionId(2),
            connection: ConnectionId(3),
            generation: Uuid::new_v4(),
            profile_digest: "b".repeat(64),
            database_label: "private-source-sentinel".into(),
            production: true,
            engine: Engine::Postgres,
        },
        scope_sha256: "a".repeat(64),
        tenant_policy_revision: 1,
        desktop_id: desktop,
        materials: vec![],
    };
    let instance = AiDelegationAdmissionPolicy {
        enabled: true,
        arbitrary_sql_enabled: false,
        scope_sha256: "a".repeat(64),
        max_calls: 3,
        max_duration_secs: 600,
    };
    (store, chat.id, request, content, instance)
}
async fn enable(store: &MetadataStore) {
    let changed = store
        .set_ai_delegation_tenant_policy(
            TenantId(1),
            PrincipalId(1),
            SetAiDelegationTenantPolicy {
                expected_revision: 0,
                enabled: true,
                arbitrary_sql_enabled: false,
            },
        )
        .await
        .unwrap();
    assert_eq!(changed.policy.revision, 1);
    assert!(changed.closed_runs.is_empty());
}

#[tokio::test]
async fn encrypted_review_replay_production_approval_rotation_and_cleanup() {
    let (store, chat, request, content, instance) = fixture().await;
    let actor = PrincipalId(1);
    let before = store
        .ai_content_handles_offline(Some(TenantId(1)))
        .unwrap()
        .len();
    assert!(
        store
            .create_ai_delegation_review(
                chat,
                actor,
                request.clone(),
                content.clone(),
                instance.clone()
            )
            .await
            .is_err(),
        "tenant opt-in defaults off"
    );
    assert_eq!(
        store
            .ai_content_handles_offline(Some(TenantId(1)))
            .unwrap()
            .len(),
        before
    );
    enable(&store).await;
    let (first, replay) = tokio::join!(
        store.create_ai_delegation_review(
            chat,
            actor,
            request.clone(),
            content.clone(),
            instance.clone()
        ),
        store.create_ai_delegation_review(
            chat,
            actor,
            request.clone(),
            content.clone(),
            instance.clone()
        )
    );
    let first = first.unwrap();
    let replay = replay.unwrap();
    assert_eq!(first.summary.id, replay.summary.id);
    assert_eq!(first.summary.fingerprint, replay.summary.fingerprint);
    assert!(!format!("{first:?}").contains("private_delegate_sentinel"));
    assert!(!format!("{first:?}").contains("private-source-sentinel"));
    assert_eq!(
        store
            .ai_content_handles_offline(Some(TenantId(1)))
            .unwrap()
            .len(),
        before + 1
    );
    assert_eq!(
        store
            .conn()
            .unwrap()
            .pragma_query_value::<u32, _>(None, "user_version", |r| r.get(0))
            .unwrap(),
        65
    );
    assert!(store
        .ai_delegation_review(first.summary.id, PrincipalId(999))
        .await
        .is_err());
    assert!(store
        .approve_ai_delegation_review(
            first.summary.id,
            actor,
            ApproveAiDelegationReview {
                fingerprint: first.summary.fingerprint.clone(),
                production_confirmation: None
            },
            instance.clone()
        )
        .await
        .is_err());
    let approved = store
        .approve_ai_delegation_review(
            first.summary.id,
            actor,
            ApproveAiDelegationReview {
                fingerprint: first.summary.fingerprint.clone(),
                production_confirmation: Some(content.source.database_label.clone()),
            },
            instance.clone(),
        )
        .await
        .unwrap();
    assert_eq!(approved.state, AiDelegationReviewState::Approved);
    assert_eq!(approved.approved_by, Some(1));
    assert!(store
        .prepare_ai_delegation_review(first.summary.id, actor, instance)
        .await
        .is_ok());
    assert_eq!(
        store
            .conn()
            .unwrap()
            .query_row::<i64, _, _>("SELECT count(*) FROM ai_run_delegation", [], |r| r.get(0))
            .unwrap(),
        0,
        "human approval alone never activates a run or native action"
    );
    let handle: String = store
        .conn()
        .unwrap()
        .query_row(
            "SELECT content_handle FROM ai_delegation_review WHERE id=?1",
            [first.summary.id.to_string()],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        store
            .rotate_ai_content_key(TenantId(1), actor)
            .await
            .unwrap(),
        before + 1
    );
    assert_eq!(
        store
            .ai_delegation_review(first.summary.id, actor)
            .await
            .unwrap()
            .summary
            .fingerprint,
        first.summary.fingerprint
    );
    assert!(!store
        .ai_content_recovery_key_handles(TenantId(1))
        .await
        .unwrap()
        .is_empty());
    store.delete_ai_chat(chat, actor).await.unwrap();
    assert!(store.ai_content.get(1, &handle).await.unwrap().is_none());
    assert!(store
        .ai_content_handles_offline(Some(TenantId(1)))
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn tenant_policy_revisions_revoke_old_reviews_and_arbitrary_sql_needs_both_gates() {
    let (store, chat, mut request, mut content, mut instance) = fixture().await;
    let actor = PrincipalId(1);
    enable(&store).await;
    let first = store
        .create_ai_delegation_review(
            chat,
            actor,
            request.clone(),
            content.clone(),
            instance.clone(),
        )
        .await
        .unwrap();
    store
        .approve_ai_delegation_review(
            first.summary.id,
            actor,
            ApproveAiDelegationReview {
                fingerprint: first.summary.fingerprint.clone(),
                production_confirmation: Some(content.source.database_label.clone()),
            },
            instance.clone(),
        )
        .await
        .unwrap();
    let changed = store
        .set_ai_delegation_tenant_policy(
            TenantId(1),
            actor,
            SetAiDelegationTenantPolicy {
                expected_revision: 1,
                enabled: true,
                arbitrary_sql_enabled: false,
            },
        )
        .await
        .unwrap();
    assert_eq!(changed.policy.revision, 2);
    assert_eq!(
        store
            .ai_delegation_review(first.summary.id, actor)
            .await
            .unwrap()
            .summary
            .state,
        AiDelegationReviewState::Revoked
    );
    assert!(store
        .prepare_ai_delegation_review(first.summary.id, actor, instance.clone())
        .await
        .is_err());
    assert!(store
        .set_ai_delegation_tenant_policy(
            TenantId(1),
            actor,
            SetAiDelegationTenantPolicy {
                expected_revision: 1,
                enabled: false,
                arbitrary_sql_enabled: false
            }
        )
        .await
        .is_err());
    assert!(store
        .set_ai_delegation_tenant_policy(
            TenantId(1),
            PrincipalId(999),
            SetAiDelegationTenantPolicy {
                expected_revision: 2,
                enabled: true,
                arbitrary_sql_enabled: true
            }
        )
        .await
        .is_err());
    content.policy.rules[0].action = AiDelegationActionRule::ArbitrarySql {
        acknowledge_unbounded_effects: true,
    };
    content.tenant_policy_revision = 2;
    request.policy = content.policy.clone();
    request.client_request_id = Uuid::new_v4();
    instance.arbitrary_sql_enabled = true;
    assert!(
        store
            .create_ai_delegation_review(
                chat,
                actor,
                request.clone(),
                content.clone(),
                instance.clone()
            )
            .await
            .is_err(),
        "instance alone cannot enable arbitrary SQL"
    );
    store
        .set_ai_delegation_tenant_policy(
            TenantId(1),
            actor,
            SetAiDelegationTenantPolicy {
                expected_revision: 2,
                enabled: true,
                arbitrary_sql_enabled: true,
            },
        )
        .await
        .unwrap();
    content.tenant_policy_revision = 3;
    instance.arbitrary_sql_enabled = false;
    assert!(
        store
            .create_ai_delegation_review(
                chat,
                actor,
                request.clone(),
                content.clone(),
                instance.clone()
            )
            .await
            .is_err(),
        "tenant alone cannot enable arbitrary SQL"
    );
    instance.arbitrary_sql_enabled = true;
    assert_eq!(
        store
            .create_ai_delegation_review(chat, actor, request, content, instance)
            .await
            .unwrap()
            .summary
            .state,
        AiDelegationReviewState::AwaitingApproval
    );
}

#[tokio::test]
async fn query_review_requires_original_text_and_matching_human_reference() {
    let (store, chat, mut request, mut content, instance) = fixture().await;
    enable(&store).await;
    let id = content.policy.rules[0].id;
    let sql = "SELECT private_query_sentinel";
    content.policy.rules[0].action = AiDelegationActionRule::QueryEdit {
        target: AiQueryExecutionTarget::PrivateEditor {
            desktop_id: content.desktop_id,
            item_id: 9,
        },
        before_sha256: format!("{:x}", Sha256::digest(sql.as_bytes())),
        base_revision: 7,
        replacement: AiDelegationTextRule::Bounded {
            max_utf8_bytes: 128,
        },
    };
    request.policy = content.policy.clone();
    request.material_references = vec![AiDelegationMaterialReference::PrivateQuery {
        rule_id: id,
        original_sql: sql.into(),
        base_revision: 7,
    }];
    assert!(
        store
            .create_ai_delegation_review(
                chat,
                PrincipalId(1),
                request.clone(),
                content.clone(),
                instance.clone()
            )
            .await
            .is_err(),
        "a hash is not review material"
    );
    content.materials = vec![AiDelegationReviewMaterial::Query {
        rule_id: id,
        original_sql: "changed".into(),
        base_revision: 7,
    }];
    assert!(store
        .create_ai_delegation_review(
            chat,
            PrincipalId(1),
            request.clone(),
            content.clone(),
            instance.clone()
        )
        .await
        .is_err());
    content.materials = vec![AiDelegationReviewMaterial::Query {
        rule_id: id,
        original_sql: sql.into(),
        base_revision: 7,
    }];
    let detail = store
        .create_ai_delegation_review(chat, PrincipalId(1), request, content, instance)
        .await
        .unwrap();
    assert!(!format!("{detail:?}").contains("private_query_sentinel"));
    assert_eq!(
        detail.summary.state,
        AiDelegationReviewState::AwaitingApproval
    );
}

fn turn(request: &CreateAiDelegationReview) -> StartAiTurnRequest {
    serde_json::from_value(serde_json::json!({"client_request_id":Uuid::new_v4(),"desktop_id":request.desktop_id,"prompt":"Use only the reviewed run policy","provider":"codex","mode":"execute","context":{"target":request.target,"dialect":"postgres","staged_change_count":0}})).unwrap()
}
async fn approve(
    store: &MetadataStore,
    id: Uuid,
    content: &AiDelegationReviewContent,
    instance: &AiDelegationAdmissionPolicy,
) -> PreparedAiDelegationReview {
    let detail = store
        .ai_delegation_review(id, PrincipalId(1))
        .await
        .unwrap();
    store
        .approve_ai_delegation_review(
            id,
            PrincipalId(1),
            ApproveAiDelegationReview {
                fingerprint: detail.summary.fingerprint,
                production_confirmation: Some(content.source.database_label.clone()),
            },
            instance.clone(),
        )
        .await
        .unwrap();
    store
        .prepare_ai_delegation_review(id, PrincipalId(1), instance.clone())
        .await
        .unwrap()
}

#[tokio::test]
async fn activation_rolls_back_with_run_and_source_and_replay_cannot_renew() {
    let (store, chat, request, content, instance) = fixture().await;
    enable(&store).await;
    let actor = PrincipalId(1);
    let review = store
        .create_ai_delegation_review(
            chat,
            actor,
            request.clone(),
            content.clone(),
            instance.clone(),
        )
        .await
        .unwrap();
    let prepared = approve(&store, review.summary.id, &content, &instance).await;
    let start = turn(&request);
    let before = store
        .ai_content_handles_offline(Some(TenantId(1)))
        .unwrap()
        .len();
    store.conn().unwrap().execute_batch("CREATE TRIGGER fail_grant BEFORE INSERT ON ai_run_delegation BEGIN SELECT RAISE(ABORT,'injected activation failure'); END;").unwrap();
    assert!(store
        .start_ai_run_with_delegation(
            chat,
            actor,
            start.clone(),
            content.source.clone(),
            prepared.clone()
        )
        .await
        .is_err());
    for table in [
        "ai_run",
        "ai_run_event",
        "ai_execute_run_source",
        "ai_run_delegation",
    ] {
        assert_eq!(
            store
                .conn()
                .unwrap()
                .query_row::<i64, _, _>(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
                .unwrap(),
            0,
            "activation failure rolls back {table}"
        );
    }
    assert_eq!(
        store
            .ai_delegation_review(review.summary.id, actor)
            .await
            .unwrap()
            .summary
            .state,
        AiDelegationReviewState::Approved
    );
    assert_eq!(
        store
            .ai_content_handles_offline(Some(TenantId(1)))
            .unwrap()
            .len(),
        before
    );
    store
        .conn()
        .unwrap()
        .execute_batch("DROP TRIGGER fail_grant;")
        .unwrap();
    let (a, b) = tokio::join!(
        store.start_ai_run_with_delegation(
            chat,
            actor,
            start.clone(),
            content.source.clone(),
            prepared.clone()
        ),
        store.start_ai_run_with_delegation(
            chat,
            actor,
            start.clone(),
            content.source.clone(),
            prepared.clone()
        )
    );
    let a = a.unwrap();
    let b = b.unwrap();
    assert_eq!((a.run.id, a.lease_token), (b.run.id, b.lease_token));
    let row = store
        .ai_delegation_review(review.summary.id, actor)
        .await
        .unwrap()
        .summary;
    assert_eq!(row.state, AiDelegationReviewState::Consumed);
    assert_eq!(row.used_run_id, Some(a.run.id));
    let expiry: String = store
        .conn()
        .unwrap()
        .query_row(
            "SELECT expires_at FROM ai_run_delegation WHERE run_id=?1",
            [a.run.id.to_string()],
            |r| r.get(0),
        )
        .unwrap();
    let expiry = super::super::parse_time_sql(expiry).unwrap();
    assert!(
        expiry <= request.expires_at
            && expiry
                <= a.run.started_at
                    + chrono::Duration::seconds(i64::from(content.policy.budget.max_duration_secs))
    );
    let replay = store
        .prepare_ai_delegation_review(review.summary.id, actor, instance)
        .await
        .unwrap();
    assert_eq!(
        store
            .start_ai_run_with_delegation(
                chat,
                actor,
                start.clone(),
                content.source.clone(),
                replay
            )
            .await
            .unwrap()
            .run
            .id,
        a.run.id
    );
    store
        .finish_ai_run(a.run.id, actor, a.lease_token, AiRunStatus::Completed)
        .await
        .unwrap();
    let mut different = start;
    different.client_request_id = Uuid::new_v4();
    assert!(
        store
            .start_ai_run_with_delegation(chat, actor, different, content.source, prepared)
            .await
            .is_err(),
        "a spent review cannot authorize a later run"
    );
    assert_eq!(
        store
            .conn()
            .unwrap()
            .query_row::<i64, _, _>("SELECT count(*) FROM ai_run", [], |r| r.get(0))
            .unwrap(),
        1
    );
}

#[tokio::test]
async fn activation_rechecks_target_source_and_policy_after_preparation() {
    let (store, chat, request, content, instance) = fixture().await;
    enable(&store).await;
    let actor = PrincipalId(1);
    let review = store
        .create_ai_delegation_review(
            chat,
            actor,
            request.clone(),
            content.clone(),
            instance.clone(),
        )
        .await
        .unwrap();
    let prepared = approve(&store, review.summary.id, &content, &instance).await;
    let start = turn(&request);
    let mut other = start.clone();
    other.context.target.room_id = Some(999);
    assert!(store
        .start_ai_run_with_delegation(chat, actor, other, content.source.clone(), prepared.clone())
        .await
        .is_err());
    let mut changed = content.source.clone();
    changed.generation = Uuid::new_v4();
    assert!(store
        .start_ai_run_with_delegation(chat, actor, start.clone(), changed, prepared.clone())
        .await
        .is_err());
    store
        .set_ai_delegation_tenant_policy(
            TenantId(1),
            actor,
            SetAiDelegationTenantPolicy {
                expected_revision: 1,
                enabled: false,
                arbitrary_sql_enabled: false,
            },
        )
        .await
        .unwrap();
    assert!(
        store
            .start_ai_run_with_delegation(chat, actor, start, content.source, prepared)
            .await
            .is_err(),
        "a decrypted snapshot cannot outlive revocation"
    );
    assert_eq!(
        store
            .conn()
            .unwrap()
            .query_row::<i64, _, _>("SELECT count(*) FROM ai_run", [], |r| r.get(0))
            .unwrap(),
        0
    );
}

#[tokio::test]
async fn call_reservations_are_once_only_bounded_and_survive_stop_as_observations() {
    let (store, chat, request, content, instance) = fixture().await;
    enable(&store).await;
    let actor = PrincipalId(1);
    let review = store
        .create_ai_delegation_review(
            chat,
            actor,
            request.clone(),
            content.clone(),
            instance.clone(),
        )
        .await
        .unwrap();
    let prepared = approve(&store, review.summary.id, &content, &instance).await;
    let lease = store
        .start_ai_run_with_delegation(chat, actor, turn(&request), content.source, prepared)
        .await
        .unwrap();
    let first = Uuid::new_v4();
    let make = |call_id| NewAiDelegationCall {
        run_id: lease.run.id,
        actor,
        lease_token: lease.lease_token,
        call_id,
        operation: "sift_execute_sql".into(),
        input: serde_json::json!({"sql":"a preparation that will be refused","params":[]}),
        reserved_output_bytes: 1024,
    };
    let (a, b) = tokio::join!(
        store.reserve_ai_delegation_call(make(first), instance.clone()),
        store.reserve_ai_delegation_call(make(first), instance.clone())
    );
    let (a, b) = (a.unwrap(), b.unwrap());
    assert_ne!(a.replayed, b.replayed);
    assert_eq!(a.expires_at, b.expires_at);
    let mut changed = make(first);
    changed.input["sql"] = serde_json::json!("changed request");
    assert!(store
        .reserve_ai_delegation_call(changed, instance.clone())
        .await
        .is_err());
    let mut changed = make(first);
    changed.reserved_output_bytes += 1;
    assert!(store
        .reserve_ai_delegation_call(changed, instance.clone())
        .await
        .is_err());
    let mut wrong_lease = make(first);
    wrong_lease.lease_token = Uuid::new_v4();
    assert!(store
        .reserve_ai_delegation_call(wrong_lease, instance.clone())
        .await
        .is_err());
    for _ in 0..2 {
        assert!(
            !store
                .reserve_ai_delegation_call(make(Uuid::new_v4()), instance.clone())
                .await
                .unwrap()
                .replayed
        );
    }
    assert!(store
        .reserve_ai_delegation_call(make(Uuid::new_v4()), instance.clone())
        .await
        .is_err());
    let counters: (u32, u64, u64) = store
        .conn()
        .unwrap()
        .query_row(
            "SELECT calls,input_bytes,output_bytes FROM ai_run_delegation WHERE run_id=?1",
            [lease.run.id.to_string()],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(
        counters,
        (
            3,
            3 * serde_json::to_vec(&(make(first).operation, make(first).input))
                .unwrap()
                .len() as u64,
            3072
        )
    );
    assert_eq!(
        store
            .conn()
            .unwrap()
            .query_row::<u32, _, _>("SELECT count(*) FROM ai_delegation_action", [], |r| r
                .get(0))
            .unwrap(),
        0,
        "refused preparation reserves quota without a native claim"
    );
    store
        .finish_ai_run(
            lease.run.id,
            actor,
            lease.lease_token,
            AiRunStatus::Completed,
        )
        .await
        .unwrap();
    let replay = store
        .reserve_ai_delegation_call(make(first), instance.clone())
        .await
        .unwrap();
    assert!(replay.replayed);
    assert_eq!(replay.expires_at, a.expires_at);
    assert!(store
        .reserve_ai_delegation_call(make(Uuid::new_v4()), instance)
        .await
        .is_err());
}

#[tokio::test]
async fn native_sql_claim_and_rule_debit_roll_back_together_and_replay_once() {
    let (store, chat, request, content, instance) = fixture().await;
    enable(&store).await;
    let actor = PrincipalId(1);
    let review = store
        .create_ai_delegation_review(
            chat,
            actor,
            request.clone(),
            content.clone(),
            instance.clone(),
        )
        .await
        .unwrap();
    let prepared = approve(&store, review.summary.id, &content, &instance).await;
    let lease = store
        .start_ai_run_with_delegation(
            chat,
            actor,
            turn(&request),
            content.source.clone(),
            prepared,
        )
        .await
        .unwrap();
    let sql = match &content.policy.rules[0].action {
        AiDelegationActionRule::ExactSql { sql, .. } => sql.clone(),
        _ => unreachable!(),
    };
    let call = Uuid::new_v4();
    store
        .reserve_ai_delegation_call(
            NewAiDelegationCall {
                run_id: lease.run.id,
                actor,
                lease_token: lease.lease_token,
                call_id: call,
                operation: "sift_execute_sql".into(),
                input: serde_json::json!({"sql":sql,"params":[]}),
                reserved_output_bytes: 1024,
            },
            instance.clone(),
        )
        .await
        .unwrap();
    let action = store
        .create_ai_sql_execution(
            lease.run.id,
            actor,
            RequestAiSqlExecution {
                call_id: call,
                lease_token: lease.lease_token,
                sql: sql.clone(),
                params: vec![],
            },
            AiSqlExecutionInput {
                sql,
                params: vec![],
                source: content.source,
            },
            Utc::now() + chrono::Duration::minutes(2),
            3,
        )
        .await
        .unwrap();
    store.conn().unwrap().execute_batch("CREATE TRIGGER fail_delegated_link BEFORE INSERT ON ai_delegation_action BEGIN SELECT RAISE(ABORT,'injected native link failure'); END;").unwrap();
    assert!(store
        .claim_delegated_ai_sql_execution(action.id, actor, lease.lease_token, instance.clone())
        .await
        .is_err());
    let unchanged = store
        .ai_sql_execution(action.id, actor)
        .await
        .unwrap()
        .summary;
    assert_eq!(unchanged.state, AiExecutionState::AwaitingApproval);
    assert_eq!(unchanged.approved_by, None);
    for table in ["ai_delegation_action", "ai_delegation_rule_usage"] {
        assert_eq!(
            store
                .conn()
                .unwrap()
                .query_row::<u32, _, _>(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
                .unwrap(),
            0
        );
    }
    assert_eq!(
        store
            .conn()
            .unwrap()
            .query_row::<u32, _, _>(
                "SELECT calls FROM ai_run_delegation WHERE run_id=?1",
                [lease.run.id.to_string()],
                |r| r.get(0)
            )
            .unwrap(),
        1,
        "failed native claim keeps the earlier request reservation"
    );
    store
        .conn()
        .unwrap()
        .execute_batch("DROP TRIGGER fail_delegated_link;")
        .unwrap();
    let (a, b) = tokio::join!(
        store.claim_delegated_ai_sql_execution(
            action.id,
            actor,
            lease.lease_token,
            instance.clone()
        ),
        store.claim_delegated_ai_sql_execution(
            action.id,
            actor,
            lease.lease_token,
            instance.clone()
        )
    );
    let (a, b) = (a.unwrap(), b.unwrap());
    assert_ne!(a.claimed, b.claimed);
    assert_eq!(a.expires_at, b.expires_at);
    assert!(
        matches!(a.approval,AiExecutionApprovalProvenance::RunDelegation {approved_by:1,review_id,run_id,rule_id,..} if review_id==review.summary.id && run_id==lease.run.id && rule_id==content.policy.rules[0].id)
    );
    assert_eq!(
        store
            .ai_sql_execution(action.id, actor)
            .await
            .unwrap()
            .summary
            .state,
        AiExecutionState::Applying
    );
    assert_eq!(
        store
            .conn()
            .unwrap()
            .query_row::<u32, _, _>("SELECT uses FROM ai_delegation_rule_usage", [], |r| r
                .get(0))
            .unwrap(),
        1
    );
    assert_eq!(
        store
            .conn()
            .unwrap()
            .query_row::<u32, _, _>("SELECT calls FROM ai_run_delegation", [], |r| r.get(0))
            .unwrap(),
        1,
        "native admission never charges a second call"
    );
}

#[tokio::test]
async fn delegated_query_claim_issues_original_editor_capability_only_once() {
    let (store, chat, mut request, mut content, instance) = fixture().await;
    enable(&store).await;
    let actor = PrincipalId(1);
    let profile=store.upsert_connection_profile(TenantId(1),actor,crate::NewConnectionProfile {name:"query source".into(),provider_id:Engine::Postgres.provider_id(),configuration:serde_json::json!({"host":"fixture.invalid","database":"query","user":"reader","engine_specific":{"engine":"postgres"}}),semantic_engine:Some(Engine::Postgres),credentials:None,credential_mode:crate::CredentialMode::Shared,tags:vec![]}).await.unwrap();
    content.source.profile_id = profile.id.0;
    content.source.profile_digest = store
        .ai_profile_source_digest(profile.id, actor)
        .await
        .unwrap();
    content.target.profile_id = Some(profile.id.0);
    request.target = content.target.clone();
    let sql = "SELECT 1";
    let before = Sha256::digest(sql.as_bytes());
    let revision = u64::from_le_bytes(before[..8].try_into().unwrap()) & i64::MAX as u64;
    let rule = content.policy.rules[0].id;
    content.policy.rules[0].action = AiDelegationActionRule::QueryEdit {
        target: AiQueryExecutionTarget::PrivateEditor {
            desktop_id: content.desktop_id,
            item_id: 9,
        },
        before_sha256: format!("{before:x}"),
        base_revision: revision,
        replacement: AiDelegationTextRule::Bounded {
            max_utf8_bytes: 128,
        },
    };
    content.materials = vec![AiDelegationReviewMaterial::Query {
        rule_id: rule,
        original_sql: sql.into(),
        base_revision: revision,
    }];
    request.policy = content.policy.clone();
    request.material_references = vec![AiDelegationMaterialReference::PrivateQuery {
        rule_id: rule,
        original_sql: sql.into(),
        base_revision: revision,
    }];
    let review = store
        .create_ai_delegation_review(
            chat,
            actor,
            request.clone(),
            content.clone(),
            instance.clone(),
        )
        .await
        .unwrap();
    let prepared = approve(&store, review.summary.id, &content, &instance).await;
    let mut start = serde_json::to_value(turn(&request)).unwrap();
    start["context"]["editor_item_id"] = serde_json::json!(9);
    start["context"]["sql"] = serde_json::json!({"text":sql,"document_revision":revision});
    let lease = store
        .start_ai_run_with_delegation(
            chat,
            actor,
            serde_json::from_value(start).unwrap(),
            content.source,
            prepared,
        )
        .await
        .unwrap();
    let proposal = store
        .stage_ai_query_proposal(
            lease.run.id,
            actor,
            StageAiQueryProposalRequest {
                client_request_id: Uuid::new_v4(),
                lease_token: lease.lease_token,
                target: request.target,
                base_revision: revision,
                proposed_sql: "SELECT 2".into(),
            },
        )
        .await
        .unwrap();
    let call = Uuid::new_v4();
    store
        .reserve_ai_delegation_call(
            NewAiDelegationCall {
                run_id: lease.run.id,
                actor,
                lease_token: lease.lease_token,
                call_id: call,
                operation: "sift_execute_query".into(),
                input: serde_json::json!({"proposal_id":proposal.proposal.id}),
                reserved_output_bytes: 1024,
            },
            instance.clone(),
        )
        .await
        .unwrap();
    let action = store
        .create_ai_query_execution(
            lease.run.id,
            actor,
            RequestAiQueryExecution {
                call_id: call,
                lease_token: lease.lease_token,
                proposal_id: proposal.proposal.id,
            },
            Utc::now() + chrono::Duration::minutes(2),
            100,
        )
        .await
        .unwrap();
    let (a, b) = tokio::join!(
        store.claim_delegated_ai_query_execution(
            action.id,
            actor,
            lease.lease_token,
            instance.clone()
        ),
        store.claim_delegated_ai_query_execution(action.id, actor, lease.lease_token, instance)
    );
    let (a, b) = (a.unwrap(), b.unwrap());
    assert_ne!(a.0.claimed, b.0.claimed);
    assert_eq!(a.1.is_some(), a.0.claimed);
    assert_eq!(b.1.is_some(), b.0.claimed);
    let dispatch = a.1.or(b.1).unwrap();
    assert_eq!(dispatch.desktop_id, request.desktop_id);
    let digest: String = store
        .conn()
        .unwrap()
        .query_row(
            "SELECT dispatch_digest FROM ai_query_execution WHERE id=?1",
            [action.id.to_string()],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        digest,
        format!("{:x}", Sha256::digest(dispatch.token.as_bytes()))
    );
    assert_eq!(
        store
            .conn()
            .unwrap()
            .query_row::<u32, _, _>("SELECT uses FROM ai_delegation_rule_usage", [], |r| r
                .get(0))
            .unwrap(),
        1
    );
    assert!(
        store
            .ai_query_execution(action.id, actor)
            .await
            .unwrap()
            .receipt
            .is_none(),
        "claiming never fabricates an applied editor receipt"
    );
}

#[tokio::test]
async fn delegated_extension_links_rpc_approval_and_usage_atomically() {
    let (store, chat, mut request, mut content, instance) = fixture().await;
    enable(&store).await;
    let actor = PrincipalId(1);
    let operation = ExtensionOperation {
        extension_id: ExtensionId::new("acme/conformance").unwrap(),
        contribution_id: ContributionId::new("acme/conformance/governed_tool/inspect").unwrap(),
        action: SegmentId::new("run").unwrap(),
        classification: OperationClassification::Read,
        target_kind: SegmentId::new("connection").unwrap(),
        target_id: Some("2:3".into()),
        sanitized_arguments: Default::default(),
    };
    let tool = GovernedToolDescriptor {
        id: "fixture.inspect".into(),
        title: "Inspect".into(),
        description: "fixture".into(),
        operation: operation.clone(),
        input_schema: serde_json::json!({"type":"object"}),
        output_schema: serde_json::json!({"type":"object"}),
        required_context: vec![],
        mcp_exposable: true,
        schedulable: false,
        interactive: true,
    };
    let extension = AiExtensionExecutionReview {
        tool,
        context: request.target.clone(),
        arguments: serde_json::json!({}),
        prepared_contract: serde_json::json!({"operation":operation,"contract":{"manifest":"pinned","grant_revision":1}}),
        source: content.source.clone(),
    };
    let rule = content.policy.rules[0].id;
    content.policy.budget.max_duration_secs = 30;
    content.policy.rules[0].action = AiDelegationActionRule::Extension {
        contract_sha256: sift_core::ai_delegation::extension_contract_digest(&extension).unwrap(),
        arguments: AiDelegationArgumentRule::Object {
            fields: Default::default(),
            required: vec![],
        },
        max_argument_bytes: 128,
    };
    content.materials = vec![AiDelegationReviewMaterial::ExtensionTool {
        rule_id: rule,
        tool: Box::new(extension.tool.clone()),
        context: extension.context.clone(),
        prepared_contract: extension.prepared_contract.clone(),
    }];
    request.policy = content.policy.clone();
    request.material_references = vec![AiDelegationMaterialReference::ExtensionTool {
        rule_id: rule,
        tool_id: extension.tool.id.clone(),
    }];
    let review = store
        .create_ai_delegation_review(
            chat,
            actor,
            request.clone(),
            content.clone(),
            instance.clone(),
        )
        .await
        .unwrap();
    let prepared = approve(&store, review.summary.id, &content, &instance).await;
    let lease = store
        .start_ai_run_with_delegation(chat, actor, turn(&request), content.source, prepared)
        .await
        .unwrap();
    let call = Uuid::new_v4();
    store.reserve_ai_delegation_call(NewAiDelegationCall {run_id:lease.run.id,actor,lease_token:lease.lease_token,call_id:call,operation:"sift_execute_extension".into(),input:serde_json::json!({"tool_id":extension.tool.id,"arguments":extension.arguments}),reserved_output_bytes:1024},instance.clone()).await.unwrap();
    let action = store
        .create_ai_extension_execution(
            lease.run.id,
            actor,
            RequestAiExtensionExecution {
                call_id: call,
                lease_token: lease.lease_token,
                tool_id: extension.tool.id.clone(),
                arguments: extension.arguments.clone(),
            },
            extension.clone(),
            Utc::now() + chrono::Duration::minutes(2),
            100,
        )
        .await
        .unwrap();
    let input = crate::NewExtensionExecution {
        request_id: action.id,
        binding: crate::ApprovalBinding {
            principal_id: actor,
            operation_id: extension.tool.id.clone(),
            context_fingerprint: fingerprint(
                &serde_json::json!({"tool":extension.tool,"action":extension.prepared_contract}),
            )
            .unwrap(),
            input_fingerprint: fingerprint(&extension.arguments).unwrap(),
        },
        approval_id: None,
        operation,
        context: extension.context,
        deadline_unix_ms: (Utc::now() + chrono::Duration::minutes(1)).timestamp_millis(),
        correlation_id: "delegated-extension-test".into(),
    };
    store.conn().unwrap().execute_batch("CREATE TRIGGER fail_delegated_rpc_link BEFORE INSERT ON ai_delegation_action BEGIN SELECT RAISE(ABORT,'injected RPC link failure'); END;").unwrap();
    assert!(store
        .claim_delegated_ai_extension_execution(
            action.id,
            actor,
            lease.lease_token,
            input.clone(),
            instance.clone()
        )
        .await
        .is_err());
    for table in [
        "operation_approval",
        "extension_execution",
        "ai_delegation_action",
        "ai_delegation_rule_usage",
    ] {
        assert_eq!(
            store
                .conn()
                .unwrap()
                .query_row::<u32, _, _>(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
                .unwrap(),
            0,
            "failed link rolls back {table}"
        );
    }
    store
        .conn()
        .unwrap()
        .execute_batch("DROP TRIGGER fail_delegated_rpc_link;")
        .unwrap();
    let (proof, native) = store
        .claim_delegated_ai_extension_execution(
            action.id,
            actor,
            lease.lease_token,
            input.clone(),
            instance.clone(),
        )
        .await
        .unwrap();
    assert!(proof.claimed);
    let native = native.unwrap();
    assert!(native.execution.claimed);
    let deadline: i64 = store
        .conn()
        .unwrap()
        .query_row(
            "SELECT deadline_unix_ms FROM extension_execution WHERE id=?1",
            [native.execution.receipt.id.to_string()],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(deadline, proof.expires_at.timestamp_millis());
    let (replay, native) = store
        .claim_delegated_ai_extension_execution(
            action.id,
            actor,
            lease.lease_token,
            input,
            instance,
        )
        .await
        .unwrap();
    assert!(!replay.claimed);
    assert!(native.is_none(), "replay cannot issue another RPC dispatch");
}

#[tokio::test]
async fn delegated_rows_claim_native_proposal_and_rule_usage_together() {
    let (store, chat, mut request, mut content, instance) = fixture().await;
    enable(&store).await;
    let actor = PrincipalId(1);
    let profile = store
        .upsert_connection_profile(
            TenantId(1),
            actor,
            crate::NewConnectionProfile {
                name: "typed source".into(),
                provider_id: Engine::Postgres.provider_id(),
                configuration: serde_json::json!({"database":"typed"}),
                semantic_engine: Some(Engine::Postgres),
                credentials: None,
                credential_mode: crate::CredentialMode::Shared,
                tags: vec![],
            },
        )
        .await
        .unwrap();
    content.source.profile_id = profile.id.0;
    content.source.profile_digest = store
        .ai_profile_source_digest(profile.id, actor)
        .await
        .unwrap();
    content.target.profile_id = Some(profile.id.0);
    request.target = content.target.clone();
    let mut table = ObjectPath::new("items");
    table.schema = Some("public".into());
    let edits = EditSet {
        table: table.clone(),
        edits: vec![RowEdit::Insert {
            values: vec![CellEdit {
                column: "label".into(),
                value: Value::Text("reviewed typed value".into()),
            }],
        }],
    };
    content.policy.rules[0].action = AiDelegationActionRule::ExactRowEdits {
        edit_set: edits.clone(),
    };
    request.policy = content.policy.clone();
    let review = store
        .create_ai_delegation_review(
            chat,
            actor,
            request.clone(),
            content.clone(),
            instance.clone(),
        )
        .await
        .unwrap();
    let prepared = approve(&store, review.summary.id, &content, &instance).await;
    let lease = store
        .start_ai_run_with_delegation(
            chat,
            actor,
            turn(&request),
            content.source.clone(),
            prepared,
        )
        .await
        .unwrap();
    let proposal = store
        .stage_ai_database_proposal(
            lease.run.id,
            actor,
            StageAiDatabaseProposalRequest {
                client_request_id: Uuid::new_v4(),
                lease_token: lease.lease_token,
                draft: AiDatabaseDraft::RowEditSet {
                    edit_set: edits,
                    expected_catalog_revision: CatalogRevision(1),
                },
            },
            content.source.profile_digest.clone(),
            "db:typed".into(),
            None,
        )
        .await
        .unwrap();
    let preview = store
        .save_ai_database_review(
            actor,
            Uuid::new_v4(),
            AiDatabaseProposalReview {
                id: Uuid::new_v4(),
                proposal_id: proposal.proposal.id,
                reviewer_id: 1,
                session: content.source.session,
                connection: content.source.connection,
                source_digest: content.source.profile_digest.clone(),
                content_sha256: proposal.proposal.content_sha256.clone(),
                review_digest: String::new(),
                database_label: content.source.database_label,
                production: true,
                preview: AiDatabasePreview::RowEditSet {
                    plan: EditPlan {
                        table,
                        identity: IdentitySource::PrimaryKey {
                            columns: vec!["id".into()],
                        },
                        statements: vec![],
                    },
                },
                expires_at: Utc::now() + chrono::Duration::minutes(3),
            },
        )
        .await
        .unwrap();
    let call = Uuid::new_v4();
    store
        .reserve_ai_delegation_call(
            NewAiDelegationCall {
                run_id: lease.run.id,
                actor,
                lease_token: lease.lease_token,
                call_id: call,
                operation: "sift_execute_database".into(),
                input: serde_json::json!({"proposal_id":proposal.proposal.id}),
                reserved_output_bytes: 1024,
            },
            instance.clone(),
        )
        .await
        .unwrap();
    let action = store
        .create_ai_database_execution(
            lease.run.id,
            actor,
            RequestAiDatabaseExecution {
                call_id: call,
                lease_token: lease.lease_token,
                proposal_id: proposal.proposal.id,
            },
            preview,
            Utc::now() + chrono::Duration::minutes(2),
            100,
        )
        .await
        .unwrap();
    store.conn().unwrap().execute_batch("CREATE TRIGGER fail_delegated_database_link BEFORE INSERT ON ai_delegation_action BEGIN SELECT RAISE(ABORT,'injected database link failure'); END;").unwrap();
    assert!(store
        .claim_delegated_ai_database_execution(
            action.id,
            actor,
            lease.lease_token,
            AiDelegationDatabaseMaterial::RowEdits,
            instance.clone()
        )
        .await
        .is_err());
    assert_eq!(
        store
            .ai_database_execution(action.id, actor)
            .await
            .unwrap()
            .state,
        AiExecutionState::AwaitingApproval
    );
    for table in [
        "ai_proposal_apply",
        "ai_delegation_action",
        "ai_delegation_rule_usage",
    ] {
        assert_eq!(
            store
                .conn()
                .unwrap()
                .query_row::<u32, _, _>(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
                .unwrap(),
            0
        );
    }
    store
        .conn()
        .unwrap()
        .execute_batch("DROP TRIGGER fail_delegated_database_link;")
        .unwrap();
    let (a, b) = tokio::join!(
        store.claim_delegated_ai_database_execution(
            action.id,
            actor,
            lease.lease_token,
            AiDelegationDatabaseMaterial::RowEdits,
            instance.clone()
        ),
        store.claim_delegated_ai_database_execution(
            action.id,
            actor,
            lease.lease_token,
            AiDelegationDatabaseMaterial::RowEdits,
            instance
        )
    );
    assert_ne!(a.unwrap().claimed, b.unwrap().claimed);
    let (uses, rows): (u32, u32) = store
        .conn()
        .unwrap()
        .query_row(
            "SELECT uses,submitted_rows FROM ai_delegation_rule_usage",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!((uses, rows), (1, 1));
    assert_eq!(
        store
            .ai_database_execution(action.id, actor)
            .await
            .unwrap()
            .state,
        AiExecutionState::Applying
    );
}
