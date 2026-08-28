CREATE TABLE database_change_ledger (
    id                       INTEGER PRIMARY KEY AUTOINCREMENT,
    at                       TEXT NOT NULL,
    tenant_id                INTEGER,
    room_id                  INTEGER,
    connection_profile_id    INTEGER,
    database_target          TEXT,
    operation_kind           TEXT NOT NULL,
    affected_object          TEXT,
    row_count                INTEGER,
    sql_fingerprint          TEXT,
    row_identity_fingerprint TEXT,
    transaction_id           TEXT,
    correlation_id           TEXT,
    workspace_id             INTEGER,
    workspace_revision       INTEGER,
    checkpoint_id            INTEGER,
    workspace_path           TEXT,
    git_commit               TEXT,
    source_workflow          TEXT NOT NULL,
    authored_by              INTEGER,
    approved_by              INTEGER,
    executed_by              INTEGER NOT NULL,
    database_actor           TEXT,
    outcome                  TEXT NOT NULL,
    result_code              TEXT,
    identity_source          TEXT NOT NULL DEFAULT 'sift',
    identity_confidence      TEXT NOT NULL DEFAULT 'authenticated',
    previous_hash            TEXT NOT NULL,
    entry_hash               TEXT NOT NULL UNIQUE,
    CHECK (outcome IN ('committed', 'failed', 'conflicted', 'rolled_back', 'partial')),
    CHECK (identity_source IN ('sift', 'postgres', 'sql_server', 'external')),
    CHECK (identity_confidence IN ('authenticated', 'database_native', 'mapped', 'unknown')),
    CHECK (workspace_revision IS NULL OR workspace_revision >= 0),
    CHECK (row_count IS NULL OR row_count >= 0)
);

CREATE INDEX idx_database_change_ledger_scope
    ON database_change_ledger(tenant_id, at DESC, id DESC);
CREATE INDEX idx_database_change_ledger_target
    ON database_change_ledger(connection_profile_id, database_target, affected_object, id DESC);
CREATE INDEX idx_database_change_ledger_actor
    ON database_change_ledger(executed_by, operation_kind, id DESC);
CREATE INDEX idx_database_change_ledger_commit
    ON database_change_ledger(git_commit, id DESC)
    WHERE git_commit IS NOT NULL;

CREATE TRIGGER database_change_ledger_no_update
BEFORE UPDATE ON database_change_ledger
BEGIN
    SELECT RAISE(ABORT, 'database change ledger is append-only');
END;

CREATE TRIGGER database_change_ledger_no_delete
BEFORE DELETE ON database_change_ledger
BEGIN
    SELECT RAISE(ABORT, 'database change ledger is append-only');
END;

CREATE TABLE database_change_ledger_policy (
    tenant_id           INTEGER PRIMARY KEY,
    retention_days      INTEGER NOT NULL DEFAULT 2555,
    external_sink       TEXT,
    updated_by          INTEGER NOT NULL,
    updated_at          TEXT NOT NULL,
    CHECK (retention_days >= 30)
);

-- Explain plans remain redacted, but retain the same validated artifact
-- provenance as executions so regressions can be traced to a reviewed file.
ALTER TABLE plan_capture ADD COLUMN source_json TEXT;
