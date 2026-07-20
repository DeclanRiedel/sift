CREATE TABLE password_reset_token (
    id INTEGER PRIMARY KEY,
    auth_identity_id INTEGER NOT NULL REFERENCES auth_identity(id) ON DELETE CASCADE,
    token_lookup TEXT NOT NULL UNIQUE,
    token_digest TEXT NOT NULL,
    created_by INTEGER NOT NULL REFERENCES principal(id),
    created_at TEXT NOT NULL,
    expires_at TEXT NOT NULL,
    consumed_at TEXT,
    revoked_at TEXT
);

CREATE INDEX idx_password_reset_identity
    ON password_reset_token(auth_identity_id, created_at DESC);
