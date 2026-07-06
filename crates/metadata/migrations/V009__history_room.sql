ALTER TABLE query_history ADD COLUMN room_id INTEGER REFERENCES room(id) ON DELETE SET NULL;

CREATE INDEX idx_query_history_room
    ON query_history(room_id, started_at DESC);
