-- Endpoint/tool definitions are encrypted AI blobs. Credentials are SecretStore
-- handles, never SQLite values. Unreviewed registrations cannot execute tools.
CREATE TABLE ai_external_source (
    id TEXT PRIMARY KEY,
    tenant_id INTEGER NOT NULL REFERENCES tenant(id) ON DELETE CASCADE,
    owner_principal_id INTEGER NOT NULL REFERENCES principal(id) ON DELETE CASCADE,
    vault_id INTEGER REFERENCES vault(id) ON DELETE CASCADE,
    revision INTEGER NOT NULL DEFAULT 1 CHECK (revision > 0),
    state TEXT NOT NULL CHECK (state IN ('draft','active','disabled')),
    config_handle TEXT NOT NULL UNIQUE,
    config_sha256 TEXT NOT NULL CHECK (length(config_sha256) = 64),
    credential_identity TEXT NOT NULL CHECK (length(credential_identity) = 36),
    credential_handle TEXT UNIQUE,
    credential_scope_reviewed INTEGER NOT NULL DEFAULT 0 CHECK (credential_scope_reviewed IN (0,1)),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE INDEX ai_external_source_tenant ON ai_external_source(tenant_id,id);

CREATE TABLE ai_external_room_grant (
    id TEXT PRIMARY KEY,
    tenant_id INTEGER NOT NULL REFERENCES tenant(id) ON DELETE CASCADE,
    room_id INTEGER NOT NULL REFERENCES room(id) ON DELETE CASCADE,
    source_id TEXT NOT NULL REFERENCES ai_external_source(id) ON DELETE CASCADE,
    published_by INTEGER NOT NULL REFERENCES principal(id) ON DELETE CASCADE,
    content_handle TEXT NOT NULL UNIQUE,
    created_at TEXT NOT NULL,
    UNIQUE(room_id,source_id)
);

CREATE TRIGGER ai_external_source_queue_delete BEFORE DELETE ON ai_external_source BEGIN
    INSERT OR IGNORE INTO ai_content_cleanup(tenant_id,content_handle,queued_at)
    VALUES(OLD.tenant_id,OLD.config_handle,strftime('%Y-%m-%dT%H:%M:%fZ','now'));
    INSERT OR IGNORE INTO vault_secret_cleanup_queue(namespace,secret_handle,reason,not_before)
    SELECT 'sift.ai.external.v1',OLD.credential_handle,'external_source_deleted',strftime('%Y-%m-%dT%H:%M:%fZ','now')
    WHERE OLD.credential_handle IS NOT NULL;
END;
CREATE TRIGGER ai_external_source_queue_update BEFORE UPDATE OF config_handle,credential_handle ON ai_external_source BEGIN
    INSERT OR IGNORE INTO ai_content_cleanup(tenant_id,content_handle,queued_at)
    SELECT OLD.tenant_id,OLD.config_handle,strftime('%Y-%m-%dT%H:%M:%fZ','now')
    WHERE OLD.config_handle IS NOT NEW.config_handle;
    INSERT OR IGNORE INTO vault_secret_cleanup_queue(namespace,secret_handle,reason,not_before)
    SELECT 'sift.ai.external.v1',OLD.credential_handle,'external_source_rotated',strftime('%Y-%m-%dT%H:%M:%fZ','now')
    WHERE OLD.credential_handle IS NOT NULL AND OLD.credential_handle IS NOT NEW.credential_handle;
END;
CREATE TRIGGER ai_external_grant_queue_delete BEFORE DELETE ON ai_external_room_grant BEGIN
    INSERT OR IGNORE INTO ai_content_cleanup(tenant_id,content_handle,queued_at)
    VALUES(OLD.tenant_id,OLD.content_handle,strftime('%Y-%m-%dT%H:%M:%fZ','now'));
END;
CREATE TRIGGER ai_external_grant_queue_update BEFORE UPDATE OF content_handle ON ai_external_room_grant BEGIN
    INSERT OR IGNORE INTO ai_content_cleanup(tenant_id,content_handle,queued_at)
    SELECT OLD.tenant_id,OLD.content_handle,strftime('%Y-%m-%dT%H:%M:%fZ','now')
    WHERE OLD.content_handle IS NOT NEW.content_handle;
END;
