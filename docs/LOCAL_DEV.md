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
