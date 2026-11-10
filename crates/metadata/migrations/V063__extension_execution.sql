-- External execution facts are preserved across tenant metadata rollback.
-- No arguments, result bodies, credentials or native diagnostic text live here.
CREATE TABLE extension_execution (
 id TEXT PRIMARY KEY,
 request_id TEXT NOT NULL,
 actor_principal_id INTEGER NOT NULL REFERENCES principal(id) ON DELETE RESTRICT,
 tenant_id INTEGER,
 approval_id TEXT UNIQUE,
 operation_id TEXT NOT NULL,
 operation_json TEXT NOT NULL CHECK(length(operation_json)<=8192),
 context_json TEXT NOT NULL CHECK(length(context_json)<=4096),
 context_fingerprint TEXT NOT NULL CHECK(length(context_fingerprint)=64),
 input_fingerprint TEXT NOT NULL CHECK(length(input_fingerprint)=64),
 correlation_id TEXT NOT NULL CHECK(length(correlation_id)<=256),
 state TEXT NOT NULL CHECK(state IN ('claimed','dispatching','acknowledged','refused','outcome_unknown')),
 created_at TEXT NOT NULL,
 deadline_unix_ms INTEGER NOT NULL,
 observed_at TEXT,
 frame_started INTEGER CHECK(frame_started IN (0,1)),
 response_kind TEXT CHECK(response_kind IN ('result','error')),
 late_response INTEGER NOT NULL DEFAULT 0 CHECK(late_response IN (0,1)),
 result_code TEXT CHECK(result_code IN ('acknowledged','reported_error','admission_refused','request_timed_out','invalid_response','transport_failed','owner_interrupted','delivery_denied')),
 audit_id INTEGER UNIQUE,
 UNIQUE(actor_principal_id,request_id),
 CHECK ((state IN ('claimed','dispatching') AND observed_at IS NULL AND audit_id IS NULL AND frame_started IS NULL AND response_kind IS NULL AND late_response=0 AND result_code IS NULL)
 OR (state='acknowledged' AND observed_at IS NOT NULL AND audit_id IS NOT NULL AND frame_started=1 AND response_kind IS NOT NULL AND result_code IS NOT NULL)
 OR (state='refused' AND observed_at IS NOT NULL AND audit_id IS NOT NULL AND frame_started=0 AND response_kind IS NULL AND late_response=0 AND result_code IS NOT NULL)
 OR (state='outcome_unknown' AND observed_at IS NOT NULL AND audit_id IS NOT NULL AND (frame_started IS NULL OR frame_started=1) AND response_kind IS NULL AND result_code IS NOT NULL))
);
CREATE INDEX extension_execution_owner ON extension_execution(actor_principal_id,created_at DESC,id DESC);
CREATE INDEX extension_execution_pending ON extension_execution(state) WHERE state IN ('claimed','dispatching');
CREATE TRIGGER extension_execution_keep_active BEFORE DELETE ON extension_execution
WHEN OLD.state IN ('claimed','dispatching')
BEGIN SELECT RAISE(ABORT,'extension execution still has an unsettled owner'); END;
CREATE TRIGGER extension_execution_keep_identity BEFORE UPDATE OF id,request_id,actor_principal_id,tenant_id,approval_id,operation_id,operation_json,context_json,context_fingerprint,input_fingerprint,correlation_id,created_at,deadline_unix_ms ON extension_execution
BEGIN SELECT RAISE(ABORT,'extension execution identity is immutable'); END;
CREATE TRIGGER extension_execution_keep_terminal BEFORE UPDATE ON extension_execution
WHEN OLD.state IN ('acknowledged','refused','outcome_unknown')
BEGIN SELECT RAISE(ABORT,'extension execution observation is immutable'); END;
