CREATE TABLE query_history (
    id INTEGER PRIMARY KEY,
    principal_id INTEGER NOT NULL REFERENCES principal(id) ON DELETE CASCADE,
    connection_profile_id INTEGER REFERENCES connection_profile(id) ON DELETE SET NULL,
    sql_text TEXT NOT NULL,
    started_at TEXT NOT NULL,
    duration_ms INTEGER,
    row_count INTEGER,
    status TEXT NOT NULL CHECK (status IN ('ok', 'error', 'canceled')),
    error_code TEXT,
    error_message TEXT
);

CREATE INDEX idx_query_history_principal_started ON query_history(principal_id, started_at DESC);

CREATE TABLE saved_query (
    id INTEGER PRIMARY KEY,
    tenant_id INTEGER NOT NULL REFERENCES tenant(id) ON DELETE CASCADE,
    principal_id INTEGER REFERENCES principal(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    sql_text TEXT NOT NULL,
    connection_profile_id INTEGER REFERENCES connection_profile(id) ON DELETE SET NULL,
    tags_json TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
