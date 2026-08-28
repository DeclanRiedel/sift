# Reproducible server instances

A Sift server root contains two portable files:

- `sift.toml` is the editable desired state: server settings, immutable GitHub
  identities and allowlist, tenants, memberships, credential-free database
  connection strings, policies, and logical credential-slot names.
- `sift.lock` is generated: it binds the normalized configuration to exact
  Sift, protocol, provider-schema, extension, and artifact identities.

Passwords and OAuth client secrets are deliberately not a third portable
configuration file. They are typed values imported into the destination's
encrypted secret store. SQLite contains opaque handles only. Copy the two
files, apply them, then import the missing slots on the destination.

```text
source device                         destination device
sift.toml + sift.lock  ── copy ──>   validate + plan + apply
                                         │
                                         ├─ immutable generation
                                         ├─ managed metadata
                                         └─ missing credential slots
                                                  │
                                      local typed secret import
                                                  │
                                               start
```

## Quick start

Create a root or use `examples/reproducible-instance`:

```sh
sift instance new my-server --github-subject 12345678
sift instance validate my-server
sift instance lock my-server
sift instance plan my-server
sift instance apply my-server
sift instance generations my-server
sift instance status my-server
sift instance credentials status my-server
```

Import a PostgreSQL or SQL Server password without placing it in shell
arguments, logs, TOML, the lock, or SQLite:

```sh
sift instance credentials import my-server \
  --slot credential:default/postgres/shared --stdin
```

The command reads an exact JSON object from standard input:

```json
{"password":"replace interactively"}
```

Hosted GitHub OAuth slots accept exactly `{"client_secret":"..."}`. Stop the
instance before apply or credential import. A destructive plan needs
`--allow-destroy`; a resource with `prevent_destroy = true` still blocks it.

Start only an applied, current generation:

```sh
sift-server --instance-root my-server
```

For `bind = "auto-loopback"`, run `sift instance status my-server` from the
app/supervisor to discover the published daemon endpoint. Each manifest id has
an isolated state directory, so multiple local roots can run concurrently.

Use `--state-dir PATH` consistently with apply, status, import, and startup to
override the platform-native private state location.

## Workspace Git policy

Git requires at least one operator-owned workspace root. Its process and
parser ceilings are portable instance policy rather than client preferences:

```toml
[server.workspaces]
enabled = true

[[server.workspaces.roots]]
handle = "primary"
path = "/srv/sift/workspaces"
read_only = false

[server.vcs]
enabled = true
network_enabled = false
# Optional absolute fixed executable; omit to resolve `git` once at startup.
# executable = "/usr/bin/git"
local_timeout_secs = 30
network_timeout_secs = 120
max_output_bytes = 8388608
max_file_bytes = 8388608
max_status_entries = 20000
max_history_page = 200
max_commit_files = 5000
max_diff_files = 2000
max_diff_hunks = 4000
max_diff_lines = 200000
```

Keep `network_enabled = false` for an offline instance. An instance admin can
inspect the realized executable, version, helper state, health, and these
effective limits through `/v1/admin/instance/vcs-diagnostics`.

## Security and portability boundary

- Both files are UTF-8 TOML, strict, bounded, and cross-platform. Unknown
  fields, symlinks, inline password-like fields, stale locks, unsupported
  topology, and unsafe limits fail closed.
- Connection strings must contain public endpoint and username information
  only. Shared passwords are credential-slot values; per-user credentials stay
  per user.
- Apply owns only rows recorded for the manifest. A fresh manifest refuses to
  adopt an existing database, and deletion is explicit and foreign-key safe.
- Changing a credential consumer (such as its host or provider) invalidates
  the old secret. It must be imported again.
- Generations are immutable destination-private records. The current pointer
  changes only after metadata reconciliation commits. An edited but unapplied
  root cannot start.
- Personal local-device mode is guarded initially by the local OS account,
  private filesystem permissions, and a verified loopback peer. Network/team
  modes cannot use this bypass. Hosted OAuth secrets must be ready before
  startup, and authenticated instance-admin operations remain audited.
- The model reproduces Sift's declared server behavior, not the operating
  system, database contents, DNS/TLS infrastructure, or secret values. It is
  flake-like within that boundary, not a replacement for Nix.

The normative design, threat model, and later extension/package work are in
`docs/PLANS/reproducible-instances.md`.
