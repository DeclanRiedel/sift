CREATE TABLE instance_manifest_state (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    manifest_id TEXT NOT NULL,
    configuration_digest TEXT NOT NULL,
    lock_digest TEXT NOT NULL,
    generation INTEGER NOT NULL CHECK (generation > 0),
    applied_at TEXT NOT NULL
);

CREATE TABLE instance_managed_resource (
    address TEXT PRIMARY KEY,
    manifest_id TEXT NOT NULL,
    resource_kind TEXT NOT NULL CHECK (
        resource_kind IN ('principal', 'tenant', 'membership', 'connection')
    ),
    row_id INTEGER,
    secondary_row_id INTEGER,
    desired_digest TEXT NOT NULL,
    prevent_destroy INTEGER NOT NULL DEFAULT 0 CHECK (prevent_destroy IN (0, 1)),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE INDEX idx_instance_managed_resource_kind
ON instance_managed_resource(resource_kind, row_id);

CREATE TABLE instance_credential_slot (
    slot_id TEXT PRIMARY KEY,
    credential_kind TEXT NOT NULL CHECK (
        credential_kind IN ('github-oauth-client-secret', 'postgres', 'sql-server')
    ),
    consumer_digest TEXT NOT NULL,
    secret_handle TEXT,
    readiness TEXT NOT NULL CHECK (
        readiness IN ('missing', 'ready', 'invalid')
    ),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE TABLE instance_credential_consumer (
    slot_id TEXT NOT NULL REFERENCES instance_credential_slot(slot_id) ON DELETE CASCADE,
    resource_address TEXT NOT NULL,
    consumer_digest TEXT NOT NULL,
    PRIMARY KEY (slot_id, resource_address)
);
