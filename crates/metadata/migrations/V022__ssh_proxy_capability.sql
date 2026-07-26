CREATE TABLE ssh_proxy_capability (
    capability_id TEXT PRIMARY KEY,
    capability_digest TEXT NOT NULL,
    principal_id INTEGER NOT NULL REFERENCES principal(id) ON DELETE CASCADE,
    principal_key_id INTEGER REFERENCES principal_key(id) ON DELETE CASCADE,
    instance_audience TEXT NOT NULL,
    daemon_generation TEXT NOT NULL,
    issued_at TEXT NOT NULL,
    expires_at TEXT NOT NULL,
    consumed_at TEXT,
    revoked_at TEXT
);

CREATE INDEX idx_ssh_proxy_capability_expiry
    ON ssh_proxy_capability(expires_at);
CREATE INDEX idx_ssh_proxy_capability_principal
    ON ssh_proxy_capability(principal_id, issued_at DESC);
