ALTER TABLE api_token ADD COLUMN token_lookup TEXT;

CREATE UNIQUE INDEX idx_api_token_lookup
    ON api_token(token_lookup)
    WHERE token_lookup IS NOT NULL;
