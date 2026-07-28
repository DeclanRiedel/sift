CREATE TABLE operation_approval (
    id TEXT PRIMARY KEY,
    principal_id INTEGER NOT NULL REFERENCES principal(id) ON DELETE CASCADE,
    operation_id TEXT NOT NULL,
    context_fingerprint TEXT NOT NULL CHECK (length(context_fingerprint) = 64),
    input_fingerprint TEXT NOT NULL CHECK (length(input_fingerprint) = 64),
    expires_at TEXT NOT NULL,
    approved_at TEXT,
    consumed_at TEXT,
    revision INTEGER NOT NULL DEFAULT 0 CHECK (revision >= 0),
    created_at TEXT NOT NULL
);

CREATE INDEX idx_operation_approval_principal_expiry
    ON operation_approval(principal_id, expires_at);
