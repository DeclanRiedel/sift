CREATE TABLE operation_audit (
    id INTEGER PRIMARY KEY,
    at TEXT NOT NULL,
    actor_principal_id INTEGER REFERENCES principal(id) ON DELETE SET NULL,
    action TEXT NOT NULL,
    target TEXT NOT NULL,
    target_id INTEGER,
    status TEXT NOT NULL CHECK (status IN ('succeeded', 'failed')),
    result_code TEXT,
    row_count INTEGER,
    error_message TEXT
);

CREATE INDEX idx_operation_audit_at ON operation_audit(at DESC);
