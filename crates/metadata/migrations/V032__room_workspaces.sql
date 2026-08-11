CREATE TABLE workspace (
    id         INTEGER PRIMARY KEY,
    room_id    INTEGER NOT NULL REFERENCES room(id) ON DELETE CASCADE,
    name       TEXT NOT NULL COLLATE NOCASE,
    revision   INTEGER NOT NULL DEFAULT 1 CHECK (revision > 0),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    UNIQUE (room_id, name)
);

CREATE INDEX idx_workspace_room ON workspace(room_id, id);

-- Existing `document.id` is an INTEGER PRIMARY KEY and can recycle the highest
-- deleted id. A tiny AUTOINCREMENT allocator gives every old and new document
-- creation path one monotonic source without rebuilding the heavily-referenced
-- document table.
CREATE TABLE document_id_allocator (
    id INTEGER PRIMARY KEY AUTOINCREMENT
);
INSERT INTO document_id_allocator (id)
    SELECT MAX(id) FROM document HAVING MAX(id) IS NOT NULL;
DELETE FROM document_id_allocator;

CREATE TABLE workspace_node (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    workspace_id INTEGER NOT NULL REFERENCES workspace(id) ON DELETE CASCADE,
    parent_id    INTEGER,
    path         TEXT NOT NULL,
    path_key     TEXT NOT NULL,
    kind         TEXT NOT NULL CHECK (kind IN ('folder', 'sql_document')),
    document_id  INTEGER UNIQUE REFERENCES document(id) ON DELETE CASCADE,
    revision     INTEGER NOT NULL DEFAULT 1 CHECK (revision > 0),
    created_at   TEXT NOT NULL,
    updated_at   TEXT NOT NULL,
    UNIQUE (workspace_id, path_key),
    UNIQUE (id, workspace_id),
    FOREIGN KEY (parent_id, workspace_id)
        REFERENCES workspace_node(id, workspace_id) ON DELETE CASCADE,
    CHECK (
        (kind = 'folder' AND document_id IS NULL) OR
        (kind = 'sql_document' AND document_id IS NOT NULL)
    )
);

CREATE INDEX idx_workspace_node_parent
    ON workspace_node(workspace_id, parent_id, id);

-- A workspace node owns its SQL document. Cascading in the other direction
-- keeps a node from surviving direct document deletion; this trigger closes
-- the ownership loop when a node/subtree/workspace is deleted.
CREATE TRIGGER workspace_node_delete_document
AFTER DELETE ON workspace_node
WHEN OLD.document_id IS NOT NULL
BEGIN
    DELETE FROM document WHERE id = OLD.document_id;
END;

CREATE TRIGGER workspace_delete_documents
BEFORE DELETE ON workspace
BEGIN
    DELETE FROM document
    WHERE id IN (
        SELECT document_id FROM workspace_node
        WHERE workspace_id = OLD.id AND document_id IS NOT NULL
    );
END;

CREATE TABLE workspace_content_blob (
    digest           TEXT PRIMARY KEY,
    snapshot_bytes   BLOB NOT NULL,
    snapshot_version BLOB NOT NULL,
    retained_bytes   INTEGER NOT NULL CHECK (retained_bytes >= 0),
    created_at       TEXT NOT NULL
);

CREATE TABLE workspace_checkpoint (
    id                 INTEGER PRIMARY KEY,
    workspace_id       INTEGER NOT NULL REFERENCES workspace(id) ON DELETE CASCADE,
    workspace_revision INTEGER NOT NULL CHECK (workspace_revision > 0),
    reason             TEXT NOT NULL CHECK (
        reason IN ('automatic', 'named', 'before_reconcile', 'before_run', 'before_vcs')
    ),
    name               TEXT,
    created_by         INTEGER NOT NULL REFERENCES principal(id) ON DELETE RESTRICT,
    created_at         TEXT NOT NULL
);

CREATE INDEX idx_workspace_checkpoint_page
    ON workspace_checkpoint(workspace_id, id DESC);

CREATE TABLE workspace_checkpoint_node (
    checkpoint_id INTEGER NOT NULL REFERENCES workspace_checkpoint(id) ON DELETE CASCADE,
    node_id       INTEGER NOT NULL,
    parent_id     INTEGER,
    path          TEXT NOT NULL,
    kind          TEXT NOT NULL CHECK (kind IN ('folder', 'sql_document')),
    content_digest TEXT REFERENCES workspace_content_blob(digest) ON DELETE RESTRICT,
    PRIMARY KEY (checkpoint_id, node_id),
    CHECK (
        (kind = 'folder' AND content_digest IS NULL) OR
        (kind = 'sql_document' AND content_digest IS NOT NULL)
    )
);

CREATE INDEX idx_workspace_checkpoint_node_path
    ON workspace_checkpoint_node(checkpoint_id, path);
