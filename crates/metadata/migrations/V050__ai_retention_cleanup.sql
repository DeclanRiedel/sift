-- Cleanup survives deletion of its tenant/room/principal. No keys or bodies.
CREATE TABLE ai_content_cleanup (
    tenant_id INTEGER NOT NULL CHECK (tenant_id > 0),
    content_handle TEXT NOT NULL CHECK (length(content_handle) = 36),
    queued_at TEXT NOT NULL,
    attempts INTEGER NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    PRIMARY KEY (tenant_id, content_handle)
);
CREATE INDEX ai_content_cleanup_retry ON ai_content_cleanup(attempts, queued_at);

-- Capture the full child inventory before parent cascades hide tenant identity.
CREATE TRIGGER ai_chat_queue_content BEFORE DELETE ON ai_chat BEGIN
    INSERT OR IGNORE INTO ai_content_cleanup(tenant_id,content_handle,queued_at)
    SELECT OLD.tenant_id,handle,strftime('%Y-%m-%dT%H:%M:%fZ','now') FROM (
        SELECT OLD.title_handle AS handle
        UNION SELECT prompt_handle FROM ai_run WHERE chat_id=OLD.id
        UNION SELECT context_handle FROM ai_run WHERE chat_id=OLD.id
        UNION SELECT lease_handle FROM ai_run WHERE chat_id=OLD.id
        UNION SELECT e.content_handle FROM ai_run_event e JOIN ai_run r ON r.id=e.run_id WHERE r.chat_id=OLD.id
        UNION SELECT target_handle FROM ai_proposal WHERE chat_id=OLD.id
        UNION SELECT content_handle FROM ai_proposal WHERE chat_id=OLD.id
    ) WHERE handle IS NOT NULL;
END;
CREATE TRIGGER ai_run_queue_content BEFORE DELETE ON ai_run BEGIN
    INSERT OR IGNORE INTO ai_content_cleanup(tenant_id,content_handle,queued_at)
    SELECT c.tenant_id,b.handle,strftime('%Y-%m-%dT%H:%M:%fZ','now') FROM ai_chat c CROSS JOIN (
        SELECT OLD.prompt_handle AS handle
        UNION SELECT OLD.context_handle
        UNION SELECT OLD.lease_handle
        UNION SELECT content_handle FROM ai_run_event WHERE run_id=OLD.id
        UNION SELECT target_handle FROM ai_proposal WHERE run_id=OLD.id
        UNION SELECT content_handle FROM ai_proposal WHERE run_id=OLD.id
    ) b WHERE c.id=OLD.chat_id AND b.handle IS NOT NULL;
END;
CREATE TRIGGER ai_event_queue_content BEFORE DELETE ON ai_run_event BEGIN
    INSERT OR IGNORE INTO ai_content_cleanup(tenant_id,content_handle,queued_at)
    SELECT c.tenant_id,OLD.content_handle,strftime('%Y-%m-%dT%H:%M:%fZ','now')
    FROM ai_run r JOIN ai_chat c ON c.id=r.chat_id WHERE r.id=OLD.run_id AND OLD.content_handle IS NOT NULL;
END;
CREATE TRIGGER ai_proposal_queue_content BEFORE DELETE ON ai_proposal BEGIN
    INSERT OR IGNORE INTO ai_content_cleanup(tenant_id,content_handle,queued_at)
    SELECT c.tenant_id,b.handle,strftime('%Y-%m-%dT%H:%M:%fZ','now')
    FROM ai_chat c CROSS JOIN (SELECT OLD.target_handle AS handle UNION SELECT OLD.content_handle) b WHERE c.id=OLD.chat_id;
END;
