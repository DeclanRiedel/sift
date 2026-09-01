CREATE TABLE vault (
    id                 INTEGER PRIMARY KEY AUTOINCREMENT,
    tenant_id          INTEGER NOT NULL REFERENCES tenant(id) ON DELETE CASCADE,
    scope              TEXT NOT NULL CHECK (scope IN ('personal', 'team')),
    owner_principal_id INTEGER REFERENCES principal(id) ON DELETE CASCADE,
    name               TEXT NOT NULL,
    revision           INTEGER NOT NULL DEFAULT 1 CHECK (revision > 0),
    created_by         INTEGER NOT NULL REFERENCES principal(id),
    created_at         TEXT NOT NULL,
    updated_at         TEXT NOT NULL,
    CHECK ((scope = 'personal') = (owner_principal_id IS NOT NULL))
);

CREATE UNIQUE INDEX idx_vault_personal_owner
    ON vault(tenant_id, owner_principal_id)
    WHERE scope = 'personal';
CREATE UNIQUE INDEX idx_vault_team_name
    ON vault(tenant_id, name)
    WHERE scope = 'team';

CREATE TABLE vault_grant (
    vault_id       INTEGER NOT NULL REFERENCES vault(id) ON DELETE CASCADE,
    principal_id   INTEGER NOT NULL REFERENCES principal(id) ON DELETE CASCADE,
    can_inspect    INTEGER NOT NULL CHECK (can_inspect IN (0, 1)),
    can_use        INTEGER NOT NULL CHECK (can_use IN (0, 1)),
    can_reveal     INTEGER NOT NULL CHECK (can_reveal IN (0, 1)),
    can_edit       INTEGER NOT NULL CHECK (can_edit IN (0, 1)),
    can_manage     INTEGER NOT NULL CHECK (can_manage IN (0, 1)),
    revision       INTEGER NOT NULL DEFAULT 1 CHECK (revision > 0),
    created_by     INTEGER NOT NULL REFERENCES principal(id),
    created_at     TEXT NOT NULL,
    updated_at     TEXT NOT NULL,
    PRIMARY KEY (vault_id, principal_id)
);

CREATE TABLE vault_item (
    id             INTEGER PRIMARY KEY AUTOINCREMENT,
    vault_id       INTEGER NOT NULL REFERENCES vault(id) ON DELETE CASCADE,
    kind           TEXT NOT NULL CHECK (kind IN ('connection', 'login', 'token', 'secure_note')),
    label          TEXT NOT NULL,
    metadata_json  TEXT NOT NULL,
    head_version   INTEGER NOT NULL DEFAULT 1 CHECK (head_version > 0),
    revision       INTEGER NOT NULL DEFAULT 1 CHECK (revision > 0),
    created_by     INTEGER NOT NULL REFERENCES principal(id),
    created_at     TEXT NOT NULL,
    updated_at     TEXT NOT NULL,
    UNIQUE (vault_id, label)
);

CREATE TABLE vault_item_version (
    item_id               INTEGER NOT NULL REFERENCES vault_item(id) ON DELETE CASCADE,
    version               INTEGER NOT NULL CHECK (version > 0),
    parent_version        INTEGER,
    metadata_json         TEXT NOT NULL,
    secret_handle         TEXT,
    secret_schema_version INTEGER NOT NULL DEFAULT 1,
    change_summary        TEXT NOT NULL,
    created_by            INTEGER NOT NULL REFERENCES principal(id),
    created_at            TEXT NOT NULL,
    PRIMARY KEY (item_id, version)
);

CREATE TABLE vault_secret_cleanup_queue (
    namespace    TEXT NOT NULL,
    secret_handle TEXT NOT NULL,
    reason       TEXT NOT NULL,
    not_before   TEXT NOT NULL,
    attempts     INTEGER NOT NULL DEFAULT 0,
    last_error   TEXT,
    PRIMARY KEY (namespace, secret_handle)
);

CREATE INDEX idx_vault_tenant ON vault(tenant_id, scope, name);
CREATE INDEX idx_vault_item_vault ON vault_item(vault_id, label);
