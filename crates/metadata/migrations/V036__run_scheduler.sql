CREATE TABLE run_schedule (
    id                    INTEGER PRIMARY KEY,
    configuration_id      INTEGER NOT NULL REFERENCES run_configuration(id) ON DELETE CASCADE,
    owner_principal_id    INTEGER NOT NULL REFERENCES principal(id) ON DELETE RESTRICT,
    cron                  TEXT NOT NULL,
    timezone              TEXT NOT NULL,
    misfire_policy        TEXT NOT NULL CHECK (misfire_policy IN ('skip', 'run_once')),
    concurrency_json      TEXT NOT NULL,
    enabled               INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0, 1)),
    next_fire_at          TEXT,
    revision              INTEGER NOT NULL DEFAULT 1 CHECK (revision > 0),
    created_at            TEXT NOT NULL,
    updated_at            TEXT NOT NULL
);

CREATE INDEX idx_run_schedule_due
    ON run_schedule(enabled, next_fire_at, id);

CREATE TABLE schedule_occurrence (
    id                    INTEGER PRIMARY KEY,
    schedule_id           INTEGER NOT NULL REFERENCES run_schedule(id) ON DELETE CASCADE,
    scheduled_for         TEXT NOT NULL,
    state                 TEXT NOT NULL CHECK (state IN ('queued', 'leased', 'running', 'succeeded', 'failed', 'blocked', 'rejected', 'outcome_unknown')),
    run_id                INTEGER REFERENCES run_execution(id) ON DELETE RESTRICT,
    lease_owner           TEXT,
    lease_expires_at      TEXT,
    error_code            TEXT,
    created_at            TEXT NOT NULL,
    finished_at           TEXT,
    UNIQUE (schedule_id, scheduled_for)
);

CREATE INDEX idx_schedule_occurrence_state
    ON schedule_occurrence(state, lease_expires_at, id);
