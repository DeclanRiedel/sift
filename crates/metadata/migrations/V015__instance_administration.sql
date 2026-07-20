ALTER TABLE principal ADD COLUMN is_instance_admin INTEGER NOT NULL DEFAULT 0
    CHECK (is_instance_admin IN (0, 1));

CREATE INDEX idx_principal_instance_admin
    ON principal(is_instance_admin)
    WHERE is_instance_admin = 1 AND disabled_at IS NULL;
