-- Human room proposal commits survive document update-log compaction.
CREATE TABLE ai_room_query_commit (
 proposal_id TEXT PRIMARY KEY REFERENCES ai_proposal(id) ON DELETE CASCADE,
 document_id INTEGER NOT NULL,
 document_sequence INTEGER NOT NULL CHECK(document_sequence>0),
 before_sha256 TEXT NOT NULL CHECK(length(before_sha256)=64),
 after_sha256 TEXT NOT NULL CHECK(length(after_sha256)=64),
 applied_by INTEGER NOT NULL REFERENCES principal(id) ON DELETE RESTRICT,
 applied_at TEXT NOT NULL
);
