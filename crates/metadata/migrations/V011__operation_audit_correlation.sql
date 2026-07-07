ALTER TABLE operation_audit ADD COLUMN correlation_id TEXT;

CREATE INDEX idx_operation_audit_correlation ON operation_audit(correlation_id);
