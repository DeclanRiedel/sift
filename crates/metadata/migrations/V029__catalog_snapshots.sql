CREATE TABLE catalog_snapshot (
    id                    TEXT PRIMARY KEY NOT NULL,
    tenant_id             INTEGER NOT NULL REFERENCES tenant(id) ON DELETE CASCADE,
    connection_profile_id INTEGER REFERENCES connection_profile(id) ON DELETE SET NULL,
    creator_principal_id  INTEGER NOT NULL REFERENCES principal(id) ON DELETE RESTRICT,
    description           TEXT,
    graph_json            TEXT NOT NULL,
    retained_bytes        INTEGER NOT NULL CHECK (retained_bytes >= 0),
    source_revision       INTEGER NOT NULL CHECK (source_revision > 0),
    content_digest        TEXT NOT NULL,
    coverage_json         TEXT NOT NULL,
    format_version        INTEGER NOT NULL CHECK (format_version > 0),
    revision              INTEGER NOT NULL DEFAULT 0 CHECK (revision >= 0),
    created_at            TEXT NOT NULL
);

CREATE INDEX idx_catalog_snapshot_tenant_created
    ON catalog_snapshot(tenant_id, created_at DESC, id DESC);

CREATE INDEX idx_catalog_snapshot_profile
    ON catalog_snapshot(tenant_id, connection_profile_id, created_at DESC);
