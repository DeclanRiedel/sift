ALTER TABLE connection_profile
ADD COLUMN semantic_engine TEXT
CHECK (semantic_engine IS NULL OR semantic_engine IN ('postgres', 'sql_server'));

UPDATE connection_profile
SET semantic_engine = engine;
