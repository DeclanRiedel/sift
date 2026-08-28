CREATE TABLE repository_hosting_credential (
    binding_id        INTEGER NOT NULL REFERENCES repository_binding(id) ON DELETE CASCADE,
    principal_id      INTEGER NOT NULL REFERENCES principal(id) ON DELETE CASCADE,
    credential_handle TEXT NOT NULL,
    created_at        TEXT NOT NULL,
    updated_at        TEXT NOT NULL,
    PRIMARY KEY (binding_id, principal_id)
);
