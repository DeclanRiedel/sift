-- Record which principal bound the room's connection (ADR-037). The binder's
-- identity is the provenance the server-owned room connection opens under.
-- Cleared (SET NULL) if the principal is removed.
ALTER TABLE room ADD COLUMN bound_connection_by INTEGER
    REFERENCES principal(id) ON DELETE SET NULL;
