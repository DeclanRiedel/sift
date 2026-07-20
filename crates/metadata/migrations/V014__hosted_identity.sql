ALTER TABLE principal ADD COLUMN avatar_url TEXT;
ALTER TABLE principal ADD COLUMN disabled_at TEXT;

CREATE TABLE auth_identity (
    id INTEGER PRIMARY KEY,
    principal_id INTEGER NOT NULL REFERENCES principal(id) ON DELETE CASCADE,
    method TEXT NOT NULL CHECK (
        method IN ('local_bypass', 'password', 'github', 'oidc', 'legacy')
    ),
    issuer TEXT NOT NULL,
    subject TEXT NOT NULL,
    provider_login TEXT,
    credential_handle TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    last_used_at TEXT,
    disabled_at TEXT,
    UNIQUE(method, issuer, subject)
);

CREATE INDEX idx_auth_identity_principal ON auth_identity(principal_id);
CREATE UNIQUE INDEX idx_auth_identity_password_login
    ON auth_identity(subject)
    WHERE method = 'password' AND disabled_at IS NULL;

-- Preserve all existing principals without changing their ids. The local
-- bootstrap identity gets its explicit method; pre-Phase-E test/dev principals
-- remain resolvable as legacy identities until an admin links a real method.
INSERT INTO auth_identity
    (principal_id, method, issuer, subject, provider_login, credential_handle,
     created_at, updated_at)
SELECT id,
       CASE WHEN external_id = 'local:1' THEN 'local_bypass' ELSE 'legacy' END,
       'sift', external_id, NULL, NULL, created_at, updated_at
FROM principal;

CREATE TABLE github_allowlist (
    id INTEGER PRIMARY KEY,
    normalized_login TEXT NOT NULL,
    target_principal_id INTEGER REFERENCES principal(id) ON DELETE CASCADE,
    created_by INTEGER NOT NULL REFERENCES principal(id) ON DELETE RESTRICT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    consumed_at TEXT,
    revoked_at TEXT
);

CREATE UNIQUE INDEX idx_github_allowlist_active_login
    ON github_allowlist(normalized_login)
    WHERE consumed_at IS NULL AND revoked_at IS NULL;

CREATE TABLE auth_session (
    id TEXT PRIMARY KEY,
    principal_id INTEGER NOT NULL REFERENCES principal(id) ON DELETE CASCADE,
    refresh_family_id TEXT NOT NULL,
    client_kind TEXT NOT NULL CHECK (client_kind IN ('native', 'web', 'keypair')),
    client_label TEXT,
    created_at TEXT NOT NULL,
    last_used_at TEXT,
    expires_at TEXT NOT NULL,
    revoked_at TEXT,
    revocation_reason TEXT
);

CREATE INDEX idx_auth_session_principal ON auth_session(principal_id, created_at DESC);
CREATE INDEX idx_auth_session_family ON auth_session(refresh_family_id);

CREATE TABLE auth_access_token (
    id INTEGER PRIMARY KEY,
    auth_session_id TEXT NOT NULL REFERENCES auth_session(id) ON DELETE CASCADE,
    token_lookup TEXT NOT NULL UNIQUE,
    token_digest TEXT NOT NULL,
    created_at TEXT NOT NULL,
    expires_at TEXT NOT NULL,
    revoked_at TEXT
);

CREATE INDEX idx_auth_access_session ON auth_access_token(auth_session_id);

CREATE TABLE auth_refresh_token (
    id INTEGER PRIMARY KEY,
    auth_session_id TEXT NOT NULL REFERENCES auth_session(id) ON DELETE CASCADE,
    family_id TEXT NOT NULL,
    parent_id INTEGER REFERENCES auth_refresh_token(id) ON DELETE SET NULL,
    token_lookup TEXT NOT NULL UNIQUE,
    token_digest TEXT NOT NULL,
    created_at TEXT NOT NULL,
    expires_at TEXT NOT NULL,
    consumed_at TEXT,
    replaced_by_id INTEGER REFERENCES auth_refresh_token(id) ON DELETE SET NULL,
    revoked_at TEXT
);

CREATE INDEX idx_auth_refresh_session ON auth_refresh_token(auth_session_id);
CREATE INDEX idx_auth_refresh_family ON auth_refresh_token(family_id);

CREATE TABLE oauth_login_attempt (
    id TEXT PRIMARY KEY,
    provider TEXT NOT NULL CHECK (provider IN ('github', 'oidc')),
    state_lookup TEXT NOT NULL UNIQUE,
    state_digest TEXT NOT NULL,
    pkce_verifier_handle TEXT NOT NULL,
    client_kind TEXT NOT NULL CHECK (client_kind IN ('native', 'web')),
    client_redirect_uri TEXT,
    created_at TEXT NOT NULL,
    expires_at TEXT NOT NULL,
    consumed_at TEXT
);

CREATE TABLE tenant_invitation (
    id INTEGER PRIMARY KEY,
    tenant_id INTEGER NOT NULL REFERENCES tenant(id) ON DELETE CASCADE,
    intended_role TEXT NOT NULL CHECK (intended_role IN ('owner', 'admin', 'member', 'viewer')),
    created_by INTEGER NOT NULL REFERENCES principal(id) ON DELETE RESTRICT,
    target_principal_id INTEGER REFERENCES principal(id) ON DELETE CASCADE,
    token_lookup TEXT NOT NULL UNIQUE,
    token_digest TEXT NOT NULL,
    created_at TEXT NOT NULL,
    expires_at TEXT NOT NULL,
    consumed_at TEXT,
    revoked_at TEXT
);

CREATE INDEX idx_tenant_invitation_tenant ON tenant_invitation(tenant_id, created_at DESC);

ALTER TABLE keypair_challenge ADD COLUMN principal_key_id INTEGER REFERENCES principal_key(id) ON DELETE CASCADE;
ALTER TABLE keypair_challenge ADD COLUMN consumed_at TEXT;
CREATE INDEX idx_keypair_challenge_key ON keypair_challenge(principal_key_id, expires_at);
