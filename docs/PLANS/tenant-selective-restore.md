# Tenant-selective recovery

Implemented design; extends ADR-039's offline replacement journal. See ADR-059.

V1 restores one source tenant under its original IDs into an existing,
schema-compatible installation. It is recovery, not an identity-renaming or
cross-instance migration tool. When installation IDs exist they must match.
An existing target tenant must match the source name/kind. Every referenced
principal must already exist with the same ID/external identity; source auth,
principal flags and memberships in other tenants are never copied.

The operator selects `backup restore-tenant --tenant-id ID`; preview is default
and also requires the stopped-server maintenance lock. Both preview and apply
build and validate a private copy of the destination. Explicit table ownership
selectors choose the tenant subtree; FK boundary checks reject cross-tenant
references. Unknown schema tables, incompatible schemas, ID collisions and
tenant extension-storage state fail closed. Original IDs preserve embedded
references in CRDT/checkpoint/run definitions without unsafe JSON rewriting.

Restore tenant definitions, memberships, connection profiles/credentials,
rooms/documents, saved queries/scoped history, workspaces/checkpoints, DDL,
run definitions/history, transfer recipes, plans/catalog snapshots and vaults.
Filesystem projections are disabled, repository network access/credentials
are cleared, schedules disabled and interrupted work made terminal. Artifacts,
projection reconciliation state and repository credentials are not restored.

Destination authentication, instance manifests, extensions/grants, resource
limits and append-only audit/change ledgers remain destination-owned. Do not
copy unscoped principal history merely because a principal belongs to the tenant.
Pending unconsumed approvals for tenant members block recovery until expiration,
since their context is not tenant-addressable. Selected tenant invitations and
tenant-scoped API tokens are revoked without altering global auth sessions.

Portable file-secret archives and file-secret destinations are supported first;
memory mode is permitted only when no selected secret references exist.
Copy the destination secret store to private staging and copy only selected
source connection/vault values under new opaque handles. Never merge source
authentication keys or overwrite a destination secret handle. Old unreferenced
destination secrets are retained; garbage collection is a separate operation.

Apply creates an encrypted rescue backup, installs staged secrets then metadata,
and commits the existing durable restore journal. On failure use the same
rollback/recovery path. No queries execute against connected databases, no
checkout is touched, and no scheduler resumes imported work automatically.

Acceptance: two tenants sharing a principal; selected SQL/document/credential
recovery; unchanged unrelated data/credentials/auth; dry-run with audit-only
destination writes; identity/ID/FK/schema refusal; source secrets remapped; rescue backup
and journal recovery retained. Full workspace checks before final handoff.
