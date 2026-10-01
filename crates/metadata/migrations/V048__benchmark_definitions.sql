-- Names, SQL and configuration live in SecretStore. This table only indexes
-- owner-scoped opaque handles and revision/retention metadata.
CREATE TABLE benchmark_definition (
    id TEXT PRIMARY KEY,
    tenant_id INTEGER NOT NULL REFERENCES tenant(id) ON DELETE CASCADE,
    owner_principal_id INTEGER NOT NULL REFERENCES principal(id) ON DELETE CASCADE,
    revision INTEGER NOT NULL CHECK (revision > 0),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    secret_handle TEXT NOT NULL UNIQUE,
    payload_bytes INTEGER NOT NULL CHECK (payload_bytes > 0)
);
CREATE INDEX benchmark_definition_owner ON benchmark_definition(tenant_id, owner_principal_id, updated_at DESC, id DESC);
