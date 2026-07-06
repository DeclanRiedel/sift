CREATE TABLE document (
    id INTEGER PRIMARY KEY,
    room_id INTEGER NOT NULL REFERENCES room(id) ON DELETE CASCADE,
    kind TEXT NOT NULL,
    title TEXT NOT NULL,
    crdt_type TEXT NOT NULL CHECK (crdt_type IN ('loro', 'automerge')),
    crdt_state BLOB NOT NULL,
    position INTEGER NOT NULL,
    connection_profile_id INTEGER REFERENCES connection_profile(id) ON DELETE SET NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE INDEX idx_document_room_position ON document(room_id, position);
