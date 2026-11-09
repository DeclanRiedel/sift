-- A run's existing mode remains its read/staging base. Execution is an
-- independent permission; effective-mode readers must consult execute_mode.
ALTER TABLE ai_run ADD COLUMN execute_mode INTEGER NOT NULL DEFAULT 0
    CHECK (execute_mode IN (0,1) AND (execute_mode=0 OR mode='propose'));
CREATE TABLE ai_sql_execution (
    id TEXT PRIMARY KEY,
    run_id TEXT NOT NULL REFERENCES ai_run(id) ON DELETE CASCADE,
    chat_id TEXT NOT NULL REFERENCES ai_chat(id) ON DELETE CASCADE,
    tenant_id INTEGER NOT NULL REFERENCES tenant(id) ON DELETE CASCADE,
    initiator_id INTEGER NOT NULL REFERENCES principal(id) ON DELETE RESTRICT,
    call_id TEXT NOT NULL,
    request_sha256 TEXT NOT NULL CHECK(length(request_sha256)=64),
    action_sha256 TEXT NOT NULL CHECK(length(action_sha256)=64),
    input_handle TEXT NOT NULL UNIQUE,
    state TEXT NOT NULL CHECK(state IN ('awaiting_approval','applying','completed','denied','outcome_unknown','reconciled')),
    approved_by INTEGER REFERENCES principal(id) ON DELETE RESTRICT,
    receipt_handle TEXT UNIQUE,
    created_at TEXT NOT NULL,
    expires_at TEXT NOT NULL,
    finished_at TEXT,
    UNIQUE(run_id,call_id)
);
CREATE INDEX ai_sql_execution_chat ON ai_sql_execution(chat_id,created_at DESC,id DESC);
CREATE UNIQUE INDEX ai_sql_execution_one_active ON ai_sql_execution(run_id)
    WHERE state IN ('applying','outcome_unknown');
CREATE TRIGGER ai_sql_execution_keep_active_chat BEFORE DELETE ON ai_chat
WHEN EXISTS(SELECT 1 FROM ai_sql_execution WHERE chat_id=OLD.id AND state='applying')
BEGIN SELECT RAISE(ABORT,'AI execution still has native work pending'); END;
CREATE TRIGGER ai_sql_execution_cleanup BEFORE DELETE ON ai_sql_execution BEGIN
    INSERT OR IGNORE INTO ai_content_cleanup(tenant_id,content_handle,queued_at)
    VALUES(OLD.tenant_id,OLD.input_handle,strftime('%Y-%m-%dT%H:%M:%fZ','now'));
    INSERT OR IGNORE INTO ai_content_cleanup(tenant_id,content_handle,queued_at)
    SELECT OLD.tenant_id,OLD.receipt_handle,strftime('%Y-%m-%dT%H:%M:%fZ','now')
    WHERE OLD.receipt_handle IS NOT NULL;
END;
CREATE TABLE ai_execute_run_source (
    run_id TEXT PRIMARY KEY REFERENCES ai_run(id) ON DELETE CASCADE,
    tenant_id INTEGER NOT NULL REFERENCES tenant(id) ON DELETE CASCADE,
    source_handle TEXT NOT NULL UNIQUE
);
CREATE TRIGGER ai_execute_run_source_cleanup BEFORE DELETE ON ai_execute_run_source BEGIN
    INSERT OR IGNORE INTO ai_content_cleanup(tenant_id,content_handle,queued_at)
    VALUES(OLD.tenant_id,OLD.source_handle,strftime('%Y-%m-%dT%H:%M:%fZ','now'));
END;
-- Stop/expiry/recovery revoke unclaimed approval work without overwriting an
-- independently owned native apply or its eventual outcome.
CREATE TRIGGER ai_sql_execution_close_pending AFTER UPDATE OF status ON ai_run
WHEN NEW.status <> 'running' BEGIN
    UPDATE ai_sql_execution SET state='denied',
        finished_at=COALESCE(NEW.ended_at,strftime('%Y-%m-%dT%H:%M:%fZ','now'))
    WHERE run_id=NEW.id AND state='awaiting_approval';
END;
