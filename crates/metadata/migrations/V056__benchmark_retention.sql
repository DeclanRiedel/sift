-- Policies contain no snapshot text or secret bytes.
CREATE TABLE benchmark_retention (
    tenant_id INTEGER NOT NULL,
    owner_principal_id INTEGER NOT NULL,
    max_age_days INTEGER CHECK(max_age_days BETWEEN 1 AND 36500),
    max_runs INTEGER CHECK(max_runs BETWEEN 1 AND 500),
    revision INTEGER NOT NULL CHECK(revision > 0),
    updated_at TEXT NOT NULL,
    last_checked_at TEXT,
    PRIMARY KEY(tenant_id,owner_principal_id),
    FOREIGN KEY(tenant_id,owner_principal_id) REFERENCES membership(tenant_id,principal_id) ON DELETE CASCADE
);
CREATE INDEX benchmark_retention_maintenance ON benchmark_retention(last_checked_at,tenant_id,owner_principal_id)
WHERE max_age_days IS NOT NULL OR max_runs IS NOT NULL;
CREATE INDEX vault_secret_cleanup_due ON vault_secret_cleanup_queue(namespace,julianday(not_before),secret_handle);
CREATE TRIGGER benchmark_run_delete_cleanup AFTER DELETE ON benchmark_run BEGIN
    INSERT OR IGNORE INTO vault_secret_cleanup_queue(namespace,secret_handle,reason,not_before)
    VALUES('benchmark-runs',OLD.secret_handle,'benchmark snapshot removed',strftime('%Y-%m-%dT%H:%M:%fZ','now'));
END;
CREATE TRIGGER benchmark_run_replace_cleanup AFTER UPDATE OF secret_handle ON benchmark_run
WHEN OLD.secret_handle != NEW.secret_handle BEGIN
    INSERT OR IGNORE INTO vault_secret_cleanup_queue(namespace,secret_handle,reason,not_before)
    VALUES('benchmark-runs',OLD.secret_handle,'benchmark snapshot replaced',strftime('%Y-%m-%dT%H:%M:%fZ','now'));
END;
