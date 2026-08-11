CREATE TABLE projection_binding (
    id                      INTEGER PRIMARY KEY,
    workspace_id            INTEGER NOT NULL UNIQUE REFERENCES workspace(id) ON DELETE CASCADE,
    adapter_id              TEXT NOT NULL,
    root_handle             TEXT NOT NULL UNIQUE,
    mode                    TEXT NOT NULL CHECK (mode IN ('read_only', 'read_write')),
    last_workspace_revision INTEGER,
    adapter_generation      TEXT NOT NULL,
    health                  TEXT NOT NULL CHECK (
        health IN ('ready', 'disabled', 'missing', 'read_only', 'conflicted', 'unavailable')
    ),
    revision                INTEGER NOT NULL DEFAULT 1 CHECK (revision > 0),
    created_at              TEXT NOT NULL,
    updated_at              TEXT NOT NULL
);

CREATE TABLE projection_file_state (
    binding_id       INTEGER NOT NULL REFERENCES projection_binding(id) ON DELETE CASCADE,
    node_id          INTEGER REFERENCES workspace_node(id) ON DELETE SET NULL,
    path             TEXT NOT NULL,
    path_key         TEXT NOT NULL,
    workspace_digest TEXT,
    projection_digest TEXT,
    PRIMARY KEY (binding_id, path_key)
);

CREATE INDEX idx_projection_file_node
    ON projection_file_state(binding_id, node_id);

CREATE TABLE ddl_source (
    id                 INTEGER PRIMARY KEY,
    workspace_id       INTEGER NOT NULL REFERENCES workspace(id) ON DELETE CASCADE,
    name               TEXT NOT NULL COLLATE NOCASE,
    dialect_id         TEXT NOT NULL,
    workspace_revision INTEGER NOT NULL CHECK (workspace_revision > 0),
    model_revision     INTEGER NOT NULL DEFAULT 1 CHECK (model_revision > 0),
    coverage           TEXT NOT NULL CHECK (coverage IN ('complete', 'partial', 'stale', 'invalid')),
    diagnostic_count   INTEGER NOT NULL DEFAULT 0 CHECK (diagnostic_count >= 0),
    model_json         TEXT,
    diagnostics_json   TEXT NOT NULL DEFAULT '[]',
    revision           INTEGER NOT NULL DEFAULT 1 CHECK (revision > 0),
    created_at         TEXT NOT NULL,
    updated_at         TEXT NOT NULL,
    UNIQUE (workspace_id, name)
);

CREATE TABLE ddl_source_root (
    source_id INTEGER NOT NULL REFERENCES ddl_source(id) ON DELETE CASCADE,
    node_id   INTEGER NOT NULL REFERENCES workspace_node(id) ON DELETE CASCADE,
    position  INTEGER NOT NULL CHECK (position >= 0),
    PRIMARY KEY (source_id, node_id),
    UNIQUE (source_id, position)
);

CREATE TABLE ddl_source_mapping (
    source_id             INTEGER NOT NULL REFERENCES ddl_source(id) ON DELETE CASCADE,
    connection_profile_id INTEGER NOT NULL REFERENCES connection_profile(id) ON DELETE CASCADE,
    catalog               TEXT,
    schema_name           TEXT,
    PRIMARY KEY (source_id, connection_profile_id, catalog, schema_name)
);
