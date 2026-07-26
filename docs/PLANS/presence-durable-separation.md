# Design — Presence vs durable separation (Phase G)

> Status: **drafted.** Phase G collaboration item G4. Splits the room
> broadcast into an **ephemeral presence lane** (drop-OK) and a **durable
> document-op lane** (lag ⇒ forced resync), and wires the already-built but
> dead lag-recovery scaffolding (`runtime_epoch`, `next_event_seq`,
> `RoomServerMessage::ResyncRequired`). Composes over the existing
> `broadcast`-based `RoomRuntime` and the CRDT document actor — **no new
> `Driver` method, no protocol client-message change** (ADR-017 trait lock and
> the existing room protocol are preserved).
>
> Implies **ADR-035 (candidate): room lane separation + CRDT-safe lag
> recovery.** Weight on README **Goal #4 (Zed-class snappiness)**: presence
> spam must never stall or evict a peer's durable edits, and a lagged peer must
> recover silently without a reconnect.

## The bug this fixes

Today one `broadcast::channel(1024)` per room carries **every** broadcast
message — `Presence`, `QueryResult`, and the durable
`DocumentUpdateCommitted` (`room_runtime.rs:24`, published at
`http.rs:4895` and `http.rs:5791`). The WS receive loop has a single select
arm; on `RecvError::Lagged(n)` it resends **presence only**
(`http.rs:5476-5480`):

```rust
Err(RecvError::Lagged(_)) => {
    send_json(&mut sender, &RoomServerMessage::Presence {
        presence: state.rooms.presence(room.0),
    }).await?;   // durable DocumentUpdateCommitted in the dropped window is LOST
}
```

Any committed CRDT update inside the dropped window is **silently discarded**;
the peer diverges until it happens to issue a fresh `DocumentSync`. Two lanes
of unrelated traffic (chatty presence pings vs rare durable commits) share one
1024-slot ring, so bursty presence can also *evict* durable ops before a slow
consumer drains them.

The recovery machinery for this already exists and is **never used**:

- `RoomServerMessage::ResyncRequired { runtime_epoch, event_seq }` — defined
  (`protocol/src/room.rs:141`), never constructed anywhere in `server/`.
- `DocumentRegistry::runtime_epoch()` + `next_event_seq()` — built for "lag
  recovery" (`document_registry.rs:2, 51-59`), never called.

## Decisions locked (this session)

1. **Two per-room broadcast channels.** `presence_events` (ephemeral,
   drop-OK) and `doc_events` (durable). Not a single tagged channel:
   `Lagged(n)` cannot report *which* message kinds were dropped, so a shared
   channel would force a document resync on any presence-only spam. Separate
   lanes make each lane's lag semantics independent.
2. **`QueryResult` rides the ephemeral lane.** It is a run notification, not
   durable CRDT state; losing it on lag is recoverable by re-query and it is
   outside the ADR-014 CRDT scope. The durable lane stays **strictly CRDT
   document ops**.
3. **Durable-lane lag ⇒ `ResyncRequired`.** Wire the existing
   `runtime_epoch` + `next_event_seq` scaffolding. Recovery is CRDT-safe: the
   client re-issues `DocumentSync` from its Loro version vector and Loro
   merges idempotently — no data loss, no reconnect.

## The two lanes

| | Presence lane | Document lane |
|---|---|---|
| Channel | `presence_events` | `doc_events` |
| Carries | `Presence`, `Attached` refresh, `QueryResult`, `RateLimited` | `DocumentUpdateCommitted` |
| Durability | ephemeral, never persisted | durable (already persisted by the actor before broadcast) |
| On `Lagged` | resend full presence snapshot | emit `ResyncRequired`; client re-runs `DocumentSync` |
| Capacity | small ring (256) — newest-wins is fine | keep 1024 — depth buys resync-free recovery |
| Loss model | last-writer-wins snapshot heals it | version-vector resync heals it |

Point-to-point document traffic (`DocumentSync` → `DocumentChunk` /
`DocumentSynced`, `DocumentUpdateAck`, `DocumentError`) is unchanged — it
already goes straight to the requesting `sender`, not through any broadcast,
so it is unaffected by lane splitting.

## Changes

### `room_runtime.rs` — split the channel

`RoomRuntimeRoom` holds two senders instead of one:

```rust
struct RoomRuntimeRoom {
    presence: DashMap<i64, RoomPresence>,
    presence_events: broadcast::Sender<RoomServerMessage>, // ephemeral, cap 256
    doc_events: broadcast::Sender<RoomServerMessage>,       // durable,    cap 1024
    subscribers: AtomicUsize,
}
```

- `subscribe()` returns a `RoomSubscription` holding **both** receivers.
- `publish()` splits into intent-named methods so the call site cannot pick
  the wrong lane:
  - `publish_presence(room_id, msg)` — presence/attach refresh, query results,
    rate-limit notices.
  - `publish_doc(room_id, msg)` — stamps `next_event_seq()` and sends on
    `doc_events`.
- `attach()` / `detach()` publish presence on `presence_events` (today they
  use the single `events` sender at `room_runtime.rs:65, 85`).
- Room eviction (`RoomSubscription::drop`, `room_runtime.rs:178-194`) is
  unchanged: it keys on `subscribers` + empty presence, independent of lane
  count.

### `http.rs` — two select arms + real doc-lane recovery

The single `event = events.recv()` arm (`http.rs:5473-5483`) becomes two:

```rust
ev = presence.recv() => match ev {
    Ok(msg)            => send_json(&mut sender, &msg).await?,
    Err(Lagged(_))     => send_json(&mut sender, &RoomServerMessage::Presence {
                              presence: state.rooms.presence(room.0),
                          }).await?,          // unchanged behavior, now scoped
    Err(Closed)        => break,
}
ev = docs.recv() => match ev {
    Ok(msg)            => send_json(&mut sender, &msg).await?,
    Err(Lagged(_))     => send_json(&mut sender, &RoomServerMessage::ResyncRequired {
                              runtime_epoch: state.rooms.documents().runtime_epoch().to_string(),
                              event_seq:     state.rooms.documents().current_event_seq(),
                          }).await?,          // was: silent presence-only refresh
    Err(Closed)        => break,
}
```

- The `DocumentUpdateCommitted` publish (`http.rs:5791`) switches
  `publish` → `publish_doc`. The `QueryResult` publish (`http.rs:4895`)
  switches → `publish_presence`.
- `event_seq` is a process-monotonic high-water marker returned to the client
  purely as an opaque diagnostic / dedupe key; the client does **not** diff it
  to reconstruct ops. Recovery is unconditional: on `ResyncRequired` the client
  re-issues `DocumentSync { known_version }` and Loro merges the delta. Add a
  `current_event_seq()` peek (load without increment) beside the existing
  `next_event_seq()`.

### Client contract

`ResyncRequired` is already in the protocol; today no server emits it, so
clients that ignore it are silently correct only because it never fires. The
contract this design activates: **on `ResyncRequired`, re-run `DocumentSync`
from the current Loro version vector.** A changed `runtime_epoch` (server
restart) means the client's `event_seq` cursor is meaningless and it must
resync from an empty/known version. Document this in `client-sdk` when the
room client is implemented.

## Non-goals

- **No persistence change.** Presence is already ephemeral (in-memory
  `DashMap` only); "not persisted" needs no new work. Durable doc ops are
  already persisted by the document actor *before* broadcast — the lane split
  changes delivery, not durability.
- **No result replication.** `QueryResult` stays a summary/reference
  (ADR-014 scope); riding the ephemeral lane does not make it durable.
- **No op-log / compaction changes.** Late-join snapshot+ops-since is Phase G
  item G7, tracked separately.

## Tests

- Presence burst that overflows the presence ring does **not** drop a
  concurrently-published `DocumentUpdateCommitted` (separate rings).
- Forcing `doc_events` lag yields exactly one `ResyncRequired` (not a
  presence refresh), and a follow-up `DocumentSync` from the stale version
  vector converges the replica (Loro merge idempotence).
- Forcing `presence_events` lag yields a presence snapshot and **no**
  `ResyncRequired`.
- `runtime_epoch` in `ResyncRequired` matches `documents().runtime_epoch()`;
  changes across a fresh `RoomRuntime` (simulated restart).
- Room eviction still fires when both lanes have zero subscribers and presence
  is empty.
