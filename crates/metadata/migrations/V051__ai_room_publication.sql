-- Explicit publication is separate from connection access and chat visibility.
CREATE TABLE ai_room_publication (
    id TEXT PRIMARY KEY,
    client_request_id TEXT NOT NULL,
    tenant_id INTEGER NOT NULL REFERENCES tenant(id) ON DELETE CASCADE,
    room_id INTEGER NOT NULL REFERENCES room(id) ON DELETE CASCADE,
    profile_id INTEGER NOT NULL REFERENCES connection_profile(id) ON DELETE CASCADE,
    binder_id INTEGER NOT NULL REFERENCES principal(id) ON DELETE RESTRICT,
    scope_digest TEXT NOT NULL CHECK (length(scope_digest) = 64),
    allow_rows INTEGER NOT NULL CHECK (allow_rows IN (0,1)),
    created_by INTEGER NOT NULL REFERENCES principal(id) ON DELETE RESTRICT,
    created_at TEXT NOT NULL,
    revoked_at TEXT,
    UNIQUE (room_id, client_request_id)
);
CREATE UNIQUE INDEX ai_room_publication_current ON ai_room_publication(room_id)
    WHERE revoked_at IS NULL;
