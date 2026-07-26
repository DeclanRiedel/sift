-- Phase G (Collaboration Depth): Loro-backed room documents.
--
-- `crdt_state` continues to hold the full Loro snapshot bytes. New columns track
-- the durable CRDT format, the compaction snapshot sequence, the next per-document
-- update sequence, and the encoded Loro version of the stored snapshot.
--
-- crdt_format_version 0 marks a legacy row whose `crdt_state` is raw UTF-8 text
-- rather than a genuine Loro snapshot; the server upgrades those to version 1 at
-- startup (creating a Loro document from the text, failing on invalid UTF-8).
ALTER TABLE document ADD COLUMN crdt_format_version INTEGER NOT NULL DEFAULT 0;
-- snapshot_seq is the highest update sequence covered by the stored snapshot
-- (0 = none). Sequences are 1-based so `snapshot_seq = 0` unambiguously means
-- "no updates covered yet".
ALTER TABLE document ADD COLUMN snapshot_seq INTEGER NOT NULL DEFAULT 0;
ALTER TABLE document ADD COLUMN next_update_seq INTEGER NOT NULL DEFAULT 1;
ALTER TABLE document ADD COLUMN snapshot_version BLOB NOT NULL DEFAULT x'';

-- Loro is the only CRDT backend; normalize any legacy automerge label.
UPDATE document SET crdt_type = 'loro' WHERE crdt_type <> 'loro';

-- Append-only durable update log. Each row is one committed Loro update; the
-- server acknowledges and rebroadcasts an update only after its row commits.
-- Deleting a document cascades its update log so old offline replicas cannot
-- recreate it.
CREATE TABLE document_update (
    id INTEGER PRIMARY KEY,
    document_id INTEGER NOT NULL REFERENCES document(id) ON DELETE CASCADE,
    server_seq INTEGER NOT NULL,
    update_id TEXT NOT NULL,
    replica_id TEXT NOT NULL,
    submitted_by INTEGER NOT NULL REFERENCES principal(id) ON DELETE CASCADE,
    update_bytes BLOB NOT NULL,
    decoded_len INTEGER NOT NULL,
    created_at TEXT NOT NULL,
    UNIQUE (document_id, server_seq)
);

CREATE INDEX idx_document_update_doc_seq ON document_update(document_id, server_seq);
