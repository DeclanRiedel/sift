ALTER TABLE oauth_login_attempt ADD COLUMN handoff_lookup TEXT;
ALTER TABLE oauth_login_attempt ADD COLUMN handoff_digest TEXT;
ALTER TABLE oauth_login_attempt ADD COLUMN result_principal_id INTEGER REFERENCES principal(id);
ALTER TABLE oauth_login_attempt ADD COLUMN completed_at TEXT;
ALTER TABLE oauth_login_attempt ADD COLUMN claimed_at TEXT;

CREATE UNIQUE INDEX idx_oauth_handoff_lookup
    ON oauth_login_attempt(handoff_lookup)
    WHERE handoff_lookup IS NOT NULL;
