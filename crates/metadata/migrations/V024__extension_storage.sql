CREATE TABLE extension_storage_namespace (
    id INTEGER PRIMARY KEY,
    extension_id TEXT NOT NULL,
    tenant_scope INTEGER NOT NULL,
    generation INTEGER NOT NULL CHECK (generation >= 0),
    schema_version INTEGER NOT NULL CHECK (schema_version >= 0),
    state TEXT NOT NULL CHECK (state IN ('active', 'staged', 'rollback', 'orphaned')),
    total_bytes INTEGER NOT NULL DEFAULT 0 CHECK (total_bytes >= 0),
    revision INTEGER NOT NULL DEFAULT 0 CHECK (revision >= 0),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    UNIQUE (extension_id, tenant_scope, generation)
);

CREATE UNIQUE INDEX idx_extension_storage_active
    ON extension_storage_namespace(extension_id, tenant_scope)
    WHERE state = 'active';

CREATE TABLE extension_storage_blob (
    sha256 TEXT PRIMARY KEY CHECK (length(sha256) = 64),
    value BLOB NOT NULL,
    byte_count INTEGER NOT NULL CHECK (byte_count >= 0),
    reference_count INTEGER NOT NULL CHECK (reference_count > 0)
);

CREATE TABLE extension_storage_entry (
    namespace_id INTEGER NOT NULL
        REFERENCES extension_storage_namespace(id) ON DELETE CASCADE,
    key TEXT NOT NULL CHECK (length(key) BETWEEN 1 AND 255),
    blob_sha256 TEXT NOT NULL
        REFERENCES extension_storage_blob(sha256) ON DELETE RESTRICT,
    revision INTEGER NOT NULL CHECK (revision > 0),
    PRIMARY KEY (namespace_id, key)
);

CREATE INDEX idx_extension_storage_entry_blob
    ON extension_storage_entry(blob_sha256);
