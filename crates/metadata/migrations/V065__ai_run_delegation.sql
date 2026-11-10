-- Run delegation identity, budgets and immutable native claim links (ADR-204).
-- Policy/source/argument bodies are encrypted external content handles only.
CREATE TABLE ai_delegation_tenant_policy (
 tenant_id INTEGER PRIMARY KEY REFERENCES tenant(id) ON DELETE CASCADE,
 revision INTEGER NOT NULL CHECK(revision>0),
 enabled INTEGER NOT NULL DEFAULT 0 CHECK(enabled IN (0,1)),
 arbitrary_sql_enabled INTEGER NOT NULL DEFAULT 0 CHECK(arbitrary_sql_enabled IN (0,1)),
 updated_by INTEGER NOT NULL REFERENCES principal(id) ON DELETE RESTRICT,
 updated_at TEXT NOT NULL,
 CHECK(arbitrary_sql_enabled=0 OR enabled=1)
);
CREATE TABLE ai_delegation_review (
 id TEXT PRIMARY KEY,
 chat_id TEXT NOT NULL REFERENCES ai_chat(id) ON DELETE CASCADE,
 tenant_id INTEGER NOT NULL REFERENCES tenant(id) ON DELETE CASCADE,
 initiator_id INTEGER NOT NULL REFERENCES principal(id) ON DELETE RESTRICT,
 client_request_id TEXT NOT NULL,
 request_sha256 TEXT NOT NULL CHECK(length(request_sha256)=64),
 content_handle TEXT NOT NULL UNIQUE CHECK(length(content_handle)=36),
 review_sha256 TEXT NOT NULL CHECK(length(review_sha256)=64),
 scope_sha256 TEXT NOT NULL CHECK(length(scope_sha256)=64),
 tenant_policy_revision INTEGER NOT NULL CHECK(tenant_policy_revision>0),
 state TEXT NOT NULL CHECK(state IN ('awaiting_approval','approved','consumed','denied','revoked')),
 approved_by INTEGER REFERENCES principal(id) ON DELETE RESTRICT,
 approved_at TEXT,
 -- No FK: retention may delete the original run, but can never unspend review.
 used_run_id TEXT UNIQUE,
 created_at TEXT NOT NULL,
 expires_at TEXT NOT NULL,
 CHECK((approved_by IS NULL)=(approved_at IS NULL)),
 CHECK(state NOT IN ('approved','consumed') OR approved_by IS NOT NULL),
 UNIQUE(chat_id,initiator_id,client_request_id),
 CHECK((state='consumed')=(used_run_id IS NOT NULL))
);
CREATE INDEX ai_delegation_review_chat ON ai_delegation_review(chat_id,created_at DESC,id DESC);
CREATE TABLE ai_run_delegation (
 run_id TEXT PRIMARY KEY REFERENCES ai_run(id) ON DELETE CASCADE,
 review_id TEXT NOT NULL UNIQUE REFERENCES ai_delegation_review(id) ON DELETE NO ACTION DEFERRABLE INITIALLY DEFERRED,
 state TEXT NOT NULL CHECK(state IN ('active','ended','revoked','interrupted')),
 created_at TEXT NOT NULL,
 expires_at TEXT NOT NULL,
 closed_at TEXT,
 max_calls INTEGER NOT NULL CHECK(max_calls BETWEEN 1 AND 100),
 max_input_bytes INTEGER NOT NULL CHECK(max_input_bytes BETWEEN 1 AND 67108864),
 max_output_bytes INTEGER NOT NULL CHECK(max_output_bytes BETWEEN 1 AND 67108864),
 calls INTEGER NOT NULL DEFAULT 0 CHECK(calls BETWEEN 0 AND max_calls),
 input_bytes INTEGER NOT NULL DEFAULT 0 CHECK(input_bytes BETWEEN 0 AND max_input_bytes),
 output_bytes INTEGER NOT NULL DEFAULT 0 CHECK(output_bytes BETWEEN 0 AND max_output_bytes),
 CHECK((state='active')=(closed_at IS NULL))
);
CREATE TABLE ai_delegation_call (
 run_id TEXT NOT NULL REFERENCES ai_run_delegation(run_id) ON DELETE CASCADE,
 call_id TEXT NOT NULL,
 operation TEXT NOT NULL CHECK(length(operation) BETWEEN 1 AND 512),
 request_sha256 TEXT NOT NULL CHECK(length(request_sha256)=64),
 input_bytes INTEGER NOT NULL CHECK(input_bytes BETWEEN 1 AND 1048576),
 reserved_output_bytes INTEGER NOT NULL CHECK(reserved_output_bytes BETWEEN 1 AND 1048576),
 created_at TEXT NOT NULL,
 PRIMARY KEY(run_id,call_id)
);
CREATE TABLE ai_delegation_rule_usage (
 run_id TEXT NOT NULL REFERENCES ai_run_delegation(run_id) ON DELETE CASCADE,
 rule_id TEXT NOT NULL,
 uses INTEGER NOT NULL CHECK(uses BETWEEN 1 AND 100),
 submitted_rows INTEGER NOT NULL CHECK(submitted_rows BETWEEN 0 AND 10000),
 schema_changes INTEGER NOT NULL CHECK(schema_changes BETWEEN 0 AND 1000),
 PRIMARY KEY(run_id,rule_id)
);
CREATE TABLE ai_delegation_action (
 run_id TEXT NOT NULL,
 call_id TEXT NOT NULL,
 rule_id TEXT NOT NULL,
 family TEXT NOT NULL CHECK(family IN ('sql','database','query','extension')),
 -- No FK to family journals: absent restored evidence must remain uncertain.
 action_id TEXT NOT NULL,
 action_sha256 TEXT NOT NULL CHECK(length(action_sha256)=64),
 claimed_at TEXT NOT NULL,
 PRIMARY KEY(run_id,call_id),
 UNIQUE(family,action_id),
 FOREIGN KEY(run_id,call_id) REFERENCES ai_delegation_call(run_id,call_id) ON DELETE CASCADE,
 FOREIGN KEY(run_id,rule_id) REFERENCES ai_delegation_rule_usage(run_id,rule_id) ON DELETE NO ACTION DEFERRABLE INITIALLY DEFERRED
);
CREATE TRIGGER ai_delegation_review_identity BEFORE UPDATE OF id,chat_id,tenant_id,initiator_id,client_request_id,request_sha256,content_handle,review_sha256,scope_sha256,tenant_policy_revision,created_at,expires_at ON ai_delegation_review
BEGIN SELECT RAISE(ABORT,'delegation review is immutable'); END;
CREATE TRIGGER ai_delegation_review_approval BEFORE UPDATE OF approved_by,approved_at ON ai_delegation_review
WHEN OLD.state<>'awaiting_approval' OR OLD.approved_by IS NOT NULL
BEGIN SELECT RAISE(ABORT,'delegation approval cannot be replaced'); END;
CREATE TRIGGER ai_delegation_review_state BEFORE UPDATE OF state,used_run_id ON ai_delegation_review
WHEN NOT (
 OLD.state='awaiting_approval' AND NEW.state IN ('approved','denied','revoked')
 OR OLD.state='approved' AND NEW.state IN ('consumed','revoked')
)
BEGIN SELECT RAISE(ABORT,'delegation review cannot reissue authority'); END;
CREATE TRIGGER ai_delegation_run_identity BEFORE UPDATE OF run_id,review_id,created_at,expires_at,max_calls,max_input_bytes,max_output_bytes ON ai_run_delegation
BEGIN SELECT RAISE(ABORT,'run delegation is immutable'); END;
CREATE TRIGGER ai_delegation_run_state BEFORE UPDATE OF state,closed_at ON ai_run_delegation
WHEN OLD.state<>'active' OR NEW.state='active'
BEGIN SELECT RAISE(ABORT,'closed delegation cannot reopen'); END;
CREATE TRIGGER ai_delegation_budget_monotonic BEFORE UPDATE OF calls,input_bytes,output_bytes ON ai_run_delegation
WHEN NEW.calls<OLD.calls OR NEW.input_bytes<OLD.input_bytes OR NEW.output_bytes<OLD.output_bytes
BEGIN SELECT RAISE(ABORT,'delegation reservations cannot be refunded'); END;
CREATE TRIGGER ai_delegation_rule_monotonic BEFORE UPDATE ON ai_delegation_rule_usage
WHEN NEW.run_id<>OLD.run_id OR NEW.rule_id<>OLD.rule_id OR NEW.uses<OLD.uses OR NEW.submitted_rows<OLD.submitted_rows OR NEW.schema_changes<OLD.schema_changes
BEGIN SELECT RAISE(ABORT,'delegation rule usage cannot be refunded'); END;
CREATE TRIGGER ai_delegation_call_identity BEFORE UPDATE ON ai_delegation_call
BEGIN SELECT RAISE(ABORT,'delegation request is immutable'); END;
CREATE TRIGGER ai_delegation_action_identity BEFORE UPDATE ON ai_delegation_action
BEGIN SELECT RAISE(ABORT,'delegation action claim is immutable'); END;
CREATE TRIGGER ai_delegation_close_run AFTER UPDATE OF status ON ai_run WHEN NEW.status<>'running'
BEGIN UPDATE ai_run_delegation SET state=CASE WHEN NEW.status='interrupted' THEN 'interrupted' ELSE 'ended' END,closed_at=strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE run_id=NEW.id AND state='active'; END;
CREATE TRIGGER ai_delegation_review_cleanup BEFORE DELETE ON ai_delegation_review
BEGIN INSERT OR IGNORE INTO ai_content_cleanup(tenant_id,content_handle,queued_at) VALUES(OLD.tenant_id,OLD.content_handle,strftime('%Y-%m-%dT%H:%M:%fZ','now')); END;

CREATE TRIGGER ai_delegation_bind_run BEFORE INSERT ON ai_run_delegation
WHEN NEW.state='active' AND NOT EXISTS(
 SELECT 1 FROM ai_delegation_review d JOIN ai_run r ON r.id=NEW.run_id JOIN ai_chat c ON c.id=r.chat_id
 WHERE d.id=NEW.review_id AND d.state='approved' AND d.used_run_id IS NULL
  AND d.chat_id=r.chat_id AND d.tenant_id=c.tenant_id AND d.initiator_id=r.initiator_principal_id
  AND c.visibility='private' AND r.execute_mode=1 AND r.status='running'
  AND julianday(NEW.expires_at)>julianday('now') AND julianday(NEW.expires_at)<=julianday(d.expires_at)
  AND julianday(NEW.expires_at)<=julianday(r.started_at)+1.0/24.0
)
BEGIN SELECT RAISE(ABORT,'delegation requires its original approved run and deadline'); END;
CREATE TRIGGER ai_delegation_bind_review BEFORE UPDATE OF state ON ai_delegation_review
WHEN NEW.state='consumed' AND NOT EXISTS(SELECT 1 FROM ai_run_delegation WHERE review_id=NEW.id AND run_id=NEW.used_run_id AND state='active')
BEGIN SELECT RAISE(ABORT,'delegation review requires its atomic run claim'); END;
CREATE TRIGGER ai_delegation_call_live BEFORE INSERT ON ai_delegation_call
WHEN NOT EXISTS(
 SELECT 1 FROM ai_run_delegation g JOIN ai_run r ON r.id=g.run_id JOIN ai_delegation_review d ON d.id=g.review_id
 JOIN ai_delegation_tenant_policy p ON p.tenant_id=d.tenant_id
 WHERE g.run_id=NEW.run_id AND g.state='active' AND r.status='running'
  AND p.enabled=1 AND p.revision=d.tenant_policy_revision AND d.state='consumed' AND d.used_run_id=g.run_id
  AND julianday(g.expires_at)>julianday('now')
)
BEGIN SELECT RAISE(ABORT,'delegation is no longer active'); END;
CREATE TRIGGER ai_delegation_action_live BEFORE INSERT ON ai_delegation_action
WHEN NOT EXISTS(
 SELECT 1 FROM ai_run_delegation g JOIN ai_run r ON r.id=g.run_id JOIN ai_delegation_review d ON d.id=g.review_id
 JOIN ai_delegation_tenant_policy p ON p.tenant_id=d.tenant_id
 WHERE g.run_id=NEW.run_id AND g.state='active' AND r.status='running'
  AND p.enabled=1 AND p.revision=d.tenant_policy_revision AND d.state='consumed' AND d.used_run_id=g.run_id
  AND julianday(g.expires_at)>julianday('now')
)
BEGIN SELECT RAISE(ABORT,'delegation cannot claim more work'); END;

CREATE TRIGGER ai_delegation_tenant_revision BEFORE UPDATE ON ai_delegation_tenant_policy
WHEN NEW.tenant_id<>OLD.tenant_id OR NEW.revision<=OLD.revision
BEGIN SELECT RAISE(ABORT,'delegation policy revision must advance'); END;
