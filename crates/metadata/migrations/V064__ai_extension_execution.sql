-- Encrypted review/result bodies are external opaque content handles.
-- The journal link deliberately has no FK: historical restores can lack that fact.
CREATE TABLE ai_extension_execution (
 id TEXT PRIMARY KEY,
 run_id TEXT NOT NULL REFERENCES ai_run(id) ON DELETE CASCADE,
 chat_id TEXT NOT NULL REFERENCES ai_chat(id) ON DELETE CASCADE,
 tenant_id INTEGER NOT NULL REFERENCES tenant(id) ON DELETE CASCADE,
 initiator_id INTEGER NOT NULL REFERENCES principal(id) ON DELETE RESTRICT,
 call_id TEXT NOT NULL,
 tool_id TEXT NOT NULL CHECK(length(tool_id) BETWEEN 1 AND 512),
 request_sha256 TEXT NOT NULL CHECK(length(request_sha256)=64),
 action_sha256 TEXT NOT NULL CHECK(length(action_sha256)=64),
 input_handle TEXT NOT NULL,
 result_handle TEXT,
 result_availability TEXT NOT NULL DEFAULT 'not_recorded' CHECK(result_availability IN ('not_recorded','available','withheld')),
 review_state TEXT NOT NULL CHECK(review_state IN ('awaiting_approval','issued','denied')),
 execution_id TEXT UNIQUE,
 approved_by INTEGER REFERENCES principal(id) ON DELETE RESTRICT,
 approved_at TEXT,
 acknowledged_at TEXT,
 created_at TEXT NOT NULL,
 expires_at TEXT NOT NULL,
 UNIQUE(run_id,call_id),
 CHECK ((review_state='issued' AND execution_id IS NOT NULL AND approved_by IS NOT NULL AND approved_at IS NOT NULL)
    OR (review_state IN ('awaiting_approval','denied') AND execution_id IS NULL AND approved_by IS NULL AND approved_at IS NULL)),
 CHECK ((result_availability='available' AND result_handle IS NOT NULL) OR (result_availability<>'available' AND result_handle IS NULL)),
 CHECK (acknowledged_at IS NULL OR review_state='issued')
);
CREATE INDEX ai_extension_execution_chat ON ai_extension_execution(chat_id,created_at DESC,id DESC);
-- Unknown remains independent of native effects and encrypted result availability.
CREATE VIEW ai_extension_execution_admission AS
 SELECT e.id,e.chat_id FROM ai_extension_execution e LEFT JOIN extension_execution j ON j.id=e.execution_id
 WHERE e.review_state='issued' AND CASE
  WHEN j.id IS NULL OR j.actor_principal_id<>e.initiator_id OR j.request_id<>e.id OR j.tenant_id IS NOT e.tenant_id
   THEN e.acknowledged_at IS NULL
  ELSE j.state IN ('claimed','dispatching')
   OR e.acknowledged_at IS NULL AND (j.state='outcome_unknown'
    OR j.state='acknowledged' AND j.result_code IN ('request_timed_out','owner_interrupted'))
 END;
CREATE TRIGGER ai_extension_close_pending AFTER UPDATE OF status ON ai_run WHEN NEW.status<>'running'
BEGIN UPDATE ai_extension_execution SET review_state='denied' WHERE run_id=NEW.id AND review_state='awaiting_approval'; END;
CREATE TRIGGER ai_extension_keep_identity BEFORE UPDATE OF id,run_id,chat_id,tenant_id,initiator_id,call_id,tool_id,request_sha256,action_sha256,input_handle,created_at,expires_at ON ai_extension_execution
BEGIN SELECT RAISE(ABORT,'extension review identity is immutable'); END;
CREATE TRIGGER ai_extension_keep_claim BEFORE UPDATE OF execution_id,approved_by,approved_at,review_state ON ai_extension_execution
WHEN OLD.review_state<>'awaiting_approval'
BEGIN SELECT RAISE(ABORT,'extension review cannot reissue authority'); END;
CREATE TRIGGER ai_extension_keep_ack BEFORE UPDATE OF acknowledged_at ON ai_extension_execution
WHEN OLD.acknowledged_at IS NOT NULL
BEGIN SELECT RAISE(ABORT,'extension unknown acknowledgement is immutable'); END;
CREATE TRIGGER ai_extension_keep_active_chat BEFORE DELETE ON ai_chat
WHEN EXISTS(SELECT 1 FROM ai_extension_execution e JOIN extension_execution j ON j.id=e.execution_id AND j.actor_principal_id=e.initiator_id AND j.request_id=e.id AND j.tenant_id IS e.tenant_id WHERE e.chat_id=OLD.id AND j.state IN ('claimed','dispatching'))
BEGIN SELECT RAISE(ABORT,'extension execution still has an unsettled owner'); END;
CREATE TRIGGER ai_extension_keep_active_run BEFORE DELETE ON ai_run
WHEN EXISTS(SELECT 1 FROM ai_extension_execution e JOIN extension_execution j ON j.id=e.execution_id AND j.actor_principal_id=e.initiator_id AND j.request_id=e.id AND j.tenant_id IS e.tenant_id WHERE e.run_id=OLD.id AND j.state IN ('claimed','dispatching'))
BEGIN SELECT RAISE(ABORT,'extension execution still has an unsettled owner'); END;
CREATE TRIGGER ai_extension_shared_admission BEFORE UPDATE OF review_state ON ai_extension_execution
WHEN NEW.review_state='issued' AND (
 EXISTS(SELECT 1 FROM ai_extension_execution_admission WHERE chat_id=NEW.chat_id AND id<>NEW.id)
 OR EXISTS(SELECT 1 FROM ai_sql_execution WHERE chat_id=NEW.chat_id AND state IN ('applying','outcome_unknown'))
 OR EXISTS(SELECT 1 FROM ai_query_execution WHERE chat_id=NEW.chat_id AND state IN ('applying','outcome_unknown'))
 OR EXISTS(SELECT 1 FROM ai_proposal_apply a JOIN ai_proposal p ON p.id=a.proposal_id WHERE p.chat_id=NEW.chat_id AND (a.state='applying' OR a.state='outcome_unknown' AND a.reconciled_at IS NULL))
)
BEGIN SELECT RAISE(ABORT,'AI mutation must settle or be acknowledged before another action'); END;
CREATE TRIGGER ai_sql_extension_admission BEFORE UPDATE OF state ON ai_sql_execution
WHEN NEW.state='applying' AND EXISTS(SELECT 1 FROM ai_extension_execution_admission WHERE chat_id=NEW.chat_id)
BEGIN SELECT RAISE(ABORT,'extension execution must settle or be acknowledged before another action'); END;
CREATE TRIGGER ai_query_extension_admission BEFORE UPDATE OF state ON ai_query_execution
WHEN NEW.state='applying' AND EXISTS(SELECT 1 FROM ai_extension_execution_admission WHERE chat_id=NEW.chat_id)
BEGIN SELECT RAISE(ABORT,'extension execution must settle or be acknowledged before another action'); END;
CREATE TRIGGER ai_apply_extension_admission BEFORE INSERT ON ai_proposal_apply
WHEN NEW.state='applying' AND EXISTS(SELECT 1 FROM ai_extension_execution_admission e JOIN ai_proposal p ON p.chat_id=e.chat_id WHERE p.id=NEW.proposal_id)
BEGIN SELECT RAISE(ABORT,'extension execution must settle or be acknowledged before another action'); END;

CREATE TRIGGER ai_extension_content_cleanup BEFORE DELETE ON ai_extension_execution BEGIN
 INSERT OR IGNORE INTO ai_content_cleanup(tenant_id,content_handle,queued_at)
 VALUES(OLD.tenant_id,OLD.input_handle,strftime('%Y-%m-%dT%H:%M:%fZ','now'));
 INSERT OR IGNORE INTO ai_content_cleanup(tenant_id,content_handle,queued_at)
 SELECT OLD.tenant_id,OLD.result_handle,strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE OLD.result_handle IS NOT NULL;
END;
