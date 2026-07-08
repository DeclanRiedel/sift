# Language vs Feature — Capability Matrix

> Short companion to `alternative-tech-counterfactual.md`. One row per
> capability, four columns: what the capability is, what the industry ships,
> what sift has today (code-grounded), and whether Rust can already do it
> well enough that switching languages buys nothing.

| Capability | Industry norm | What sift has now | Rust's honest ceiling |
|---|---|---|---|
| DB wire protocol + row decode (PG, TDS, NUMERIC) | Native drivers per engine (C/C++/Go/JDBC) | `tokio-postgres` + `tiberius`, deep decode | **Best-in-class. No switch.** |
| Schema introspection (deep SQL + typed decode) | Per-engine `information_schema` queries | Real in both drivers | **Best-in-class. No switch.** |
| Real-time presence / room lifecycle | Phoenix (BEAM) or bespoke over Redis | Hand-rolled `DashMap` + `broadcast::channel` (`room_runtime.rs`) | **Achievable, hand-rolled.** BEAM is structurally better; Rust needs discipline. |
| Streaming fanout to N room viewers | Phoenix PubSub | `broadcast::channel(1024)`, drops on overflow, no rejoin | **Achievable.** No reconnect/merge story without building it. |
| Query-result sharing / verification | Ephemeral (no DB IDE does this) | Ephemeral, capped 10k rows (`drain_stream`) | **Language-agnostic.** Rust builds a CAS fine — design decision, not language. |
| Audit log | Append-only table or JSONL | In-memory Vec + JSONL, cap 10k, no hash chain | **Rust hashes fine.** Design decision. |
| Saved queries / lineage | Rows in a table | Dead schema, never used | **Language-agnostic.** CAS design. |
| CRDT / collaborative editor core | Yjs (JS) or Automerge (Rust) | UTF-8 buffer + apply-op placeholder (`doc/src/lib.rs`) | **Automerge/Loro Rust cores exist. Fine.** |
| Client-side browser editor | TypeScript + JS CRDT | Deferred (ADR-010) | **TS required here.** Rust→wasm for bindings only. |
| SQL parsing / equivalence | Real PG parser (`pg_query` C) or `sqlparser` | Not present | **`sqlparser-rs` competent. `pg_query` via FFI for max correctness.** |
| Zero-downtime hot upgrades | Blue-green deploys | None | **No story in Rust.** BEAM wins uniquely. |
| Horizontal clustering (hosted) | Redis + service mesh, or BEAM distribution | None | **Achievable with infra.** BEAM is built-in. |
| Per-process GC isolation (latency under mixed load) | Shared-heap runtime (most) or per-process (BEAM) | Shared Tokio allocator | **No per-process GC in Rust.** BEAM wins for "Zed-class snappiness" under mixed fast/slow load. |
| Build / reproducible artifacts | Docker, Make | Nix flake (devShell) | **Nix already in use.** Tooling, not product. |

## Verdict

Rows where "Rust's honest ceiling" reads **Best-in-class / No switch** are the
product's heart — driver, decode, schema, compute. They justify Rust as the
single-language pick.

Rows that read **Achievable** or **Language-agnostic** are where Rust is
competent but not advantaged; these are design decisions (CAS, hash-chain,
clustering infra), not language decisions.

Only three rows show a language *uniquely* beating Rust on product quality:
**BEAM for presence/clustering/hot-upgrade/per-process-GC**, and **TypeScript
for the browser editor**. Everything else is either Rust-best or
design-not-language.
