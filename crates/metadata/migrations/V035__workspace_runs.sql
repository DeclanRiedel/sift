CREATE TABLE run_configuration (
    id                    INTEGER PRIMARY KEY,
    workspace_id          INTEGER NOT NULL REFERENCES workspace(id) ON DELETE CASCADE,
    name                  TEXT NOT NULL,
    scripts_json          TEXT NOT NULL,
    connection_profile_id INTEGER NOT NULL REFERENCES connection_profile(id) ON DELETE RESTRICT,
    target_schema         TEXT,
    variables_json        TEXT NOT NULL,
    pre_tasks_json        TEXT NOT NULL,
    transaction_policy    TEXT NOT NULL CHECK (transaction_policy IN ('none', 'per_script', 'all_scripts')),
    error_policy          TEXT NOT NULL CHECK (error_policy IN ('stop', 'continue')),
    revision              INTEGER NOT NULL DEFAULT 1 CHECK (revision > 0),
    created_at            TEXT NOT NULL,
    updated_at            TEXT NOT NULL,
    UNIQUE (workspace_id, name)
);

CREATE INDEX idx_run_configuration_workspace
    ON run_configuration(workspace_id, id);

CREATE TABLE run_execution (
    id                    INTEGER PRIMARY KEY,
    configuration_id      INTEGER NOT NULL REFERENCES run_configuration(id) ON DELETE RESTRICT,
    trigger_kind          TEXT NOT NULL CHECK (trigger_kind IN ('interactive', 'schedule', 'rerun')),
    actor_principal_id    INTEGER NOT NULL REFERENCES principal(id) ON DELETE RESTRICT,
    state                 TEXT NOT NULL CHECK (state IN ('queued', 'admitted', 'preparing', 'running', 'succeeded', 'failed', 'cancelled', 'outcome_unknown', 'blocked', 'rejected')),
    manifest_json         TEXT NOT NULL,
    resolved_scripts_json TEXT NOT NULL,
    previous_run_id       INTEGER REFERENCES run_execution(id) ON DELETE RESTRICT,
    cancellation_requested INTEGER NOT NULL DEFAULT 0 CHECK (cancellation_requested IN (0, 1)),
    revision              INTEGER NOT NULL DEFAULT 1 CHECK (revision > 0),
    created_at            TEXT NOT NULL,
    started_at            TEXT,
    finished_at           TEXT
);

CREATE INDEX idx_run_execution_configuration
    ON run_execution(configuration_id, id DESC);

CREATE TABLE run_step_result (
    run_id          INTEGER NOT NULL REFERENCES run_execution(id) ON DELETE CASCADE,
    ordinal         INTEGER NOT NULL CHECK (ordinal >= 0),
    node_id         INTEGER NOT NULL REFERENCES workspace_node(id) ON DELETE RESTRICT,
    state           TEXT NOT NULL CHECK (state IN ('pending', 'running', 'succeeded', 'failed', 'cancelled')),
    row_count       INTEGER,
    error_code      TEXT,
    started_at      TEXT,
    finished_at     TEXT,
    PRIMARY KEY (run_id, ordinal)
);

CREATE TABLE run_log (
    run_id      INTEGER NOT NULL REFERENCES run_execution(id) ON DELETE CASCADE,
    sequence    INTEGER NOT NULL CHECK (sequence > 0),
    level       TEXT NOT NULL CHECK (level IN ('info', 'warning', 'error')),
    message     TEXT NOT NULL,
    created_at  TEXT NOT NULL,
    PRIMARY KEY (run_id, sequence)
);
