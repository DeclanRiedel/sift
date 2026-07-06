CREATE TABLE connection_profile (
    id INTEGER PRIMARY KEY,
    tenant_id INTEGER NOT NULL REFERENCES tenant(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    engine TEXT NOT NULL CHECK (engine IN ('postgres', 'sql_server')),
    spec_json TEXT NOT NULL,
    credential_mode TEXT NOT NULL CHECK (credential_mode IN ('shared', 'per_user', 'broker')),
    shared_secret_handle TEXT,
    tags_json TEXT NOT NULL,
    created_by INTEGER NOT NULL REFERENCES principal(id) ON DELETE RESTRICT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    UNIQUE (tenant_id, name)
);

CREATE TABLE connection_credential (
    connection_profile_id INTEGER NOT NULL REFERENCES connection_profile(id) ON DELETE CASCADE,
    principal_id INTEGER NOT NULL REFERENCES principal(id) ON DELETE CASCADE,
    secret_handle TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    PRIMARY KEY (connection_profile_id, principal_id)
);

CREATE INDEX idx_connection_profile_tenant ON connection_profile(tenant_id, name);
