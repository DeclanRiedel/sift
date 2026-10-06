-- Sensitive previews/receipts are encrypted bodies, never SQLite columns.
CREATE TABLE ai_proposal_review (
    id TEXT PRIMARY KEY,
    client_request_id TEXT NOT NULL,
    tenant_id INTEGER NOT NULL REFERENCES tenant(id) ON DELETE CASCADE,
    proposal_id TEXT NOT NULL REFERENCES ai_proposal(id) ON DELETE CASCADE,
    reviewer_id INTEGER NOT NULL REFERENCES principal(id) ON DELETE RESTRICT,
    content_handle TEXT NOT NULL UNIQUE,
    review_digest TEXT NOT NULL CHECK(length(review_digest)=64),
    created_at TEXT NOT NULL,
    expires_at TEXT NOT NULL,
    UNIQUE(proposal_id,client_request_id)
);
CREATE INDEX ai_review_proposal ON ai_proposal_review(proposal_id,created_at);
CREATE TABLE ai_proposal_apply (
    proposal_id TEXT PRIMARY KEY REFERENCES ai_proposal(id) ON DELETE CASCADE,
    tenant_id INTEGER NOT NULL REFERENCES tenant(id) ON DELETE CASCADE,
    review_id TEXT NOT NULL REFERENCES ai_proposal_review(id) ON DELETE CASCADE,
    client_request_id TEXT NOT NULL,
    request_digest TEXT NOT NULL CHECK(length(request_digest)=64),
    approved_by INTEGER NOT NULL REFERENCES principal(id) ON DELETE RESTRICT,
    state TEXT NOT NULL CHECK(state IN ('applying','applied','failed','outcome_unknown')),
    receipt_handle TEXT UNIQUE,
    claimed_at TEXT NOT NULL,
    finished_at TEXT
);
CREATE INDEX ai_apply_pending ON ai_proposal_apply(state,claimed_at);
CREATE TRIGGER ai_review_queue_content BEFORE DELETE ON ai_proposal_review BEGIN
    INSERT OR IGNORE INTO ai_content_cleanup(tenant_id,content_handle,queued_at)
    VALUES(OLD.tenant_id,OLD.content_handle,strftime('%Y-%m-%dT%H:%M:%fZ','now'));
END;
CREATE TRIGGER ai_apply_queue_content BEFORE DELETE ON ai_proposal_apply
WHEN OLD.receipt_handle IS NOT NULL BEGIN
    INSERT OR IGNORE INTO ai_content_cleanup(tenant_id,content_handle,queued_at)
    VALUES(OLD.tenant_id,OLD.receipt_handle,strftime('%Y-%m-%dT%H:%M:%fZ','now'));
END;
