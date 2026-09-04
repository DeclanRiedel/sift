<img align="left" src="crates/desktop/assets/sift-icon.png" width="180" alt="Sift icon">

# Sift

A fast, Vim-like SQL workspace built in Rust for PostgreSQL, SQL Server, and
possibly more in the future.

Run it locally or host the same server for a team. Connections, schema,
queries, results, history, audit, and collaboration share one versioned API.

Built entirely to my taste but you may use it too.

## Goals

1. Keep product behaviour in the server and expose it through the public API.
2. Use one server for local and hosted workflows.
3. Make editing, navigation, execution, and collaboration feel immediate.
4. Stay responsive through cursors, caching, prefetching, and pooling.
5. Keep the protocol versioned and usable by third-party clients.

## Docs

- [Instance configuration](docs/INSTANCE-CONFIG.md)
- [Keyboard reference](docs/keyboard-wiki/index.html)
- [Extensions](docs/EXTENSIONS.md)

## License

Copyright © 2026 Declan Riedel. Licensed under
[AGPL-3.0-only](LICENSE). Network users of a modified version must receive its
corresponding source. Third-party assets retain their own licenses; Qlementine
icon attribution lives in [crates/ui/assets/icons](crates/ui/assets/icons/README.md).
