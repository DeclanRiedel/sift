//! One durable observation writer around the existing migration execution path.
use super::*;
use native_transaction::{Action, Observation};
use sift_protocol::{
    ChangeLedgerOutcome as Outcome, MigrationNativeGroup as Group, MigrationNativePhase as Phase,
    MigrationNativeStatement as Statement,
};

#[derive(Clone)]
pub(super) struct Journal {
    store: MetadataStore,
    tenant: sift_metadata::TenantId,
    actor: PrincipalId,
    run: sift_protocol::MigrationRunId,
    pub(super) entry: ConnectionEntryClone,
}

impl Journal {
    pub(super) async fn open(
        sessions: &SessionStore,
        stored: &StoredMigrationPlan,
        entry: ConnectionEntryClone,
    ) -> ApiResult<Self> {
        let (tenant, profile, actor, session, connection) = (
            stored.tenant,
            stored.profile,
            stored.principal,
            stored.session,
            stored.connection,
        );
        let plan = &stored.plan;
        let store = sessions
            .inner
            .authorization_store
            .read()
            .unwrap()
            .clone()
            .ok_or(ApiError::MetadataUnavailable)?;
        let provider = entry.driver.provider().provider_id.clone();
        let source = sift_protocol::MigrationNativeSource {
            profile_id: profile.0,
            provider_id: provider.clone(),
            session,
            connection,
            generation: entry.execution.review_id,
            configuration_sha256: format!(
                "{:x}",
                Sha256::digest(
                    serde_json::to_vec(&entry.configuration)
                        .map_err(|_| ApiError::Internal("cannot freeze migration source".into()))?
                )
            ),
            database_target: crate::http::configured_database_target(
                &provider,
                &entry.configuration,
            ),
        };
        let journal = Self {
            store,
            tenant,
            actor,
            run: plan.run_id,
            entry,
        };
        let plan = plan.clone();
        journal
            .write(move |store, tenant, actor, _| {
                store.open_migration_native_journal(tenant, profile, actor, &plan, &source)
            })
            .await?;
        Ok(journal)
    }

    async fn write<F>(&self, work: F) -> ApiResult<()>
    where
        F: FnOnce(
                MetadataStore,
                sift_metadata::TenantId,
                PrincipalId,
                sift_protocol::MigrationRunId,
            ) -> sift_metadata::Result<u64>
            + Send
            + 'static,
    {
        let (store, tenant, actor, run) = (self.store.clone(), self.tenant, self.actor, self.run);
        tokio::task::spawn_blocking(move || work(store, tenant, actor, run))
            .await
            .map_err(|_| ApiError::Internal("migration native journal writer failed".into()))??;
        Ok(())
    }

    pub(super) async fn group(
        &self,
        group: &sift_protocol::MigrationGroup,
    ) -> ApiResult<Arc<NativeGroup>> {
        let ordinal = group.ordinal;
        self.write(move |store, tenant, actor, run| {
            store.begin_migration_native_group(tenant, actor, run, ordinal)
        })
        .await?;
        Ok(Arc::new(NativeGroup {
            journal: self.clone(),
            value: tokio::sync::Mutex::new(Group {
                ordinal,
                transactional: group.transactional,
                transaction_id: None,
                phase: if group.transactional {
                    Phase::Beginning
                } else {
                    Phase::Executing
                },
                commit_intent: false,
                rollback_intent: false,
                outcome: None,
            }),
        }))
    }
    pub(super) async fn close(&self) -> ApiResult<()> {
        self.write(|store, tenant, actor, run| {
            store.close_migration_native_journal(tenant, actor, run)
        })
        .await
    }
}

pub(super) struct NativeGroup {
    journal: Journal,
    value: tokio::sync::Mutex<Group>,
}
impl NativeGroup {
    async fn transition(&self, change: impl FnOnce(&mut Group)) -> ApiResult<()> {
        let mut current = self.value.lock().await;
        let mut next = current.clone();
        change(&mut next);
        let stored = next.clone();
        self.journal
            .write(move |store, tenant, actor, run| {
                store.record_migration_native_group(tenant, actor, run, &stored)
            })
            .await?;
        *current = next;
        Ok(())
    }
    async fn began(&self, tx: TxId) -> ApiResult<()> {
        if self.value.lock().await.transaction_id == Some(tx) {
            return Ok(());
        }
        self.transition(|value| {
            value.transaction_id = Some(tx);
            value.phase = Phase::Executing;
        })
        .await
    }
    async fn intent(&self, action: Action) -> ApiResult<()> {
        self.transition(|value| match action {
            Action::Commit => {
                value.commit_intent = true;
                value.phase = Phase::Committing;
            }
            Action::Rollback => {
                value.rollback_intent = true;
                value.phase = Phase::RollingBack;
            }
            Action::Begin => unreachable!(),
        })
        .await
    }
    async fn settled(&self, outcome: Option<Outcome>) -> ApiResult<()> {
        self.transition(|value| {
            value.phase = Phase::Settled;
            value.outcome = outcome;
        })
        .await
    }

    pub(super) async fn begin(
        self: &Arc<Self>,
        store: &SessionStore,
        session: SessionId,
        request: BeginTransactionRequest,
    ) -> ApiResult<TransactionInfo> {
        let group = self.clone();
        let cleanup: native_transaction::CleanupIntent = Arc::new(move |tx| {
            let group = group.clone();
            Box::pin(async move {
                group.began(tx).await?;
                group.intent(Action::Rollback).await
            })
        });
        let observation = Observation::structured(Action::Begin, Some(cleanup));
        let connection = request.connection;
        let result = native_transaction::scope(
            observation.clone(),
            store.begin_transaction_source(
                session,
                request,
                sift_protocol::OperationKind::ApplyMigration,
                Some(self.journal.entry.clone()),
            ),
        )
        .await;
        let evidence = observation.terminal().await;
        if let Some(info) = evidence.published {
            if let Err(error) = self.began(info.tx_id).await {
                self.rollback(store, session, connection, info.tx_id).await;
                return Err(error);
            }
            return Ok(info);
        }
        let outcome = if !evidence.admitted {
            None
        } else if evidence.cleanup_rollback == Some(true)
            && evidence.transaction_state_known == Some(true)
            && self.value.lock().await.phase == Phase::RollingBack
        {
            Some(Outcome::RolledBack)
        } else {
            Some(Outcome::OutcomeUnknown)
        };
        self.settled(outcome).await?;
        match result {
            Err(error) => Err(error),
            Ok(_) => {
                self.journal.entry.execution.fence();
                Err(ApiError::Driver(DriverError::new(
                    Code::ConnectionInvalidated,
                    "migration BEGIN lacks native publication evidence",
                )))
            }
        }
    }

    pub(super) async fn rollback(
        &self,
        store: &SessionStore,
        session: SessionId,
        connection: ConnectionId,
        tx_id: TxId,
    ) -> bool {
        // Failure to persist cannot suppress required native cleanup. It also
        // cannot be promoted into a durable known rollback witness.
        let intent = self.intent(Action::Rollback).await.is_ok();
        let observation = Observation::structured(Action::Rollback, None);
        let _ = native_transaction::scope(
            observation.clone(),
            store.rollback_transaction_as(
                session,
                EndTransactionRequest { connection, tx_id },
                sift_protocol::OperationKind::ApplyMigration,
            ),
        )
        .await;
        let evidence = observation.terminal().await;
        let known =
            evidence.acknowledged == Some(true) && evidence.transaction_state_known == Some(true);
        let persisted = self
            .settled(Some(if intent && known {
                Outcome::RolledBack
            } else {
                Outcome::OutcomeUnknown
            }))
            .await
            .is_ok();
        if !known || !persisted || !intent {
            self.journal.entry.execution.fence();
        }
        intent && known && persisted
    }

    pub(super) async fn commit(
        &self,
        store: &SessionStore,
        session: SessionId,
        connection: ConnectionId,
        tx_id: TxId,
    ) -> ApiResult<()> {
        self.intent(Action::Commit).await?;
        let observation = Observation::structured(Action::Commit, None);
        let result = native_transaction::scope(
            observation.clone(),
            store.commit_transaction_as(
                session,
                EndTransactionRequest { connection, tx_id },
                sift_protocol::OperationKind::ApplyMigration,
            ),
        )
        .await;
        let evidence = observation.terminal().await;
        if evidence.acknowledged == Some(true) && evidence.transaction_state_known == Some(true) {
            if let Err(error) = self.settled(Some(Outcome::Committed)).await {
                // Never issue rollback or new work after an acknowledged COMMIT
                // merely because terminal metadata persistence failed.
                self.journal.entry.execution.fence();
                return Err(error);
            }
            Ok(())
        } else {
            match result {
                Err(error) => Err(error),
                Ok(()) => {
                    self.journal.entry.execution.fence();
                    Err(ApiError::Driver(DriverError::new(
                        Code::ConnectionInvalidated,
                        "migration COMMIT lacks known native acknowledgement",
                    )))
                }
            }
        }
    }

    pub(super) async fn prepare(
        &self,
        statement: &sift_protocol::MigrationStatement,
    ) -> ApiResult<()> {
        let group = self.value.lock().await.ordinal;
        let ordinal = statement.ordinal;
        self.journal
            .write(move |store, tenant, actor, run| {
                store.prepare_migration_native_statement(tenant, actor, run, group, ordinal)
            })
            .await
    }
    pub(super) async fn record(
        &self,
        statement: &sift_protocol::MigrationStatement,
        evidence: native_execution::NativeObservation,
    ) -> ApiResult<()> {
        let group = self.value.lock().await.clone();
        let value = Statement {
            ordinal: statement.ordinal,
            fingerprint: statement.fingerprint.clone(),
            intent_recorded: true,
            native_admitted: Some(evidence.attempted),
            native_completed: evidence.attempted.then_some(evidence.completed),
            autocommit_known: !group.transactional && evidence.autocommit_observed == Some(true),
            affected_rows: evidence.completed.then_some(evidence.rows).flatten(),
            result_code: evidence.stable_code,
        };
        self.journal
            .write(move |store, tenant, actor, run| {
                store.record_migration_native_statement(tenant, actor, run, group.ordinal, &value)
            })
            .await
    }
    pub(super) async fn finish_nontransactional(&self) -> ApiResult<()> {
        self.settled(None).await
    }
}

#[derive(Clone)]
pub(crate) struct LedgerWitness {
    pub operation: sift_protocol::ChangeLedgerOperation,
    pub outcome: Outcome,
    pub transaction_id: Option<TxId>,
    pub rows: Option<i64>,
    pub fingerprint: String,
    pub result_code: Option<String>,
}
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Settlement {
    Unobserved,
    Committed,
    RolledBack,
    NotDispatched,
    Unknown,
}

pub(crate) struct LedgerProjection {
    pub settlement: Settlement,
    pub source: Option<sift_protocol::MigrationNativeSource>,
    pub entries: Vec<LedgerWitness>,
}

/// Both owners consume the same immutable native journal, never workflow status.
pub(crate) async fn ledger(
    store: MetadataStore,
    tenant: sift_metadata::TenantId,
    run: sift_protocol::MigrationRunId,
) -> ApiResult<LedgerProjection> {
    let mut request = sift_protocol::MigrationNativePageRequest {
        limit: 100,
        ..Default::default()
    };
    let mut source = None;
    let mut groups = std::collections::BTreeMap::<u32, (Group, Vec<Statement>)>::new();
    let digest;
    loop {
        let store = store.clone();
        let page_request = request.clone();
        let page = tokio::task::spawn_blocking(move || {
            store.migration_native_outcomes(tenant, run, &page_request)
        })
        .await
        .map_err(|_| ApiError::Internal("migration ledger observation reader failed".into()))?;
        let page = match page {
            Err(sift_metadata::MetadataError::MigrationRunNotFound) if request.offset == 0 => {
                return Ok(LedgerProjection {
                    settlement: Settlement::Unobserved,
                    source: None,
                    entries: vec![],
                })
            }
            page => page?,
        };
        if request.offset == 0 {
            source = page.source;
        }
        for observation in page.observations {
            let group = groups
                .entry(observation.group.ordinal)
                .or_insert_with(|| (observation.group, Vec::new()));
            if let Some(statement) = observation.statement {
                group.1.push(statement);
            }
        }
        match page.next_offset {
            Some(offset) => {
                request.offset = offset;
                request.expected_revision = Some(page.revision);
            }
            None => {
                digest = page.plan_digest;
                break;
            }
        }
    }
    let settlement = summarize(source.is_some(), &groups);
    let mut entries = Vec::new();
    for (group, statements) in groups.into_values() {
        entries.extend(project(&digest, &group, &statements));
    }
    Ok(LedgerProjection {
        source,
        entries,
        settlement,
    })
}

fn summarize(
    present: bool,
    groups: &std::collections::BTreeMap<u32, (Group, Vec<Statement>)>,
) -> Settlement {
    if !present || groups.is_empty() {
        return Settlement::Unobserved;
    }
    let committed = groups.values().all(|(group, statements)| {
        group.phase == Phase::Settled
            && (!group.transactional || group.outcome == Some(Outcome::Committed))
            && statements.iter().all(|statement| {
                statement.native_admitted == Some(true)
                    && statement.native_completed == Some(true)
                    && (group.transactional || statement.autocommit_known)
            })
    });
    if committed {
        return Settlement::Committed;
    }
    if groups
        .values()
        .any(|(group, _)| group.outcome == Some(Outcome::OutcomeUnknown))
    {
        return Settlement::Unknown;
    }
    let possible = |statement: &Statement| {
        statement.intent_recorded && statement.native_admitted != Some(false)
    };
    let rolled_back = groups
        .values()
        .any(|(group, _)| group.outcome == Some(Outcome::RolledBack))
        && groups.values().all(|(group, statements)| {
            group.outcome == Some(Outcome::RolledBack) || !statements.iter().any(possible)
        });
    if rolled_back {
        return Settlement::RolledBack;
    }
    if groups
        .values()
        .all(|(_, statements)| !statements.iter().any(possible))
    {
        return Settlement::NotDispatched;
    }
    Settlement::Unknown
}

fn project(digest: &str, group: &Group, statements: &[Statement]) -> Vec<LedgerWitness> {
    use sift_protocol::ChangeLedgerOperation as Operation;
    let attempted = statements
        .iter()
        .filter(|statement| statement.native_admitted == Some(true))
        .collect::<Vec<_>>();
    if attempted.is_empty() {
        return vec![];
    }
    if !group.transactional {
        return attempted
            .into_iter()
            .map(|statement| LedgerWitness {
                operation: Operation::MigrationApply,
                outcome: if statement.native_completed == Some(true) && statement.autocommit_known {
                    Outcome::Committed
                } else {
                    Outcome::OutcomeUnknown
                },
                transaction_id: None,
                rows: statement
                    .affected_rows
                    .and_then(|count| i64::try_from(count).ok()),
                fingerprint: statement.fingerprint.clone(),
                result_code: statement.result_code.clone(),
            })
            .collect();
    }
    let outcome = group.outcome.unwrap_or(Outcome::OutcomeUnknown);
    let entry = LedgerWitness {
        operation: Operation::MigrationApply,
        outcome,
        transaction_id: group.transaction_id,
        rows: attempted.iter().try_fold(0i64, |sum, statement| {
            sum.checked_add(i64::try_from(statement.affected_rows?).ok()?)
        }),
        fingerprint: digest.to_owned(),
        result_code: attempted
            .iter()
            .find_map(|statement| statement.result_code.clone()),
    };
    let rollback = (outcome == Outcome::RolledBack).then(|| LedgerWitness {
        operation: Operation::MigrationRollback,
        ..entry.clone()
    });
    std::iter::once(entry).chain(rollback).collect()
}

#[cfg(test)]
#[path = "migration_native_acceptance.rs"]
mod native_acceptance;

#[cfg(test)]
mod tests {
    use super::*;
    fn statement(ordinal: u32, rows: Option<u64>) -> Statement {
        Statement {
            ordinal,
            fingerprint: format!("sqlfp:{ordinal:064x}"),
            intent_recorded: true,
            native_admitted: Some(true),
            native_completed: Some(true),
            autocommit_known: false,
            affected_rows: rows,
            result_code: None,
        }
    }
    fn group(transactional: bool) -> Group {
        Group {
            ordinal: 1,
            transactional,
            transaction_id: transactional.then_some(TxId(41)),
            phase: Phase::Settled,
            commit_intent: transactional,
            rollback_intent: false,
            outcome: transactional.then_some(Outcome::Committed),
        }
    }
    #[test]
    fn group_ledger_counts_and_outcomes_require_actual_native_witnesses() {
        let mut group = group(true);
        for (rows, expected) in [
            ([Some(0), Some(0)], Some(0)),
            ([Some(i64::MAX as u64), Some(1)], None),
            ([Some(1), None], None),
            ([Some(u64::MAX), Some(0)], None),
        ] {
            let entries = project(
                "plan",
                &group,
                &[statement(1, rows[0]), statement(2, rows[1])],
            );
            assert_eq!(entries.len(), 1);
            assert_eq!(entries[0].rows, expected);
            assert_eq!(entries[0].transaction_id, Some(TxId(41)));
            assert_eq!(entries[0].outcome, Outcome::Committed);
        }
        group.outcome = Some(Outcome::RolledBack);
        group.rollback_intent = true;
        let entries = project("plan", &group, &[statement(1, Some(0))]);
        assert_eq!(entries.len(), 2);
        assert_eq!(
            entries[1].operation,
            sift_protocol::ChangeLedgerOperation::MigrationRollback
        );
        assert!(entries
            .iter()
            .all(|entry| entry.outcome == Outcome::RolledBack));
        for admitted in [None, Some(false)] {
            let mut intent = statement(1, None);
            intent.native_admitted = admitted;
            intent.native_completed = None;
            assert!(project("plan", &group, &[intent]).is_empty());
        }
        group.outcome = Some(Outcome::OutcomeUnknown);
        let entries = project("plan", &group, &[statement(1, None)]);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].outcome, Outcome::OutcomeUnknown);
    }
    #[test]
    fn nontransactional_ledger_keeps_each_statement_and_positive_autocommit_proof() {
        let group = group(false);
        let mut first = statement(1, Some(0));
        first.autocommit_known = true;
        let second = statement(2, None);
        let entries = project("plan", &group, &[first, second]);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].outcome, Outcome::Committed);
        assert_eq!(entries[0].rows, Some(0));
        assert_eq!(entries[1].outcome, Outcome::OutcomeUnknown);
        assert!(entries.iter().all(|entry| entry.transaction_id.is_none()));
        assert_ne!(entries[0].fingerprint, entries[1].fingerprint);
    }
    #[test]
    fn settlement_requires_all_frozen_work_and_preserves_control_uncertainty() {
        let mut groups =
            std::collections::BTreeMap::from([(1, (group(true), vec![statement(1, Some(0))]))]);
        assert!(summarize(true, &groups) == Settlement::Committed);
        assert!(summarize(false, &groups) == Settlement::Unobserved);
        let mut pending = statement(1, None);
        pending.intent_recorded = false;
        pending.native_admitted = None;
        pending.native_completed = None;
        let untouched = Group {
            ordinal: 2,
            transactional: true,
            transaction_id: None,
            phase: Phase::Settled,
            commit_intent: false,
            rollback_intent: false,
            outcome: None,
        };
        groups.insert(2, (untouched.clone(), vec![pending.clone()]));
        assert!(
            summarize(true, &groups) == Settlement::Unknown,
            "an earlier commit cannot complete a later untouched group"
        );
        groups.get_mut(&1).unwrap().0.outcome = Some(Outcome::RolledBack);
        groups.get_mut(&1).unwrap().0.rollback_intent = true;
        assert!(summarize(true, &groups) == Settlement::RolledBack);
        groups.get_mut(&2).unwrap().0.outcome = Some(Outcome::OutcomeUnknown);
        assert!(
            summarize(true, &groups) == Settlement::Unknown,
            "unknown control settlement cannot be erased by another group's rollback"
        );
        let mut groups = std::collections::BTreeMap::from([(2, (untouched, vec![pending]))]);
        assert!(summarize(true, &groups) == Settlement::NotDispatched);
        groups.get_mut(&2).unwrap().1[0].intent_recorded = true;
        assert!(
            summarize(true, &groups) == Settlement::Unknown,
            "an unresolved intent is possible dispatch"
        );
        groups.get_mut(&2).unwrap().1[0].native_admitted = Some(false);
        assert!(summarize(true, &groups) == Settlement::NotDispatched);
        let mut native = statement(1, Some(0));
        native.autocommit_known = true;
        let groups = std::collections::BTreeMap::from([(1, (group(false), vec![native]))]);
        assert!(
            summarize(true, &groups) == Settlement::Committed,
            "raw EOF and positive autocommit are independent of delivery status"
        );
    }
    #[tokio::test]
    async fn producer_preserves_an_earlier_commit_when_a_later_group_rolls_back() {
        use sift_driver_api::mock::MockDriver;
        use sift_protocol::*;
        let metadata =
            MetadataStore::open_in_memory(Arc::new(sift_metadata::MemorySecretStore::new()))
                .unwrap();
        metadata.bootstrap_local("multi-group owner").unwrap();
        let actor = PrincipalId(1);
        let tenant = sift_metadata::TenantId(1);
        let profile = metadata.upsert_connection_profile(tenant, actor, sift_metadata::NewConnectionProfile {
            name: "multi-group".into(), provider_id: Engine::Postgres.provider_id(), semantic_engine: Some(Engine::Postgres),
            configuration: serde_json::json!({"host":"fixture.invalid", "port":5432, "database":"fixture", "user":"fixture", "ssl_mode":"disable"}),
            credentials: None, credential_mode: sift_metadata::CredentialMode::Shared, tags: vec![],
        }).await.unwrap();
        let options = CatalogGraphOptions::default();
        let snapshot = SchemaSnapshot {
            trees: vec![],
            scope: SchemaScope {
                depth: SchemaDepth::Graph {
                    options: options.clone(),
                },
                filter: None,
            },
            fetched_at: chrono::Utc::now(),
            incomplete: false,
            graph: Some(CatalogGraphData {
                coverage: CatalogCoverage::complete(),
                nodes: vec![],
                edges: vec![],
            }),
        };
        // The final catalog refresh deliberately has no queued response.
        let driver = Arc::new(
            MockDriver::builder()
                .schema_ok(snapshot)
                .execute_ok(vec![Page::Done {
                    affected_rows: Some(0),
                    warnings: vec![],
                }])
                .execute_err(DriverError::new(
                    Code::Other {
                        message: "PRIVATE-GROUP-FAILURE".into(),
                    },
                    "PRIVATE-GROUP-FAILURE",
                ))
                .build(),
        );
        let registry = DriverRegistry::new();
        registry
            .providers()
            .replace([
                Arc::new(crate::registry::BuiltinProviderAdapter::new(driver.clone()))
                    as Arc<dyn crate::registry::DatabaseProvider>,
            ])
            .unwrap();
        let store = SessionStore::new(registry);
        store.set_authorization_store(metadata.clone());
        let session = store
            .open_session_with_owner(
                OpenSessionRequest {
                    tag: None,
                    tenant_id: Some(1),
                },
                Some(actor),
                Some(tenant),
                false,
            )
            .unwrap()
            .id;
        let connection = store
            .open_managed_connection(
                session,
                Engine::Postgres.provider_id(),
                profile.configuration.clone(),
                Default::default(),
                actor,
                tenant,
                profile.id,
                0,
                true,
            )
            .await
            .unwrap()
            .id;
        let revision = store
            .catalog_graph(session, connection, CatalogGraphRequest::default())
            .await
            .unwrap()
            .revision;
        let groups = (1..=3)
            .map(|ordinal| MigrationGroup {
                ordinal,
                transactional: true,
                statements: vec![MigrationStatement {
                    ordinal: 1,
                    sql: format!("CREATE TABLE fixture_{ordinal} (id int)"),
                    fingerprint: format!("sqlfp:{ordinal:064x}"),
                    change_ids: vec![],
                    risk: SchemaChangeRisk::Safe,
                }],
            })
            .collect::<Vec<_>>();
        let plan = MigrationPlan {
            id: MigrationPlanId(uuid::Uuid::new_v4()),
            run_id: MigrationRunId(uuid::Uuid::new_v4()),
            digest: format!(
                "migfp:{:x}",
                Sha256::digest(serde_json::to_vec(&groups).unwrap())
            ),
            diff_digest: "fixture".into(),
            expected_live_revision: revision,
            groups,
            rollback_groups: vec![],
            required_acknowledgements: vec![],
            warnings: vec![],
            expires_at: chrono::Utc::now() + chrono::Duration::seconds(60),
        };
        store
            .store_migration_plan(
                plan.clone(),
                MigrationPlanScope {
                    review_evidence: None,
                    ai_proposal_id: None,
                    session,
                    connection,
                    principal: actor,
                    tenant,
                    profile: profile.id,
                    policy_revision: 0,
                    live_options: options,
                },
            )
            .unwrap();
        let request = ApplyMigrationRequest {
            plan_id: plan.id,
            plan_digest: plan.digest.clone(),
            acknowledgements: vec![],
            source: None,
        };
        let run = store
            .apply_migration(session, connection, actor, request.clone())
            .await
            .unwrap();
        assert_eq!(run.state, MigrationRunState::Partial);
        let native = metadata
            .migration_native_outcomes(tenant, run.id, &MigrationNativePageRequest::default())
            .unwrap();
        assert!(native.closed && native.journal_present);
        assert_eq!(native.observations.len(), 3);
        let first = &native.observations[0];
        let second = &native.observations[1];
        let untouched = &native.observations[2];
        assert_eq!(first.group.outcome, Some(Outcome::Committed));
        assert_eq!(second.group.outcome, Some(Outcome::RolledBack));
        assert_ne!(first.group.transaction_id, second.group.transaction_id);
        assert!(first.group.transaction_id.is_some() && second.group.transaction_id.is_some());
        assert!(!untouched.statement.as_ref().unwrap().intent_recorded);
        assert!(untouched.group.transaction_id.is_none() && untouched.group.outcome.is_none());
        assert!(!serde_json::to_string(&native)
            .unwrap()
            .contains("PRIVATE-GROUP-FAILURE"));
        let projected = ledger(metadata.clone(), tenant, run.id).await.unwrap();
        assert_eq!(projected.entries.len(), 3);
        assert!(projected.settlement == Settlement::Unknown);
        assert_eq!(projected.entries[0].outcome, Outcome::Committed);
        assert_eq!(projected.entries[0].rows, Some(0));
        assert_eq!(projected.entries[1].outcome, Outcome::RolledBack);
        assert_eq!(
            projected.entries[2].operation,
            ChangeLedgerOperation::MigrationRollback
        );
        assert_eq!(metadata.interrupt_migration_native_journals().unwrap(), 0);
        assert!(store
            .apply_migration(session, connection, actor, request)
            .await
            .is_err());
        assert_eq!(
            driver
                .invocations()
                .iter()
                .filter(|&&call| call == "execute")
                .count(),
            2
        );
        assert_eq!(
            driver
                .invocations()
                .iter()
                .filter(|&&call| call == "commit")
                .count(),
            1
        );
        assert_eq!(
            driver
                .invocations()
                .iter()
                .filter(|&&call| call == "rollback")
                .count(),
            1
        );
    }
}
