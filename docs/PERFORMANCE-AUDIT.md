# Performance Audit Note

Date: 2026-07-08

This note records the local component performance pass run against the current
workspace. Numbers are useful as a baseline, not as a portable benchmark:
hardware, kernel scheduler, Postgres configuration, release/debug profile, and
network path all change the result. The "industry standard" and "best"
columns below are heuristic targets for a responsive local/hosted database IDE,
not a formal standard.

## Summary

No component failed the available correctness suite. Two benchmark harness
items were addressed:

- `crates/driver-postgres/examples/bench.rs` hardcoded a socket path and could
  not target the same throwaway database as the live tests. It now honors
  `SIFT_PG_HOST`, `SIFT_PG_PORT`, `SIFT_PG_DB`, `SIFT_PG_USER`, and
  `SIFT_PG_PASSWORD`.
- `crates/driver-sqlserver` had live tests but no equivalent live performance
  benchmark. It now has a gated `live-mssql` benchmark example using the same
  `SIFT_MSSQL_*` connection contract as the live tests.

The strongest measured areas are the in-process server tests, metadata tests,
and Postgres read/query paths. The weakest areas are coverage gaps rather than
measured slowness: SQL Server live performance was not run, `client-sdk`,
`protocol`, `driver-api`, and `core` have little or no direct crate-local
tests, and `doc` is still a byte-buffer apply-op abstraction rather than a real
CRDT.

## Verification Run

Commands run:

```sh
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo test -p sift-driver-postgres --features live-pg --test live_pg -- --nocapture
cargo run -p sift-driver-postgres --release --features live-pg --example bench
cargo check -p sift-driver-sqlserver --features live-mssql --example bench
```

Results:

- `cargo fmt --check`: passed.
- `cargo clippy --workspace --all-targets -- -D warnings`: passed.
- `cargo test --workspace`: passed.
- Postgres live tests: 13/13 passed against a throwaway local Postgres 17
  instance over a Unix socket.
- SQL Server benchmark harness compiled with `live-mssql`.
- SQL Server live tests and live benchmark were not run because no live SQL
  Server instance or `SIFT_MSSQL_PASSWORD` was configured.

## Component Timing Baseline

Warm release-mode crate test timing, with release artifacts already built:

| Component | Local result | Standard target | Best target | Notes |
| --- | ---: | ---: | ---: | --- |
| `sift-core` | 0.12s | <1s | <250ms | Empty placeholder crate. |
| `sift-doc` | 0.14s | <1s | <250ms | Fast, but not a real CRDT yet. |
| `sift-driver-api` | 0.17s | <1s | <250ms | No direct tests. Mostly trait/types. |
| `sift-driver-sqlserver` | 0.32s | <2s | <500ms | Unit path only; live path not measured. |
| `sift-client-sdk` | 0.34s | <2s | <500ms | No direct tests; exercised via server integration tests. |
| `sift-driver-postgres` | 0.44s | <2s | <500ms | Unit path only in this number. |
| `sift-protocol` | 0.53s | <1s | <250ms | No direct tests despite public contract role. |
| `sift-metadata` | 0.59s | <2s | <750ms | Includes SQLite/refinery/secret tests. Good. |
| `sift-server` | 1.00s | <5s | <2s | Broad integration suite. Good. |

Interpretation: the warm local test suite is comfortably fast. Cold release
builds are much more expensive, especially server and bundled SQLite, but that
is compile cost, not runtime weakness.

## Postgres Driver Performance

Measured with the existing Postgres benchmark example against a local
Postgres 17 throwaway instance over a Unix socket, seeded with a 10k-row
`bench` table.

| Operation | Local result | Standard target | Best target | Assessment |
| --- | ---: | ---: | ---: | --- |
| Open, cold pool build | 1.753ms | <50ms local / <250ms remote | <10ms local | Strong. |
| Open, cached pool | 1.753ms | <10ms local | <2ms local | Good. Similar to cold because it still acquires a connection. |
| Ping p50 | 58us | <2ms local | <250us local | Strong. |
| Ping p95 | 68us | <5ms local | <500us local | Strong. |
| `SELECT 1` p50 | 56us | <2ms local | <250us local | Strong. |
| `SELECT 1` p95 | 102us | <5ms local | <500us local | Strong. |
| `SELECT * LIMIT 1` p50 | 90us | <3ms local | <500us local | Strong. |
| `SELECT * LIMIT 1` p95 | 217us | <5ms local | <1ms local | Strong. |
| `SELECT * LIMIT 100` p50 | 144us | <5ms local | <1ms local | Strong. |
| `SELECT * LIMIT 100` p95 | 223us | <10ms local | <2ms local | Strong. |
| DML update 10k rows p50 | 38ms | workload-dependent | workload-dependent | Acceptable for full-table update; not a UI latency proxy. |
| DML update 10k rows p95 | 85ms | workload-dependent | workload-dependent | Watch p95 under larger tables. |
| Schema shallow p50 | 569us | <50ms | <10ms | Strong. |
| Schema deep p50 | 2.32ms | <100ms | <25ms | Strong. |
| 10k-row scan | 816k rows/sec | >100k rows/sec local | >500k rows/sec local | Strong. |
| Sequential ping burst | 15.6k pings/sec | >1k/sec local | >10k/sec local | Strong. |

Main caveat: this is the best-case local Unix socket path. TCP, TLS,
cloud-hosted databases, wider rows, large schemas, and concurrent clients will
be slower. Add repeatable Criterion or integration benchmarks before using
these numbers as a regression gate.

## SQL Server Driver Performance

The SQL Server driver now has an equivalent benchmark harness:

```sh
SIFT_MSSQL_PASSWORD=... \
  cargo run -p sift-driver-sqlserver --release --features live-mssql --example bench
```

It uses the same environment variables as `tests/live_mssql.rs`:

- `SIFT_MSSQL_HOST`, default `127.0.0.1`
- `SIFT_MSSQL_PORT`, default `1433`
- `SIFT_MSSQL_USER`, default `sa`
- `SIFT_MSSQL_PASSWORD`, required
- `SIFT_MSSQL_DB`, default `master`

The harness measures open, ping, `SELECT 1`, `SELECT TOP (1)`, `SELECT TOP
(100)`, a 10k-row DML update, shallow schema, deep schema, 10k-row scan
throughput, and a 100-ping sequential burst.

No live SQL Server readings were available in this run. Until measured, use
the same practical bar as Postgres with a wider allowance for TDS/TLS overhead:

| Operation | Local target | Best target | Status |
| --- | ---: | ---: | --- |
| Open | <250ms | <50ms | Harness ready, unmeasured. |
| Ping p95 | <10ms local | <2ms local | Harness ready, unmeasured. |
| `SELECT 1` p95 | <10ms local | <2ms local | Harness ready, unmeasured. |
| `SELECT TOP (100)` p95 | <25ms local | <5ms local | Harness ready, unmeasured. |
| Shallow schema p95 | <150ms | <50ms | Harness ready, unmeasured. |
| Deep schema p95 | <250ms | <75ms | Harness ready, unmeasured. |
| 10k-row scan | >50k rows/sec local | >250k rows/sec local | Harness ready, unmeasured. |

This remains the largest measured-performance gap in the driver layer.

## Weak Spots

| Area | Weakness | Risk | Suggested next step |
| --- | --- | --- | --- |
| SQL Server live performance | Harness exists but was not run in this pass. | Unknown latency and throughput under real TDS/MARS/TLS behavior. | Run `cargo run -p sift-driver-sqlserver --release --features live-mssql --example bench` against a real instance and record the table above. |
| `client-sdk` | No crate-local tests. | SDK regressions may only be caught indirectly through server integration tests. | Add unit tests for URL construction, auth headers, response mapping, and WebSocket message handling with a mock server. |
| `sift-protocol` | Public wire contract has no direct round-trip/schema snapshot tests. | Accidental serde shape changes may pass compile/tests but break clients. | Add JSON golden tests and schema snapshot tests for key request/response and WS messages. |
| `driver-api` | Trait/handle layer has no direct tests. | Mock/stream semantics could drift silently. | Add tests for `ResultSetStream` behavior, `IdCounter`, handle identity/debug output, and mock driver contracts. |
| `core` | Empty placeholder crate. | No runtime risk today, but it can become a dumping ground. | Keep empty until a real shared server-internal type is needed, or remove from workspace if it stays unused. |
| `doc` | Fast, but not a real CRDT. | Collaboration correctness will fail under concurrent edits despite good local speed. | Replace the byte-buffer apply-op backend with the chosen CRDT and benchmark merge/apply/snapshot sizes. |
| Postgres benchmark harness | Was hardcoded to one socket path. | Performance tests could silently be skipped or fail outside one machine. | Fixed for env vars; next step is documenting fixture setup or adding a scripted runner. |
| Server integration performance | Only command-level timings were measured. | Route-level latency regressions could hide inside a passing 1s suite. | Add route-level benchmark tests for session create/list, metadata operations, HTTP execute, and room WebSocket broadcast. |
| Metadata scale | Current tests are small. | SQLite operations may degrade with many rooms, documents, history rows, and audit rows. | Add seeded scale tests for 10k+ audit/history rows, tenant lookup, token verification, and room membership queries. |
| Query result limits | Correctness tested, performance not stress-tested. | Wide rows or large values may inflate memory/CPU before caps trip. | Add benchmarks for row-byte accounting and cap enforcement on wide result sets. |

## Practical Performance Bar

For this product goal, "Zed-class snappiness" should mean:

- Local metadata and session actions: p95 under 10ms.
- Local driver ping/simple query: p95 under 5ms over Unix socket or loopback.
- Hosted driver ping/simple query: p95 dominated by network, but server
  overhead should stay under 5ms outside driver I/O.
- Shallow schema refresh: p95 under 100ms on ordinary schemas.
- Deep single-object schema fetch: p95 under 150ms.
- First page of query results: p95 under 100ms plus database execution time.
- UI-facing HTTP endpoints: p95 under 50ms for metadata/session-only routes.
- WebSocket fanout: p95 under 25ms for room document broadcasts at modest room
  sizes, excluding client render time.

Current measured data suggests the local Postgres driver and server substrate
are not the bottleneck yet. The main product risk is unmeasured scale and
collaboration behavior, especially SQL Server, metadata growth, and the future
CRDT backend.
