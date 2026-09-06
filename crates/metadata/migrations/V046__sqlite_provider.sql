-- Widen provider discriminators without dropping the parent table or cascading
-- its profile/credential/vault references. Every existing value is preserved.
ALTER TABLE connection_profile ADD COLUMN sift_engine_copy TEXT;
UPDATE connection_profile SET sift_engine_copy = engine;
ALTER TABLE connection_profile DROP COLUMN engine;
ALTER TABLE connection_profile ADD COLUMN engine TEXT NOT NULL DEFAULT 'postgres'
    CHECK (engine IN ('postgres', 'sql_server', 'sqlite'));
UPDATE connection_profile SET engine = sift_engine_copy;
UPDATE connection_profile SET sift_engine_copy = semantic_engine;
ALTER TABLE connection_profile DROP COLUMN semantic_engine;
ALTER TABLE connection_profile ADD COLUMN semantic_engine TEXT
    CHECK (semantic_engine IS NULL OR semantic_engine IN ('postgres', 'sql_server', 'sqlite'));
UPDATE connection_profile SET semantic_engine = sift_engine_copy;
ALTER TABLE connection_profile DROP COLUMN sift_engine_copy;
