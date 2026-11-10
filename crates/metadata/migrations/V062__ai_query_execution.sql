-- SQL and targets remain in encrypted proposal/run context storage.
CREATE TABLE ai_query_execution (
 id TEXT PRIMARY KEY,
 run_id TEXT NOT NULL REFERENCES ai_run(id) ON DELETE CASCADE,
 chat_id TEXT NOT NULL REFERENCES ai_chat(id) ON DELETE CASCADE,
 tenant_id INTEGER NOT NULL REFERENCES tenant(id) ON DELETE CASCADE,
 initiator_id INTEGER NOT NULL REFERENCES principal(id) ON DELETE RESTRICT,
 call_id TEXT NOT NULL,
 proposal_id TEXT NOT NULL UNIQUE REFERENCES ai_proposal(id) ON DELETE CASCADE,
 source_profile_id INTEGER REFERENCES connection_profile(id) ON DELETE SET NULL,
 source_digest TEXT NOT NULL CHECK(length(source_digest)=64),
 fingerprint TEXT NOT NULL CHECK(length(fingerprint)=64),
 before_sha256 TEXT NOT NULL CHECK(length(before_sha256)=64),
 after_sha256 TEXT NOT NULL CHECK(length(after_sha256)=64),
 target_kind TEXT NOT NULL CHECK(target_kind IN ('room_document','private_editor')),
 document_id INTEGER REFERENCES document(id) ON DELETE SET NULL,
 room_id INTEGER REFERENCES room(id) ON DELETE SET NULL,
 desktop_id TEXT NOT NULL,
 state TEXT NOT NULL CHECK(state IN ('awaiting_approval','applying','completed','denied','outcome_unknown','reconciled')),
 dispatch_digest TEXT CHECK(dispatch_digest IS NULL OR length(dispatch_digest)=64),
 ack_digest TEXT CHECK(ack_digest IS NULL OR length(ack_digest)=64),
 outcome TEXT CHECK(outcome IN ('applied','refused','outcome_unknown')),
 evidence TEXT CHECK(evidence IN ('room_commit','companion_acknowledgement','no_dispatch','interrupted')),
 document_sequence INTEGER,
 observed_at TEXT,
 late_companion_outcome TEXT CHECK(late_companion_outcome IN ('applied','refused','outcome_unknown')),
 late_observed_at TEXT,
 created_at TEXT NOT NULL,
 expires_at TEXT NOT NULL,
 UNIQUE(run_id,call_id)
);
CREATE INDEX ai_query_execution_chat ON ai_query_execution(chat_id,created_at DESC,id DESC);
CREATE INDEX ai_query_execution_pending_editor ON ai_query_execution(expires_at,id) WHERE target_kind='private_editor' AND state='applying';
CREATE TRIGGER ai_query_close_pending AFTER UPDATE OF status ON ai_run WHEN NEW.status<>'running'
BEGIN UPDATE ai_query_execution SET state='denied' WHERE run_id=NEW.id AND state='awaiting_approval'; END;
CREATE TRIGGER ai_query_discard_pending AFTER UPDATE OF status ON ai_proposal WHEN NEW.status<>'staged'
BEGIN UPDATE ai_query_execution SET state='denied' WHERE proposal_id=NEW.id AND state='awaiting_approval'; END;
CREATE TRIGGER ai_query_keep_claimed_proposal BEFORE UPDATE OF status ON ai_proposal
WHEN NEW.status<>OLD.status AND EXISTS(SELECT 1 FROM ai_query_execution WHERE proposal_id=OLD.id AND state IN ('applying','outcome_unknown','reconciled'))
BEGIN SELECT RAISE(ABORT,'issued query edit must retain its original proposal'); END;
CREATE TRIGGER ai_query_require_execution BEFORE UPDATE OF status ON ai_proposal
WHEN NEW.status='applied' AND EXISTS(SELECT 1 FROM ai_query_execution WHERE proposal_id=OLD.id AND NOT(state='completed' AND outcome='applied'))
BEGIN SELECT RAISE(ABORT,'query proposal requires its exact execution settlement'); END;
CREATE TRIGGER ai_query_keep_active_chat BEFORE DELETE ON ai_chat
WHEN EXISTS(SELECT 1 FROM ai_query_execution WHERE chat_id=OLD.id AND state='applying')
BEGIN SELECT RAISE(ABORT,'query edit still has issued work pending'); END;
CREATE TRIGGER ai_query_keep_active_run BEFORE DELETE ON ai_run
WHEN EXISTS(SELECT 1 FROM ai_query_execution WHERE run_id=OLD.id AND state='applying')
BEGIN SELECT RAISE(ABORT,'query edit still has issued work pending'); END;
CREATE TRIGGER ai_query_shared_admission BEFORE UPDATE OF state ON ai_query_execution
WHEN NEW.state='applying' AND (
 EXISTS(SELECT 1 FROM ai_sql_execution WHERE chat_id=NEW.chat_id AND state IN ('applying','outcome_unknown')) OR
 EXISTS(SELECT 1 FROM ai_proposal_apply a JOIN ai_proposal p ON p.id=a.proposal_id WHERE p.chat_id=NEW.chat_id AND (a.state='applying' OR (a.state='outcome_unknown' AND a.reconciled_at IS NULL))) OR
 EXISTS(SELECT 1 FROM ai_query_execution WHERE chat_id=NEW.chat_id AND id<>NEW.id AND state IN ('applying','outcome_unknown')))
BEGIN SELECT RAISE(ABORT,'AI mutation must settle or be acknowledged before another action'); END;
CREATE TRIGGER ai_sql_query_admission BEFORE UPDATE OF state ON ai_sql_execution
WHEN NEW.state='applying' AND EXISTS(SELECT 1 FROM ai_query_execution WHERE chat_id=NEW.chat_id AND state IN ('applying','outcome_unknown'))
BEGIN SELECT RAISE(ABORT,'query edit must settle or be acknowledged before another action'); END;
CREATE TRIGGER ai_apply_query_admission BEFORE INSERT ON ai_proposal_apply
WHEN NEW.state='applying' AND EXISTS(SELECT 1 FROM ai_query_execution q JOIN ai_proposal p ON p.chat_id=q.chat_id WHERE p.id=NEW.proposal_id AND q.state IN ('applying','outcome_unknown'))
BEGIN SELECT RAISE(ABORT,'query edit must settle or be acknowledged before another action'); END;
