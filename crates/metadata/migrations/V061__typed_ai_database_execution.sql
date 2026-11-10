-- Bodies remain in existing encrypted proposal/review/run-source storage.
CREATE TABLE ai_database_execution (
    id TEXT PRIMARY KEY,
    run_id TEXT NOT NULL REFERENCES ai_run(id) ON DELETE CASCADE,
    chat_id TEXT NOT NULL REFERENCES ai_chat(id) ON DELETE CASCADE,
    tenant_id INTEGER NOT NULL REFERENCES tenant(id) ON DELETE CASCADE,
    initiator_id INTEGER NOT NULL REFERENCES principal(id) ON DELETE RESTRICT,
    call_id TEXT NOT NULL,
    proposal_id TEXT NOT NULL UNIQUE REFERENCES ai_proposal(id) ON DELETE CASCADE,
    review_id TEXT NOT NULL UNIQUE REFERENCES ai_proposal_review(id) ON DELETE CASCADE,
    source_profile_id INTEGER REFERENCES connection_profile(id) ON DELETE SET NULL,
    source_digest TEXT NOT NULL CHECK(length(source_digest)=64),
    fingerprint TEXT NOT NULL CHECK(length(fingerprint)=64),
    state TEXT NOT NULL CHECK(state IN ('awaiting_approval','claimed','denied')),
    created_at TEXT NOT NULL,
    expires_at TEXT NOT NULL,
    UNIQUE(run_id,call_id)
);
CREATE INDEX ai_database_execution_chat ON ai_database_execution(chat_id,created_at DESC,id DESC);
CREATE TRIGGER ai_database_execution_close_pending AFTER UPDATE OF status ON ai_run
WHEN NEW.status<>'running' BEGIN
    UPDATE ai_database_execution SET state='denied' WHERE run_id=NEW.id AND state='awaiting_approval';
END;
CREATE TRIGGER ai_database_execution_discard AFTER UPDATE OF status ON ai_proposal
WHEN NEW.status<>'staged' BEGIN
    UPDATE ai_database_execution SET state='denied' WHERE proposal_id=NEW.id AND state='awaiting_approval';
END;
-- Older/direct writers cannot bypass the typed human approval/claim transaction.
CREATE TRIGGER ai_database_execution_claim BEFORE INSERT ON ai_proposal_apply
WHEN EXISTS(SELECT 1 FROM ai_database_execution e WHERE e.proposal_id=NEW.proposal_id
    AND (e.state<>'claimed' OR e.id<>NEW.client_request_id OR e.review_id<>NEW.review_id OR e.initiator_id<>NEW.approved_by))
BEGIN SELECT RAISE(ABORT,'typed execution requires its exact human approval claim'); END;
