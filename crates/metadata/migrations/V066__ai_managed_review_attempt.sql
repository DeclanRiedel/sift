-- A lost setup response must never implicitly repeat native connection setup.
CREATE TABLE ai_managed_review_attempt (
 client_request_id TEXT PRIMARY KEY,
 proposal_id TEXT NOT NULL REFERENCES ai_proposal(id) ON DELETE CASCADE,
 reviewer_id INTEGER NOT NULL REFERENCES principal(id) ON DELETE RESTRICT,
 created_at TEXT NOT NULL
);
CREATE INDEX ai_managed_review_attempt_proposal ON ai_managed_review_attempt(proposal_id);
