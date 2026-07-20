ALTER TABLE connection_profile
    ADD COLUMN policy_json TEXT NOT NULL DEFAULT '{}';

ALTER TABLE connection_profile
    ADD COLUMN policy_revision INTEGER NOT NULL DEFAULT 0
    CHECK (policy_revision >= 0);

CREATE TABLE tenant_limit_override (
    tenant_id INTEGER PRIMARY KEY REFERENCES tenant(id) ON DELETE CASCADE,
    limits_json TEXT NOT NULL,
    updated_by INTEGER NOT NULL REFERENCES principal(id) ON DELETE RESTRICT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
