# Plan — Local Metadata Store & Auth Foundation (Phase 1, sprint 1)

Status: proposed. Owner: TBD. Target crates: new `sift-metadata`, extensions to `sift-server` and `sift-protocol`.

## 1. Motivation

Today the sift server keeps **zero** state across restarts:

- `SessionStore` is a `DashMap` in memory (`crates/server/src/session.rs:38`).
- `ConnectionSpec` is passed inline on every `open_connection` call and dropped after use.
- The only durable artifact is the JSONL operation log (`SIFT_AUDIT__OPERATION_LOG_PATH`) — an audit trail, not a workspace.
- Auth is a single shared bearer token (`crates/server/src/config.rs`). No user model, no per-principal audit, no way to share a connection library across a team.

This blocks Phase 1 (`docs/NEXT_STEPS.md`):
- Saved connection library (Navicat-parity locally, team-shareable when hosted).
- Restoring workspaces/tabs on relaunch.
- Query history and saved queries.
- The desktop client, which cannot begin until it has a stable persistence + auth surface to render.

## 2. Design principle: **auth and connections are layered, never bundled**

Navicat/DBeaver bundle identity with the connection library: your credentials *are* the app's state, stored on your disk. That's why they cannot be shared or hosted without a rewrite.

Sift models the two axes separately from day one:

```
Tenant       ← team/org boundary; connections and members live here
  Principal  ← a user (hosted) or the implicit local user
  Connection ← belongs to a tenant; visible to members per RBAC
```

Local mode auto-provisions one implicit tenant + one implicit principal on first launch — the schema is identical to hosted mode, the UI just skips the login screen. Hosted mode plugs in real auth. This mirrors ADR-010 ("local-first by default, hosted as a mode").

Copying Navicat's *UX* (zero friction locally) is the goal. Copying its *model* (single principal baked into storage) is exactly what we're avoiding.

## 3. Authentication model

Sift accepts credentials via a menu of mechanisms, all resolving to the same `principal`. The active mechanisms are chosen by config; multiple can be enabled simultaneously.

| Mechanism | Purpose | When |
|---|---|---|
| **Loopback bypass** | zero-auth local mode | bind is loopback AND `auth.loopback_bypass = true` (default in local mode) |
| **OIDC** | human web login | hosted mode; delegate to Google/GitHub/IdP |
| **API token** (bearer) | CI, scripts, SDK | scoped to a principal, revocable, optional expiry |
| **Keypair** (Ed25519) | desktop client → remote server | private key in OS keychain, signs a per-request nonce |
| **mTLS** | enterprise/gateway | later, optional |

Sift itself never stores a human password. That code path does not exist.

### 3.1 Loopback bypass

If the server bound to `127.0.0.1`/`::1` **and** the incoming connection's peer address is loopback **and** `auth.loopback_bypass = true`, the request is authenticated as `principal_id = 1` (the local principal, §4.4). Any non-loopback peer must present a real credential.

Rationale: the OS user's login session already protects loopback; adding an app-level password on top is worse UX and no better security. This is how `psql` works over a Unix socket.

### 3.2 OIDC

Standard OIDC authorization code + PKCE flow. Sift verifies the ID token, then either:
- Looks up an existing `principal` by `(issuer, subject)`, or
- Creates one if `auth.oidc.allow_signup = true` and the tenant permits it.

Browser sessions use a signed cookie (`sift_session`, HttpOnly, Secure, SameSite=Lax). SDK/desktop clients that go through OIDC receive a refresh token they store in the OS keychain.

Config:
```toml
[auth.oidc]
enabled = false
issuer = "https://accounts.google.com"
client_id = "..."
client_secret_env = "SIFT_OIDC_CLIENT_SECRET"
allow_signup = false
```

### 3.3 API token (bearer)

Long random string, hashed with `argon2id` at rest. Header: `Authorization: Bearer sift_<random>`.

- Minted via `POST /v1/auth/tokens` (authenticated).
- Scoped to a `principal_id`; optionally scoped to a single `tenant_id`.
- Optional `expires_at`; `last_used_at` tracked for revocation UX.
- Full plaintext returned exactly once at creation; only the hash is stored.

Replaces the current single shared token when metadata is available. During M1 the current single-token config continues to work as a fallback.

### 3.4 Keypair (Ed25519)

The nicest fit for **your own desktop client talking to your own remote sift server**. Signature-based, no long-lived tokens on the wire.

Flow (borrowed shape from SSH):
1. Desktop client generates an Ed25519 keypair on first launch. Private key stored in OS keychain; public key exportable.
2. User registers the public key with the remote server once (via web UI while logged in over OIDC, or via CLI while authenticated some other way).
3. On each connect the client requests a challenge (`GET /v1/auth/keypair/challenge`) — server returns a fresh 32-byte nonce with a short TTL.
4. Client signs `(nonce || method || path || body_hash)` and sends the signature + public-key fingerprint in `Authorization: Sift-Keypair fp=<fp>, sig=<sig>, nonce=<nonce>`.
5. Server verifies and issues a short-lived session token (5–60 minutes) so subsequent requests skip the challenge.

Trade-offs vs bearer tokens:
- (+) Steal the disk file and you still need the keychain unlock (which the OS gates on lock screen).
- (+) Server never sees a reusable secret.
- (+) Trivial per-device revocation — delete the public-key row.
- (−) More code to implement and get right.
- (−) Requires clock sync for challenge TTL (already required for TLS).

### 3.5 Where do secrets live?

Nothing sensitive lives in SQLite. Secrets addressed by opaque handles stored in SQLite; the real value resolves through a `SecretStore` trait:

| Backend | Local mode | Hosted mode |
|---|---|---|
| OS keychain (`keyring` crate) | default | not appropriate |
| Age-encrypted file (headless fallback) | opt-in | not appropriate |
| KMS/Vault | not usually | required |
| In-memory | tests only | tests only |

## 4. Data model

All tables carry `id INTEGER PRIMARY KEY` (except join tables), `created_at`, `updated_at`. Timestamps stored as ISO-8601 TEXT (SQLite convention; sortable).

### 4.1 Identity

```
tenant
  id, name, kind ('personal' | 'team'), created_at

principal
  id, external_id (OIDC 'issuer:subject', or 'local:1' in local mode),
  display_name, email (nullable), created_at
  UNIQUE (external_id)

membership
  tenant_id → tenant.id, principal_id → principal.id,
  role ('owner' | 'admin' | 'member' | 'viewer'), created_at
  PK (tenant_id, principal_id)

api_token
  id, principal_id → principal.id, tenant_id (nullable; scope),
  token_hash (argon2id), name, created_at, last_used_at,
  expires_at (nullable), revoked_at (nullable)

principal_key                        -- keypair auth
  id, principal_id → principal.id,
  algorithm ('ed25519'),
  public_key BLOB, fingerprint TEXT UNIQUE,
  label, created_at, last_used_at, revoked_at (nullable)

keypair_challenge                    -- ephemeral, TTL a few minutes
  nonce BLOB PK, fingerprint TEXT, issued_at, expires_at
```

### 4.2 Connections

```
connection_profile
  id, tenant_id → tenant.id, name, engine ('postgres'|'sqlserver'),
  spec_json (JSON blob of ConnectionSpec with password field null'd),
  credential_mode ('shared' | 'per_user' | 'broker'),
  shared_secret_handle (nullable; only when credential_mode='shared'),
  tags_json, created_by → principal.id, created_at, updated_at
  UNIQUE (tenant_id, name)

connection_credential                -- populated only for credential_mode='per_user'
  connection_profile_id → connection_profile.id,
  principal_id → principal.id,
  secret_handle,
  PK (connection_profile_id, principal_id)
```

Credential modes:
- **shared** — one secret stored via the `SecretStore`, everyone in the tenant uses it. Audit shows *who* ran the query; the wire secret is shared. Simplest.
- **per_user** — each member enters their own DB creds against the same profile. Principle-of-least-privilege.
- **broker** — hosted mode issues short-lived DB credentials (RDS IAM, Cloud SQL, HashiCorp Vault). Not implemented in this sprint; column exists so schema is stable.

### 4.3 Workspaces, tabs, history — per-principal

```
workspace                            -- a saved arrangement of tabs, per-principal
  id, tenant_id, principal_id, name, created_at, updated_at

session_snapshot                     -- persisted point-in-time of a live session
  id, workspace_id → workspace.id, tag, opened_at, closed_at (nullable)

tab
  id, session_id → session_snapshot.id,
  kind ('query' | 'schema' | ...),
  connection_profile_id (nullable),
  title, body_text (nullable), position, created_at, updated_at

query_history                        -- private to each principal
  id, principal_id, connection_profile_id (nullable), sql_text,
  started_at, duration_ms, row_count (nullable),
  status ('ok' | 'error' | 'canceled'),
  error_code (nullable), error_message (nullable)

saved_query
  id, tenant_id, principal_id (nullable — NULL = shared to tenant),
  name, sql_text, connection_profile_id (nullable), tags_json,
  created_at, updated_at
```

### 4.4 Local bootstrap

On first launch with an empty metadata DB **and** local mode active:

1. `INSERT tenant(id=1, name='local', kind='personal')`
2. `INSERT principal(id=1, external_id='local:1', display_name=<os user>)`
3. `INSERT membership(1, 1, 'owner')`

All local requests resolve to `(principal=1, tenant=1)`. Feels exactly like Navicat: no login, no wizard.

### 4.5 Hosted bootstrap

No implicit rows. Fresh install exposes a `POST /v1/setup` one-shot endpoint that creates the initial tenant + owner via OIDC login. After first use it 404s.

### 4.6 Indexes

- `connection_profile(tenant_id, name)` UNIQUE.
- `query_history(principal_id, started_at DESC)` — recent-history is the hot query.
- `tab(session_id, position)`.
- `principal(external_id)` UNIQUE.
- `principal_key(fingerprint)` UNIQUE.
- `api_token(token_hash)` UNIQUE.

## 5. Design choices to make

### 5.1 SQLite driver: `rusqlite` with `bundled` feature

- Bundled SQLite → no `libsqlite3` system dep; matches ADR-008 (Nix reproducibility).
- Metadata queries are cheap, low-frequency, and run on request threads via `tokio::task::spawn_blocking`.
- `sqlx` compile-time SQL checking is nice but needs a live DB during build — hurts reproducibility.
- Enable `PRAGMA journal_mode=WAL` and `PRAGMA foreign_keys=ON` at open.

### 5.2 Secret storage backends

`SecretStore` trait with three impls:

- **`KeyringSecretStore`** — `keyring` crate; default for local mode. Namespace = `sift.local`; account = the `secret_handle` (UUID v4 minted at insert).
- **`FileSecretStore`** — opt-in for headless Linux/CI. Age-encrypted file at `${state_dir}/secrets.age`; passphrase from `SIFT_METADATA__FILE_SECRET_PASSPHRASE`.
- **`MemorySecretStore`** — tests only.

Never mix backends in one process. Config picks one at startup.

### 5.3 File location

- Local mode default: `${XDG_STATE_HOME:-$HOME/.local/state}/sift/metadata.sqlite` on Linux; `~/Library/Application Support/sift/metadata.sqlite` on macOS.
- Override via `SIFT_METADATA__PATH`.
- Test/mock mode: in-memory SQLite (`sqlite::memory:`).

### 5.4 Migrations

- `refinery` with embedded `.sql` files under `crates/metadata/migrations/`.
- Applied on `MetadataStore::open()`; tracked in `_sift_migrations`.
- Forward-only in Phase 1. Local rollback = delete the file.

## 6. Crate layout

```
crates/metadata/
  Cargo.toml
  migrations/
    V001__identity.sql          -- tenant/principal/membership/api_token/principal_key
    V002__connections.sql       -- connection_profile/connection_credential
    V003__workspaces.sql        -- workspace/session_snapshot/tab
    V004__history.sql           -- query_history/saved_query
  src/
    lib.rs                       -- MetadataStore, MetadataError
    schema.rs                    -- domain structs
    identity/
      mod.rs
      tenants.rs
      principals.rs
      memberships.rs
      tokens.rs
      keys.rs
    connections.rs
    workspaces.rs
    sessions.rs
    tabs.rs
    history.rs
    saved_queries.rs
    secrets/
      mod.rs                     -- SecretStore trait
      keyring.rs
      file.rs
      memory.rs
```

Depends on `sift-protocol` only for cross-wire types (`ConnectionSpec`, `Engine`).

Public API sketch:

```rust
pub struct MetadataStore { /* rusqlite pool + Arc<dyn SecretStore> */ }

impl MetadataStore {
    pub fn open(path: &Path, secrets: Arc<dyn SecretStore>) -> Result<Self>;
    pub fn open_in_memory(secrets: Arc<dyn SecretStore>) -> Result<Self>;

    // Identity
    pub fn resolve_principal_by_external_id(&self, external_id: &str) -> Result<Option<Principal>>;
    pub fn list_principal_tenants(&self, principal: PrincipalId) -> Result<Vec<TenantMembership>>;
    pub fn issue_api_token(&self, principal: PrincipalId, scope: Option<TenantId>, name: &str, expires_at: Option<DateTime<Utc>>) -> Result<(ApiTokenRow, String /*plaintext*/)>;
    pub fn verify_api_token(&self, presented: &str) -> Result<Option<ApiTokenRow>>;
    pub fn register_principal_key(&self, principal: PrincipalId, public_key: &[u8], label: &str) -> Result<PrincipalKey>;
    pub fn issue_keypair_challenge(&self, fingerprint: &str) -> Result<Nonce>;
    pub fn verify_keypair_signature(&self, fingerprint: &str, nonce: &Nonce, signature: &[u8], canonical_request: &[u8]) -> Result<Option<PrincipalKey>>;

    // Connections
    pub fn list_connection_profiles(&self, tenant: TenantId) -> Result<Vec<ConnectionProfile>>;
    pub fn upsert_connection_profile(&self, tenant: TenantId, actor: PrincipalId, input: NewConnectionProfile) -> Result<ConnectionProfile>;
    pub fn delete_connection_profile(&self, tenant: TenantId, id: ConnectionProfileId) -> Result<()>;
    pub fn resolve_connection_spec(&self, tenant: TenantId, principal: PrincipalId, id: ConnectionProfileId) -> Result<ConnectionSpec>;

    // ... same shape for workspaces/tabs/history/saved_queries
}

#[async_trait::async_trait]
pub trait SecretStore: Send + Sync {
    async fn put(&self, namespace: &str, handle: &str, secret: &[u8]) -> Result<()>;
    async fn get(&self, namespace: &str, handle: &str) -> Result<Option<Vec<u8>>>;
    async fn delete(&self, namespace: &str, handle: &str) -> Result<()>;
}
```

## 7. Protocol additions (`sift-protocol`)

Extend the `Operation` enum with:

```
# Identity (writes are hosted-only in Phase 1)
ListTenants
ListMyKeys / RegisterKey / RevokeKey
ListMyTokens / MintToken / RevokeToken

# Connections
ListConnectionProfiles { tenant }
CreateConnectionProfile { tenant, name, engine, spec, credential_mode, tags }
UpdateConnectionProfile { tenant, id, patch }
DeleteConnectionProfile { tenant, id }
SetPerUserCredential { profile_id, secret }         -- stored via SecretStore, not in Operation payload
OpenConnectionFromProfile { session_id, profile_id }

# Workspaces / tabs / sessions
ListWorkspaces / CreateWorkspace / RenameWorkspace / DeleteWorkspace
ListSessionSnapshots / SnapshotSession / RestoreSession
ListTabs / UpsertTab / DeleteTab

# History
ListQueryHistory { limit, before? } / ClearQueryHistory

# Saved queries
ListSavedQueries / CreateSavedQuery / UpdateSavedQuery / DeleteSavedQuery
```

Secret payloads (passwords) do **not** flow through `Operation` — they arrive out-of-band on a separate route and are written straight to `SecretStore` so they never enter the operation log.

All non-secret mutations flow through the existing operation-log audit pipeline (`SessionStore::push_operation`) so metadata edits are replayable like every other action.

## 8. HTTP routes (`sift-server`)

### Auth
```
POST   /v1/auth/oidc/login                          -- start OIDC PKCE flow
GET    /v1/auth/oidc/callback
POST   /v1/auth/logout
POST   /v1/auth/tokens                              -- mint API token (returns plaintext once)
GET    /v1/auth/tokens
DELETE /v1/auth/tokens/:id
GET    /v1/auth/keys                                -- list registered public keys
POST   /v1/auth/keys                                -- register public key
DELETE /v1/auth/keys/:id
GET    /v1/auth/keypair/challenge                   -- issue nonce
POST   /v1/auth/keypair/verify                      -- exchange signed challenge for session
POST   /v1/setup                                    -- hosted-mode first-run only
```

### Metadata
```
GET    /v1/metadata/tenants                         -- tenants I can see
GET    /v1/metadata/connections                     -- ?tenant=<id>
POST   /v1/metadata/connections
GET    /v1/metadata/connections/:id
PATCH  /v1/metadata/connections/:id
DELETE /v1/metadata/connections/:id
POST   /v1/metadata/connections/:id/credential      -- write per-user secret

POST   /v1/sessions/:id/connections/from-profile    { profile_id }

GET    /v1/metadata/workspaces (+ POST, PATCH, DELETE)
GET    /v1/metadata/workspaces/:id/sessions
POST   /v1/sessions/:id/snapshot                    -- persist current tabs
POST   /v1/metadata/sessions/:snapshot_id/restore

GET    /v1/metadata/history?connection_id=&limit=&before=
DELETE /v1/metadata/history

GET    /v1/metadata/saved-queries (+ POST, PATCH, DELETE)
```

Every route resolves `(principal_id, tenant_id)` through the auth middleware chain (§9). Local mode: always `(1, 1)`.

OpenAPI generator (`http.rs:157-401`) picks up new schemas via `schemars` on the new protocol types.

## 9. Auth middleware chain

Order (outer → inner):

1. `protocol_version_header` (existing).
2. `audit_middleware` (existing).
3. **`principal_resolver`** (new): tries mechanisms in order and stops at the first that matches:
   - Loopback bypass — if configured and peer is loopback.
   - Session cookie — verify HMAC, load principal.
   - `Authorization: Bearer` — hash and lookup in `api_token`.
   - `Authorization: Sift-Keypair` — verify signature against a registered public key.
   - None matched → 401.
4. **`tenant_scoper`** (new): for routes with `?tenant=` or `/tenants/:id`, verify the principal has a membership; attaches `TenantContext` to request extensions.
5. Handler.

`AuthContext { principal: Principal, tenants: Vec<TenantMembership>, mechanism: AuthMechanism }` is inserted into request extensions so handlers can log which mechanism authenticated the caller.

## 10. Query history hook

Not user-initiated CRUD — a side effect of `execute`. Wire it in `session.rs::execute_http` / `execute_stream`:

- Start of execute → allocate a `query_history` row with `started_at`, `principal_id`, `connection_profile_id` (if opened from a profile).
- Drain complete → patch `duration_ms`, `row_count`, `status='ok'`.
- Error / cancel → patch `status`, `error_code`, `error_message`.

Runs in `spawn_blocking`. Errors are logged, never surfaced — never fail a query because history couldn't be written.

## 11. Wiring changes in `sift-server`

- `Config` gains:
  ```toml
  [metadata]
  path = "..."                # optional
  secret_backend = "keyring"  # keyring | file | memory

  [auth]
  loopback_bypass = true      # local-mode default
  session_secret_env = "SIFT_SESSION_SECRET"

  [auth.oidc]
  enabled = false
  # ... §3.2
  ```
- `main.rs` builds `MetadataStore` before `SessionStore` and passes it in.
- `SessionStore` gains `metadata: Arc<MetadataStore>` and helpers that consume `AuthContext`.
- New `open_connection_from_profile(session_id, actor, profile_id)`:
  1. Load `ConnectionProfile` (checked against `actor`'s tenant membership).
  2. Fetch password from `SecretStore` (shared handle or per-user handle).
  3. Reconstruct `ConnectionSpec`, delegate to existing `open_connection`.
  4. Update `last_used_at`.
- `close_session` patches `session_snapshot.closed_at` if the session was snapshotted.

## 12. Testing

Unit tests in `sift-metadata` using `open_in_memory` + `MemorySecretStore` for full CRUD/migration coverage.

Add to `crates/server/tests/api_smoke.rs`:

1. **Local bootstrap**: fresh DB → GET `/v1/metadata/tenants` returns exactly one tenant, membership owner.
2. **Profile round-trip**: create profile → list → `open_connection/from-profile` → execute → row appears in `query_history` scoped to principal.
3. **API token round-trip**: mint → auth with it → revoke → 401.
4. **Keypair round-trip**: register ed25519 pubkey → request challenge → sign → verify → session token returned.
5. **Tenant scoping**: two tenants, two principals; principal A cannot list principal B's connection profiles.
6. **Snapshot/restore**: snapshot session → close → restore into new session → tabs match.
7. **Per-user credential**: profile with `credential_mode='per_user'`; principal without a credential row gets a helpful 400, not a driver error.

Add one live-pg case: create profile with real password → keyring roundtrip → open works.

## 13. Milestones

Sized for a Codex-style agent to pick up incrementally. Each is a clean PR.

| # | Deliverable | Rough scope |
|---|---|---|
| **M1** | `sift-metadata` crate skeleton, migrations V001+V002, `SecretStore` trait + memory impl, `MetadataStore::open_in_memory`, unit tests for identity + connection CRUD | ~700 LOC, no server wiring |
| **M2** | Keyring backend, XDG state-dir resolution, `SIFT_METADATA__*` config, local bootstrap on first launch | ~300 LOC |
| **M3** | `principal_resolver` + `tenant_scoper` middleware, loopback bypass, API-token auth (replaces current single token), `/v1/auth/tokens` routes | ~500 LOC |
| **M4** | Keypair auth (registration, challenge, verify), `/v1/auth/keys*` + `/v1/auth/keypair/*` routes | ~400 LOC |
| **M5** | Connection-profile HTTP routes, `Operation` variants, OpenAPI updates, `open_connection_from_profile` | ~600 LOC |
| **M6** | Workspaces + session snapshot/restore + tabs | ~500 LOC |
| **M7** | Query-history hook wired into execute path + endpoints | ~300 LOC |
| **M8** | Saved queries endpoints | ~200 LOC |
| **M9** | OIDC (opt-in, hosted-mode) + `/v1/setup` | ~600 LOC |
| **M10** | Docs update (`PHASE0_STATUS.md` addendum, `NEXT_STEPS.md` tick-off, README auth section) | trivial |

**M1–M3 unblock the desktop client to render a connection library over loopback with zero login.** M4 unblocks the same client hitting a remote sift server without OIDC infrastructure. M9 is the only milestone that requires an external IdP and can be deferred until hosted mode is a real requirement.

## 14. Risks and open questions

- **Async vs sync SQLite.** Sync + `spawn_blocking` recommended; confirm before M1.
- **Keychain on Linux CI.** Test environments have no keyring daemon. Memory backend covers CI; keyring must fail with a clear log line, not panic.
- **Session-cookie secret rotation.** `SIFT_SESSION_SECRET` rotation invalidates all sessions. Acceptable; document.
- **Keypair replay across TLS boundaries.** Signing `(nonce || method || path || body_hash)` prevents replay across endpoints. Do we also bind to the TLS exporter (RFC 5705) to bind to the exact connection? Nice-to-have, not required — deferred.
- **Password rotation on `connection_profile`.** Edit password → mint new `secret_handle`, delete old. Keeps keychain history clean.
- **Multi-process access.** WAL mode handles it. One process per host today; document the constraint.
- **What tenant does a per-user credential live in?** The per-user secret is tenant-scoped (via `connection_profile.tenant_id`) and principal-scoped (via `principal_id`). A principal moving between tenants gets a clean slate — deliberate.
- **Do we ever offer a master-password model like Navicat?** No. OS keychain + OIDC is stronger and less friction. Explicitly rejected.

## 15. What this unblocks

- **Schema cache** (next sprint) — stores per-`connection_profile_id` snapshots in the same DB.
- **Result export** — no direct dependency; can run in parallel.
- **Desktop client** — can begin against M3; against M4 for remote-server support.
- **Hosted mode** — schema is ready. M9 (OIDC) + a KMS-backed `SecretStore` impl is the delta.
