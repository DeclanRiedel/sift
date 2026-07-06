CREATE TABLE tenant (
    id INTEGER PRIMARY KEY,
    name TEXT NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('personal', 'team')),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE TABLE principal (
    id INTEGER PRIMARY KEY,
    external_id TEXT NOT NULL UNIQUE,
    display_name TEXT NOT NULL,
    email TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE TABLE membership (
    tenant_id INTEGER NOT NULL REFERENCES tenant(id) ON DELETE CASCADE,
    principal_id INTEGER NOT NULL REFERENCES principal(id) ON DELETE CASCADE,
    role TEXT NOT NULL CHECK (role IN ('owner', 'admin', 'member', 'viewer')),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    PRIMARY KEY (tenant_id, principal_id)
);

CREATE TABLE api_token (
    id INTEGER PRIMARY KEY,
    principal_id INTEGER NOT NULL REFERENCES principal(id) ON DELETE CASCADE,
    tenant_id INTEGER REFERENCES tenant(id) ON DELETE CASCADE,
    token_hash TEXT NOT NULL UNIQUE,
    name TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    last_used_at TEXT,
    expires_at TEXT,
    revoked_at TEXT
);

CREATE TABLE principal_key (
    id INTEGER PRIMARY KEY,
    principal_id INTEGER NOT NULL REFERENCES principal(id) ON DELETE CASCADE,
    algorithm TEXT NOT NULL CHECK (algorithm IN ('ed25519')),
    public_key BLOB NOT NULL,
    fingerprint TEXT NOT NULL UNIQUE,
    label TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    last_used_at TEXT,
    revoked_at TEXT
);

CREATE TABLE keypair_challenge (
    nonce BLOB PRIMARY KEY,
    fingerprint TEXT NOT NULL,
    issued_at TEXT NOT NULL,
    expires_at TEXT NOT NULL
);

CREATE INDEX idx_api_token_principal ON api_token(principal_id);
