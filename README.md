# sift

A server-first, collaboration-native database IDE.

The **server is the product**. `sift-server` owns all product behavior:
connections, sessions, schema, execution, history, audit, and collaboration.
Clients (desktop / web / automation) are thin, stateless renderers over a
single versioned HTTP + WebSocket protocol.

**Local-first by default, hosted-capable:** one binary runs in-process next to
a desktop client _or_ as a daemon for hosted multi-user — same code, same
model. Single-user local mode is a one-member room; multiplayer is the same
model with more members.

**Collaboration is built in, not bolted on:** the durable unit is a room.
CRDT is used _only_ for shared SQL editor text; results, schema, sessions, and
connections stay server-authoritative.

**The protocol is pure serde, public, and semver-stable from v0.1:** a third
party should be able to build a working UI against the OpenAPI spec alone.

## Status

The server substrate is in place: two real drivers (Postgres, SQL Server),
HTTP + WebSocket surfaces, sessions, rooms, metadata, and audit. Phase M now
includes a native GPUI desktop shell with instance switching, workspace
navigation, query editing/execution, and result rendering; product-client work
is still active. For the code-grounded backlog and verified gaps, see
`docs/PLANS/server-build-list-v2.md`; for load-bearing decisions, see
`docs/DECISIONS.md`. Remote SSH operation, lifecycle modes, and signed release
staging are covered in `docs/REMOTE-AND-UPDATES.md`.

Reproducible server roots use one editable `sift.toml` plus a generated
`sift.lock`, with destination-private generations and typed secret slots. See
`docs/INSTANCE-CONFIG.md` for the short operator workflow and security
boundary.

Installed local and daemon releases start through `sift-launcher`, which
health-checks staged updates, commits healthy candidates, and rolls back failed
candidates before handing the server lifecycle to the caller.

Phase I extensibility is implemented and graduated. The normative contract is
`docs/PLANS/phase-i-extensibility.md`; the public compatibility, security, and
operator guide is `docs/EXTENSIONS.md`. ODBC/JDBC compatibility bridges remain
deliberately deferred.

Phase K SQL intelligence and database modeling is implemented and graduated.
Its catalog, diff, migration, comparison, diagram, semantic-plan, safety, and
performance evidence is recorded in
`docs/PLANS/phase-k-graduation-matrix.md`.

Phase L virtual workspaces, projections, hardened Git, run configurations,
durable scheduling, and extensible transfer recipes are implemented and
graduated. Its deployment, conflict, recovery, security, compatibility, and
measured budget evidence is recorded in
`docs/PLANS/phase-l-graduation-matrix.md`.

Phase M is the active product-client phase. ADR-040 selects an exactly pinned
GPUI foundation and a Zed-inspired entity/action/workspace architecture while
preserving the public API as the only product boundary. The normative desktop
plan and milestone gates are in `docs/PLANS/phase-m-gpui-desktop.md`.

## The five goals this product wishes to achieve

1. **The server is the product.** All product behavior lives in `sift-server`;
   clients are thin renderers. The HTTP + WebSocket protocol is the public
   surface, versioned and inspectable.
2. **Local-first, hosted-capable.** One binary, one model — runs alongside a
   desktop client for a single user, or as a daemon for a hosted multi-user
   deployment. Local mode is not a degenerate case; it is the same room with
   one member.
3. **Collaboration-native.** Rooms are the durable boundary. Multiple people
   edit the same query (CRDT for text only), share a connection, and observe
   each other's results — server-authoritative everywhere except the editor
   pane.
4. **Zed-class snappiness.** Server-side cursors, schema caching with
   invalidation, prefetch, warm pools, progressive post-paint indexing. The
   differentiator vs Navicat / DataGrip is feel, not feature count.
5. **A genuinely public API.** The protocol crate is pure data, semver-stable,
   and consumable by native and wasm clients. OpenAPI is a release artifact,
   not an afterthought; a 3rd-party UI is a valid target, not a threat.

## consider

<https://github.com/pavi2410/based>
