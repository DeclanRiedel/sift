# sift

A server-first, collaboration-native database IDE. 

The **server is the product**. `sift-server` owns all product behavior:
connections, sessions, schema, execution, history, audit, and collaboration.
Clients (desktop / web / automation) are thin, stateless renderers over a
single versioned HTTP + WebSocket protocol.

**Local-first by default, hosted-capable:** one binary runs in-process next to
a desktop client *or* as a daemon for hosted multi-user — same code, same
model. Single-user local mode is a one-member room; multiplayer is the same
model with more members.

**Collaboration is built in, not bolted on:** the durable unit is a room.
CRDT is used *only* for shared SQL editor text; results, schema, sessions, and
connections stay server-authoritative.

**The protocol is pure serde, public, and semver-stable from v0.1:** a third
party should be able to build a working UI against the OpenAPI spec alone.

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
