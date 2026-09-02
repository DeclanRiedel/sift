CREATE TABLE sql_snippet (
    id INTEGER PRIMARY KEY,
    tenant_id INTEGER NOT NULL REFERENCES tenant(id) ON DELETE CASCADE,
    workspace_id INTEGER REFERENCES workspace(id) ON DELETE CASCADE,
    owner_principal_id INTEGER REFERENCES principal(id) ON DELETE CASCADE,
    scope TEXT NOT NULL CHECK (scope IN ('personal', 'workspace', 'tenant')),
    trigger TEXT NOT NULL,
    title TEXT NOT NULL,
    description TEXT NOT NULL,
    body TEXT NOT NULL,
    dialects_json TEXT NOT NULL,
    revision INTEGER NOT NULL DEFAULT 1,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE INDEX idx_sql_snippet_tenant_scope
    ON sql_snippet(tenant_id, workspace_id, owner_principal_id, trigger);
