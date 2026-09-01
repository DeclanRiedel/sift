CREATE TABLE vault_connection_binding (
    item_id               INTEGER PRIMARY KEY REFERENCES vault_item(id) ON DELETE CASCADE,
    connection_profile_id INTEGER NOT NULL UNIQUE REFERENCES connection_profile(id) ON DELETE CASCADE,
    created_at            TEXT NOT NULL
);

CREATE INDEX idx_vault_connection_binding_profile
    ON vault_connection_binding(connection_profile_id);
