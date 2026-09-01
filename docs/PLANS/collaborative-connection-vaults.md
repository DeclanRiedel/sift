# Collaborative Server Vaults

Status: **Vault-backed connection use and controlled generic-secret reveal are
implemented; the broader item editor, restore flows, and hardening matrix remain
in progress.** ADR-052 is graduated. Each implementation milestone remains
independently gated by the security and test exits below.

## Outcome

The server owns an encrypted vault available from the Collaboration panel. It
contains one private **My Vault** for the signed-in principal and explicitly
shared **Team Vaults** for tenant members. It supports:

- database connections whose credentials can be used by the server without
  revealing them to a client;
- passwords, tokens, and secure notes that an explicitly authorized person can
  reveal temporarily; and
- masked version history, credential rotation, append-only restore, and team
  access management.

The feature is tenant-scoped rather than room-scoped. A room is still the
collaboration boundary for query text and presence, but a team commonly needs
the same vault across several rooms. The Collaboration panel is the discovery
and management surface because membership and sharing live there. The
Connections panel shows vault-backed connection shortcuts, but does not grow a
second vault editor.

The first end-to-end team story is:

1. A tenant member creates a team vault and grants another member permission
   to use connections, but not reveal secrets.
2. An editor adds a PostgreSQL connection through a write-only credential
   form.
3. The member connects and runs a query without receiving credential bytes.
4. The editor adds a separate shared password and grants reveal permission to
   a specific team member.
5. A reveal requires recent interactive reauthentication, is time-limited in
   the desktop, and produces a secret-free audit record.
6. Rotation and restore create new immutable versions and invalidate affected
   active database sessions.

## Product model

A principal has exactly one personal vault per tenant. Personal vaults have an
implicit owner and cannot be shared. Team vaults have explicit per-principal
grants, and a principal sees only vaults for their current tenant.

V1 item kinds are deliberately small:

| Kind | Non-secret metadata | Secret payload | Normal consumption |
| --- | --- | --- | --- |
| `connection` | provider, host, port, database, options | typed provider credentials | server opens a session; no reveal |
| `login` | label, username, URL | password | temporary reveal/copy |
| `token` | label, service, expiry hint | token or key | temporary reveal/copy |
| `secure_note` | label only | note body | temporary reveal/copy |

Supported database URIs are parsed at admission into credential-free provider
configuration and typed credentials. Sift never persists a raw URI containing
a password. An unsupported connection string can only be stored as a generic
secret item and cannot be used by a driver until a provider can parse it.

Every saved connection profile references one `connection` vault item. Existing
profiles are backfilled into the current principal's personal vault in local
and personal deployments. A team deployment requires an explicit migration
choice before a profile becomes team-visible; migration must never broaden
credential access silently.

## Security boundaries

ADR-008 remains load-bearing: SQLite stores metadata and random opaque handles;
secret bytes live only behind `SecretStore`. A vault is an authorization and
versioning layer over that store, not a second encryption implementation.

Two consumption paths remain distinct:

- **Use** resolves a connection credential inside the server and passes it to
  the existing driver/session path. The client never receives the value.
- **Reveal** is available only for `login`, `token`, and `secure_note`. It is a
  separate high-risk operation and is never an incidental field on a normal
  item response. Connection credentials are not revealable in V1.

Reveal requires an authenticated TLS connection outside trusted loopback, a
recent interactive step-up bound to the principal, tenant, vault item, and
action digest, and an explicit `reveal` grant. API tokens, refresh tokens, and
background jobs cannot step up or reveal. The reveal response is single-item,
non-cacheable, body-limited, rate-limited, and returned only by `POST`; it never
travels over room WebSockets.

The desktop keeps a revealed value only in the reveal surface's memory, masks
it again after 30 seconds or when focus leaves the surface, and never writes it
to settings, workspace state, notifications, undo history, crash reports, or
analytics. Copy is explicit. Where the platform permits safe comparison, Sift
clears its copied value after 30 seconds only if the clipboard still contains
that exact value; it never overwrites newer clipboard content.

Secret-bearing requests and responses use hand-written redacted `Debug`, strict
size limits, sanitizer tests, `Cache-Control: no-store`, and response headers
that prohibit browser caching. Secret values and opaque handles never enter
protocol `Operation` fields, audit payloads, logs, errors, traces, CRDT state,
query history, checkpoints, exports, Git, fixtures, or desktop presentation
snapshots.

Tenant owners/admins can recover `manage` access to a team vault but do not
implicitly gain `use` or `reveal`. Host administrators and database readers are
outside Sift's confidentiality boundary; this must be stated in deployment
documentation.

## Authorization

Team grants are capability sets rather than a single ascending role, because
editing a password must not automatically grant permission to learn its prior
value:

| Capability | Meaning |
| --- | --- |
| `inspect` | list items and see masked metadata/readiness |
| `use` | use a connection through the server |
| `reveal` | temporarily reveal eligible generic secret items |
| `edit` | create metadata and set, replace, clear, or restore secret values |
| `manage` | rename/delete the vault and administer grants |

The UI offers presets such as Viewer, Connection User, Secret Reader, Editor,
and Owner, but the server stores and evaluates the explicit capability set.
`reveal`, `use`, `edit`, and `manage` imply `inspect`; `edit` does not imply
`reveal`. Personal-vault ownership grants all capabilities.

Runtime authorization is the intersection of tenant membership, vault grant,
item kind, connection-profile policy, and room role when a room operation is
involved. Any denial wins. Cross-tenant identifiers, deleted membership, stale
revisions, and revoked grants fail closed. Grant revocation terminates reveal
leases immediately and invalidates affected server-owned connections before
their next operation.

## Metadata and version model

Suggested metadata is additive:

```text
vault
  id, tenant_id, scope(personal|team), owner_principal_id?, name,
  revision, created_by, created_at, updated_at, deleted_at?

vault_grant
  vault_id, principal_id, capability_bits, revision,
  created_by, created_at, updated_at

vault_item
  id, vault_id, kind, label, metadata_json, head_version, revision,
  created_by, created_at, updated_at, deleted_at?

vault_item_version
  item_id, version, parent_version?, secret_handle?, secret_schema_version,
  metadata_json, change_summary, created_by, created_at

vault_connection_binding
  item_id, connection_profile_id

secret_cleanup_queue
  namespace, secret_handle, reason, not_before, attempts, last_error?
```

`metadata_json` is kind-specific, schema-validated, bounded, and guaranteed to
be credential-free. Secret payloads are typed envelopes with a kind and schema
version; the server never accepts a client-supplied handle.

Each secret change writes a fresh immutable handle before committing its
metadata version. Since SQLite and `SecretStore` cannot share a transaction,
failed writes and retired handles enter a durable cleanup queue. Deletion is a
tombstone until every retained version expires and every unreferenced handle is
removed. Secret-store deletion cannot rely only on best-effort logging.

History returns authorship, timestamps, non-secret metadata diffs, and booleans
such as `secret_changed`; it never hashes, compares, or exposes secret values.
Restore is append-only: the server copies a retained secret internally to a new
handle and creates a new head. All mutations take `expected_revision`; a stale
writer receives a typed conflict containing the current redacted head.

## Public contract

Add pure-serde identifiers, item kinds, capabilities, redacted views, and
credential readiness to `sift-protocol`. Secret-bearing HTTP request/response
types stay in `sift-api-types`; the pure wire contract must not gain I/O or
secret-store behavior.

Implemented routes use the metadata namespace. Remaining routes stay planned:

```text
GET    /v1/metadata/vaults                         implemented
POST   /v1/metadata/vaults                         implemented
GET    /v1/metadata/vaults/{vault}                 planned
PATCH  /v1/metadata/vaults/{vault}                 planned
DELETE /v1/metadata/vaults/{vault}                 planned

GET    /v1/metadata/vaults/{vault}/grants          implemented
PUT    /v1/metadata/vaults/{vault}/grants/{principal} implemented
DELETE /v1/metadata/vaults/{vault}/grants/{principal} planned

GET    /v1/metadata/vaults/{vault}/items           implemented
POST   /v1/metadata/vaults/{vault}/items           implemented
GET    /v1/metadata/vault-items/{item}             planned
PUT    /v1/metadata/vault-items/{item}             planned
DELETE /v1/metadata/vault-items/{item}             planned
POST   /v1/metadata/vault-items/{item}/secret      planned
POST   /v1/metadata/vault-items/{item}/reveal-step-up implemented
POST   /v1/metadata/vault-items/{item}/reveal      implemented

GET    /v1/metadata/vault-items/{item}/versions    implemented
GET    /v1/metadata/vault-items/{item}/versions/{version} planned
GET    /v1/metadata/vault-items/{item}/diff?from=&to= planned
POST   /v1/metadata/vault-items/{item}/restore     planned
POST   /v1/metadata/vault-items/{item}/test        planned
```

Create/update requests separate non-secret `metadata` from an optional
write-only `secret`. Normal responses return only `secret_status`, rotation
metadata, and effective capabilities. Reveal returns a dedicated one-use
response shape. No route returns a secret handle.

Every user-visible action is a typed, audited `Operation` variant: vault and
grant mutations, item create/update/delete, secret set/rotate/clear, restore,
test, reveal, and connection use. Audit fields contain stable ids, item kind,
outcome, and revision only. A reveal audit proves who accessed which item and
when without containing the value.

The existing session route continues to open a `ConnectionProfileId`. Vaults
govern discovery, credentials, and authorization; they do not create a parallel
driver path.

## Collaboration-panel experience

The Collaboration dock becomes two compact keyboard-navigable views:

```text
People | Vault

Vault
  My Vault
    Local development             connection · ready
    Package registry              token · configured
  Team Vaults
    Analytics
      Production read-only        connection · ready
      Reporting login             login · configured
```

`j/k` selects, `h/l` collapses or expands, `Enter` opens, `/` filters, and the
usual contextual action menu exposes only authorized actions. Switching to the
Vault view requests redacted metadata; secret bytes are never prefetched.

The item editor has Overview, Secret, Access, and History sections. Secret
fields show Configured, Missing, Invalid, or Rotated states. Connection items
offer Test and Open Connection. Generic items offer Reveal only when permitted;
the value appears in a focused temporary reveal surface with a visible expiry.
Set, Replace, Clear, Restore, and Reveal are visually distinct actions.

Access explains the effective capability intersection and why an action is
disabled. Team membership and vault grants share the Collaboration panel, but
remain separate concepts. Connections renders a lightweight `From Vault`
group and links back to the canonical vault item instead of duplicating its
history or access editor.

## Delivery milestones

### V0 — contract and threat-model graduation

- [x] Graduate ADR-052 covering item kinds, non-hierarchical capabilities, reveal,
  audit vocabulary, revocation, retention, and the host-admin boundary.
- [x] Specify the exact step-up proof and one-use reveal response lifecycle.
- [x] Add protocol/API redaction tests before any secret-bearing route exists.
- [ ] Decide retention defaults, tenant quotas, maximum item/value sizes, and
  cleanup retry policy in instance configuration.

Exit: reviewers can trace every path secret bytes may take and every persisted
representation is credential-free.

### V1 — personal vault and vault-backed connections

- [x] Add schema, lazy default personal-vault creation, immutable versions, and cleanup
  queue.
- [x] Route existing connection profile creation and credential rotation through
  vault items while preserving the `Driver` trait and session APIs.
- [~] Add the Collaboration `Vault` view, write-only connection form, masked
  history, test, and connection shortcuts.
- [ ] Prove no connection credential reaches a client or non-secret store.

Exit: a user can create, rotate, test, restore, and use a personal connection
without any reveal path.

### V2 — team vaults and use-without-reveal

- [~] Add team-vault lifecycle, capability grants, admin recovery, and tenant
  membership invalidation.
- [ ] Intersect vault authorization with room and connection policies.
- [x] Invalidate active descendants on credential rotation or grant revocation.
- [~] Cover concurrent editors, stale revisions, cross-tenant ids, member removal,
  and rotation during active queries.

Exit: a member with `use` can query through a team connection but cannot obtain
its credential bytes.

### V3 — controlled generic secret reveal

- [x] Add login, token, and secure-note items plus bounded typed envelopes.
- [x] Implement interactive digest-bound step-up and single-use reveal.
- [x] Add the timed desktop reveal/copy surface and safe clipboard clearing.
- [~] Add per-principal reveal rate limits, immediate lease revocation, durable
  reveal audit, and negative cache/log/crash tests.

Exit: an explicitly granted member can reveal one eligible item, while API
tokens, background work, connection users, tenant recovery admins, and revoked
members cannot.

### Later, behind separate decisions

- External secret-manager and dynamic credential brokers.
- Client-side end-to-end encryption (incompatible with server-side connection
  use unless items use separate key paths).
- Encrypted portable export or sharing outside a tenant.
- File attachments, SSH private keys, and arbitrary binary secret types.

## Graduation gates

- SQLite, protocol responses, logs, errors, traces, audit rows, OpenAPI
  examples, fixtures, Git, backups, crash recovery, and presentation state
  contain neither secret bytes nor secret handles.
- `use` never returns a credential; `reveal` never works for a connection item.
- Reveal requires item-specific recent step-up and an explicit capability, is
  non-cacheable, audited, rate-limited, and expires in the desktop.
- `edit` without `reveal` can replace a secret but cannot read its old value.
- Cross-tenant ids, stale revisions, removed membership, and revoked grants
  fail closed under concurrency.
- Every security-critical mutation and reveal has a transactionally durable,
  sanitized audit record.
- Rotation and revocation invalidate affected live connections before their
  next operation.
- Restore creates a new version and never exposes or reuses a client-supplied
  handle.
- Deletion eventually removes every unreferenced secret through the retryable
  durable cleanup queue.
- Workspace `fmt`, strict `clippy`, tests, route sanitizer tests, and secret
  sentinel scans remain green at every milestone.
