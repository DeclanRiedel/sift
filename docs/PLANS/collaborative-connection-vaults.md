# Collaborative Connection Vaults

Status: **design proposal; implementation deferred until the M4 connection
editor is complete.** Team sharing belongs to M5, after desktop tenant/member
management is usable. This feature does not block the M3 daily-driver SQL
slice.

## Outcome

A vault groups saved database connections. A personal vault is private to one
principal within one tenant. A team vault has explicit grants for tenant
members. Users can inspect and version connection configuration, use authorized
credentials without learning them, rotate credentials, and restore an earlier
working version.

The first collaborative vertical slice is:

1. A tenant owner creates a team vault and grants another member `use` access.
2. An owner or editor adds a PostgreSQL connection and supplies its credential
   through a write-only form.
3. The member sees the non-secret connection configuration, connects, and runs
   a query without receiving the credential bytes.
4. An editor changes the configuration or rotates the credential using an
   expected revision.
5. History shows who changed which non-secret fields and whether credentials
   changed. Restoring a version creates a new head revision and invalidates
   active descendants when required.

## Placement in the product model

Vaults are tenant-scoped, not room-scoped. Rooms collaborate on query text and
history; connections may be used by several rooms. Runtime authorization is the
intersection of the vault grant, tenant role, room role when applicable, and
the existing connection-profile policy. An explicit denial at any layer wins.

Every connection profile belongs to exactly one vault. Existing profiles are
backfilled into a default vault:

- personal tenants: `My Vault`, owned by the bootstrapped principal;
- team tenants: `Team Connections`, managed by tenant owners and admins.

A personal vault is identified by `(tenant_id, owner_principal_id)`. Keeping it
inside a tenant prevents an accidental cross-tenant credential bridge for a
principal who belongs to several organizations.

## Security boundary

ADR-008 remains unchanged: SQLite contains opaque handles only, while secret
bytes live behind `SecretStore`. A connection URI is parsed into provider
configuration and credential fields at admission. Sift never persists a raw
URI containing a password.

“View connection” means viewing the complete non-secret configuration plus
masked credential field names, readiness, and last-rotation metadata. V1 has no
secret reveal endpoint. Editing a masked secret field either leaves it
unchanged or replaces it with newly supplied bytes; placeholder text is never
submitted as a value. Authorized users can use a credential through the server
without receiving it.

This preserves the existing explicit rule that stored credentials cannot be
revealed. Adding raw reveal later requires a separate ADR and threat review; it
must not arrive as an incidental UI convenience.

Secret-bearing requests require authenticated TLS outside trusted loopback,
strict body limits, redacted debug formatting, and sanitizer coverage. Secret
values never enter protocol `Operation` values, audit rows, logs, errors,
notifications, analytics, CRDT state, workspace checkpoints, exports, Git, or
desktop presentation state.

## Grants

Personal vaults have one implicit owner and no share operation. Team vaults use
explicit per-principal grants:

| Grant | Inspect masked config | Use connection | Edit config | Rotate/restore | Manage grants |
| --- | --- | --- | --- | --- | --- |
| `viewer` | yes | no | no | no | no |
| `user` | yes | yes | no | no | no |
| `editor` | yes | yes | yes | yes | no |
| `owner` | yes | yes | yes | yes | yes |

Tenant owners/admins may recover vault administration but do not implicitly
gain `use` access or bypass connection policy. Grant changes, credential
rotation, restoration, deletion, and administrative recovery are
security-critical mutations with transactionally durable audit records.

## Version model

Versioning is native server history, not Git. Git can safely contain a redacted
declaration that references a vault entry, but never a secret handle or value.

Suggested metadata:

```text
vault
  id, tenant_id, scope, owner_principal_id?, name, revision,
  created_by, created_at, updated_at, deleted_at?

vault_grant
  vault_id, principal_id, role, created_by, created_at, updated_at

vault_connection
  id, vault_id, connection_profile_id, head_version, revision,
  created_by, created_at, updated_at, deleted_at?

vault_connection_version
  connection_id, version, parent_version?, provider_id,
  configuration_json, credential_mode, secret_handle?,
  credential_schema_version, change_summary, created_by, created_at

secret_cleanup_queue
  namespace, secret_handle, reason, not_before, attempts, last_error?
```

`configuration_json` is schema-validated and guaranteed credential-free.
`secret_handle` points to an immutable typed credential envelope in
`SecretStore`. Each rotation writes a new handle before committing the new
metadata version. Old handles remain available while their versions are inside
the retention window.

History and diff responses expose configuration changes and a boolean
`credentials_changed`; they never expose, hash, or compare secret values in a
client-visible form. Restore is append-only: the server copies the selected
historical secret internally to a fresh handle and creates a new version. It
does not move the head backward or reuse a client-supplied handle.

All mutations take `expected_revision`. A stale editor receives a typed
conflict containing the current redacted head, then chooses to reload or submit
a new edit. Configuration and credential changes commit as one logical vault
version. Since SQLite and `SecretStore` cannot share a transaction, failed
writes and retired handles are reconciled through the durable cleanup queue;
they are not left solely to best-effort logging.

## Public contract

Add pure-serde identifiers and redacted views to `sift-protocol`, and expose
additive endpoints such as:

```text
GET    /v1/vaults
POST   /v1/vaults
GET    /v1/vaults/{vault}
PATCH  /v1/vaults/{vault}
DELETE /v1/vaults/{vault}

GET    /v1/vaults/{vault}/grants
PUT    /v1/vaults/{vault}/grants/{principal}
DELETE /v1/vaults/{vault}/grants/{principal}

GET    /v1/vaults/{vault}/connections
POST   /v1/vaults/{vault}/connections
GET    /v1/vault-connections/{connection}
PUT    /v1/vault-connections/{connection}
DELETE /v1/vault-connections/{connection}
POST   /v1/vault-connections/{connection}/credential

GET    /v1/vault-connections/{connection}/versions
GET    /v1/vault-connections/{connection}/versions/{version}
GET    /v1/vault-connections/{connection}/diff?from=&to=
POST   /v1/vault-connections/{connection}/restore
POST   /v1/vault-connections/{connection}/test
```

Create/update requests separate `configuration` from optional write-only
`credentials`. Responses return `credential_status`, never credentials or an
opaque secret handle. Every mutation and connection test is an audited
`Operation` variant with secret-free fields.

The existing session route continues to open a `ConnectionProfileId`. Vaults
govern discovery, mutation, and authorization; they do not create a parallel
driver path.

## Desktop experience

The Connections dock gains two roots:

```text
My Vault
  Local development
Team Vaults
  Analytics
    Production read-only
    Warehouse
```

The connection editor has Overview, Credentials, Access, Policy, and History
sections. Credentials render as field-level `Configured`, `Missing`, or
`Invalid` states. The only secret actions are Set, Replace, and Clear. History
shows author, time, reason, non-secret diff, and `Credentials rotated`; Restore
previews effects before creating a new version.

Team sharing is discoverable from an Access section. Disabled actions explain
the missing vault grant, tenant role, room role, or connection-policy
capability rather than collapsing them into a generic permission error.

## Delivery order

### M4A — personal vault foundation

- Add vault/version schema and backfill default vaults.
- Build the complete connection-profile editor and write-only credential form.
- Add masked history/diff and optimistic updates.
- Migrate existing profile mutation through the version writer.
- Prove no secret appears in SQLite, protocol responses, logs, audit, backups,
  or presentation persistence.

### M5A — team collaboration

- Add team-vault creation, explicit grants, and recovery administration.
- Intersect vault authorization with existing tenant/room/profile policy.
- Add rotation, append-only restore, active-session invalidation, and durable
  secret cleanup.
- Exercise concurrent editors, revoked grants, removed tenant membership,
  rotation during active queries, and restore conflicts.

### Later, only with a separate decision

- Raw secret reveal.
- External secret managers and broker-backed dynamic credentials.
- Encrypted portable sharing or client-side end-to-end encryption.
- Git representation beyond credential-free references.

## Graduation gates

- A member with `user` can connect but cannot retrieve credential bytes.
- A `viewer` cannot connect, mutate, or infer secret presence beyond redacted
  readiness metadata.
- Cross-tenant ids and stale revisions fail closed.
- Every security-critical mutation has an atomic durable audit row.
- Rotation invalidates affected active connections before their next operation.
- History restore creates a new version and does not expose the old secret.
- Deleting a vault eventually removes every unreferenced secret through a
  retryable durable cleanup path.
- Backup/restore preserves supported encrypted secret history while maintaining
  destination-owned keys and the exclusions in ADR-039.
- Logs, errors, OpenAPI examples, fixtures, and crash recovery contain no secret
  bytes or handles.
