# Local Development State

This repo should commit recipes, templates, and generators. It should not
commit local state, credentials, logs, or machine-specific paths.

## Committed Templates

- `.env.example` is the environment-variable template. Copy it to `.env` if
  your shell workflow loads env files.
- `sift.example.toml` is the file-based config template. Copy it to
  `sift.toml` if you prefer TOML config.
- `scripts/dev-secret-key.sh` creates the local key file used by the encrypted
  file secret backend.

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
scripts/dev-secret-key.sh
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
