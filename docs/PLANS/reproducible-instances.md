# Reproducible Sift instances

Status: design revised around a flake-style two-file contract; Phase 0 freezes
exact schemas and selected dependencies; no implementation has started.

## Goal

Recreate the operator-controlled configuration of a Sift server on another
device without copying the source installation's runtime identity, active
sessions, user content, or client preferences.

One Sift desktop application may host and connect to many independent Sift
servers at the same time. Every server has its own configuration set, runtime
identity, state directory, metadata, secrets, auth boundary, audit trail, and
process lifecycle. There is no combined multi-server manifest and no server
process containing multiple independent Sift instances.

The user-facing contract deliberately mirrors the useful part of a Nix flake:
one editable desired-state file, one generated lock, content-addressed inputs,
and immutable generations. It does not embed the Nix language or attempt to
reproduce the host operating system.

```text
server-root/
  sift.instance.toml   # operator edits and copies this
  sift.instance.lock   # Sift generates and copies this
```

Everything else is generated private state or an optional transport artifact,
not another canonical configuration file.

## V1 artifacts

### `sift.instance.toml`

The public, human-readable desired configuration. "Public" means
non-secret, not anonymously readable through the server. It contains:

- deployment and transport policy, limits, timeouts, audit policy, and exact
  or schema-declared symbolic device bindings;
- tenants, declarative admission rules, memberships, and instance-admin
  intent;
- connection profiles, provider configuration, tags, and connection policy;
- named credential slots, but never credential values;
- extension selections, requested versions, grants, and tenant allowlists.

It excludes themes, keybindings, window state, selected instances, pane
layout, documents, results, query history, audit rows, runtime locks, caches,
auth sessions, refresh tokens, API tokens, and the destination instance id.

Each Sift server owns exactly one active manifest/lock pair. For a
desktop-managed local server the files live in its registered server root. For
a remote server they live with the remote deployment; the desktop bookmark
does not silently cache or override them. An authorized admin
may explicitly export/check out a remote manifest as a draft.

### `sift.instance.lock`

Generated, public resolution data. It pins the configuration digest, Sift and
protocol compatibility, provider schema digests, and external extension
artifacts. It is mandatory for `up`/apply in v1 and callers must never
hand-edit it.

### Secret input and optional transport bundle

Secrets are inputs, never a third canonical config file. Normal setup prompts
for missing typed slots and writes values directly to the destination secret
backend. Automation may stream a bounded typed document over stdin. Sift does
not create or require a persistent plaintext credentials file.

An optional encrypted `.siftbundle` packages the manifest, lock, and selected
exportable shared secrets for transport. It is a one-file delivery envelope,
not active configuration: import verifies and extracts the public pair into a
server root, then writes decrypted values directly to `SecretStore`. Sift never
writes a plaintext secret export.

V1 encrypted bundles may contain server-owned or shared configuration secrets:
shared connection credentials, OAuth client secrets, and extension service
credentials. Per-user database credentials, login password verifiers, API
tokens, refresh tokens, and one-use credentials are excluded. Users provision
their own per-user database credentials after signing in. Destination login
credentials are established during bootstrap or through ordinary identity
administration.

Every encrypted bundle carries the configuration digest and typed slot ids.
Import rejects a digest mismatch, preventing credentials prepared for one set
of hosts from being silently applied to a modified manifest.

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
running server uses the selected immutable generation under private machine
state. It does not hot-watch or automatically apply edits.

This creates one explicit boundary:

```text
editable manifest -> lock -> resolve -> plan -> approve -> generation -> switch
```

The generation is not a second hand-edited configuration. It records the exact
normalized manifest, lock, resolved device bindings, revision, ownership map,
and apply report. `instance export` reconstructs the public pair from it.
Runtime environment may locate the initial config root only; it may not supply
or silently override manifest-managed policy or bindings after initialization.

The resolved source mode is private machine state, not another config file:

- **managed** (desktop/default): an approved UI/API change atomically writes
  the exact new manifest plus regenerated lock into the registered server root,
  fsyncs them, then selects the matching generation;
- **read-only** (GitOps/container): online UI/API may inspect, plan, and export
  a draft but cannot apply it; the operator updates the mounted pair and invokes
  apply through the deployment workflow.

Manual file edits never hot-apply. `inspect/plan/apply` compares file,
generation, and lock digests. Lock regeneration after an ordinary semantic edit
preserves already selected input versions; only `lock --update` changes them.

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
|   +-- own config root, process, state, metadata, secrets
|
+-- Managed local server: Team/network
|   +-- own config root, process, state, metadata, secrets
|
+-- Remote server: Work HTTPS
|   +-- internal bookmark -> independently configured remote server
|
+-- Remote server: Lab SSH proxy
    +-- internal bookmark -> independently configured remote server
```

The desktop is a client and process supervisor, not a shared control-plane
server. It may run zero or more managed local child processes and remember zero
or more remote servers. Each window/workspace targets exactly one server at a
time; separate windows may target different servers concurrently.

Each actual server owns its own two-file config root. Desktop display names,
ordering, last selection, remote bookmarks, start policy, and credential-store
handles are ordinary client preferences in implementation-owned private state.
They are not another public TOML format and are excluded from server export.
Connecting to a remote server does not require or create a local copy of its
server config. An authorized admin may explicitly check out its manifest and
lock as an inert draft.

"Server profile" or "Sift instance" is used for these entries. "Connection"
remains reserved for database connection profiles inside one Sift server.

### Identity model

Three identifiers must never be conflated:

| Identifier | Scope | Portable | Purpose |
| --- | --- | --- | --- |
| catalog id | one desktop attachment | no | internal bookmark/process identity |
| `instance_id` | one destination server | no | auth, sessions, token and server-identity scope |
| `manifest_id` | one desired configuration lineage | yes | managed-resource ownership across copies |

Two copied servers may share a `manifest_id`; they must have different
`instance_id` and desktop catalog ids. Renaming a desktop entry never changes
either server identifier.

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
- Bind addresses and ports come from the server's realized generation. The
  supervisor preflights collisions before process start and never silently
  changes a claimed server's resolved address.
- Starting, stopping, or restarting one server cannot acquire another server's
  maintenance lock, mutate its journal, or change its readiness.
- A child crash is contained to its profile. Backoff and health state are
  tracked separately; restart storms are bounded per profile and globally.
- Forgetting a desktop entry does not delete its config root or private state.
  Destructive removal is a separate operation that states whether the server
  is stopped and whether its files are recoverable.

### Connecting and switching

- A saved client credential is keyed by `(catalog_id, instance_id, principal)`
  in the OS credential store.
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
  servers.

### Server versus tenant boundary

A tenant is an authorization/data-sharing boundary inside one server. A Sift
server is an operational and trust boundary. Use another tenant when the same
operators, auth system, lifecycle, secret backend, upgrades, and failure domain
are intended. Use another server when any of those must be independent.

## Device binding without another config file

Host-specific values use constrained symbolic values inside the same editable
manifest. They are not arbitrary overlays or environment interpolation:

```toml
[server]
deployment = "personal"
transport = "loopback"
bind = "auto-loopback"
state = "managed"
public_base_url = "auto-loopback"
```

For a hosted server, the operator normally pins exact values:

```toml
[server]
deployment = "team"
transport = "network"
bind = "0.0.0.0:7474"
public_base_url = "https://sift.example.com"
```

Only schema-declared binding fields may use `auto-*`, `managed`, or
`prompt-required`. Resolution occurs during `inspect/up`, appears in the plan,
and is recorded in the immutable generation. It never creates `target.toml`.
Environment variables cannot supply or silently override a managed binding.

Two digests distinguish intent from realization:

- **configuration digest**: normalized editable manifest; stable across
  devices when symbolic bindings are unchanged;
- **realization digest**: configuration digest, lock digest, selected platform,
  and every resolved binding; identifies the exact running generation.

Normal export preserves symbolic bindings for portability. `export --realized`
emits a new single manifest with resolved values baked in for an exact
same-topology replacement. Security-sensitive bindings used by a credential
slot, such as a database destination or OAuth public origin, participate in
that slot's consumer digest; changing them prevents silent credential reuse.

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
flow = "hosted-code"
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
- `github.flow = "local-device"` is valid only for managed-loopback first claim
  and forbids a client-secret slot. Network/team sign-in uses `hosted-code` and
  requires its exact public origin and client credential binding.
- Connection `config` is checked against the selected provider schema. Sift
  rejects provider fields it does not understand.
- Secret references use named typed slots. TOML cannot contain inline secret
  values in a field whose schema is secret-bearing.
- Portable host fields accept only exact values or their schema-declared
  symbolic binding. Exact local paths are absolute. There are no `~`,
  environment, command, or arbitrary relative-path expansions.
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
configuration_digest = "sha256:..."

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

[signature]
algorithm = "ed25519"
key_id = "operator:production"
value = "base64:..."
```

The configuration digest is over a versioned canonical representation of the
parsed typed manifest, not raw TOML bytes. Comments and harmless formatting
therefore do not invalidate the lock or encrypted bundle. Canonicalization
materializes defaults, encodes maps in sorted-key order, preserves arrays where order is
semantic, and has golden fixtures. The optional lock signature covers the
configuration digest plus canonical lock content excluding the signature block.
Personal local setup may accept an unsigned lock after displaying its digest;
team policy may require a key from a configured trust root. Publisher
signatures separately authenticate downloaded artifacts. Editing manifest or
lock invalidates the operator signature and requires `lock` plus a new plan.

The lock records a closure for every selected platform: exact Sift binary,
protocol/schema, providers, extensions, publisher identities, signatures, and
artifact hashes. Locked artifacts live in a destination content-addressed
store. Generations reference hashes, never mutable download paths. Fetching a
missing closure is explicit during `lock/up`; switching or rollback performs
no network fetch once the closure is present.

Sift versions are pinned for reproducibility but compatibility is checked
before mutation. A destination may refuse an older vulnerable Sift version
even if locked; the error must distinguish security policy from a missing
artifact. `lock --update` is the only normal operation that changes resolved
versions. `vendor` creates a public offline bundle containing the manifest,
lock, exact platform closure, signatures, and trust metadata but no secrets.

## Immutable generations

Every successful realization creates one immutable generation in private
machine state:

```text
generations/42/
  normalized-manifest.cbor
  lock.cbor
  realization.cbor
  plan.cbor
  apply-report.cbor
current -> generations/42
```

These are generated records, not operator config files. A generation records:

- configuration and lock digests;
- realization digest and every resolved device binding;
- destination instance id and selected platform;
- exact content-addressed artifact closure;
- managed-resource ownership/revision;
- credential slot handles and versions, never values; and
- parent generation, actor, approval, timestamps, and sanitized result.

Apply builds the new generation beside the current one, verifies it, commits
metadata/secrets, then atomically switches `current`. Runtime starts only from
the selected complete generation. Failed realization leaves the old generation
selected.

```text
sift instance generations
sift instance diff 41 42
sift instance rollback 41
sift instance pin 41
sift instance gc
```

Rollback produces a new plan and generation whose desired state matches the
selected ancestor; history remains append-only. It requires the same admin,
step-up, lock, final-admin, and destination-change checks as apply. Credentials
do not silently roll back: an old handle may be reused only when still present,
allowed by backend policy, and explicitly shown; otherwise the slot becomes
Missing. GC retains current, pinned, pending, rollback-protected, and policy-
retained generations plus their artifact closures. It deletes nothing still
reachable.

## Reproducibility boundary

For the same supported platform, exact bindings, supplied secret slots, and
available locked closure, Sift guarantees the same normalized control-plane
configuration, selected Sift/provider/extension artifacts, managed resources,
and activation policy. `verify` compares the running generation against those
inputs; `doctor` separately reports mutable prerequisites.

Sift does not claim to reproduce destination identity, timestamps, audit rows,
sessions, user content, database contents/schema, database-side permissions,
DNS answers, certificates issued after realization, GitHub availability, OS
kernel, system libraries outside the shipped closure, hardware, or network
behavior. Those are checked prerequisites or external state. Thus the feature
is flake-like and reproducible inside Sift's declared boundary, not a NixOS
system derivation or a bit-identical machine image.

## Credential schemas

The public manifest defines slot identity and derives type from its use. UI and
`credentials template` may display or stream this generated shape, but it is
not another required file:

```toml
format_version = 1
manifest_id = "b654b918-b1f1-4d70-924d-e4c1014f482f"
configuration_digest = "sha256:..."

[credentials."instance/github-oauth-client-secret"]
type = "oauth-client-secret"
required = true

[credentials."analytics/warehouse/shared"]
type = "postgres-password"
required = true
fields = ["username", "password"]
```

Automation may stream an import-only plaintext document with the same header
over stdin or a private IPC pipe:

```toml
format_version = 1
manifest_id = "b654b918-b1f1-4d70-924d-e4c1014f482f"
configuration_digest = "sha256:..."

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

Encrypted secret transport lives only inside `.siftbundle`. Its plaintext
payload may use deterministic CBOR, but the authenticated envelope records
magic bytes, version, manifest id, configuration digest, slot consumer
digests, recipient metadata, cipher suite, and ciphertext. V1 should use a
well-reviewed existing age implementation and its passphrase or recipient
modes, not invent cryptography. Exact algorithms and dependency review
graduate into the ADR before encrypted export lands.

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
   runs `instance up <server-root|bundle>` while the server is stopped.
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
7. Bootstrap-critical secrets are supplied. Managed-loopback GitHub bootstrap
   needs no per-instance client secret; hosted auth/TLS may. Connection
   credentials may remain unresolved.
8. The importer stages files, bootstrap secrets, and a pending metadata plan,
   records the destination identity, enters AwaitingClaim, and emits a
   sanitized audit record with no actor principal. Product metadata and an
   admin row are not made active yet.
9. The server starts claim-only mode and exposes no product routes. Managed
   loopback setup uses the constrained device claim described below; hosted
   setup uses its configured HTTPS callback.
10. The operator proves GitHub identity. Sift fetches the authenticated `/user`
    profile and compares its numeric id with the manifest. The requested or
    returned login is never the authority.
11. A matching result receives only a narrow, one-use claim-completion
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
pipe or local IPC. Its GitHub result reaches only a narrow claim capability,
not an unauthenticated setup API. Headless and container initialization still
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
| Apply a standard-risk draft | authenticated instance admin |
| Apply an elevated/destructive draft or arm network transport | instance admin plus recent step-up |
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
security-sensitive/restart-required/destructive configuration apply, rollback,
network arming, or prune. Cosmetic standard-risk changes do not force repeated
device proof.

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
elevated configuration apply, credential mutation/export, rollback, network
arming, or prune.
Refresh-token rotation and API tokens cannot step up in v1.

The apply/import request consumes the grant atomically. Changing the draft or
credential batch invalidates it. Cookie-authenticated requests retain the
existing CSRF protections.

Online value-bearing export accepts a client-generated encryption recipient,
not a plaintext destination path. The server encrypts each selected secret
before streaming the bundle. The client retains the corresponding private
identity and may protect it with a passphrase locally. This keeps plaintext
values out of HTTP responses, browser downloads, and server-side export files.

## Draft and apply model

The UI never edits live configuration field by field:

```text
view -> draft -> validate -> plan -> [step-up when elevated] -> apply
```

Drafts carry the effective-config revision and an ETag. Apply rejects stale
base revisions. Plans contain secret presence and change flags only, never
secret values or reusable secret hashes.

Every managed UI/API change edits the desired manifest draft and creates a new
generation. In managed source mode it also writes the approved public pair; in
read-only mode apply is rejected and the draft is export-only. There is no
second direct-write path that can silently diverge managed metadata from the
file source of truth. Secret-value rotation updates the typed slot revision
without placing the value in the manifest.

Plans classify realization:

- **live-safe**: transactional metadata changes such as labels, memberships,
  allowlists, or connection policy; switch generation without process restart;
- **restart-required**: bind, transport, auth provider, secret backend,
  runtime, or executable extension changes; launcher switches under the
  exclusive maintenance lock; and
- **destructive**: prune, ownership transfer, or final-admin-sensitive changes;
  requires separate impact confirmation in addition to step-up.

Lifecycle class and authorization risk are orthogonal. A membership or
connection-policy change can be live-safe but elevated. A display-name or tag
change can be live-safe and standard. Any auth/admin, network origin/bind, TLS
downgrade, secret consumer, execution policy, extension code/grant, prune,
rollback, or restart-required change is elevated at minimum. Mixed plans take
the strongest lifecycle and authorization class.

Approval creates a content-addressed pending apply record authenticated by a
destination-owned system key. Elevated records also carry the consumed
step-up grant. The record contains the exact manifest, lock, plan, base
revision, and approval expiry, but no plaintext credentials.
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
- Plaintext credential streams have strict size/time limits, accept only stdin
  or authenticated private IPC, and are never persisted or echoed by Sift.
- Encrypted bundles reject wrong configuration/consumer digests, unsupported
  versions, malformed
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
- Exact machine bindings are copied exactly. Symbolic bindings are resolved
  only through their typed resolver and recorded in the generation.
- Lock verification covers every selected platform artifact available from
  the trusted publisher. Development extension overrides make the instance
  non-reproducible and block locked apply unless explicitly allowed.
- Manifest parsing performs no shell expansion, command substitution, network
  fetch, or environment interpolation.
- Overlapping managed server/state directories or an expected destination
  instance-id mismatch prevent connection/process start.
- Copying a desktop bookmark does not copy its OS-keystore credential; the
  copied attachment starts Unverified and requires identity confirmation plus
  sign-in.

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
  also makes clear that base64 is not encryption. Optional secret transport
  must therefore use authenticated encryption, not a CBOR/base64 disguise.
- [Terraform sensitive-data guidance](https://developer.hashicorp.com/terraform/language/manage-sensitive-data)
  shows the value of config, lock, plan, and apply workflows, and the danger of
  letting sensitive inputs leak into state or saved plans. Sift borrows the
  workflow while structurally excluding secret values from plan/state DTOs.

The operational model intentionally borrows from
[Nix flakes and locked inputs](https://nix.dev/manual/nix/stable/command-ref/new-cli/nix3-flake.html#lock-files):
one editable root, generated lock, explicit input updates, content-addressed
artifacts, immutable realizations, atomic selection, rollback, pinning, and GC.
It does not claim Nix-level host or bit-for-bit build reproducibility; Sift
realizes application control-plane state, not an OS derivation. Nix itself
notes that deterministic dependency references and sandboxing are strong
foundations but do not alone remove all build nondeterminism in
[its reproducible-build guidance](https://reproducible.nixos.org/).

A future Nix/CUE/Nickel generator may emit the same single manifest. Making a
general evaluator the v1 server input would add impurity, secret interpolation,
language-version, sandbox, and remote-fetch questions before the portable data
and authorization contract is stable.

## Decisions locked for v1

- One desktop app may host and connect to many servers concurrently.
- Every actual server has one independent editable `sift.instance.toml` and
  generated `sift.instance.lock`; desktop bookmarks are private app state.
- One server process owns exactly one instance identity/state directory. A
  server manifest never defines multiple independent Sift servers.
- Managed-local and remote servers use the same client API and workspace UI,
  but remote configuration remains owned and applied by the remote server.
- TOML describes desired state; it is data, not executable configuration.
- The lock is mandatory and generated.
- Secret values never appear in the public manifest, lock, plan, generation,
  or SQLite.
- GitHub bootstrap authority is its immutable numeric user id. A login is only
  a hint. Authoring UI may ask for a login, but it must authenticate the user or
  resolve and display the numeric id before writing the file.
- Initialization is local/offline plus a narrow identity claim; there is no
  anonymous setup API.
- Claimed-instance mutation requires instance-admin plus destination-key
  step-up for the exact digest.
- Apply is explicit, generation-based, non-pruning by default, and audited;
  only restart-required plans restart.
- Instance admin and database execution authority remain separate.
- Shared secrets may move only by manual input, ephemeral plaintext stdin/IPC,
  or an authenticated encrypted bundle. Plaintext export is absent.

## Open decisions that block API freeze

These are Phase 0 deliverables, not license for implementation to improvise:

- exact classification and symbolic resolver for every current `Config` field;
- exact canonical encoding and Unicode/name normalization rules;
- destination key storage on desktop, browser, and headless clients, including
  revocation and recovery UX;
- distribution-owned local GitHub client registration/rotation and hosted
  OAuth App provisioning UX for different-origin copies;
- whether the existing secret backends need an explicit `exportable()`
  capability rather than treating every successful `get` as exportable;
- exact age crate, bundle profile, passphrase KDF limits, recipient types, and
  dependency/security review;
- generation retention, pinning, credential rollback, and disk-budget policy;
- optional manifest-signature/trust-root format; and
- multi-platform closure representation inside one lock.

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
- No hidden direct-write path around managed desired state. Live-safe and
  restart-required changes both create generations.
- No portable destination identity or active login/session state.
- No shared session, automatic single sign-on, cross-server query execution,
  or merged workspace across desktop servers.
- No bulk apply across servers. Each server produces and authorizes its own
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
   |     `-- Open server root or .siftbundle
   |
   `-- Connect to existing server
         `-- HTTPS/SSH origin -> verify instance id -> sign in
```

`Host here` registers a server root containing the editable manifest and lock,
then creates private generated state and a supervised process. `Connect` saves
only an internal bookmark and credential-store entry. It never copies or
assumes administrative control of the remote server's manifest.

### New device: simplest path

Fast path is copy the two-file directory and run `sift instance up .`; when
shared secrets must travel too, move one `.siftbundle` and run `up` on it.
With fixed/safely automatic bindings, the only interactive security step is
claiming the configured admin. Sift asks only for unresolved required bindings
or credentials.

```text
Choose server root or .siftbundle
        |
        v
Verify lock + resolve device bindings
        |
        v
Review destination + GitHub admin
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
sift instance inspect ./analytics-sift
sift instance up ./analytics-sift
# server starts claim-only mode; operator completes displayed HTTPS/loopback URL
sift instance status
# after claim
sift credentials fill
sift instance arm
```

`inspect` is read-only and works without state. `up` verifies or explicitly
creates the lock, resolves bindings, creates the first generation, and starts
claim-only mode under local filesystem authority. Missing bootstrap secrets
are prompted or read from an authenticated bundle/private stdin/IPC. Secret
values never enter argv or environment variables.

### Copy from an existing instance

1. Admin signs in and exports the two-file server root.
2. Export shows exclusions: users' credentials, sessions, documents, query
   history, results, audit history, client preferences, and instance identity.
3. If shared secrets are needed, admin supplies a new encryption recipient,
   completes destination-key step-up, and downloads one `.siftbundle` instead.
4. Files move through the operator's chosen channel.
5. New device follows first initialization and claims as the configured
   immutable GitHub subject.
6. Shared credentials import only when manifest and slot consumer digests
   match.
7. Per-user database credentials are re-entered by each user.

### Change an established instance

1. Admin exports or uploads a manifest into a server-side draft.
2. Server parses, validates, resolves lock data, and returns a redacted plan.
3. Admin reviews additions, mutations, drift, preserved omissions, credential
   invalidations, network changes, and prerequisites.
4. Admin completes destination-key step-up for the exact plan digest.
5. Server stages a one-use pending apply.
6. Sift creates and atomically selects a generation. Live-safe plans switch
   transactionally; restart-required plans use the exclusive maintenance lock.
7. UI reports success, warnings, disabled resources, or RecoveryRequired.

No apply button appears when the plan would remove the final usable admin,
enable a connection with missing required credentials, weaken a team secret
backend, or use an unresolved/untrusted artifact.

## Command and API surface

Names may change during API review; capability boundaries may not.

Offline/local commands:

```text
sift instance new <server-root>
sift instance inspect <server-root|bundle>
sift instance lock <server-root>
sift instance lock <server-root> --update [input]
sift instance vendor <server-root> --platform <target>
sift instance up <server-root|bundle>
sift instance verify
sift instance doctor
sift instance status
sift instance generations
sift instance diff <generation-a> <generation-b>
sift instance rollback <generation>
sift instance pin <generation>
sift instance gc
sift instance recover-admin --github-subject <numeric-id>
sift instance apply-pending
```

Desktop catalog commands use the same internal catalog and supervisor as the
UI; they do not create another public config format:

```text
sift desktop servers list
sift desktop servers add-local <server-root|bundle>
sift desktop servers add-remote <origin>
sift desktop servers start <catalog-id>
sift desktop servers stop <catalog-id>
sift desktop servers forget <catalog-id>
```

`forget` removes only the bookmark and saved client credential. Local server
root/private-state deletion is a different destructive command and is not part
of v1 unless a recoverable trash implementation is available.

Online admin API:

```text
GET    /v1/admin/instance/config
POST   /v1/admin/instance/drafts
PUT    /v1/admin/instance/drafts/{id}/manifest
POST   /v1/admin/instance/drafts/{id}/validate
POST   /v1/admin/instance/drafts/{id}/plan
POST   /v1/admin/instance/drafts/{id}/approve
GET    /v1/admin/instance/applies/{id}
GET    /v1/admin/instance/generations
GET    /v1/admin/instance/generations/{id}/diff
POST   /v1/admin/instance/generations/{id}/rollback
POST   /v1/admin/instance/generations/{id}/pin
POST   /v1/admin/instance/gc
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
trust-state marker, generation store/current pointer, journal, content store,
and atomic rename/fsync sequence. All `sift-admin` offline mutations are moved
behind a shared offline
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

Draft manifest bodies and pending generations belong in private bounded state
files addressed by content digest, not unbounded SQLite text columns.
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
2. Managed-loopback first claim may use a distribution-owned public client with
   GitHub device flow only during AwaitingClaim. Sift itself obtains the device
   code, displays GitHub's verification origin/code, obeys polling intervals,
   and accepts no caller-supplied device code. Exact subject matching, local OS
   authority, bounded expiry/attempts, and the claim-only router constrain the
   [phishing/impersonation risk GitHub documents for device flow](https://docs.github.com/en/apps/oauth-apps/building-oauth-apps/best-practices-for-creating-an-oauth-app#dont-enable-device-flow-without-reason).
3. Hosted/network claim and normal team sign-in use authorization code flow
   with exact registered HTTPS callback, random state, S256 PKCE, short expiry,
   one attempt, and bounded server-side state.
4. Sift exchanges the result and calls GitHub's authenticated user endpoint.
5. The returned numeric user id, parsed without truncation, must equal the
   configured decimal subject. Login and email are display data only.
6. Token scopes are minimized. The temporary GitHub token is zeroized where
   practical and discarded after profile resolution; it is not a Sift session
   or stored credential.
7. Principal/admin creation, claim consumption, destination-key registration,
   and trust-state transition are atomic or resumable without broadening the
   claim surface.
8. Failed claims are rate-limited by source and claim state and produce generic
   responses. They never fall back to local admin creation.

Hosted GitHub client secret is supplied as a credential slot, never inline in
the public manifest. Side-by-side hosted copies with different origins require
destination OAuth client bindings; replacing a failed device behind the same
origin may reuse the binding. Local device claim is not available to a network
listener and is not a general login or step-up mechanism.

GitHub OAuth is not the step-up mechanism. The bootstrap admin registers a
destination Ed25519 key stored in an OS-backed keystore where available. Sift
stores the public key. For a personal managed-loopback instance that key is
also the ongoing local primary login after bootstrap; team users continue
through configured server authentication. Recovery codes or alternate key
custody are a separate explicit design gate; until then, lost keys require
stopped-server recovery.

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
                                      -> GenerationSelected -> Complete
```

Startup recovery behavior is deterministic:

- Before AwaitingClaim: delete staged handles and discard the draft.
- At AwaitingClaim: retain the authenticated pending plan and bootstrap
  secrets so claim may resume until expiry or explicit offline cancellation.
- After MetadataCommitted but before GenerationSelected: finish activation from
  authenticated staged content, or enter RecoveryRequired if verification
  fails; never start with mixed revisions.
- After GenerationSelected: treat the selected complete generation as
  authoritative, finish cleanup, and mark Complete.
- Unknown/corrupt journal version: RecoveryRequired with offline inspection;
  never guess rollback direction.

Generation/current-pointer and trust-state writes use same-filesystem temporary
files, file fsync, atomic rename, and parent-directory fsync where supported. Recovery
commands first produce a redacted diagnostic and require explicit confirmation
for any trust-state or bootstrap-subject change. Recovery never exposes a
network setup endpoint.

## Observability and audit

Add explicit `Operation` variants for:

```text
InspectInstanceConfig
LockInstanceConfig
VendorInstanceClosure
VerifyInstanceGeneration
DiagnoseInstancePrerequisites
InitializeInstance
ClaimInstanceAdmin
CreateConfigDraft
ValidateConfigDraft
PlanConfigApply
ApproveConfigApply
ApplyInstanceConfig
SwitchInstanceGeneration
RollbackInstanceGeneration
PinInstanceGeneration
CollectInstanceGenerations
ArmInstance
ImportCredentialSlots
ReplaceCredentialSlot
ExportEncryptedCredentials
PruneManagedResources
RecoverInstanceAdmin
ManageDesktopServerCatalog
StartManagedInstance
StopManagedInstance
SwitchActiveInstance
```

Each records result, actor when one exists, instance id, manifest id,
configuration/realization digests, generation, affected logical resource
ids/counts, and correlation id.
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

- The current singular local target becomes one internal catalog entry pointing
  at its existing config root/state directory; files are not moved until a
  separately recoverable migration is implemented.
- Every current saved remote server becomes an internal catalog entry.
- A saved remote token is rekeyed to `(catalog_id, observed_instance_id,
  principal)` only after TLS and instance-id verification. Otherwise the entry
  is created signed-out.
- Presentation ids such as `local` and `hosted:<profile>` migrate to the new
  profile/instance namespace. Stale workspace and connection selections are
  cleared rather than guessed.
- Command-line startup remote settings create an ephemeral bookmark unless the
  user explicitly chooses Save; environment input never overwrites server
  desired state.

## Test strategy and acceptance criteria

### Pure schema tests

- Golden parse/normalize/export/digest fixtures for every schema version.
- Comments/formatting preserve configuration digest; semantic changes alter it.
- Unknown, duplicate, oversized, non-normalized, ambiguous, and wrong-type
  inputs fail with paths but without reflecting secret-shaped values.
- Provider schemas reject unknown config and credential fields.
- Lock resolution and compatibility errors are deterministic offline.
- Lock refresh preserves selected inputs; only explicit update changes them.
- Valid signatures cover config plus canonical lock; edits, substitution, and
  unknown/untrusted signing keys fail required-signature policy.
- Every symbolic binding has a bounded deterministic resolver and produces no
  hidden environment dependency.
- Property/fuzz tests never panic or allocate beyond configured bounds.

### Security tests

- Login/email/org match with wrong GitHub numeric id cannot claim.
- Numeric id with renamed login can claim and emits a rename notice.
- OAuth state, PKCE, expiry, callback, replay, rate-limit, and concurrent-claim
  failures remain claim-only.
- Local device claim rejects caller-supplied device codes, network/team use,
  post-claim use, wrong polling interval, and wrong distribution client; its
  temporary token cannot become a Sift session.
- AwaitingClaim exposes exactly health and claim routes; all other routes and
  background workers remain unavailable.
- Normal session, API token, refresh token, expired grant, wrong operation,
  wrong digest, wrong instance, or replayed step-up cannot mutate config.
- Secret fields never appear in logs, errors, audit rows, plans, generations,
  SQLite, HTTP responses, crash diagnostics, or test snapshots.
- Malicious host substitution cannot receive retained credentials without the
  separate confirmation.
- Symlink swaps, special files, loose bundle/config permissions, oversized
  streams/files, and TOCTOU attempts fail closed.
- A saved credential is never sent after a bookmark URL change or destination
  instance-id mismatch.
- Tokens, device keys, tenant/connection ids, caches, and sessions from server
  A are unusable against server B, including when both use the same URL host
  or have colliding SQLite row ids.
- A remote draft cannot mutate its target without that remote instance's admin
  session and digest-bound step-up.
- Read-only source mode rejects online apply; managed source mode switches only
  when the public pair and generation authenticate the same content.

### Reconciliation and crash tests

- Same manifest apply is a no-op with stable ownership.
- Omission preserves by default; prune affects only matching ownership.
- Stale revision and concurrent apply reject before secret mutation.
- Equivalent portable configs resolve to the same configuration digest;
  different device bindings retain that digest but produce distinct realization
  digests and generations.
- Formatting-only edits create no generation. Semantic edits create exactly one
  generation; same-generation apply is a no-op.
- Rollback creates a new append-only generation, never rewrites history, and
  never silently selects an unavailable old credential.
- GC preserves current, pinned, pending, and rollback-protected generations and
  every reachable artifact while collecting only unreachable content.
- With a vendored closure and secrets supplied, `up`, switch, and rollback work
  with network access disabled.
- Crash injection between staged pair writes, metadata commit, and generation
  switch either completes the authenticated target or retains/restores the old
  pair/generation; mixed manifest/lock bytes never run.
- Crash injection at every journal phase reaches old state, new state, or
  RecoveryRequired—never a mixed running state.
- Secret-store failure and SQLite failure clean or journal orphan handles.
- Final-admin removal, team memory backend, missing auth secret, corrupt lock,
  and unsupported extension block apply.
- Connection destination changes disable use and all background execution.
- Starting, stopping, crashing, applying, recovering, or deleting a draft for
  one managed local server leaves every other server's process, lock, journal,
  state files, and active sessions unchanged.
- Duplicate catalog ids, config roots, state-directory overlap, bind
  collisions, and symlinked roots fail before process start.
- Forgetting an entry preserves managed server files and reports how to
  reattach them.

### Desktop multi-instance tests

- Mixed managed-local and remote entries restore from private catalog state;
  managed locals still use separate two-file server roots.
- Multiple managed local child processes run concurrently with independent
  health, backoff, logs, and shutdown behavior.
- Multiple windows may target different servers; each window holds at most one
  live server/session target at a time.
- Switching targets closes old connections/subscriptions/cursors before new
  execution becomes possible and clears server-scoped UI selections.
- Catalog rename/reorder/start-policy changes never alter server config,
  manifest id, instance id, or credential contents.
- Remote bookmark creation does not create a local active server manifest.
- Explicit admin checkout is inert; apply goes to the pinned destination only.
- Per-server and global supervisor resource/backoff limits prevent restart
  storms without silently stopping a healthy server.

### Deployment matrix

Cover personal/team x loopback/network/ssh-proxy x in-process/daemon/container,
including invalid combinations. Exercise new initialization, existing adoption,
same-origin replacement, different-origin copy, GitHub outage, no network,
read-only manifest mounts, key loss, and offline recovery. Repeat representative
cases with several concurrent local servers plus HTTPS and SSH remote entries.

### Definition of done

A release is complete only when:

- copying manifest plus lock to a clean supported device yields the same
  normalized effective desired state and locked artifacts;
- fixed bindings reproduce the same realization on the same supported
  platform; symbolic bindings produce an explicit recorded realization with no
  hidden input;
- vendored closure plus supplied secrets can realize offline;
- generation history supports audited diff, pin, rollback, and safe GC;
- no source instance identity, session, user preference, user credential, or
  content crosses unless explicitly in an allowed encrypted `.siftbundle`;
- a person can complete the desktop happy path as `Choose config -> GitHub ->
  credentials -> Start` without reading documentation;
- one desktop can concurrently host multiple isolated local servers, connect to
  multiple remote servers, and switch/window them without cross-instance state;
- every actual server has exactly one editable manifest plus generated lock;
  desktop bookkeeping adds no public config file;
- the CLI completes the same flow without a browser-hosted setup API;
- all mutations require the documented authority and emit sanitized Operations;
- interruption at every durable phase has tested recovery; and
- format, clippy, workspace tests, cargo-deny, schema fixtures, and the full
  security/deployment matrix pass.

## Ordered implementation plan

### Phase 0 — decisions and fixtures

1. Graduate the two-file public contract, symbolic binding/realization model,
   immutable generations, artifact closure, multi-instance process isolation,
   trust state, GitHub subject, secret boundary, reconciliation, and apply
   classes into an ADR.
2. Classify every current `Config` field as portable exact, portable symbolic,
   generated private state, or unsupported; define every permitted resolver.
3. Freeze v1 manifest, lock, generation, credential-stream, bundle, redacted
   plan, and apply-report fixtures.
4. Decide generation retention/GC, platform closure format, lock-signature
   encoding/trust roots, and distribution GitHub client lifecycle.
5. Threat-model initialization, binding resolution, rollback, OAuth claim,
   connection redirection, artifact fetch, offline recovery, and bundle export.

Exit: ADR accepted; examples parse against a written schema; no public DTO or
metadata migration has landed first.

### Phase 1 — two-file model, lock, and inspect tooling

1. Add `instance-config` with strict serde models, normalization, validation,
   symbolic binding types, canonical config digest, redaction, compatibility,
   and lock verification.
2. Add provider config/credential schema adapters without coupling the pure
   crate to drivers or I/O.
3. Implement exact platform closure resolution and content-addressed artifact
   identity without fetching during pure evaluation.
4. Implement `instance new`, `inspect`, `lock`, `lock --update`, and generated
   credential-template output.
5. Add golden, property, fuzz, canonicalization, and size-limit tests.

Exit: a two-file server root can be safely inspected, locked, and
deterministically digested without server state; lock changes are explicit.

### Phase 2 — generations, artifact store, and offline safety

1. Add durable destination identity, trust-state marker, private state layout,
   initialization/apply journal, immutable generation store, and atomic current
   pointer.
2. Centralize exclusive maintenance-lock acquisition for every offline command,
   including existing `sift-admin` mutations.
3. Add Virgin detection that cannot be recreated by deleting SQLite rows.
4. Add content-addressed artifact fetch/verify/store, reachability, pinning, and
   safe GC primitives.
5. Implement binding resolution, realization digests, crash-safe switch,
   generation diff/pin/rollback skeletons, and RecoveryRequired diagnostics.

Exit: offline mutation and running server are mutually exclusive; generation
switches survive crashes; reachable artifacts cannot be collected.

### Phase 3 — multi-instance desktop vertical slice

1. Replace the desktop's singular local target with an internal catalog of
   managed config roots and remote bookmarks; add no public profile format.
2. Supervise several isolated local child processes with independent state,
   health, backoff, logs, ports, and lifecycle.
3. Pin remote credentials to catalog id, destination instance id, principal,
   and verified TLS/SSH origin.
4. Namespace workspace selections/caches by instance id and tear down live
   state before target switching.
5. Ship `Add server -> Host here | Connect`, basic two-file import, and
   multi-window isolation tests against a minimal generated server.

Exit: product topology is proven early: one desktop hosts/connects to several
independent servers through the same API and two-file config roots.

### Phase 4 — metadata ownership and planning

1. Add manifest revision/ownership and credential-slot metadata migrations.
2. Project existing metadata into logical desired/current resource models.
3. Implement deterministic diff, live-safe/restart-required/destructive
   classification, drift, final-admin invariant, activation gates, and redacted
   apply report.
4. Add `instance adopt` for legacy installations without taking ownership.

Exit: Sift can plan create/update/preserve/prune accurately with zero live
mutation and no secret reads.

### Phase 5 — first `up` and GitHub claim

1. Implement `instance up` through Prepared/SecretsStaged/AwaitingClaim, then
   claim completion through MetadataCommitted/GenerationSelected.
2. Add separate AwaitingClaim runtime/router with all workers disabled.
3. Implement constrained managed-loopback device claim plus hosted code flow
   with state, S256 PKCE, exact callback, numeric subject match, token disposal,
   bounded attempts, and rate limits.
4. Add atomic first principal/admin creation and destination-key registration.
5. Add readiness review and explicit network arming.

Exit: a clean destination can be safely reproduced and claimed; there is no
unauthenticated config upload/edit endpoint.

### Phase 6 — established apply, generations, and step-up

1. Add admin config read/export, bounded drafts, ETags, validate, and plan APIs.
2. Add destination-key challenge/registration/revocation and one-use,
   digest-scoped step-up grants.
3. Realize live-safe generations transactionally and restart-required
   generations under the maintenance lock; remove direct writes to managed
   resources.
4. Complete generation list/diff/pin/rollback/GC with credential-version and
   final-admin safety.
5. Add status/apply-report/generation APIs and reference client SDK methods.

Exit: authenticated admins can change the file-based desired state without
stale apply, hidden drift, session-only authorization, or unsafe rollback.

### Phase 7 — typed credentials

1. Add readiness/status UI model and provider-typed manual entry.
2. Add strict plaintext streaming from stdin/private IPC and online encrypted
   request bodies, with no persistent plaintext file or plaintext export.
3. Implement staged handle swap, rollback journal, orphan sweeper, idempotence,
   and non-exportable backend behavior.
4. Enforce destination-change invalidation and explicit retained-credential
   confirmation using per-slot consumer digests before tests/use.

Exit: shared credentials can be supplied portably or manually while secret
bytes remain outside SQLite, logs, plans, generations, and responses.

### Phase 8 — complete locked closure and offline vendoring

1. Generate/verify exact Sift, protocol, provider-schema, extension artifact,
   publisher-key, and platform entries.
2. Reuse signed extension resolution/staging; separate online lock generation
   from offline apply.
3. Implement `vendor`, offline closure import, signatures/trust metadata, and
   missing/wrong-platform diagnostics.
4. Implement `verify` for declared generation inputs and `doctor` for mutable
   external prerequisites without conflating their results.
5. Mark development overrides non-reproducible and block locked apply unless
   explicitly allowed outside production policy.

Exit: every executable/config schema dependency affecting reproduced behavior
is pinned or explicitly diagnosed.

### Phase 9 — optional encrypted transport bundle

1. Complete crypto/dependency ADR and independent security review.
2. Implement `.siftbundle` creation/import with manifest, lock, optional
   vendored public closure, and age recipient/passphrase secret payload.
3. Bind authenticated secret payload to manifest id, configuration digest,
   slot ids/types/consumer digests, format version, and limits.
4. Add corrupt/wrong-recipient/downgrade/replay/redaction tests.

Exit: shared secret portability never requires Sift to export plaintext.

### Phase 10 — desktop UX and hardening

1. Finish new-device `Open -> Review -> GitHub -> credentials -> Start` using a
   server root or one `.siftbundle`; hide generated state by default.
2. Finish instance switcher plus generation history/diff/rollback, export,
   step-up, readiness, and apply status screens with sensitive changes
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
- Multiple bootstrap identity providers.
- Automatic GitOps reconciliation and carefully scoped prune policy.
- Portable per-user credentials with each user's own recipient keys.
- Hardware-backed/WebAuthn step-up and managed recovery key escrow.

These consume the same data contract. None should add an evaluator, remote
fetch, secret interpolation, or weaker authentication to v1 initialization.
