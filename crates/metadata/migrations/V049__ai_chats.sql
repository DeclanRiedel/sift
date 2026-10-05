-- AI content lives in encrypted blobs outside SQLite. Rows hold identity,
-- lifecycle, limits, and opaque handles only.
CREATE TABLE ai_chat (
    id TEXT PRIMARY KEY,
    tenant_id INTEGER NOT NULL REFERENCES tenant(id) ON DELETE CASCADE,
    room_id INTEGER REFERENCES room(id) ON DELETE CASCADE,
    owner_principal_id INTEGER NOT NULL REFERENCES principal(id) ON DELETE CASCADE,
    visibility TEXT NOT NULL CHECK (visibility IN ('private', 'room_public')),
    title_handle TEXT NOT NULL UNIQUE,
    revision INTEGER NOT NULL DEFAULT 1 CHECK (revision > 0),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    CHECK (visibility = 'private' OR room_id IS NOT NULL)
);
CREATE INDEX ai_chat_owner_recent
    ON ai_chat(tenant_id, owner_principal_id, updated_at DESC, id DESC);
CREATE INDEX ai_chat_room_recent
    ON ai_chat(room_id, updated_at DESC, id DESC)
    WHERE visibility = 'room_public';

CREATE TABLE ai_run (
    id TEXT PRIMARY KEY,
    chat_id TEXT NOT NULL REFERENCES ai_chat(id) ON DELETE CASCADE,
    client_request_id TEXT NOT NULL,
    request_sha256 TEXT NOT NULL CHECK (length(request_sha256) = 64),
    initiator_principal_id INTEGER NOT NULL REFERENCES principal(id) ON DELETE CASCADE,
    desktop_id TEXT NOT NULL,
    lease_digest TEXT NOT NULL UNIQUE,
    provider TEXT NOT NULL CHECK (provider IN ('codex', 'claude_code', 'open_code')),
    model TEXT,
    mode TEXT NOT NULL CHECK (mode IN ('read', 'propose')),
    status TEXT NOT NULL CHECK (status IN ('running', 'completed', 'failed', 'canceled', 'interrupted')),
    prompt_handle TEXT NOT NULL UNIQUE,
    context_handle TEXT NOT NULL UNIQUE,
    lease_handle TEXT NOT NULL UNIQUE,
    next_sequence INTEGER NOT NULL DEFAULT 1 CHECK (next_sequence > 0),
    started_at TEXT NOT NULL,
    ended_at TEXT,
    UNIQUE(chat_id, client_request_id)
);
CREATE UNIQUE INDEX ai_run_one_active_per_chat ON ai_run(chat_id)
    WHERE status = 'running';
CREATE INDEX ai_run_chat_recent ON ai_run(chat_id, started_at DESC, id DESC);

CREATE TABLE ai_run_event (
    run_id TEXT NOT NULL REFERENCES ai_run(id) ON DELETE CASCADE,
    sequence INTEGER NOT NULL CHECK (sequence > 0),
    client_event_id TEXT,
    kind TEXT NOT NULL,
    at TEXT NOT NULL,
    content_handle TEXT UNIQUE,
    tool_call_id TEXT,
    proposal_id TEXT,
    PRIMARY KEY (run_id, sequence)
);
CREATE UNIQUE INDEX ai_run_event_client_id ON ai_run_event(run_id, client_event_id)
    WHERE client_event_id IS NOT NULL;

CREATE TABLE ai_proposal (
    id TEXT PRIMARY KEY,
    client_request_id TEXT NOT NULL,
    chat_id TEXT NOT NULL REFERENCES ai_chat(id) ON DELETE CASCADE,
    run_id TEXT NOT NULL REFERENCES ai_run(id) ON DELETE CASCADE,
    kind TEXT NOT NULL CHECK (kind IN ('query_text_patch', 'row_edit_set', 'migration_draft')),
    status TEXT NOT NULL CHECK (status IN ('staged', 'applied', 'discarded', 'conflicted')),
    target_handle TEXT NOT NULL UNIQUE,
    content_handle TEXT NOT NULL UNIQUE,
    base_revision INTEGER,
    content_sha256 TEXT NOT NULL CHECK (length(content_sha256) = 64),
    created_by INTEGER NOT NULL REFERENCES principal(id) ON DELETE RESTRICT,
    applied_by INTEGER REFERENCES principal(id) ON DELETE SET NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE INDEX ai_proposal_chat ON ai_proposal(chat_id, created_at DESC, id DESC);
CREATE UNIQUE INDEX ai_proposal_run_request ON ai_proposal(run_id, client_request_id);

CREATE TABLE ai_tenant_retention (
    tenant_id INTEGER PRIMARY KEY REFERENCES tenant(id) ON DELETE CASCADE,
    retention_days INTEGER CHECK (retention_days IS NULL OR retention_days >= 1),
    updated_by INTEGER NOT NULL REFERENCES principal(id) ON DELETE RESTRICT,
    updated_at TEXT NOT NULL
);
