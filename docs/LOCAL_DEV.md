# Local Development State

This repo should commit recipes, templates, and generators. It should not
commit local state, credentials, logs, or machine-specific paths.

## Committed Templates

- `.env.example` is the environment-variable template. Copy it to `.env` if
  your shell workflow loads env files.
- `sift.example.toml` is the file-based config template. Copy it to
  `sift.toml` if you prefer TOML config.
- `examples/reproducible-instance/scripts/dev-secret-key.sh` creates the local
  key file used by the encrypted file secret backend.

## Ignored Local Files

These are intentionally ignored:

- `.env`, `.env.*`
- `sift.toml`
- `.sift/`
- `*.sqlite`, `*.sqlite3`, `*.db`
- `*.jsonl`
- `*.log`
- local database directories such as `pgdata/`, `postgres-data/`,
  `mssql-data/`

## Recreating Local State

Enter the dev shell:

```sh
nix develop
```

The shell hook generates `.sift/dev-secret.key` if it does not already exist
and exports `SIFT_METADATA__SECRET_KEY_FILE` to that path.

To generate the key manually:

```sh
examples/reproducible-instance/scripts/dev-secret-key.sh
# or, inside the Nix dev shell:
sift-dev-secret-key
```

To use the encrypted file secret backend locally:

```sh
cp .env.example .env
# edit .env:
# SIFT_METADATA__SECRET_BACKEND=file
# SIFT_METADATA__SECRET_KEY_FILE=.sift/dev-secret.key
```

Or with TOML:

```sh
cp sift.example.toml sift.toml
# edit sift.toml:
# [metadata]
# secret_backend = "file"
# secret_key_file = ".sift/dev-secret.key"
```

## Seeded Desktop Demo

To exercise desktop UI against real query results instead of an empty client:

```sh
nix develop
sift-desktop-demo
```

To launch the same demo with the keyboard wiki at
`http://127.0.0.1:8787`:

```sh
nix run .#sift-desktop-demo-wiki
```

Development builds expose that address through the appbar **Wiki** link.
Override the server with `SIFT_DESKTOP_DEMO_WIKI_BIND` and
`SIFT_DESKTOP_DEMO_WIKI_PORT`; the appbar link intentionally targets the
default loopback address.

That command starts a throwaway Postgres cluster (default `/tmp/sift-demo-pg`,
port 5433) seeded with a relational `lab` dataset, creates and applies a reproducible instance
root (default `/tmp/sift-desktop-demo-instance-$UID`), imports a generated
destination-local database credential, and launches `sift-desktop` with that
root. The desktop supervises the real server through its discovered
auto-loopback endpoint. The manifest-managed `demo/postgres` profile appears in
the Connections dock. The seed includes nine tables, three views, a materialized
view, a SQL function, indexes, foreign keys, JSON/array/network values, and over
15,000 rows. Use `SELECT * FROM lab.large ORDER BY id;` for the deterministic
10,000-row result fixture, or start with
`SELECT * FROM lab.order_summary ORDER BY placed_at DESC;`.

Both helper steps are also usable on their own:

```sh
examples/reproducible-instance/scripts/dev-seed-postgres.sh                                   # prints the port
examples/reproducible-instance/scripts/dev-register-demo-connection.sh http://127.0.0.1:7474  # prints profile id
```

The seed helper and desktop demo are idempotent. Rerunning reuses the existing
cluster and the stable desktop-demo identity. Older throwaway desktop-demo
inventory entries are replaced automatically instead of accumulating.

`sift-demo-postgres` also creates a writable demo workspace at
`/tmp/sift-demo-workspace-$UID`, binds it to the seeded room, and initializes
it as a Git repository through Sift. Override the location with
`SIFT_DEMO_WORKSPACE_ROOT`.

`sift-desktop-demo` does the same inside its reproducible instance root before
launching the desktop. Override that location with
`SIFT_DESKTOP_DEMO_WORKSPACE_ROOT`.

Useful overrides:

- `SIFT_DEMO_PG_PORT` — port for the demo cluster.
- `SIFT_DEMO_PGDATA` — cluster data directory.
- `SIFT_DEMO_KEEP_POSTGRES=1` — leave the cluster running after the desktop exits.
- `SIFT_BIND` — backend bind address.

The demo cluster uses trust auth on loopback with an empty password. It is a
disposable dev fixture; never point it at real data.

## Build Resource Limits

The committed `.cargo/config.toml` sets `jobs = -6` (all logical cores minus
six) so a workspace build leaves headroom for the editor, rust-analyzer, a
running server, and Postgres. `Cargo.toml` limits dev debuginfo to line tables
and builds dependencies at `opt-level = 2` with no debuginfo, which cuts both
link-time memory and `target/` size while keeping GPUI usable at runtime.

The Nix dev shell additionally links with `mold` (`RUSTFLAGS=-C
link-arg=-fuse-ld=mold`), the largest single reduction in peak build memory.
Because `RUSTFLAGS` is part of the build fingerprint, the first build after
entering the shell rebuilds the workspace once.

For a fast optimized local build without the release profile's LTO and
single-codegen-unit memory spike:

```sh
cargo run --profile release-dev -p sift-desktop
```

## Sensitive Values

Never commit real values for:

- `SIFT_AUTH__BEARER_TOKEN`
- `SIFT_PG_PASSWORD`
- `SIFT_MSSQL_PASSWORD`
- `.sift/dev-secret.key`
- `.sift/secrets.enc`
- local metadata databases
- operation/audit JSONL logs

Treat private hostnames, usernames, database names, and local filesystem paths
as sensitive unless they are clearly disposable dev defaults.

## Testing a Desktop Against a LAN Server

The desktop defaults to its bundled server at `127.0.0.1:7474`. To run the
server on one machine and only `sift-desktop` on another, configure the server
machine's ignored `.env` with a random bearer token:

```dotenv
SIFT_DEPLOYMENT=personal
SIFT_TRANSPORT=network
SIFT_MODE=daemon
SIFT_BIND=0.0.0.0:7474
SIFT_AUTH__LOOPBACK_BYPASS=false
SIFT_AUTH__BEARER_TOKEN=replace-with-a-strong-random-token
```

Apply migrations before the first daemon start, then start the installed
server through its launcher:

```sh
sift-server migrate apply
sift-launcher --mode daemon
```

Allow TCP port 7474 only from the intended private subnet. Start
`sift-desktop`, then choose **Sift → Connect to Server…**. Enter a display
name, the server URL, and the bearer token, then select **Test & Connect**.
The desktop completes the protocol handshake and authenticates before it
switches the active instance.

The top app bar always shows the active Sift server and its connection state.
Select it to switch quickly between Local Sift and saved servers, or open the
full server-management dialog. The center shows the active room/workspace;
database profiles remain in the Connections dock. The square account control
shows the identity returned by the current server.

To add a PostgreSQL or SQL Server database, choose **+ Add database
connection…** at the top of the Connections dock. Select the tenant and
provider, enter the host and credentials, then choose **Save & Connect**.
Connection passwords are sent to the server's configured secret backend and
are not persisted in desktop presentation state.

Enable **Remember token in the OS keychain** to save the credential in the
platform credential service. Disable it for a session-only token. Saved
instance names and URLs live in the private desktop `instances.json`; bearer
tokens never enter that file. The last selected saved server is restored on
the next launch. The same dialog can edit or forget saved servers and switch
back to the bundled **Local Sift** instance.

For scripted testing, the startup flags remain available. Put the token in a
private file and launch:

```sh
sift-desktop \
  --server-url http://192.168.1.20:7474 \
  --server-name "LAN Sift" \
  --bearer-token-file /private/path/sift-token
```

Equivalently, set `SIFT_DESKTOP__SERVER_URL`,
`SIFT_DESKTOP__SERVER_NAME`, and either `SIFT_DESKTOP__BEARER_TOKEN_FILE` or
`SIFT_DESKTOP__BEARER_TOKEN` in the desktop environment. When a remote URL is
configured, the desktop does not look for or start a local `sift-launcher`.

Plain HTTP exposes the bearer token to anyone able to observe LAN traffic. It
is suitable only for short-lived testing on a trusted network; use an HTTPS
reverse proxy or an encrypted private network for persistent deployments.
