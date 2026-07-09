-- Full-text search over saved_query(name, sql_text). SQLite FTS5
-- virtual table + triggers to keep it in sync with the base table.
--
-- Query semantics: use MATCH on the fts column with a normalized
-- prefix pattern (see server code). Tags are matched by the caller
-- against tags_json in SQL, not FTS5 (JSON arrays don't tokenize
-- cleanly and tag counts stay small).

CREATE VIRTUAL TABLE saved_query_fts USING fts5(
    name,
    sql_text,
    content='saved_query',
    content_rowid='id',
    tokenize='unicode61 remove_diacritics 2'
);

-- Backfill existing rows.
INSERT INTO saved_query_fts (rowid, name, sql_text)
SELECT id, name, sql_text FROM saved_query;

-- Keep FTS index in sync with the base table.
CREATE TRIGGER saved_query_ai AFTER INSERT ON saved_query BEGIN
    INSERT INTO saved_query_fts (rowid, name, sql_text)
    VALUES (new.id, new.name, new.sql_text);
END;

CREATE TRIGGER saved_query_ad AFTER DELETE ON saved_query BEGIN
    INSERT INTO saved_query_fts (saved_query_fts, rowid, name, sql_text)
    VALUES ('delete', old.id, old.name, old.sql_text);
END;

CREATE TRIGGER saved_query_au AFTER UPDATE ON saved_query BEGIN
    INSERT INTO saved_query_fts (saved_query_fts, rowid, name, sql_text)
    VALUES ('delete', old.id, old.name, old.sql_text);
    INSERT INTO saved_query_fts (rowid, name, sql_text)
    VALUES (new.id, new.name, new.sql_text);
END;
