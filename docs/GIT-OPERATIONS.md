# Git operations and recovery

This is the operator contract for ADR-034's optional Git projection. The
virtual workspace and its immutable checkpoints are protected Sift state.
Repository bindings and commit/checkpoint provenance live in the encrypted
metadata backup. A checkout, its `.git` directory, index, worktree files, and
derived status/diff caches are disposable projections and are not archived.

Repository credential handles and `vcs-credential` secret values are removed
from portable backups. This avoids restoring an authorization grant onto a
different host. After restore, the binding remains, reports that credentials
are absent, and requires an authorized user to bind a destination-local
credential again. Other portable file-secret namespaces remain encrypted in
the archive; keychain secrets remain external as described by ADR-039.

## Deployment matrix

| Deployment | Where Git executes | Checkout ownership | Credential source |
| --- | --- | --- | --- |
| Personal/local | local server process | configured local root | destination `SecretStore` |
| SSH remote | remote daemon | remote configured root | remote `SecretStore` |
| Network hosted | hosted server | hosted configured root | hosted `SecretStore` |
| Container | container server | mounted configured root | container `SecretStore` |

The client always uses the same typed HTTP contract; it never mounts, mirrors,
or executes Git against a remote checkout. A read-only root permits projection
inspection but rejects materialization and every writable repository context.
With Git disabled, workspace capability discovery reports no Git capability.
With network disabled (or no one-operation askpass helper), local Git remains
available while clone, fetch, push, hosting mutation, and credential testing
fail with a typed network-disabled/helper error.

## Safety and recovery evidence

- The executable is resolved and canonicalized once at startup. The admin-only
  `/v1/admin/instance/vcs-diagnostics` probe reports that path, the observed
  version, helper availability, health, and effective limits.
- Every process uses structured arguments, an empty inherited environment,
  bounded output and time, and disables hooks, system/global configuration,
  ambient credential helpers, interactive prompts, pagers, external diffs,
  fsmonitor, optional locks, signature programs, and extension protocols.
- Credentials travel only through a private, one-operation askpass socket.
  Remote URLs reject embedded user information; errors are classified without
  returning Git stderr. SQLite and audit rows contain opaque handles only.
- Normalized relative `WorkspacePath` values and capability-confined roots
  reject traversal, symlinks, hard-link aliases, special files, and platform
  path aliases. The adapter selects `NUL`/`/dev/null` and the platform askpass
  executable name at compile time. Git's byte-oriented porcelain and patch
  parsing preserves file content line endings and rejects malformed structure.
- Git ownership/safe-directory failures return an actionable redacted
  diagnostic. Malformed UTF-8, invalid object ids, invalid refs, incomplete
  records, excess records, and excess output fail closed.
- All public mutations carry an expected binding revision and re-load context
  before mutation, so stale stage, commit, branch, fetch, and push attempts
  conflict. Destructive worktree/ref transitions checkpoint the canonical
  workspace first. A crash can leave only Git's lock-protected native state;
  restart re-observes the repository, preserves the prior checkpoint, and
  requires explicit continue, abort, repair, or retry—never automatic replay.

The executable test evidence is concentrated in `git_adapter` unit tests,
`server/tests/workspaces.rs`, `workspace_adapter` confinement tests,
`state_backup` round trips, metadata optimistic-revision tests, and the
instance-runtime configuration tests. Performance measurements are recorded in
`docs/PERFORMANCE.md`.
