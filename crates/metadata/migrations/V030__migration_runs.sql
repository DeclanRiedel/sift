CREATE TABLE migration_run (
    id                         TEXT PRIMARY KEY NOT NULL,
    tenant_id                  INTEGER NOT NULL REFERENCES tenant(id) ON DELETE CASCADE,
    connection_profile_id      INTEGER REFERENCES connection_profile(id) ON DELETE SET NULL,
    creator_principal_id       INTEGER NOT NULL REFERENCES principal(id) ON DELETE RESTRICT,
    plan_id                    TEXT NOT NULL,
    plan_digest                TEXT NOT NULL,
    session_id                 INTEGER NOT NULL CHECK (session_id > 0),
    connection_id              INTEGER NOT NULL CHECK (connection_id > 0),
    state                      TEXT NOT NULL,
    started_at                 TEXT NOT NULL,
    finished_at                TEXT,
    outcomes_json              TEXT NOT NULL,
    resulting_catalog_revision INTEGER,
    format_version             INTEGER NOT NULL CHECK (format_version > 0)
);

CREATE INDEX idx_migration_run_tenant_started
    ON migration_run(tenant_id, started_at DESC, id DESC);

CREATE INDEX idx_migration_run_profile_started
    ON migration_run(tenant_id, connection_profile_id, started_at DESC);
