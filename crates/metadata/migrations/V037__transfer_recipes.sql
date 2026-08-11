CREATE TABLE transfer_recipe (
    id             INTEGER PRIMARY KEY,
    workspace_id   INTEGER NOT NULL REFERENCES workspace(id) ON DELETE CASCADE,
    name           TEXT NOT NULL,
    direction      TEXT NOT NULL CHECK (direction IN ('import', 'export')),
    source_json    TEXT NOT NULL,
    sink_json      TEXT NOT NULL,
    format_id      TEXT NOT NULL,
    format_version TEXT NOT NULL,
    options_json   TEXT NOT NULL,
    revision       INTEGER NOT NULL DEFAULT 1 CHECK (revision > 0),
    created_at     TEXT NOT NULL,
    updated_at     TEXT NOT NULL,
    UNIQUE (workspace_id, name)
);

CREATE INDEX idx_transfer_recipe_workspace ON transfer_recipe(workspace_id, id);

CREATE TABLE workspace_artifact (
    id           INTEGER PRIMARY KEY,
    workspace_id INTEGER NOT NULL REFERENCES workspace(id) ON DELETE CASCADE,
    content_type TEXT NOT NULL,
    digest       TEXT NOT NULL,
    byte_len     INTEGER NOT NULL CHECK (byte_len >= 0),
    content      BLOB NOT NULL,
    expires_at   TEXT,
    pinned       INTEGER NOT NULL DEFAULT 0 CHECK (pinned IN (0, 1)),
    created_at   TEXT NOT NULL
);

CREATE INDEX idx_workspace_artifact_workspace ON workspace_artifact(workspace_id, id);
