# External-provider estimated plans

Work tracking (2026-11-11): **Done (scoped contract)**. Remaining product work: [current TODO](../TODO.md#extensions-and-mcp).

Design: graduate a typed estimated-only plan family for external providers. Driver
RPC v3 adds `driver.explain.estimated@1` and `explain_estimated`; v1/v2 and existing
savepoints remain supported. Package/development validation requires minimum v3
for this capability. Do not accept the broader driver.explain@1 claim from an
external package: Profile/analyzed execution remains unavailable. No change to the
locked Driver/PgExt/MssqlExt signatures or public protocol version is needed.

Request carries exact connection, original validated single SQL/DML statement,
typed parameters and a byte ceiling. Response echoes connection and a reusable
connection disposition, with one typed PostgreSQL JSON, SQL Server XML or SQLite
EQP-row payload. Provider must obtain an estimated plan without running/analyzing
the workload, restore temporary session settings or discard the connection.
Operator-trusted process isolation cannot attest native behavior or confer quality.
Host rejects dialect/format mismatch, analyzed/runtime evidence, malformed/missing
plans, excessive depth/nodes or bytes, wrong handles and uncertain disposition.
Failure after dispatch stops the tenant/package generation; unrelated providers
remain live. Diagnostics never include raw plans/parameters. No implicit execute,
ANALYZE, session-setting SQL or fallback to a built-in provider occurs.

Existing Explain HTTP/SDK/Vim and governed estimated-plan reads use the family
only for host-supported semantic dialects. Capability discovery distinguishes
estimated Explain from Profile and refuses unknown dialects. Shared plan parsers
produce the existing tree/raw response with an explicit provider-supplied,
unattested estimated-plan warning. Raw plan text can contain sensitive query data;
current authorization, policy, audit, ownership and result limits apply.

Capture owns an exclusive connection admission lease, refuses active managed
transactions, rechecks current policy/source after waiting and retains query/quota/
shutdown ownership through actual RPC completion, including observer loss or
bounded observation timeout. Explain HTTP uses captured native source, original
actor/correlation and owned terminal audit. No source reopening or stronger login
replay. Native/built-in Explain behavior stays on its existing path.

Acceptance: executable v3 fixture through package/handshake, RuntimeDriver,
HTTP/SDK and ordinary Explain response; exact SQL/typed parameters and all three
formats; v1/v2 refusal, v3 savepoint inheritance, unknown dialect and Profile
refusal; wrong handle/format/mode, malformed/oversized/deep/cyclic plans and
uncertain disposition; blocked admission/policy/source changes, active transaction,
timeout/caller-loss ownership and independent provider generations. Update schema,
version docs, inventory and stable ADR after acceptance. Run fmt, strict Clippy,
focused process corpus and workspace tests under the existing 10% host cap. Native
third-party database certification and Windows/macOS acceptance remain separate.


## Linux acceptance

Formatting check and strict workspace/all-target Clippy passed with
`sift-server/live-mssql`. All workspace packages passed in two sequential Cargo
invocations under the user-requested resource cap:

- `cargo test -p sift-server --features live-mssql --locked --offline`:
  662 passed, zero failed, five ignored.
- `cargo test --workspace --exclude sift-server --features sift-server/live-mssql --locked --offline`:
  1,195 passed, zero failed, five ignored.

The combined total is 1,857 passed and ten ignored. The explicitly built existing
conformance executable separately passed three focused unit/parser cases, two
HTTP/SDK integration cases and the existing v2 savepoint lifecycle corpus.
Coverage includes all three native formats and original SQL/typed parameters,
invalid/nonfinite or foreign-provider parameters without generation loss, v1/v2
refusal, inherited v3 savepoints, analyzed-request refusal, malformed/runtime/wrong
handle/uncertain-state fencing, an independent surviving process, managed
transaction refusal, admission expiry, replaced source and queued source-authority
revocation, caller loss and observation timeout with one audit and retained gate/
shutdown ownership. The shared v2 corpus also exercises queued durable policy
revocation. Pure parser checks cover dialect mismatch, byte/depth bounds, bad
estimates, prohibited XML entities, cyclic SQLite rows and multiple XML plan roots.

Rust 1.96.1 used one Cargo job/test thread, single-core affinity, disabled
incremental compilation, debug-free package overrides and bounded codegen units
(server 4096, UI/workspace UI 256, desktop 512). Shared/service limits stayed at
CPUQuota 160%, MemoryMax 3,287,567,155 bytes and zero swap; successful grouped run
peak was 3,283,980,288 bytes. No project compiler settings or CI changed.

The single feature-unified `cargo test --workspace` build exceeded that memory
ceiling in the server unit-test compiler before execution. Grouped invocations
cover every package, with different Cargo feature unification; the subsequent combined Linux workspace gate passes at `e3d6b13e`
(1,988 passed, 32 ignored) under the same cap. No limit was
raised to obtain the passing evidence. Third-party native-provider certification,
external analyzed/Profile execution and Windows/macOS acceptance remain deferred
or outside this slice; the parent extension inventory remains partial.
