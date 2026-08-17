# Reproducible Sift instances

Status: design boundary finalized; Phase 0 freezes exact schemas and selected
dependencies; no implementation has started.

## Goal

Recreate the operator-controlled configuration of a Sift server on another
device without copying the source installation's runtime identity, active
sessions, user content, or client preferences.

One Sift desktop application may host and connect to many independent Sift
servers at the same time. Every server has its own configuration set, runtime
identity, state directory, metadata, secrets, auth boundary, audit trail, and
process lifecycle. There is no combined multi-server manifest and no server
process containing multiple independent Sift instances.

The canonical contract is pure data. A future Nix, CUE, Nickel, or other
generator may produce that data, but evaluating a configuration language is
not part of v1.

## V1 artifacts

### `sift.profile.toml`

A desktop-local, non-secret attachment file for exactly one Sift server. It
describes how this desktop hosts or reaches that server. It is a client
preference, not part of the portable server configuration, and never grants
server authority.

Every server shown in the desktop instance switcher has its own profile file.
There is no monolithic profile database. The desktop discovers profiles from
private per-profile directories and stores tokens/device private keys in the
OS credential store, never in this TOML.

### `sift.instance.toml`

The public, human-readable desired configuration. "Public" means
non-secret, not anonymously readable through the server. It contains:

- deployment and transport policy, limits, timeouts, audit policy, and
  machine paths or URLs the operator deliberately chooses to reproduce;
- tenants, declarative admission rules, memberships, and instance-admin
  intent;
- connection profiles, provider configuration, tags, and connection policy;
- named credential slots, but never credential values;
- extension selections, requested versions, grants, and tenant allowlists.

It excludes themes, keybindings, window state, selected instances, pane
layout, documents, results, query history, audit rows, runtime locks, caches,
auth sessions, refresh tokens, API tokens, and the destination instance id.

Each Sift server owns exactly one active manifest/lock pair. For a
desktop-managed local server the files live in that profile's private
directory. For a remote server they live with the remote deployment; the
desktop profile does not silently cache or override them. An authorized admin
may explicitly export/check out a remote manifest as a draft.

### `sift.instance.lock`

Generated, public resolution data. It pins the manifest digest, Sift and
protocol compatibility, provider schema digests, and external extension
artifacts. It is mandatory for initialize/apply in v1 and callers must never
hand-edit it.

### Credential input

Credential values are a separate input to the same typed credential-slot
contract. V1 supports:

1. manual entry through the authenticated credential setup UI;
2. explicit import of a plaintext `sift.credentials.toml` key/value file; and
3. an optional encrypted `sift.secrets` pack for portable automation.

Plaintext credential files are import-only. Sift never exports plaintext
credentials. A value-bearing export always creates an encrypted pack. Normal
export is an authenticated, stepped-up admin operation; stopped-server
recovery export is a separate local-operator path.

V1 credential packs contain server-owned or shared configuration secrets:
shared connection credentials, OAuth client secrets, and extension service
credentials. Per-user database credentials, login password verifiers, API
tokens, refresh tokens, and one-use credentials are excluded. Users provision
their own per-user database credentials after signing in. Destination login
credentials are established during bootstrap or through ordinary identity
administration.

Every secret pack carries the exact manifest digest and typed slot ids. Import
rejects a digest mismatch, preventing credentials prepared for one set of
hosts from being silently applied to a modified manifest.

## Stable names

Manifests and credential inputs refer to logical names, never SQLite ids or
secret handles. Examples:

```text
tenant:analytics
principal:declan
connection:analytics/warehouse
credential:analytics/warehouse/shared
extension:acme/redaction
```

The importer resolves those names to destination-owned ids and creates new
opaque secret handles.

Each manifest also has an operator-generated `manifest_id` UUID. Copying the
file preserves this id. Managed-resource ownership is the tuple
`(manifest_id, resource_kind, logical_name)`; it is never a destination
database id. `sift instance new` generates the id, while hand-written files
must provide one.

## Source-of-truth model

`sift.instance.toml` is the operator's portable desired-state artifact. The
running server uses a validated, immutable active snapshot stored under its
private state directory. It does not watch an arbitrary TOML path or apply
file changes automatically.

This creates one explicit boundary:

```text
operator file -> parse -> validate -> resolve -> plan -> approve -> active snapshot
```

The active snapshot is not a second hand-edited configuration. It records the
exact normalized manifest, lock, revision, ownership map, and apply report
that produced the current state. `instance export` reconstructs a public
manifest from that snapshot and managed metadata. Runtime environment
overrides may select the state directory and initial file paths, but may not
silently override manifest-managed security policy after initialization.

Container deployments may mount the manifest and lock read-only. Startup
reports drift and exits with a dedicated status when the mounted desired
revision differs from the active revision; an explicit init/apply job performs
the mutation. Daemon and desktop deployments use the same engine through CLI
or authenticated UI.

## Multi-instance desktop topology

This topology is a v1 requirement:

```text
One Sift desktop app
|
+-- Managed local server: Personal
|   +-- own profile.toml, manifest, lock, process, state, metadata, secrets
|
+-- Managed local server: Team/network
|   +-- own profile.toml, manifest, lock, process, state, metadata, secrets
|
+-- Remote server: Work HTTPS
|   +-- local profile.toml -> independently configured remote server
|
+-- Remote server: Lab SSH proxy
    +-- local profile.toml -> independently configured remote server
```

The desktop is a client and process supervisor, not a shared control-plane
server. It may run zero or more managed local server child processes and hold
zero or more remote profiles. Each window/workspace targets exactly one server
at a time; separate windows may target different servers concurrently.

| Desktop entry | Desktop-owned file | Server configuration authority |
| --- | --- | --- |
| Managed local | its own `sift.profile.toml` | its own local manifest + lock |
| Remote HTTPS | its own `sift.profile.toml` | manifest + lock on remote server |
| Remote SSH proxy | its own `sift.profile.toml` | manifest + lock on remote server |
| Imported copy | new `sift.profile.toml` | copied manifest + lock, new instance id |

Thus every entry is file-configured, but the desktop never treats its remote
attachment file as authority over the remote server configuration.

"Server profile" or "Sift instance" is used for these entries. "Connection"
remains reserved for database connection profiles inside one Sift server.

### Per-profile file

Proposed managed-local profile:

```toml
format_version = 1
profile_id = "018f6f31-9fe8-77c2-a705-e5db71debd9e"
kind = "managed-local"
display_name = "Personal"
expected_instance_id = "01J8SIFT6Y6P6PZ7KME9GG4X60"

[managed]
start_policy = "on-demand" # on-demand | app-start | manual
server_directory = "server"
```

Proposed remote profile:

```toml
format_version = 1
profile_id = "018f6f32-3065-76c8-bd80-686061ddfc1c"
kind = "remote"
display_name = "Work"
expected_instance_id = "01J8WORK1W0M6X9CK25J38T9YX"

[remote]
base_url = "https://sift.example.com"
transport = "https" # https | ssh-proxy
credential_ref = "desktop-profile:018f6f32-3065-76c8-bd80-686061ddfc1c"
```

`display_name`, ordering, last selection, and `start_policy` are desktop
preferences and are not exported with the server manifest. `credential_ref`
is an opaque OS-credential-store handle, not a token. A profile may not contain
a bearer token, refresh token, OAuth code, password, device private key, or
server secret.

`expected_instance_id` is absent only for a newly created, unclaimed managed
server. The desktop writes it atomically after successful claim and refuses
automatic credential use before that. Remote profile creation must observe and
confirm an instance id before any credential is saved.

Managed-local paths are relative only to the already-private profile directory
and may not escape it. Remote URLs remain explicit origins. Unknown profile
fields and kinds fail rather than being ignored.

### Desktop directory layout

```text
desktop-state/
  profiles/
    <profile-id>/
      sift.profile.toml
      server/                         # managed-local only
        sift.instance.toml
        sift.instance.lock
        state/
          instance-id
          trust-state
          maintenance.lock
          active/
          metadata.sqlite
      drafts/                         # explicit local/remote admin checkouts
```

Each profile directory is private to the desktop OS user. Remote profiles do
not get a `server/` directory. A remote draft under `drafts/` is inert until an
authenticated admin validates, approves, and applies it to that exact remote
instance.

### Identity model

Three identifiers must never be conflated:

| Identifier | Scope | Portable | Purpose |
| --- | --- | --- | --- |
| `profile_id` | one desktop profile | no | bookmark/process-supervisor identity |
| `instance_id` | one destination server | no | auth, sessions, token and server-identity scope |
| `manifest_id` | one desired configuration lineage | yes | managed-resource ownership across copies |

Two copied servers may share a `manifest_id`; they must have different
`instance_id` and desktop `profile_id` values. Renaming a desktop profile never
changes either server identifier.

### Hosting and supervision

- Every managed local server receives its own child process and state
  directory. The desktop does not multiplex independent servers through one
  runtime or SQLite database.
- `on-demand` starts when selected, `app-start` starts with the desktop, and
  `manual` requires explicit start. A daemon may outlive the desktop only when
  its server runtime mode explicitly permits that.
- Multiple local servers may run concurrently. The supervisor enforces a
  bounded process count, reports resource pressure, and never kills another
  profile to satisfy an implicit limit.
- Bind addresses and ports come from each server's own manifest/destination
  bindings. The supervisor preflights collisions before process start. A
  generated local profile may allocate a free loopback port once and write it;
  it never silently changes a claimed server's address.
- Starting, stopping, or restarting one server cannot acquire another server's
  maintenance lock, mutate its journal, or change its readiness.
- A child crash is contained to its profile. Backoff and health state are
  tracked separately; restart storms are bounded per profile and globally.
- Removing a desktop profile does not delete its server directory by default.
  Destructive removal is a separate, explicit operation that states whether
  the server is stopped and whether its files are recoverable.

### Connecting and switching

- A profile's saved client credential is keyed by `(profile_id, instance_id,
  principal)` in the OS credential store.
- TLS validation and the server-reported immutable `instance_id` are both
  checked before a saved credential is sent. A new id at an existing URL, or a
  changed URL for an existing profile, clears automatic credential use and
  requires explicit review and sign-in.
- Switching a window closes/drops its prior server session, database
  connections, subscriptions, cursors, and cached capabilities before opening
  the new target. Query text may remain local only through an explicit user
  action; it is never submitted automatically to the new server.
- Workspace ids, tenant ids, connection-profile ids, and result caches are
  namespaced by `instance_id`. Equal SQLite ids on two servers are unrelated.
- A local server receives no automatic admin bypass merely because the desktop
  launched it. Before claim, OS authority permits initialization; after claim,
  normal server authentication and step-up rules apply.
- There is no shared login across profiles in v1. The same GitHub account may
  authenticate to several servers, but each issues independent sessions and
  holds independent device-key registration.
- Cross-server copy is explicit export then import. The desktop cannot directly
  transplant metadata, secret handles, sessions, or runtime identity between
  profile directories.

### Server versus tenant boundary

A tenant is an authorization/data-sharing boundary inside one server. A Sift
server is an operational and trust boundary. Use another tenant when the same
operators, auth system, lifecycle, secret backend, upgrades, and failure domain
are intended. Use another server profile when any of those must be independent.

## Proposed manifest schema

The exact schema is finalized with fixtures before implementation. This is the
v1 shape, not executable configuration:

```toml
format_version = 1
manifest_id = "b654b918-b1f1-4d70-924d-e4c1014f482f"
name = "analytics-sift"

[compatibility]
sift = ">=0.8,<0.9"

[server]
deployment = "team"
transport = "network"
mode = "daemon"
bind = "0.0.0.0:7474"
public_base_url = "https://sift.example.com"

[server.timeouts]
request_secs = 30
shutdown_drain_secs = 60

[server.metadata]
secret_backend = "file"
store_sql = false

[auth.github]
client_id = "Ov23liExample"
client_secret = "credential:instance/github-oauth-client-secret"

[[identity.github_principals]]
name = "operator"
subject = "12345678"              # immutable numeric GitHub user id
login_hint = "declan"             # display only
instance_admin = true
bootstrap = true

[[tenants]]
name = "analytics"

[[tenants.memberships]]
principal = "operator"
role = "owner"

[[connections]]
name = "analytics/warehouse"
tenant = "analytics"
provider = "postgres"
credential_mode = "shared"
credential = "credential:analytics/warehouse/shared"
enabled = true
tags = ["production", "warehouse"]

[connections.config]
host = "warehouse.internal"
port = 5432
database = "analytics"
tls_mode = "verify-full"

[connections.policy]
allow_sql = true
allow_schema_read = true
allow_export = false
```

Schema rules:

- `format_version` is mandatory. Unknown versions and unknown fields fail.
- Logical names are unique within their kind, length-bounded, normalized, and
  immutable once a resource is managed. Renames use an explicit `moved_from`
  field in a later version; v1 plans rename as create plus preserve-old.
- Exactly one GitHub principal has `bootstrap = true` in v1. It must also have
  `instance_admin = true` and an immutable decimal GitHub user id. A login or
  organization name is never an authentication subject.
- `bootstrap` authorizes the first claim only. On a claimed instance, changing
  principals or admin roles is an ordinary high-risk desired-state change
  requiring an existing admin's step-up. It never reopens claim mode.
- Connection `config` is checked against the selected provider schema. Sift
  rejects provider fields it does not understand.
- Secret references use named typed slots. TOML cannot contain inline secret
  values in a field whose schema is secret-bearing.
- Local paths are literal, absolute, and surfaced as destination prerequisites.
  There are no `~`, environment, command, or relative-path expansions.
- Every collection has count and byte limits before allocation. Duplicate
  tables and ambiguous dotted-key forms fail.

Not every existing `sift.toml` field should become portable. Process discovery,
temporary directories, key-file paths, cache paths, development driver mocks,
and launcher-owned paths remain destination bindings. The plan reports these
separately and requires the destination to supply them. Security-relevant
behavior such as deployment, transport, auth provider, public origin, limits,
connection policy, workspace roots, VCS network permission, and extension
grants belongs in the manifest.

## Lock schema and reproducibility

`sift.instance.lock` is machine-generated and committed or copied beside the
manifest:

```toml
format_version = 1
manifest_id = "b654b918-b1f1-4d70-924d-e4c1014f482f"
desired_digest = "sha256:..."

[sift]
version = "0.8.3"
protocol = 14
schema_digest = "sha256:..."

[[providers]]
name = "postgres"
schema_digest = "sha256:..."

[[extensions]]
name = "acme/redaction"
version = "1.4.2"
artifact = "sha256:..."
publisher_key = "sha256:..."
```

The desired digest is over a versioned canonical representation of the parsed
typed manifest, not raw TOML bytes. Comments and harmless formatting therefore
do not invalidate the lock or credential pack. Canonicalization materializes
defaults, encodes maps in sorted-key order, preserves arrays where order is
semantic, and has golden fixtures. The lock itself is not a signature: trusted
publisher signatures and configured trust roots authenticate downloaded
artifacts. A changed manifest requires `instance lock` and a new plan.

Sift versions are pinned for reproducibility but compatibility is checked
before mutation. A destination may refuse an older vulnerable Sift version
even if locked; the error must distinguish security policy from a missing
artifact. Offline apply never fetches. Lock creation/fetch is a separate,
bounded operation.

## Credential schemas

The public manifest defines slot identity and derives type from its use. The
generated credential template contains required fields and no values:

```toml
format_version = 1
manifest_id = "b654b918-b1f1-4d70-924d-e4c1014f482f"
desired_digest = "sha256:..."

[credentials."instance/github-oauth-client-secret"]
type = "oauth-client-secret"
required = true

[credentials."analytics/warehouse/shared"]
type = "postgres-password"
required = true
fields = ["username", "password"]
```

An import-only plaintext input uses the same header and typed tables:

```toml
format_version = 1
manifest_id = "b654b918-b1f1-4d70-924d-e4c1014f482f"
desired_digest = "sha256:..."

[credentials."instance/github-oauth-client-secret"]
client_secret = "..."

[credentials."analytics/warehouse/shared"]
username = "sift_runtime"
password = "..."
```

Provider schema decides field names, types, maximum sizes, and whether a value
may be exported. Values are encoded into a versioned typed secret envelope
before `SecretStore::put`; SQLite stores only a random opaque handle, slot id,
type/version, readiness state, and timestamps. Debug formatting is redacted.

Encrypted `sift.secrets` is a small authenticated binary envelope rather than
CBOR alone. Its plaintext payload may use deterministic CBOR, but the outer
format records magic bytes, version, manifest id, desired digest, recipient
metadata, cipher suite, and ciphertext. V1 should use a well-reviewed existing
age implementation and its passphrase or recipient modes, not invent
cryptography. Exact algorithms and dependency review graduate into the ADR
before encrypted export lands.

## Initialization and trust states

The destination has a small durable state machine outside portable metadata:

```text
Virgin -> Initializing -> AwaitingClaim -> Claimed
                    \--------------------> RecoveryRequired
```

- **Virgin** means a fresh destination has never completed setup. It is not
  inferred merely from the absence of an active administrator.
- **Initializing** holds an exclusive maintenance lock and a durable apply
  journal. No server listener is active.
- **AwaitingClaim** exposes only health and the configured identity-provider
  claim flow. It does not activate connections, extensions, hooks, schedules,
  workspaces, VCS, query routes, or general configuration APIs.
- **Claimed** has a destination instance id, at least one usable destination
  administrator, required bootstrap secrets, and an applied configuration.
- **RecoveryRequired** means a previously claimed instance has lost every
  usable administrator or has an interrupted initialization. It never
  reopens first-run setup automatically.

The claimed marker and destination instance id are machine-owned and are not
imported, exported, or restored from a configuration artifact.

## First import on a new device

V1 has no unauthenticated HTTP setup endpoint. First import is an offline,
local operation:

1. The desktop launcher, CLI, init container, SSH console, or service operator
   starts `instance initialize` while the server is stopped.
2. The command resolves an explicit state directory, creates it privately,
   and obtains the exclusive maintenance lock without waiting.
3. It verifies that the destination is genuinely Virgin. A claimed or
   non-empty destination requires authenticated apply or explicit offline
   recovery instead.
4. It parses the bounded manifest and lock without following symlinks,
   includes, remote imports, or environment interpolation.
5. It displays the full redacted plan, including requested bind address,
   public URL, database hosts, administrator rules, extensions, deletions, and
   unresolved credential slots.
6. The manifest names exactly one bootstrap administrator. For GitHub this is
   an immutable numeric GitHub user id plus a display-only expected login; a
   login string, email address, or organization membership alone can never
   bootstrap instance-admin authority.
7. Bootstrap-critical secrets are supplied. These include secrets required
   for configured authentication or TLS. Connection credentials may remain
   unresolved.
8. The importer stages files, bootstrap secrets, and a pending metadata plan,
   records the destination identity, enters AwaitingClaim, and emits a
   sanitized audit record with no actor principal. Product metadata and an
   admin row are not made active yet.
9. The server starts a claim-only listener. A local instance uses an exact
   loopback redirect; a hosted instance requires its configured HTTPS callback
   and exposes no product routes before claim.
10. The operator authenticates with GitHub authorization code flow, `state`,
    and S256 PKCE. Sift fetches the authenticated `/user` profile and compares
    its numeric id with the manifest. The requested or returned login is never
    the authority.
11. A matching callback receives only a narrow, one-use claim-completion
    capability. The client creates a destination device key and proves
    possession to that capability; it does not receive a general admin session
    yet.
12. Claim completion atomically creates the principal, GitHub identity,
    instance-admin role, and device public key; consumes the claim; realizes
    the staged metadata plan; and enters Claimed. The temporary GitHub token is
    discarded.
13. The administrator completes deferred credential setup, reviews readiness,
    and explicitly arms the configured network transport.

The guard before sign-in is local OS authority over the private state and
secret-backend files plus the exclusive maintenance lock. This is the real
trust root: an OS account or root process able to read the metadata and secret
backend can already bypass application authentication. Adding an admin
password prompt to that same local process would not protect against that
authority.

Desktop setup should invoke the initializer through a private child-process
pipe or local IPC. Its later OAuth callback is a narrow claim listener, not an
unauthenticated setup API. Headless and container initialization still
requires console, SSH, `exec`, or an init container before the claim listener
starts. No request may upload or modify a manifest while AwaitingClaim.

## Established-instance authorization

Application authentication protects every online configuration operation.
The public manifest is never served from an anonymous route.

| Operation | Required authority |
| --- | --- |
| View effective redacted configuration | authenticated instance admin |
| Export public manifest or lock | authenticated instance admin |
| Create or edit a draft | authenticated instance admin |
| Validate or plan a draft | authenticated instance admin |
| Apply a draft or arm network transport | instance admin plus recent step-up |
| Import or replace shared credential values | instance admin plus recent step-up |
| View credential status/metadata | authenticated instance admin |
| Reveal an existing credential value | unsupported |
| Export value-bearing credentials | instance admin plus step-up; encrypted output only |
| Prune managed resources | instance admin plus step-up and explicit delete approval |
| Offline recovery/import/export | OS authority, stopped server, exclusive maintenance lock |

V1 uses instance-admin authority for the entire feature. Delegated
configuration roles can be added later without weakening this boundary.

Reading or editing an exported TOML file itself cannot be protected by Sift
authentication; filesystem permissions govern that copy. Editing a file also
does not mutate the server. Only an authorized apply changes live state.

## Step-up authentication

A normal bearer or cookie session is insufficient for secret changes,
configuration apply, network arming, or prune.

Step-up repeats a primary proof and issues a short-lived, one-use grant bound
to all of:

- the current auth session and principal;
- the destination instance id;
- the operation kind;
- the manifest or credential-batch digest; and
- a short expiry.

Password identities repeat password verification. Registered destination keys
sign a new challenge. GitHub OAuth alone does not provide a reliable fresh
proof for a high-risk step-up because an existing provider session may complete
authorization without repeating the account's primary authentication. A
GitHub-only bootstrap admin therefore registers a destination key before
configuration apply, credential mutation/export, network arming, or prune.
Refresh-token rotation and API tokens cannot step up in v1.

The apply/import request consumes the grant atomically. Changing the draft or
credential batch invalidates it. Cookie-authenticated requests retain the
existing CSRF protections.

Online value-bearing export accepts a client-generated encryption recipient,
not a plaintext destination path. The server encrypts each selected secret
before streaming the pack. The client retains the corresponding private
identity and may protect it with a passphrase locally. This keeps plaintext
values out of HTTP responses, browser downloads, and server-side export files.

## Draft and apply model

The UI never edits live configuration field by field:

```text
view -> draft -> validate -> plan -> step-up -> apply
```

Drafts carry the effective-config revision and an ETag. Apply rejects stale
base revisions. Plans contain secret presence and change flags only, never
secret values or reusable secret hashes.

To keep v1 coherent, applying an instance manifest is a maintenance action.
An online administrator may upload, validate, and approve a staged draft, but
the launcher realizes it on a controlled restart under the exclusive
maintenance lock. This avoids a partial split between runtime-file changes
and metadata changes. Ordinary existing APIs remain available for isolated
live edits such as changing one connection policy or password.

Step-up approval creates a content-addressed pending apply record authenticated
by a destination-owned system key. The record contains the exact manifest,
lock, plan, base revision, and approval expiry, but no plaintext credentials.
The launcher applies only that authenticated digest. Replacing a staged file,
changing the plan, passing the expiry, or changing the live base revision
invalidates approval and requires a new online plan and step-up.

## Credential setup UI

After manifest apply, the server exposes first-class credential readiness:

```text
Ready | Missing | Invalid | External | Unsupported
```

The UI is generated from the selected provider's credential schema. A flat
password-only model is insufficient because providers may require usernames,
passwords, certificates, private keys, tunnel credentials, or broker data.

For each slot the admin may enter values, import matching key/value data, skip
an optional slot, or leave a required slot unresolved. Unresolved connections
remain visible but disabled and fail with a stable `MissingCredential` code;
hosted policy never falls back to a raw connection specification.

Connection tests are explicit. Any host, port, database, TLS mode, provider,
or tunnel change invalidates prior test status and requires confirmation before
the server sends an existing credential to the changed destination.

## Reconciliation and deletion

- The manifest creates or updates resources it names.
- Omitted unmanaged resources are preserved.
- Omitted previously manifest-managed resources are preserved by default but
  reported as drift.
- `prune` targets only resources carrying the same manifest ownership id.
- Deletions require a step-up grant and an explicit impact plan showing room,
  workspace, schedule, and active-connection consequences.
- Import never replaces the destination instance id or copies source auth
  sessions.
- Apply must prove at least one usable administrator remains after commit.

## Edge cases and required behavior

### Bootstrap and recovery

- Empty metadata is insufficient evidence of Virgin state.
- A failed or interrupted first import resumes or rolls back through a durable
  journal; it never starts a network listener halfway through.
- AwaitingClaim accepts only the exact configured identity provider and
  immutable subject. Claim attempts have bounded state, expiry, rate limits,
  generic failures, and atomic one-time consumption.
- A GitHub handle rename does not transfer authority. A numeric-id match with
  a changed login produces an explicit rename notice but may claim; a login
  match with a different numeric id is denied.
- GitHub outage, callback failure, deleted account, or lost organization
  access never widens admission. The operator uses explicit offline recovery
  to replace the bootstrap subject.
- A previously claimed instance with no administrator enters
  RecoveryRequired and accepts only offline recovery.
- Importing a personal manifest into a team destination, or the reverse,
  requires an explicit topology transition in the plan.
- Team configuration cannot start with the memory secret backend.

### Credentials

- Unknown, duplicate, missing, extra, or wrong-type secret slots fail before
  mutation.
- Plaintext credential imports have strict size limits, private-file checks,
  no recursive directory reads, and no automatic deletion claim.
- Secret packs reject wrong manifest digests, unsupported versions, malformed
  encryption parameters, oversized payloads, and authentication failure.
- Online export requires a supported public recipient and never returns or
  writes a plaintext value-bearing archive.
- Applying the same secret value is idempotent and does not disconnect active
  connections unnecessarily.
- Failed metadata commit removes newly staged secret handles best-effort and
  records recoverable journal state.
- Export fails closed when any selected backend value is non-exportable.

### Configuration and artifacts

- Unknown manifest fields fail in v1 rather than being ignored.
- Defaults are materialized in the plan so two devices compare effective
  configuration, not omitted syntax.
- Absolute machine paths and public URLs are copied exactly and reported as
  target prerequisites; Sift never guesses replacements.
- Lock verification covers every selected platform artifact available from
  the trusted publisher. Development extension overrides make the instance
  non-reproducible and block locked apply unless explicitly allowed.
- Manifest parsing performs no shell expansion, command substitution, network
  fetch, or environment interpolation.
- Duplicate profile ids, overlapping managed server directories, or an
  `expected_instance_id` mismatch prevent connection/process start.
- Copying a remote `sift.profile.toml` does not copy its OS-keystore credential;
  the copied attachment starts Unverified and requires identity confirmation
  plus sign-in.

### Authorization and audit

- A stolen normal session cannot apply config or replace credentials without
  new primary proof.
- GitHub account compromise grants the configured application authority, so
  the bootstrap identity should use strong provider security and a separate
  destination key is mandatory for Sift's highest-risk operations.
- Step-up grants are non-transferable, one-use, scoped, short-lived, and
  revoked with their auth session.
- Concurrent drafts use revision conflicts rather than last-writer-wins.
- Every initialize, stage, apply, credential import/change, network arm,
  export, prune, failure, and recovery action has a sanitized `Operation`.
- Audit records may contain slot ids and resource ids, never values, plaintext
  paths to secret input, password hashes, or secret equality fingerprints.

## Prior-art lessons

This design deliberately combines patterns instead of copying one product:

- [pgAdmin server import/export](https://www.pgadmin.org/docs/pgadmin4/latest/import_export_servers.html)
  proves database connection definitions can be portable while passwords are
  excluded. Sift keeps that public/private split but adds typed named slots so
  missing credentials are actionable rather than implicit.
- [DBeaver connection configuration](https://dbeaver.com/docs/dbeaver/Admin-Manage-Connections/)
  separates connection data from credential storage, while its server tooling
  also documents the danger of credentials remaining plaintext in predefined
  datasource files before first use. Sift therefore never permits inline
  secrets in the public manifest and never claims plaintext import is safe
  storage.
- [Metabase serialization](https://www.metabase.com/docs/latest/installation-and-operation/serialization)
  uses stable exported resources and excludes database secrets by default, but
  can optionally emit them in plaintext. Sift adopts stable logical identities
  and rejects the plaintext-export escape hatch.
- [Kubernetes Secrets guidance](https://kubernetes.io/docs/concepts/security/secrets-good-practices/)
  separates non-secret and secret resources and stresses least privilege; it
  also makes clear that base64 is not encryption. `sift.secrets` must therefore
  be authenticated encryption, not a CBOR/base64 disguise.
- [Terraform sensitive-data guidance](https://developer.hashicorp.com/terraform/language/manage-sensitive-data)
  shows the value of config, lock, plan, and apply workflows, and the danger of
  letting sensitive inputs leak into state or saved plans. Sift borrows the
  workflow while structurally excluding secret values from plan/state DTOs.

Nix remains useful later as a generator and deployment wrapper. Making a
general evaluator the v1 server input would add impurity, secret interpolation,
language-version, sandbox, and remote-fetch questions before the underlying
portable data and authorization contracts are stable.

## Decisions locked for v1

- One desktop app may host and connect to many servers concurrently.
- Every desktop server entry has its own `sift.profile.toml`; every server has
  its own independent `sift.instance.toml` and `sift.instance.lock`.
- One server process owns exactly one instance identity/state directory. A
  server manifest never defines multiple independent Sift servers.
- Managed-local and remote servers use the same client API and workspace UI,
  but remote configuration remains owned and applied by the remote server.
- TOML describes desired state; it is data, not executable configuration.
- The lock is mandatory and generated.
- Secret values never appear in the public manifest, lock, plan, snapshot, or
  SQLite.
- GitHub bootstrap authority is its immutable numeric user id. A login is only
  a hint. Authoring UI may ask for a login, but it must authenticate the user or
  resolve and display the numeric id before writing the file.
- Initialization is local/offline plus a narrow identity claim; there is no
  anonymous setup API.
- Claimed-instance mutation requires instance-admin plus destination-key
  step-up for the exact digest.
- Apply is explicit, restart-realized, non-pruning by default, and audited.
- Instance admin and database execution authority remain separate.
- Shared secrets may move only by manual input, import-only plaintext input, or
  authenticated encryption. Plaintext export is absent.

## Open decisions that block API freeze

These are Phase 0 deliverables, not license for implementation to improvise:

- exact portable versus destination-bound inventory for every current
  `server::Config` field;
- exact canonical encoding and Unicode/name normalization rules;
- destination key storage on desktop, browser, and headless clients, including
  revocation and recovery UX;
- GitHub OAuth App provisioning UX for different-origin copies;
- whether the existing secret backends need an explicit `exportable()`
  capability rather than treating every successful `get` as exportable;
- exact age crate, format profile, passphrase KDF limits, recipient types, and
  dependency/security review; and
- platform artifact selection when one lock is copied across different CPU/OS
  destinations.

## Simplifications accepted for v1

- TOML is the only canonical source format.
- No embedded Nix-like language, include graph, templates, interpolation, or
  remote imports.
- No unauthenticated HTTP setup wizard; only a fixed, claim-only OAuth surface.
- No plaintext credential export or secret reveal endpoint.
- No API-token step-up or unattended online secret export.
- No delegated configuration-admin role.
- No per-user credential migration.
- No automatic prune.
- No live all-or-nothing split apply; manifests realize on controlled restart.
- No portable destination identity or active login/session state.
- No shared session, automatic single sign-on, cross-server query execution,
  or merged workspace across desktop profiles.
- No bulk apply across profiles. Each server produces and authorizes its own
  plan; a later orchestrator may coordinate independent applies.

These limits leave clear extension points without making the v1 trust model
conditional.

## User workflows

### Add a server to the desktop

```text
Add server
   |
   +-- Host here
   |     +-- New config
   |     `-- Import sift.instance.toml + lock
   |
   `-- Connect to existing server
         `-- HTTPS/SSH origin -> verify instance id -> sign in
```

`Host here` creates a new private profile directory, profile file, server
config set, state directory, and supervised process. `Connect` creates only a
remote profile file and credential-store entry. It never copies or assumes
administrative control of the remote server's manifest.

### New device: simplest path

```text
Choose sift.instance.toml
        |
        v
Review destination + GitHub admin
        |
        v
Supply missing GitHub app secret (only if not in secret pack)
        |
        v
Continue with GitHub
        |
        v
Enter/import missing connection credentials
        |
        v
Review readiness -> Start Sift
```

The desktop may wrap the offline initializer, but it must preserve these trust
boundaries. The first screen shows the manifest name, source path, digest,
public URL, bind address, GitHub numeric id/login hint, connection hosts, and
extension publishers. It never reduces review to an undifferentiated "Import"
button.

CLI equivalent:

```text
sift instance inspect ./sift.instance.toml --lock ./sift.instance.lock
sift instance initialize ./sift.instance.toml --lock ./sift.instance.lock \
  --credentials ./bootstrap.credentials.toml
# server starts claim-only mode; operator completes displayed HTTPS/loopback URL
sift instance status
# after claim
sift credentials import ./sift.credentials.toml
sift instance arm
```

`inspect` is read-only and works without state. `initialize` requires a stopped
server and local filesystem authority. The credential argument is required
only when bootstrap-critical values are not supplied by an encrypted pack or
an existing destination secret binding. Credentials may enter through a
private file, stdin, or private desktop IPC, but never as argv values or
environment variables.

### Copy from an existing instance

1. Admin signs in and exports `sift.instance.toml` plus lock.
2. Export shows exclusions: users' credentials, sessions, documents, query
   history, results, audit history, client preferences, and instance identity.
3. If shared secrets are needed, admin supplies a new encryption recipient,
   completes destination-key step-up, and downloads `sift.secrets`.
4. Files move through the operator's chosen channel.
5. New device follows first initialization and claims as the configured
   immutable GitHub subject.
6. Shared credentials import only when manifest id and desired digest match.
7. Per-user database credentials are re-entered by each user.

### Change an established instance

1. Admin exports or uploads a manifest into a server-side draft.
2. Server parses, validates, resolves lock data, and returns a redacted plan.
3. Admin reviews additions, mutations, drift, preserved omissions, credential
   invalidations, network changes, and prerequisites.
4. Admin completes destination-key step-up for the exact plan digest.
5. Server stages a one-use pending apply.
6. Controlled restart takes the exclusive maintenance lock, rechecks base
   revision and approval, applies, writes the active snapshot, and starts.
7. UI reports success, warnings, disabled resources, or RecoveryRequired.

No apply button appears when the plan would remove the final usable admin,
enable a connection with missing required credentials, weaken a team secret
backend, or use an unresolved/untrusted artifact.

## Command and API surface

Names may change during API review; capability boundaries may not.

Offline/local commands:

```text
sift instance new <path>
sift instance inspect <manifest> [--lock <lock>]
sift instance lock <manifest> --output <lock>
sift instance initialize <manifest> --lock <lock> [--credentials <file>|-]
sift instance status
sift instance recover-admin --github-subject <numeric-id>
sift instance apply-pending
```

Desktop profile commands use the same profile parser and supervisor as the UI:

```text
sift desktop profiles list
sift desktop profiles add-local <server-directory>
sift desktop profiles add-remote <origin>
sift desktop profiles start <profile-id>
sift desktop profiles stop <profile-id>
sift desktop profiles forget <profile-id>
```

`forget` removes only the attachment file and saved client credential. Local
server file deletion is a different, explicitly destructive command and is not
part of v1 unless a recoverable trash implementation is available.

Online admin API:

```text
GET    /v1/admin/instance/config
POST   /v1/admin/instance/drafts
PUT    /v1/admin/instance/drafts/{id}/manifest
POST   /v1/admin/instance/drafts/{id}/validate
POST   /v1/admin/instance/drafts/{id}/plan
POST   /v1/admin/instance/drafts/{id}/approve
GET    /v1/admin/instance/applies/{id}
GET    /v1/admin/credentials/readiness
POST   /v1/admin/credentials/import
PUT    /v1/admin/credentials/{slot}
POST   /v1/admin/credentials/export
POST   /v1/admin/instance/arm
```

Claim-only API:

```text
GET /health
GET /v1/claim/github/start
GET /v1/claim/github/callback
POST /v1/claim/device-key
```

The claim router is constructed separately from the product router. Route
middleware allowlisting alone is insufficient. Claim responses reveal only
generic state and never enumerate configured users, tenants, connections, or
credential readiness.

HTTP DTOs live in `api-types`, not `protocol`, unless they become a shared wire
contract. Secret-bearing request types have manual redacted `Debug`; response
types cannot contain secret value fields. The reference client SDK covers all
online routes before UI integration.

## Internal architecture

### Pure instance model

Add a small `crates/instance-config` crate with serde data types,
schema-version dispatch, semantic validation, normalization, canonical digest,
lock verification, redacted plan types, and fixtures. It performs no I/O,
network access, secret-store access, SQLite work, or process launching. It does
not enter `sift-protocol` because this is an operator/admin contract rather
than the database collaboration protocol.

### Server orchestration

The server owns:

- manifest and lock file reading with safe-open rules and byte limits;
- current-state projection into logical resources;
- diff, authorization, step-up, staged approval, and audit emission;
- reconciliation through existing metadata APIs and new manifest ownership
  APIs;
- typed credential staging through `SecretStore`;
- claim-only GitHub flow and destination-key challenge verification; and
- readiness/arming gates before runtime components are activated.

The apply engine must call the same domain services used by ordinary admin
routes. It must not write application tables with ad hoc SQL. Both Postgres and
SQL Server connection profiles still flow through the existing `Driver`
contract; the instance feature does not add a third execution path.

### Launcher and offline administration

Runtime/launcher code owns the state directory, exclusive maintenance lock,
trust-state marker, active snapshot files, journal, and atomic rename/fsync
sequence. All `sift-admin` offline mutations are moved behind a shared offline
guard that requires an explicitly resolved private state directory, stopped
runtime, and exclusive lock.

The desktop calls this layer over a private child-process pipe/local IPC. It
must not parse secrets into renderer/UI logs. Headless/container use the same
commands, so desktop setup cannot become a distinct security model.

### Metadata and secret storage

New metadata concepts:

- manifest revision and semantic digest;
- manifest ownership for managed logical resources;
- GitHub issuer plus immutable subject on identities;
- configuration draft and pending-apply metadata;
- credential slot metadata/readiness, never values;
- destination public keys and revocation state;
- one-use step-up grant digests; and
- sanitized apply reports and journal correlation ids.

Draft manifest bodies and pending active snapshots belong in private bounded
state files addressed by content digest, not unbounded SQLite text columns.
SQLite may store their digest, revision, owner, status, timestamps, and path
handle. Step-up grants store a keyed digest, never a reusable bearer value.

Secret writes use random new handles. Apply stages them first, commits metadata
references transactionally, then deletes superseded handles after commit.
Rollback deletes staged handles best-effort. A journal and startup sweeper
repair orphan candidates without ever logging their values.

## Apply plan contract

A plan is deterministic for `(base_revision, normalized_manifest, lock,
destination_capabilities)`. It contains:

- exact base and desired revision/digests;
- create/update/preserve/drift/delete sets by logical name;
- old and new security-sensitive endpoints, origins, bind/transport, TLS,
  credential mode, roles, grants, and extension publishers;
- credentials retained, invalidated, missing, unsupported, or externally
  managed;
- destination prerequisites and compatibility errors;
- resources that will remain disabled;
- restart/network-arm requirements;
- final usable-admin proof; and
- an impact summary for active sessions, schedules, workspaces, and rooms.

The plan never contains credentials, OAuth codes, secret handles, password
hashes, stable secret equality hashes, or source paths for secret files.
Approval covers the canonical plan digest. Apply recomputes the plan and uses
constant-time digest comparison before mutation.

Resource actions are one of:

```text
Create | Update | Unchanged | PreserveDrift | Disable | Delete
```

`Delete` can appear only when the operator requested prune and the resource has
matching manifest ownership. V1 cannot rename implicitly or delete unmanaged
resources.

## Activation rules

Configuration application and service activation are separate commits:

- Auth/TLS secrets needed to claim must be Ready before AwaitingClaim.
- A connection with a missing required shared credential is created Disabled.
- Changing its credential destination fields makes it Disabled until explicit
  confirmation and a successful optional test according to policy.
- Extensions stay Disabled until artifact/hash/signature/grant validation is
  complete.
- Workspace/VCS features stay Disabled if a root/executable prerequisite does
  not match the destination.
- Network transport remains Disarmed after first claim until an admin reviews
  readiness and completes step-up.
- Schedules, hooks, migrations, and background jobs cannot run before Claimed
  and Armed, and cannot target a disabled connection.

Arming is idempotent and recorded separately. Restart after an already-armed,
unchanged active revision does not require another approval.

## GitHub bootstrap security

The config-declared GitHub admin is a claim authorization, not a pre-created
logged-in admin session. Required checks:

1. `issuer` is the built-in exact GitHub issuer for v1; arbitrary OAuth issuer
   URLs are not accepted under a `github` label.
2. Authorization code flow uses exact registered callback, random state,
   S256 PKCE, short expiry, one attempt, and bounded server-side state.
3. Callback exchanges the code server-side and calls GitHub's authenticated
   user endpoint.
4. The returned numeric user id, parsed without truncation, must equal the
   configured decimal subject. Login and email are display data only.
5. Token scopes are minimized. The temporary GitHub token is zeroized where
   practical and discarded after profile resolution; it is not a Sift session
   or stored credential.
6. Principal/admin creation, claim consumption, destination-key registration,
   and trust-state transition are atomic or resumable without broadening the
   claim surface.
7. Failed claims are rate-limited by source and claim state and produce generic
   responses. They never fall back to local admin creation.

GitHub OAuth client secret is supplied as a credential slot, never inline in
the public manifest. Because an OAuth App supports a fixed callback setup,
side-by-side copies with different origins require destination OAuth client
bindings; replacing a failed device behind the same origin may reuse the
binding. The plan must call this out before initialization.

GitHub OAuth is not the step-up mechanism. The bootstrap admin registers a
destination Ed25519 key stored in an OS-backed keystore where available. Sift
stores the public key. Recovery codes or alternate key custody are a separate
explicit design gate; until then, lost keys require stopped-server recovery.

## Database/SQL-specific threat model

The highest-risk configuration mutation is not changing Sift itself; it is
redirecting a trusted connection while retaining its credential. Defenses:

- Instance-admin does not imply tenant membership, connection use, SQL
  execution, credential use, or policy bypass. Those remain separate checks.
- Host, port, database, provider, TLS identity, tunnel, proxy, or credential
  mode changes invalidate connection tests and disable automatic use.
- The plan displays old and new destinations side by side. Step-up approval is
  bound to this change.
- Sift never sends a retained credential to the changed destination until an
  admin explicitly chooses `use retained credential` after the plan.
- No schedule, migration, schema refresh, or reconnect runs against the new
  destination before this confirmation.
- TLS downgrade, hostname verification disablement, public-address expansion,
  or new tunnel command is a highlighted security downgrade and may be blocked
  by deployment policy.
- Config import cannot embed SQL to run, startup queries, shell hooks, driver
  arguments outside the typed provider schema, or extension code outside the
  locked extension system.
- Query history, saved documents, results, schema caches, and active sessions
  never cross devices through the manifest.

A compromised instance-admin can deliberately change policy and, if separately
authorized for a tenant/connection, may gain database impact. High-security
deployments should use separate operator and daily-query principals and require
database-side least privilege. Sift cannot protect database credentials from
root/OS authority controlling the server process.

## Failure and recovery model

Every mutation has a journal phase:

```text
Prepared -> SecretsStaged -> AwaitingClaim -> MetadataCommitted
                                      -> SnapshotActivated -> Complete
```

Startup recovery behavior is deterministic:

- Before AwaitingClaim: delete staged handles and discard the draft.
- At AwaitingClaim: retain the authenticated pending plan and bootstrap
  secrets so claim may resume until expiry or explicit offline cancellation.
- After MetadataCommitted but before SnapshotActivated: finish activation from
  authenticated staged content, or enter RecoveryRequired if verification
  fails; never start with mixed revisions.
- After SnapshotActivated: treat the active snapshot as authoritative, finish
  cleanup, and mark Complete.
- Unknown/corrupt journal version: RecoveryRequired with offline inspection;
  never guess rollback direction.

Active snapshot and trust-state writes use same-filesystem temporary files,
file fsync, atomic rename, and parent-directory fsync where supported. Recovery
commands first produce a redacted diagnostic and require explicit confirmation
for any trust-state or bootstrap-subject change. Recovery never exposes a
network setup endpoint.

## Observability and audit

Add explicit `Operation` variants for:

```text
InspectInstanceConfig
InitializeInstance
ClaimInstanceAdmin
CreateConfigDraft
ValidateConfigDraft
PlanConfigApply
ApproveConfigApply
ApplyInstanceConfig
ArmInstance
ImportCredentialSlots
ReplaceCredentialSlot
ExportEncryptedCredentials
PruneManagedResources
RecoverInstanceAdmin
ManageDesktopServerProfile
StartManagedInstance
StopManagedInstance
SwitchActiveInstance
```

Each records result, actor when one exists, instance id, manifest id, desired
digest, revision, affected logical resource ids/counts, and correlation id.
It does not record secret values, input paths, OAuth codes/tokens, exact
credential validation errors from remote systems, or raw TOML.

Desktop-only operations use the same typed/redacted operation discipline in a
local bounded diagnostic log; they cannot be written to a remote server's
audit trail before authentication. Server-visible connect/auth actions retain
their ordinary server audit records.

Metrics use bounded labels: trust state, apply result, resource kind, failure
class, and credential readiness counts. Logical names, paths, URLs, usernames,
GitHub ids, hosts, and manifest digests are not metric labels. Logs use the
same redaction types as API errors.

## Compatibility and migration

V1 adoption must not reinterpret an existing installation as Virgin.

- Existing initialized metadata receives a Claimed marker during an explicit
  migration/startup check only when its current admin invariant is valid.
- Current `sift.toml` continues to start legacy/unmanaged installations during
  a deprecation window. `instance adopt` produces a draft manifest and a plan;
  it never silently takes ownership of existing resources.
- Existing secret handles remain valid. Adoption maps them to credential slots
  without reading/exporting their values.
- Existing GitHub identities must be backfilled from a previously verified
  immutable numeric id. If only a login exists, adoption blocks and asks for a
  fresh authenticated verification; it never looks up a mutable login and
  assumes continuity.
- Existing offline `bootstrap-admin` remains recovery-only after trust-state
  migration and acquires the maintenance lock. It cannot reset a claimed
  instance merely because admins are disabled.
- `sift.toml` environment secret fields remain accepted only as destination
  bindings during transition. Export converts references to credential slots,
  never their values.

No automatic import runs merely because `sift.instance.toml` appears in the
working directory.

Desktop migration is also explicit and one-way:

- The current singular local target becomes one generated managed-local profile
  pointing at its existing state directory; files are not moved until a
  separately recoverable migration is implemented.
- Every current saved remote server becomes its own `sift.profile.toml`.
- A saved remote token is rekeyed to `(profile_id, observed_instance_id,
  principal)` only after TLS and instance-id verification. Otherwise the
  profile is created signed-out.
- Presentation ids such as `local` and `hosted:<profile>` migrate to the new
  profile/instance namespace. Stale workspace and connection selections are
  cleared rather than guessed.
- Command-line startup remote settings create an ephemeral profile unless the
  user explicitly chooses Save; environment input never overwrites a persisted
  profile file.

## Test strategy and acceptance criteria

### Pure schema tests

- Golden parse/normalize/export/digest fixtures for every schema version.
- Comments/formatting preserve desired digest; semantic changes alter it.
- Unknown, duplicate, oversized, non-normalized, ambiguous, and wrong-type
  inputs fail with paths but without reflecting secret-shaped values.
- Provider schemas reject unknown config and credential fields.
- Lock resolution and compatibility errors are deterministic offline.
- Property/fuzz tests never panic or allocate beyond configured bounds.

### Security tests

- Login/email/org match with wrong GitHub numeric id cannot claim.
- Numeric id with renamed login can claim and emits a rename notice.
- OAuth state, PKCE, expiry, callback, replay, rate-limit, and concurrent-claim
  failures remain claim-only.
- AwaitingClaim exposes exactly health and claim routes; all other routes and
  background workers remain unavailable.
- Normal session, API token, refresh token, expired grant, wrong operation,
  wrong digest, wrong instance, or replayed step-up cannot mutate config.
- Secret fields never appear in logs, errors, audit rows, plans, snapshots,
  SQLite, HTTP responses, crash diagnostics, or test snapshots.
- Malicious host substitution cannot receive retained credentials without the
  separate confirmation.
- Symlink swaps, special files, loose credential-file permissions, oversized
  files, and TOCTOU attempts fail closed.
- A saved credential is never sent after a profile URL change or destination
  instance-id mismatch.
- Tokens, device keys, tenant/connection ids, caches, and sessions from profile
  A are unusable against profile B, including when both use the same URL host
  or have colliding SQLite row ids.
- A remote draft cannot mutate its target without that remote instance's admin
  session and digest-bound step-up.

### Reconciliation and crash tests

- Same manifest apply is a no-op with stable ownership.
- Omission preserves by default; prune affects only matching ownership.
- Stale revision and concurrent apply reject before secret mutation.
- Crash injection at every journal phase reaches old state, new state, or
  RecoveryRequired—never a mixed running state.
- Secret-store failure and SQLite failure clean or journal orphan handles.
- Final-admin removal, team memory backend, missing auth secret, corrupt lock,
  and unsupported extension block apply.
- Connection destination changes disable use and all background execution.
- Starting, stopping, crashing, applying, recovering, or deleting a draft for
  one managed local server leaves every other profile's process, lock, journal,
  state files, and active sessions unchanged.
- Duplicate profile ids, duplicate profile directories, state-directory
  overlap, bind collisions, and symlinked profile roots fail before process
  start.
- Removing a profile preserves managed server files and reports how to
  reattach them.

### Desktop multi-instance tests

- Mixed managed-local and remote profiles restore from separate profile files.
- Multiple managed local child processes run concurrently with independent
  health, backoff, logs, and shutdown behavior.
- Multiple windows may target different servers; each window holds at most one
  live server/session target at a time.
- Switching targets closes old connections/subscriptions/cursors before new
  execution becomes possible and clears server-scoped UI selections.
- Profile rename/reorder/start-policy changes never alter server config,
  manifest id, instance id, or credential contents.
- Remote profile creation does not create a local active server manifest.
- Explicit admin checkout is inert; apply goes to the pinned destination only.
- Per-profile and global supervisor resource/backoff limits prevent restart
  storms without silently stopping a healthy profile.

### Deployment matrix

Cover personal/team x loopback/network/ssh-proxy x in-process/daemon/container,
including invalid combinations. Exercise new initialization, existing adoption,
same-origin replacement, different-origin copy, GitHub outage, no network,
read-only manifest mounts, key loss, and offline recovery. Repeat representative
cases with several concurrent local servers plus HTTPS and SSH remote profiles.

### Definition of done

A release is complete only when:

- copying manifest plus lock to a clean supported device yields the same
  normalized effective desired state and locked artifacts;
- no source instance identity, session, user preference, user credential, or
  content crosses unless explicitly in an allowed encrypted shared-secret pack;
- a person can complete the desktop happy path as `Choose config -> GitHub ->
  credentials -> Start` without reading documentation;
- one desktop can concurrently host multiple isolated local servers, connect to
  multiple remote servers, and switch/window them without cross-instance state;
- every desktop entry has one profile file and points to exactly one independent
  server configuration set;
- the CLI completes the same flow without a browser-hosted setup API;
- all mutations require the documented authority and emit sanitized Operations;
- interruption at every durable phase has tested recovery; and
- format, clippy, workspace tests, cargo-deny, schema fixtures, and the full
  security/deployment matrix pass.

## Ordered implementation plan

### Phase 0 — decisions and fixtures

1. Graduate multi-instance desktop topology, per-profile files, process/state
   isolation, source-of-truth, trust-state, GitHub subject, secret-pack
   boundary, reconciliation, and restart-apply rules into an ADR.
2. Decide the exact portable/destination-bound field inventory from current
   `Config`; document every exclusion.
3. Freeze v1 desktop-profile, manifest, lock, credential-template,
   credential-input, redacted plan, and apply-report fixtures.
4. Threat-model local initialization, OAuth claim, connection redirection,
   extension artifacts, offline recovery, and encrypted export.

Exit: ADR accepted; examples parse against a written schema; no public DTO or
metadata migration has landed first.

### Phase 1 — pure model and inspect tooling

1. Add `instance-config` with strict serde models, normalization, validation,
   canonical digest, redaction, compatibility, and lock verification.
2. Add provider config/credential schema adapters without coupling the pure
   crate to drivers or I/O.
3. Implement `instance new`, `inspect`, and credential-template generation.
4. Add golden, property, fuzz, and size-limit tests.

Exit: arbitrary manifests can be safely inspected and deterministically
digested without server state or network access.

### Phase 2 — trust state and offline safety

1. Add durable destination identity, trust-state marker, private state layout,
   and initialization/apply journal.
2. Centralize exclusive maintenance-lock acquisition for every offline command,
   including existing `sift-admin` mutations.
3. Add Virgin detection that cannot be recreated by deleting SQLite rows.
4. Implement crash-safe state-file primitives and RecoveryRequired diagnostics.

Exit: offline mutation and running server are mutually exclusive; crash tests
prove deterministic state transitions.

### Phase 3 — metadata ownership and planning

1. Add manifest revision/ownership and credential-slot metadata migrations.
2. Project existing metadata into logical desired/current resource models.
3. Implement deterministic diff, drift, final-admin invariant, activation
   gates, and redacted apply report.
4. Add `instance adopt` for legacy installations without taking ownership.

Exit: Sift can plan create/update/preserve/prune accurately with zero live
mutation and no secret reads.

### Phase 4 — first initialization and GitHub claim

1. Implement offline initialize through Prepared/SecretsStaged/AwaitingClaim,
   then claim completion through MetadataCommitted/SnapshotActivated.
2. Add separate AwaitingClaim runtime/router with all workers disabled.
3. Implement GitHub code flow with state, S256 PKCE, exact callback, numeric
   subject match, token disposal, bounded attempts, and rate limits.
4. Add atomic first principal/admin creation and destination-key registration.
5. Add readiness review and explicit network arming.

Exit: a clean destination can be safely reproduced and claimed; there is no
unauthenticated config upload/edit endpoint.

### Phase 5 — established-instance drafts and step-up

1. Add admin config read/export, bounded drafts, ETags, validate, and plan APIs.
2. Add destination-key challenge/registration/revocation and one-use,
   digest-scoped step-up grants.
3. Stage authenticated pending applies and realize them under maintenance lock
   on controlled restart.
4. Add status/apply-report APIs and reference client SDK methods.

Exit: authenticated admins can change the file-based desired state without
hot mutation, stale apply, or session-only authorization.

### Phase 6 — typed credentials

1. Add readiness/status UI model and provider-typed manual entry.
2. Add strict plaintext import from private file/stdin and online encrypted
   request bodies, with no plaintext export.
3. Implement staged handle swap, rollback journal, orphan sweeper, idempotence,
   and non-exportable backend behavior.
4. Enforce destination-change invalidation and explicit retained-credential
   confirmation before tests/use.

Exit: shared credentials can be supplied portably or manually while secret
bytes remain outside SQLite, logs, plans, snapshots, and responses.

### Phase 7 — lock and extensions

1. Generate/verify exact Sift, protocol, provider-schema, extension artifact,
   publisher-key, and platform entries.
2. Reuse signed extension resolution/staging; separate online lock generation
   from offline apply.
3. Block untrusted/missing/wrong-platform artifacts and clearly mark deliberate
   development overrides as non-reproducible.

Exit: every executable/config schema dependency affecting reproduced behavior
is pinned or explicitly diagnosed.

### Phase 8 — encrypted secret packs

1. Complete crypto/dependency ADR and independent security review.
2. Implement age recipient/passphrase import and recipient-only online export.
3. Bind authenticated payload to manifest id, desired digest, slot ids/types,
   format version, and limits.
4. Add corrupt/wrong-recipient/downgrade/replay/redaction tests.

Exit: shared secret portability never requires Sift to export plaintext.

### Phase 9 — multi-instance desktop catalog and supervisor

1. Add strict `sift.profile.toml` types, private per-profile directory
   discovery, migrations, identity pinning, and OS-credential-store scoping.
2. Replace the desktop's singular local target with a collection of managed
   child-process targets plus remote targets.
3. Add bounded concurrent supervision, independent health/backoff, bind and
   directory-overlap preflight, and safe start/stop/restart lifecycle.
4. Namespace all restored workspace/server selections and client caches by
   destination instance id; make target switching tear down old live state.
5. Add multi-window/multi-target behavior and all desktop isolation tests.

Exit: one desktop can safely host and connect to the required mixed topology;
every entry is file-backed and every server remains an independent trust and
failure domain.

### Phase 10 — desktop UX and hardening

1. Build `Add server -> Host here | Connect` and the short new-device flow on
   the same CLI/server primitives.
2. Build the instance switcher plus established-instance export, draft plan,
   step-up, readiness, and apply status screens with security-sensitive diffs
   emphasized.
3. Run usability testing without documentation and remove unnecessary choices.
4. Complete the concurrent mixed-topology deployment matrix, crash injection,
   fuzzing, audit review, dependency review, and operator documentation.

Exit: all definition-of-done criteria pass. Only then consider additional
configuration languages or remote automation.

## Deferred extensions

- Nix/CUE/Nickel/Starlark generators that emit canonical TOML and lock input.
- Signed organization manifests and policy-as-code.
- Delegated configuration operator roles and multi-party approval.
- Multiple bootstrap identity providers or GitHub device flow.
- Automatic GitOps reconciliation and carefully scoped prune policy.
- Destination-variable overlays for paths/origins without general evaluation.
- Portable per-user credentials with each user's own recipient keys.
- Hardware-backed/WebAuthn step-up and managed recovery key escrow.

These consume the same data contract. None should add an evaluator, remote
fetch, secret interpolation, or weaker authentication to v1 initialization.
