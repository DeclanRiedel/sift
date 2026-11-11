//! Human withdrawal closes admission without rewriting already issued work.
use super::*;

#[derive(Debug)]
pub struct AiDelegationReviewChange {
    pub review: AiDelegationReviewSummary,
    /// Signal these original run owners after commit. Exact revocation retry
    /// returns the same identities, allowing notification after a lost response.
    pub closed_runs: Vec<Uuid>,
}

impl MetadataStore {
    /// Deny an unapproved review. No source access or content decryption is
    /// needed to remove authority. The receiving server must audit this action.
    pub async fn deny_ai_delegation_review(
        &self,
        id: Uuid,
        actor: PrincipalId,
        request: EndAiDelegationReview,
    ) -> Result<AiDelegationReviewChange> {
        self.end_ai_delegation_review(id, actor, request, false)
            .await
    }

    /// Revoke an unused review or its original grant. Consumed reviews retain
    /// their one-use identity; issued native journals keep their settlement owner.
    pub async fn revoke_ai_delegation_review(
        &self,
        id: Uuid,
        actor: PrincipalId,
        request: EndAiDelegationReview,
    ) -> Result<AiDelegationReviewChange> {
        self.end_ai_delegation_review(id, actor, request, true)
            .await
    }

    async fn end_ai_delegation_review(
        &self,
        id: Uuid,
        actor: PrincipalId,
        request: EndAiDelegationReview,
        revoke: bool,
    ) -> Result<AiDelegationReviewChange> {
        let store = self.clone();
        sqlite_blocking(move || {
            let mut conn = store.conn()?;
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let row = stored(&tx, id, actor)?;
            if row.summary.fingerprint != request.fingerprint {
                return Err(invalid("Delegation review changed"));
            }
            let mut closed_runs = Vec::new();
            match (revoke, row.summary.state) {
                (false, AiDelegationReviewState::AwaitingApproval) => {
                    tx.execute(
                        "UPDATE ai_delegation_review SET state='denied' WHERE id=?1",
                        [id.to_string()],
                    )?;
                }
                (false, AiDelegationReviewState::Denied)
                | (true, AiDelegationReviewState::Revoked) => {}
                (true, AiDelegationReviewState::AwaitingApproval | AiDelegationReviewState::Approved) => {
                    tx.execute(
                        "UPDATE ai_delegation_review SET state='revoked' WHERE id=?1",
                        [id.to_string()],
                    )?;
                }
                (true, AiDelegationReviewState::Consumed) => {
                    let run = row.summary.used_run_id.ok_or_else(|| invalid("Consumed delegation has no original run"))?;
                    let now = super::super::now_text();
                    tx.execute(
                        "UPDATE ai_run_delegation SET state='revoked',closed_at=?3 WHERE review_id=?1 AND run_id=?2 AND state='active'",
                        params![id.to_string(), run.to_string(), now],
                    )?;
                    // Completed/interrupted or retained-away grants confer no
                    // admission. A revoked grant remains retryable notification.
                    let revoked: bool = tx.query_row(
                        "SELECT EXISTS(SELECT 1 FROM ai_run_delegation WHERE review_id=?1 AND run_id=?2 AND state='revoked')",
                        params![id.to_string(), run.to_string()], |r| r.get(0),
                    )?;
                    if revoked {
                        tx.execute(
                            "INSERT INTO ai_run_event(run_id,sequence,kind,at) SELECT id,next_sequence,'stopped',?2 FROM ai_run WHERE id=?1 AND status='running'",
                            params![run.to_string(), now],
                        )?;
                        tx.execute(
                            "UPDATE ai_run SET status='canceled',ended_at=?2,next_sequence=next_sequence+1 WHERE id=?1 AND status='running'",
                            params![run.to_string(), now],
                        )?;
                        closed_runs.push(run);
                    }
                }
                _ => return Err(invalid("Delegation review cannot make this transition")),
            }
            let review = stored(&tx, id, actor)?.summary;
            tx.commit()?;
            Ok(AiDelegationReviewChange { review, closed_runs })
        }).await
    }
}
