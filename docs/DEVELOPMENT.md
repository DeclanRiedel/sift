# Development on Windows and NixOS

Build and run Sift natively on each host. The desktop can connect to either
server over the same API. Windows does not need WSL or a local database for
the regular workspace tests.

## Windows 11

Install Git, Rustup, CMake, and Visual Studio Build Tools with **Desktop
development with C++** and a Windows SDK. Use the MSVC Rust toolchain.
The repository's `rust-toolchain.toml` selects Rust and its components.

From PowerShell in the checkout:

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/dev.ps1 setup
./scripts/dev.ps1 check
./scripts/dev.ps1 desktop
```

The first build is large. Normal app builds and test builds use different
dependency features, so each needs its own initial compilation.

`setup` checks prerequisites, installs the Rust components, creates `.env`
if absent, and generates a development key readable only by your Windows
account. Every command loads the root `.env`; edit that file for local
settings. Existing values and keys are retained. Values are literal, with
optional outer quotes; shell interpolation and inline comments are not supported.
If script execution is restricted, use the first command's PowerShell prefix
for the other actions too; no permanent execution-policy change is needed.

Other actions are `build`, `test`, `server`, and `env`. `env` loads settings
into the current PowerShell session so ordinary Cargo commands use them:

```powershell
./scripts/dev.ps1 env
cargo fmt
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

`server` runs the standalone server in the foreground; Ctrl+C shuts it down.
`desktop` starts the app with its bundled local server by default. For persistent
development credentials, set `SIFT_METADATA__SECRET_BACKEND=file` in `.env`.
Keep each machine's `.env`, key, and database local to that machine.

Default personal startup initializes metadata using the existing automatic
migration policy. If an older database needs an explicit migration, stop its
server and run `./scripts/dev.ps1 server migrate apply`. Applied instances keep
their own lifecycle; see [Instance configuration](INSTANCE-CONFIG.md).

Local Git operations are available on Windows. Sift's Git credential bridge
currently needs Unix sockets, so authenticated Git network operations remain
unavailable when the server runs on Windows.

## NixOS

In the checkout, `direnv allow` enters the flake shell and loads `.env`.
Alternatively use `nix develop`, then load `.env` with
`set -a; source .env; set +a` if it exists.

```sh
cargo fmt
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo run -p sift-server --bin sift-server -- migrate apply
cargo run -p sift-server --bin sift-server
```

Build the bundled helpers with `cargo build -p sift-server --bins`, then use
`cargo run -p sift-desktop` in a graphical session for the desktop.
The flake provides the Linux build and graphics dependencies.

## Test hosting between Windows and vostro

Use matching source revisions on both devices. Start the server on the host
you are testing, then connect the desktop from the other device. For private
development, SSH forwarding lets the server keep its default loopback bind.
This assumes the existing SSH connection to `declan@vostro` works.

**NixOS hosts, Windows connects:** start the server in the NixOS checkout as
above. Keep this tunnel running in a separate Windows terminal:

```powershell
ssh -N -o ExitOnForwardFailure=yes -L 17474:127.0.0.1:7474 declan@vostro
```

In another Windows terminal:

```powershell
Invoke-RestMethod http://127.0.0.1:17474/v1/ready
./scripts/dev.ps1 desktop --server-url http://127.0.0.1:17474 --server-name vostro
```

In Sift's Connect to Server dialog, select **URL** and enter
`http://127.0.0.1:17474`. The **SSH** field accepts `user@host` or an OpenSSH
host alias, not a URL. This forwarding setup carries HTTP inside SSH; it does
not provide an HTTPS endpoint.

The desktop follows the connected server's handshake capabilities. Without an
applied `sift.toml`, Add connection opens the database wizard. With an applied
manifest it opens connection configuration. Unavailable Git, workspace-file,
and automation entry points are hidden, and their palette commands explain
why they are disabled. After changing server configuration, restart the server
so reconnecting clients receive its updated capabilities.

Connection and dialog failures remain in Problems and Notifications after the
dialog closes. Use the copy icon on an error or notification to copy its text.

**Windows hosts, NixOS connects:** run `./scripts/dev.ps1 server` on Windows.
In another Windows terminal, keep this reverse tunnel running:

```powershell
ssh -N -o ExitOnForwardFailure=yes -R 17475:127.0.0.1:7474 declan@vostro
```

In the NixOS graphical session's development shell:

```sh
curl --fail http://127.0.0.1:17475/v1/ready
cargo run -p sift-desktop -- --server-url http://127.0.0.1:17475 --server-name Windows
```

These ports are explicit local tunnel endpoints. The server remains on
`127.0.0.1:7474`; no Windows inbound firewall rule is needed. Loopback bypass
is suitable for this personal test with trusted accounts on both machines.
Stop the tunnel with Ctrl+C when finished. For shared hosting, use an applied
instance and the authentication policy in [Instance configuration](INSTANCE-CONFIG.md).

Do not use `sift-remote` to upload a Windows executable to NixOS: its current
bootstrap requires matching operating systems and architectures. Build on
the server host and use the forwarding workflow above across operating systems.

## Real databases

Live database tests are opt-in features. Configure their `SIFT_PG_*` or
`SIFT_MSSQL_*` values in `.env`, then load it and run the appropriate suite:

```powershell
./scripts/dev.ps1 env
cargo test -p sift-driver-postgres --features live-pg --test live_pg
cargo test -p sift-driver-sqlserver --features live-mssql --test live_mssql
```

Use a disposable test database: these suites create and change test objects.
Windows uses a TCP PostgreSQL host, not a Linux socket path. A new Windows
`.env` defaults to `127.0.0.1`; choose the actual port, database, and user.
The existing Nix helpers can provision databases on vostro, then SSH can
forward their ports to Windows. Database endpoints are resolved by the Sift
**server**, so use addresses reachable from whichever machine hosts it.

CI stays manual-only; run `check` on Windows and the equivalent Cargo commands
in the Nix shell before considering a change portable.
