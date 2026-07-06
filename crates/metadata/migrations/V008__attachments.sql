CREATE TABLE room_attachment (
    id INTEGER PRIMARY KEY,
    room_id INTEGER NOT NULL REFERENCES room(id) ON DELETE CASCADE,
    principal_id INTEGER NOT NULL REFERENCES principal(id) ON DELETE CASCADE,
    client_id TEXT NOT NULL,
    attached_at TEXT NOT NULL,
    detached_at TEXT
);

CREATE INDEX idx_room_attachment_active
    ON room_attachment(room_id)
    WHERE detached_at IS NULL;
