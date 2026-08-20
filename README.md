# sift

Sift is a database server and desktop client written in Rust. It supports
PostgreSQL and SQL Server.

The server manages connections, query sessions, schema data, query execution,
history, audit records, and shared rooms. Clients use its versioned HTTP and
WebSocket API.

The same server can run beside a local desktop client or as a daemon for
multiple users. Shared query text is synchronized between room members.
Results, schemas, sessions, and connections remain on the server.

Desktop preferences are stored in a local `settings.toml`. See
[`docs/SETTINGS.md`](docs/SETTINGS.md).

## Goals

1. Keep product behavior in the server and expose it through the public API.
2. Support local single-user and hosted multi-user use with the same server.
3. Support shared rooms, query editing, connections, and results.
4. Keep query execution and navigation responsive with cursors, caching,
   prefetching, and connection pools.
5. Keep the protocol versioned and usable by third-party clients.

## Documentation

- [`docs/PLANS/server-build-list-v2.md`](docs/PLANS/server-build-list-v2.md) —
  current backlog
- [`docs/DECISIONS.md`](docs/DECISIONS.md) — design decisions
- [`docs/PLANS/phase-m-gpui-desktop.md`](docs/PLANS/phase-m-gpui-desktop.md) —
  desktop plan
- [`docs/INSTANCE-CONFIG.md`](docs/INSTANCE-CONFIG.md) — instance configuration
- [`docs/REMOTE-AND-UPDATES.md`](docs/REMOTE-AND-UPDATES.md) — remote use and
  updates
- [`docs/EXTENSIONS.md`](docs/EXTENSIONS.md) — extensions
