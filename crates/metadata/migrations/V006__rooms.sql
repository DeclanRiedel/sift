DROP TABLE IF EXISTS tab;
DROP TABLE IF EXISTS session_snapshot;
DROP TABLE IF EXISTS workspace;

CREATE TABLE room (
    id INTEGER PRIMARY KEY,
    tenant_id INTEGER NOT NULL REFERENCES tenant(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('personal', 'shared')),
    created_by INTEGER NOT NULL REFERENCES principal(id) ON DELETE RESTRICT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE TABLE room_member (
    room_id INTEGER NOT NULL REFERENCES room(id) ON DELETE CASCADE,
    principal_id INTEGER NOT NULL REFERENCES principal(id) ON DELETE CASCADE,
    role TEXT NOT NULL CHECK (role IN ('owner', 'editor', 'viewer')),
    joined_at TEXT NOT NULL,
    PRIMARY KEY (room_id, principal_id)
);

CREATE INDEX idx_room_tenant ON room(tenant_id);
CREATE INDEX idx_room_member_principal ON room_member(principal_id);
