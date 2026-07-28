CREATE TABLE extension_package (
    archive_sha256 TEXT PRIMARY KEY
        CHECK (length(archive_sha256) = 64),
    extension_id TEXT NOT NULL,
    version TEXT NOT NULL,
    manifest_sha256 TEXT NOT NULL
        CHECK (length(manifest_sha256) = 64),
    manifest_json TEXT NOT NULL,
    provenance TEXT NOT NULL
        CHECK (provenance IN ('bundled', 'verified', 'local', 'development')),
    installed_at TEXT NOT NULL,
    UNIQUE (extension_id, version)
);

CREATE TABLE extension_selection (
    extension_id TEXT PRIMARY KEY,
    selected_archive_sha256 TEXT NOT NULL
        REFERENCES extension_package(archive_sha256) ON DELETE RESTRICT,
    enabled INTEGER NOT NULL DEFAULT 0 CHECK (enabled IN (0, 1)),
    lifecycle_state TEXT NOT NULL DEFAULT 'disabled'
        CHECK (lifecycle_state IN (
            'installed', 'disabled', 'starting', 'ready', 'degraded',
            'quarantined', 'uninstalled', 'orphaned'
        )),
    isolation TEXT NOT NULL DEFAULT 'process_only'
        CHECK (isolation IN ('host_enforced', 'platform_sandboxed', 'process_only')),
    quarantine_reason TEXT,
    revision INTEGER NOT NULL DEFAULT 0 CHECK (revision >= 0),
    updated_at TEXT NOT NULL
);

CREATE TABLE extension_contribution (
    contribution_id TEXT PRIMARY KEY,
    archive_sha256 TEXT NOT NULL
        REFERENCES extension_package(archive_sha256) ON DELETE CASCADE,
    extension_id TEXT NOT NULL,
    kind TEXT NOT NULL,
    local_id TEXT NOT NULL,
    descriptor_json TEXT NOT NULL,
    UNIQUE (archive_sha256, kind, local_id)
);

CREATE INDEX idx_extension_contribution_package
    ON extension_contribution(archive_sha256);

CREATE TABLE extension_grant (
    extension_id TEXT NOT NULL
        REFERENCES extension_selection(extension_id) ON DELETE CASCADE,
    capability TEXT NOT NULL,
    constraints_json TEXT NOT NULL DEFAULT '{}',
    revision INTEGER NOT NULL DEFAULT 0 CHECK (revision >= 0),
    updated_at TEXT NOT NULL,
    PRIMARY KEY (extension_id, capability)
);

CREATE TABLE extension_tenant_allowlist (
    extension_id TEXT NOT NULL
        REFERENCES extension_selection(extension_id) ON DELETE CASCADE,
    tenant_id INTEGER NOT NULL REFERENCES tenant(id) ON DELETE CASCADE,
    allowed INTEGER NOT NULL CHECK (allowed IN (0, 1)),
    revision INTEGER NOT NULL DEFAULT 0 CHECK (revision >= 0),
    updated_at TEXT NOT NULL,
    PRIMARY KEY (extension_id, tenant_id)
);

CREATE TABLE extension_publisher_key (
    publisher TEXT NOT NULL,
    fingerprint TEXT NOT NULL,
    public_key BLOB NOT NULL CHECK (length(public_key) = 32),
    valid_from TEXT NOT NULL,
    valid_until TEXT,
    revoked_at TEXT,
    revision INTEGER NOT NULL DEFAULT 0 CHECK (revision >= 0),
    PRIMARY KEY (publisher, fingerprint)
);
