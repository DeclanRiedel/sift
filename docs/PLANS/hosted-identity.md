# Phase E — instance-owned identity and authentication

Status: accepted design (ADR-030); implementation pending.

## Goal

Make a Sift instance safe to expose on a network and useful as the identity
anchor for collaboration without weakening personal local or SSH workflows.
Registration is closed: an instance administrator decides which username and
GitHub identities may sign in. Once authenticated, both methods resolve to the
same `Principal` model and have identical product permissions.

Phase E supports two interactive authentication methods:

1. an instance-owned username and password; and
2. GitHub OAuth through credentials configured on that Sift instance.

OIDC is deliberately deferred. The identity schema retains an issuer field so
adding one configured OIDC issuer later does not require changing principal
identity or account-linking semantics.

## Locked decisions

### Policy and transport are independent

`deployment = personal | team` selects the trust policy. Transport is a
separate concern (`loopback`, `network`, or the future `ssh-proxy`):

- `personal + loopback` may use the bootstrapped local principal and loopback
  bypass;
- `personal + network` requires an explicit API token or keypair and never
  inherits loopback trust;
- `team + network` requires an authenticated principal on every protected
  route; and
- `ssh-proxy` will use an ephemeral, instance-bound capability delivered over
  the authenticated SSH channel, not OAuth and not a password in process
  arguments or environment variables.

Team mode fails closed at startup if metadata, a durable secret backend, the
external public URL, or required auth configuration is absent. The configured
public URL is authoritative for OAuth callbacks; request `Host` and forwarded
headers never construct a callback URL unless an explicit trusted-proxy policy
is added later.

### A principal is not a credential

`principal` remains the stable product identity. A separate authentication
identity belongs to exactly one principal and is keyed by method, issuer, and
provider subject. One principal may therefore have a password identity, a
GitHub identity, API tokens, and registered public keys without becoming
multiple Sift users.

- Password subjects are normalized, case-insensitive usernames.
- GitHub subjects become the immutable numeric GitHub user id after the first
  successful OAuth callback. The mutable GitHub login is profile and
  allowlist data, not the durable identity key.
- Identities are never linked by matching email. Linking a GitHub identity to
  an existing password principal is an explicit admin action.
- Disabling a principal disables every authentication method and revokes its
  active auth sessions. Individual credentials may also be disabled or
  revoked.

### Closed registration and tenant creation

There is no public registration endpoint.

- An admin creates a password principal or adds a password identity to an
  existing principal.
- An admin allowlists a normalized GitHub login. The allowlist entry either
  names an existing principal to link or declares that the first successful
  login should create a new principal.
- A newly created principal receives a personal tenant owned by that
  principal. Explicit, opaque, single-use tenant invitations add the principal
  to team tenants.
- A GitHub login that is neither already linked nor currently allowlisted is
  rejected with the same generic authentication response used for other
  denied logins.

The first team-mode administrator is created through an offline CLI command
that reads a password from a TTY or stdin. The password never appears in
arguments, environment variables, logs, or the operation audit.

### Credentials stay out of SQLite

SQLite stores identity metadata and opaque secret handles only. Passwords are
normalized as specified by the password contract, salted and hashed with
Argon2id on a bounded blocking worker, and the resulting verifier is stored in
the configured `SecretStore`. Plaintext is never persisted. Work factors are
encoded with the verifier and upgraded after a successful login when policy
changes.

Password creation and change accept long password-manager-generated values,
apply a minimum length and compromised/common-password blocklist, and do not
impose character-class rules. Login is protected by per-source and
per-identity throttles, bounded Argon2 concurrency, generic errors, and a dummy
verification path for unknown usernames. Auth-specific throttling lands in
Phase E; general API rate limiting remains Phase F.

GitHub client id, client secret, and public callback base URL are instance
configuration. The client secret is supplied from the server's secret-bearing
configuration environment and never stored in metadata SQLite. The temporary
GitHub access token is used only to fetch the authenticated profile, then
discarded; it is not Sift's session token and is not persisted.

### OAuth is instance-owned

There is no Sift-operated identity broker in Phase E. Each network-hosted
instance owns its GitHub OAuth App registration and callback URL. The flow uses
authorization code, `state`, and S256 PKCE. The server validates the
authenticated GitHub user on every login and never trusts a requested handle.
Only identity/profile scopes are requested; repository access is out of scope.

### Sift sessions use opaque rotating credentials

Interactive login issues:

- a 15-minute opaque access token; and
- a 30-day opaque rotating refresh token.

Only lookup prefixes, keyed digests/hashes, lineage, timestamps, and revocation
state are durable. Refresh use is atomic: it consumes the presented token and
issues its replacement in one transaction. Reuse of a consumed token revokes
the entire refresh family and records a sanitized security audit event.

An in-memory access-token cache avoids a SQLite lookup on every request. It is
shorter-lived than the access token and is synchronously invalidated for local
revocation and principal changes. Persistence sits behind an `AuthSessionStore`
boundary so a future multi-process deployment can replace the SQLite-backed
implementation. Phase E explicitly supports one active server process per
metadata store.

Existing API tokens remain separate long-lived automation credentials.
Registered Ed25519 keys use challenge authentication and issue the same Sift
session shape; the existing `principal_key` and `keypair_challenge` tables are
adopted and migrated as needed rather than duplicated.

### Native, web, and WebSocket clients share one identity context

- Native clients and automation send bearer credentials.
- Same-origin web clients use `Secure`, `HttpOnly` cookies with an explicit
  SameSite policy. Cookie-authenticated state-changing requests receive CSRF
  protection.
- Refresh cookies are restricted to the refresh/revoke surface.
- A room WebSocket authenticates during upgrade and receives an auth lease.
  It can reauthenticate in-band before access expiry. Principal disablement,
  auth-session revocation, and room-membership removal invalidate affected
  leases without waiting for socket reconnect.

No access or refresh token is placed in a URL. A future cross-origin web client
must use an explicit origin allowlist; permissive credentialed CORS is not
allowed.

### Authentication establishes an authorization floor

Phase E owns the checks needed to prevent cross-principal resource access:

- all protected routes receive one middleware-resolved `AuthContext`;
- the public route allowlist is limited to health/readiness, OpenAPI, login,
  GitHub start/callback, refresh, and the minimum callback completion surface;
- sessions are principal-owned or room-owned;
- connections, transactions, savepoints, cursors, exports, plans, edits,
  imports, process control, and search inherit their session ownership;
- personal resources require their owning principal;
- room resources require current room membership and the existing room role
  checks; and
- listing endpoints filter rather than reveal foreign resource ids.

Phase F still owns tenant roles beyond the current model, connection policy,
general rate limits, quotas, and resource accounting.

## Metadata shape

The implementation migration should introduce or adapt these concepts; exact
SQL names may follow crate conventions:

- `principal`: add synchronized avatar/profile fields and disabled state;
- `auth_identity`: principal, method, issuer, stable subject, mutable username
  or provider login, optional password-verifier secret handle, timestamps,
  and disabled state;
- `github_allowlist`: normalized pending login, optional target principal,
  creating admin, timestamps, and consumed/revoked state;
- `auth_session`: principal, refresh family, client metadata, creation,
  activity, expiry, and revocation reason;
- `auth_access_token`: lookup/digest and expiry where the chosen lookup layout
  is not embedded in `auth_session`;
- `auth_refresh_token`: lookup/digest, family, parent, consumed/replaced state,
  expiry, and replay evidence;
- `auth_login_attempt`: short-lived OAuth state/PKCE transaction metadata;
- `tenant_invitation`: opaque token digest, tenant, intended role, creator,
  expiry, and consumed/revoked state; and
- the existing `principal_key` and `keypair_challenge`: add principal binding,
  one-use/consumed state, and indexes required for atomic challenge use if the
  current schema is insufficient.

Raw passwords, password verifiers, OAuth client secrets, GitHub access tokens,
Sift bearer tokens, refresh tokens, invitation tokens, challenge signatures,
and private keys never appear in audit records. Only password-verifier handles
live in SQLite; verifiers live in `SecretStore`.

## Public HTTP and WebSocket surface

All request and response types live in `sift-protocol` or an explicitly shared
pure-data boundary; server-only credential verification stays outside it.

Interactive authentication:

- `POST /v1/auth/login` — username/password login;
- `GET /v1/auth/github/start` — begin the instance GitHub flow;
- `GET /v1/auth/github/callback` — validate callback and complete or hand off
  the one-time Sift login result;
- `POST /v1/auth/refresh` — rotate refresh credentials;
- `POST /v1/auth/logout` — revoke the current auth session;
- `POST /v1/auth/logout-all` — revoke all interactive sessions for the
  principal;
- `GET /v1/auth/whoami` — principal profile, memberships, and authentication
  context; and
- `PUT /v1/auth/password` — change the current principal's password.

Instance administration:

- create/disable principals and password identities;
- reset a password through a one-use activation/reset flow;
- add/revoke a GitHub allowlist entry, optionally linked to a principal;
- link/unlink authentication identities without deleting the principal;
- list/revoke a principal's auth sessions; and
- create/revoke tenant invitations.

The final route names are locked when typed request/response structs are
written, before handler implementation. Admin actions, login success/failure,
refresh, replay detection, logout, password changes, GitHub allowlist changes,
key registration, and invitation lifecycle receive sanitized `Operation`
coverage. Login failures may have no actor principal but retain correlation id,
method, coarse source information, status, and timestamp.

## Ordered implementation

### E0 — Contract and ownership audit

1. Add the deployment-policy and transport enums; validate impossible or
   unsafe startup combinations.
2. Define `AuthContext`, the public-route allowlist, and a single middleware
   path. Remove the current split between static-bearer middleware and
   handler-local resolution.
3. Inventory every HTTP and WebSocket route and classify it as public,
   principal-owned, tenant-scoped, room-scoped, or admin-only.
4. Make all session-store access ownership-aware. Add negative integration
   tests using two principals for every session-derived resource family.
5. Add protocol operation variants and audit sanitizer tests before any route
   accepts a password or token.

Exit: two authenticated principals cannot list, inspect, mutate, cancel, or
close one another's non-room runtime resources.

### E1 — Identity metadata and administrator bootstrap

1. Add the identity/session/allowlist/invitation migration and typed metadata
   APIs with transactional invariants.
2. Preserve `local:1` by migrating it to an explicit local identity without
   changing its principal or tenant id.
3. Add the offline admin bootstrap/management CLI with TTY/stdin password
   input and durable audit.
4. Add principal disablement and explicit credential linking/unlinking.
5. Test migrations from every existing metadata migration level used in CI.

Exit: a team instance can be bootstrapped without putting a password or token
in configuration, command history, logs, or SQLite.

### E2 — Opaque auth sessions and password login

1. Implement token generation, lookup, rotation, replay-family revocation,
   expiry, logout, logout-all, and the bounded verification cache.
2. Implement password creation/change/reset and Argon2id verification through
   `SecretStore` on bounded blocking workers.
3. Add auth-specific throttling, password blocklist validation, generic error
   behavior, and resource caps on expensive verification work.
4. Implement `/login`, `/refresh`, `/logout`, `/logout-all`, and `/whoami` for
   bearer and cookie clients, including CSRF behavior.
5. Extend the client SDK with a token provider that rotates credentials without
   exposing them in debug output.

Exit: an admin-created password principal can authenticate, refresh safely,
use all owned resources, and be revoked immediately.

### E3 — GitHub allowlist and OAuth

1. Add instance GitHub configuration and startup validation without logging
   the client secret.
2. Implement authorization-code + state + S256 PKCE using the configured
   external base URL and exact callback.
3. Fetch the authenticated GitHub profile, enforce existing identity or
   pending allowlist, persist the numeric GitHub id, synchronize display name
   and avatar, then discard the upstream token.
4. Support explicit admin linking to an existing principal and new-principal
   creation with a personal tenant.
5. Cover denied, expired, replayed, mismatched-state, renamed-handle, revoked
   allowlist, and provider-failure paths.

Exit: an allowlisted GitHub account and an admin-created password account are
functionally equivalent principals.

### E4 — Keypair, invitations, and remote-ready grants

1. Wire Ed25519 key registration/revocation and bounded one-use challenges to
   the existing schema.
2. Exchange a valid signature for the same opaque auth-session tokens used by
   interactive login.
3. Implement tenant invitation create/accept/revoke with atomic one-use
   consumption.
4. Define, but do not yet expose over SSH, the short-lived audience- and
   instance-bound capability shape Phase H's proxy bootstrap will use.

The locked wire claims are `SshProxyCapabilityClaims`: version, exact
server-configured `instance_audience`, principal id, unique capability id,
issued-at, and expiry. Phase H owns signing/MAC encoding, one-use persistence,
issuance over the authenticated SSH channel, and exchange. It must reject an
audience mismatch and may not broaden loopback trust.

Exit: automation and future SSH proxy paths do not need passwords or portable
refresh tokens.

### E5 — Collaboration session lifecycle and release surface

1. Add WebSocket auth leases, in-band reauthentication, revocation delivery,
   and reconnect/resume tests.
2. Ensure room membership removal disconnects or downgrades the affected
   principal immediately.
3. Add every new schema and route to OpenAPI and the reference SDK.
4. Add audit, secret-redaction, hostile-input, timeout, and concurrency tests.
5. Run `cargo fmt`, workspace clippy with warnings denied, workspace tests, and
   cargo-deny before declaring Phase E complete.

Exit: a directly shared network-hosted instance supports password and GitHub
users over HTTP and long-lived collaboration sockets without auth-expiry data
loss.

## Changes to later phases

- **Phase F:** remove basic session/resource ownership from its scope; retain
  tenant/connection policy, general rate limiting, quotas, and accounting.
  Auth-endpoint throttling is an explicit Phase E prerequisite.
- **Phase G:** build room CRDT and shared-connection behavior on Phase E's
  authenticated WebSocket leases. Membership revocation and reconnect identity
  are no longer deferred collaboration details.
- **Phase H:** preserve the selected direct-shared-server topology. SSH remote
  development initially authenticates through the SSH proxy capability and
  does not make a personal daemon publicly reachable. A collaboration relay or
  central identity broker remains a separately designed future topology.
- **Phase J:** auth success/failure/replay/throttle metrics join the metrics
  surface. Typed OpenAPI generation remains Phase J, but Phase E must update
  the current hand-authored document and drift tests.

## Deliberate non-goals

- public signup, email-based account discovery, or email-based account merge;
- OIDC, SAML, social providers other than GitHub, MFA, passkeys, or recovery
  email in the first Phase E implementation;
- a Sift-operated central identity or collaboration relay;
- horizontally active server processes sharing one SQLite metadata file;
- GitHub repository permissions or persistence of GitHub access tokens; and
- Phase F's fine-grained connection policy, quotas, and general rate limits.

## Security references

- NIST SP 800-63B password guidance: salted adaptive verification, password
  blocklists, password-manager compatibility, and login throttling.
  <https://pages.nist.gov/800-63-4/sp800-63b.html>
- OAuth 2.0 Security Best Current Practice: authorization-code flows, exact
  redirect validation, and refresh replay detection.
  <https://www.rfc-editor.org/rfc/rfc9700.html>
- GitHub OAuth web flow: state, S256 PKCE, callback, and authenticated-user
  validation requirements.
  <https://docs.github.com/en/apps/oauth-apps/building-oauth-apps/authorizing-oauth-apps>
