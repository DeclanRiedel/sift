CREATE TABLE workspace (
    id INTEGER PRIMARY KEY,
    tenant_id INTEGER NOT NULL REFERENCES tenant(id) ON DELETE CASCADE,
    principal_id INTEGER NOT NULL REFERENCES principal(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE TABLE session_snapshot (
    id INTEGER PRIMARY KEY,
    workspace_id INTEGER NOT NULL REFERENCES workspace(id) ON DELETE CASCADE,
    tag TEXT,
    opened_at TEXT NOT NULL,
    closed_at TEXT
);

CREATE TABLE tab (
    id INTEGER PRIMARY KEY,
    session_id INTEGER NOT NULL REFERENCES session_snapshot(id) ON DELETE CASCADE,
    kind TEXT NOT NULL,
    connection_profile_id INTEGER REFERENCES connection_profile(id) ON DELETE SET NULL,
    title TEXT NOT NULL,
    body_text TEXT,
    position INTEGER NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE INDEX idx_tab_session_position ON tab(session_id, position);
