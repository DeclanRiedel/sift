ALTER TABLE connection_profile
ADD COLUMN provider_id TEXT NOT NULL DEFAULT 'sift/postgres';

ALTER TABLE connection_profile
ADD COLUMN configuration_json TEXT NOT NULL DEFAULT '{}';

UPDATE connection_profile
SET provider_id = CASE engine
        WHEN 'postgres' THEN 'sift/postgres'
        WHEN 'sql_server' THEN 'sift/sql-server'
    END,
    configuration_json = spec_json;

CREATE INDEX idx_connection_profile_provider
ON connection_profile(provider_id);
