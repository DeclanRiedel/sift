-- Sensitive snapshots live in SecretStore, never in SQLite.
CREATE TABLE benchmark_run (
    id TEXT PRIMARY KEY,
    tenant_id INTEGER NOT NULL REFERENCES tenant(id) ON DELETE CASCADE,
    owner_principal_id INTEGER NOT NULL REFERENCES principal(id) ON DELETE CASCADE,
    run_id TEXT NOT NULL,
    saved_at TEXT NOT NULL,
    secret_handle TEXT NOT NULL UNIQUE,
    payload_bytes INTEGER NOT NULL CHECK (payload_bytes > 0),
    UNIQUE(tenant_id, owner_principal_id, run_id)
);
CREATE INDEX benchmark_run_owner ON benchmark_run(tenant_id, owner_principal_id, saved_at DESC, id DESC);
