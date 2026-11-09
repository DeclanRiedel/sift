-- Native witnesses are separate from the existing workflow-outcome array.
-- Only fingerprints/counters/opaque source proofs belong here; never SQL or binds.
CREATE TABLE migration_native_journal (
    run_id TEXT PRIMARY KEY REFERENCES migration_run(id) ON DELETE CASCADE,
    format_version INTEGER NOT NULL CHECK(format_version=1),
    source_json TEXT NOT NULL,
    manifest_sha256 TEXT NOT NULL CHECK(length(manifest_sha256)=64),
    revision INTEGER NOT NULL CHECK(revision>0),
    closed INTEGER NOT NULL DEFAULT 0 CHECK(closed IN (0,1))
);
CREATE TABLE migration_native_group (
    run_id TEXT NOT NULL REFERENCES migration_native_journal(run_id) ON DELETE CASCADE,
    ordinal INTEGER NOT NULL CHECK(ordinal>0),
    transactional INTEGER NOT NULL CHECK(transactional IN (0,1)),
    transaction_id TEXT,
    phase TEXT NOT NULL CHECK(phase IN ('prepared','beginning','executing','committing','rolling_back','settled')),
    commit_intent INTEGER NOT NULL DEFAULT 0 CHECK(commit_intent IN (0,1)),
    rollback_intent INTEGER NOT NULL DEFAULT 0 CHECK(rollback_intent IN (0,1)),
    outcome TEXT CHECK(outcome IN ('committed','rolled_back','outcome_unknown')),
    PRIMARY KEY(run_id,ordinal)
);
CREATE TABLE migration_native_statement (
    run_id TEXT NOT NULL,
    group_ordinal INTEGER NOT NULL,
    ordinal INTEGER NOT NULL CHECK(ordinal>0),
    fingerprint TEXT NOT NULL CHECK(length(fingerprint)=70 AND substr(fingerprint,1,6)='sqlfp:'),
    intent_recorded INTEGER NOT NULL DEFAULT 0 CHECK(intent_recorded IN (0,1)),
    native_admitted INTEGER CHECK(native_admitted IN (0,1)),
    native_completed INTEGER CHECK(native_completed IN (0,1)),
    autocommit_known INTEGER NOT NULL DEFAULT 0 CHECK(autocommit_known IN (0,1)),
    affected_rows TEXT,
    result_code TEXT,
    PRIMARY KEY(run_id,group_ordinal,ordinal),
    FOREIGN KEY(run_id,group_ordinal) REFERENCES migration_native_group(run_id,ordinal) ON DELETE CASCADE
);
