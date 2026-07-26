-- A shared room can bind exactly one connection profile (server-owned
-- connection, ADR-036). Deleting the profile unbinds the room rather than
-- cascading the room away.
ALTER TABLE room ADD COLUMN bound_connection_profile_id INTEGER
    REFERENCES connection_profile(id) ON DELETE SET NULL;
