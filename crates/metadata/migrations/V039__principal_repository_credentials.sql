CREATE TABLE repository_principal_credential (
    binding_id       INTEGER NOT NULL REFERENCES repository_binding(id) ON DELETE CASCADE,
    principal_id     INTEGER NOT NULL REFERENCES principal(id) ON DELETE CASCADE,
    credential_handle TEXT NOT NULL,
    created_at       TEXT NOT NULL,
    updated_at       TEXT NOT NULL,
    PRIMARY KEY (binding_id, principal_id)
);

-- Preserve the former workspace-wide credential for the room creator only.
-- The secret remains in SecretStore; SQLite still contains only its opaque handle.
INSERT INTO repository_principal_credential (
    binding_id, principal_id, credential_handle, created_at, updated_at
)
SELECT rb.id, room.created_by, rb.credential_handle, rb.created_at, rb.updated_at
FROM repository_binding rb
JOIN workspace w ON w.id = rb.workspace_id
JOIN room ON room.id = w.room_id
WHERE rb.credential_handle IS NOT NULL;

UPDATE repository_binding SET credential_handle = NULL
WHERE credential_handle IS NOT NULL;
