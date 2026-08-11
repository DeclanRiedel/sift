CREATE TABLE plan_capture (
    id                    TEXT PRIMARY KEY NOT NULL,
    tenant_id             INTEGER NOT NULL REFERENCES tenant(id) ON DELETE CASCADE,
    connection_profile_id INTEGER REFERENCES connection_profile(id) ON DELETE SET NULL,
    creator_principal_id  INTEGER NOT NULL REFERENCES principal(id) ON DELETE RESTRICT,
    provider_json         TEXT NOT NULL,
    server_version        TEXT NOT NULL,
    engine                TEXT NOT NULL,
    source_digest         TEXT NOT NULL,
    document_revision     INTEGER NOT NULL CHECK (document_revision > 0),
    statement_id          TEXT NOT NULL,
    statement_fingerprint TEXT NOT NULL,
    catalog_revision      INTEGER NOT NULL CHECK (catalog_revision > 0),
    analyzed              INTEGER NOT NULL,
    captured_at           TEXT NOT NULL,
    duration_ms           INTEGER NOT NULL CHECK (duration_ms >= 0),
    root_json             TEXT NOT NULL,
    warnings_json         TEXT NOT NULL,
    complete              INTEGER NOT NULL,
    revision              INTEGER NOT NULL DEFAULT 0 CHECK (revision >= 0)
);

CREATE INDEX idx_plan_capture_tenant_source_time
    ON plan_capture(tenant_id, source_digest, captured_at DESC, id DESC);

CREATE INDEX idx_plan_capture_profile_time
    ON plan_capture(tenant_id, connection_profile_id, captured_at DESC);
