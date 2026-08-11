CREATE TABLE repository_binding (
    id                    INTEGER PRIMARY KEY,
    workspace_id          INTEGER NOT NULL UNIQUE REFERENCES workspace(id) ON DELETE CASCADE,
    projection_id         INTEGER NOT NULL UNIQUE REFERENCES projection_binding(id) ON DELETE CASCADE,
    adapter_id            TEXT NOT NULL,
    repository_identity   TEXT NOT NULL,
    adapter_generation    TEXT NOT NULL,
    executable_version    TEXT NOT NULL,
    network_enabled       INTEGER NOT NULL CHECK (network_enabled IN (0, 1)),
    branch                TEXT,
    head                  TEXT,
    credential_handle     TEXT,
    revision              INTEGER NOT NULL DEFAULT 1 CHECK (revision > 0),
    created_at            TEXT NOT NULL,
    updated_at            TEXT NOT NULL
);

CREATE TABLE repository_commit (
    binding_id        INTEGER NOT NULL REFERENCES repository_binding(id) ON DELETE CASCADE,
    commit_oid        TEXT NOT NULL,
    checkpoint_id     INTEGER NOT NULL REFERENCES workspace_checkpoint(id) ON DELETE RESTRICT,
    workspace_revision INTEGER NOT NULL CHECK (workspace_revision > 0),
    created_by        INTEGER NOT NULL REFERENCES principal(id) ON DELETE RESTRICT,
    created_at        TEXT NOT NULL,
    PRIMARY KEY (binding_id, commit_oid)
);

CREATE INDEX idx_repository_commit_checkpoint
    ON repository_commit(checkpoint_id);
