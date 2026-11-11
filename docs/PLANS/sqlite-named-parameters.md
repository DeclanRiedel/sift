# SQLite named parameter mapping

Work tracking (2026-11-11): **Done (scoped contract)**. Remaining product work: [current TODO](../TODO.md#sqlite).

Implemented contract. Complete native named-bind handling through normal
queries, Benchmark/Profile and reusable definitions. The current desktop counts
some mixed/sparse SQL that the driver rejects; native-name and slot discovery
must become one pure shared contract, not competing guesses.

Follow SQLite's documented parameter numbering and its bundled tokenizer:
[SQL expressions](https://sqlite.org/lang_expr.html). Support anonymous `?`,
positive `?NNN`, case-sensitive prefixed `:name`, `@name`, `$name`, repeated names,
Unicode identifiers and Tcl-style namespace/suffix names. Prefixes are part of
identity. New names/anonymous binds take the next index after the largest prior
index; explicit numbered aliases refer to that index. Keep Sift's contiguous-slot
requirement: final gaps and zero/oversized indexes are refused. A positional
Value vector maps in native slot order, with bounded alias labels and explicit truncation in review.

Add a pure core lexer/layout shared by the SQLite driver, desktop, SQL-variable
compiler and server validation. Skip SQLite strings, quoted identifiers, comments,
blob literals and identifier bodies containing `$`; reject malformed names,
unterminated quotes/suffixes and NUL. Bound slot allocation to caller limits.
Use token spans and normalized placeholders for classification only. Original
SQL executes unchanged. Typed static lexer errors preserve syntax versus invalid
parameter codes without echoing SQL or values. Native prepared parameter count/name/index metadata must
agree before binding, and parameterized batches must be refused before any step.
No Driver signature or wire change. Native read-only/authorizer/cancel boundaries
remain authoritative.

Classification of a supported SQLite named token may use its canonical `?NNN`
placeholder to bridge parser grammar differences. Use the same normalization in
Benchmark/Profile read validation and connection-policy parsing, preserving all
non-parameter text. It grants no new read/write authority and performs no textual
value substitution. Actual execution and saved SQL stay original.

Desktop normal-query and remembered plan/performance parameter forms become
engine-aware and label native slot aliases. Desktop fresh forms and definition
creation/editing/API validation use the same layout/count (maximum 256); native
API bindings retain 32766. Fresh definition-run inputs
show those aliases; stored definitions still contain no values. Repeated names
prompt once. Source/revision and workload-confirmation safeguards stay intact. WebSocket
template-history descriptor validation uses the actual connection engine.

SQL templates reserve existing native slots before generated `?NNN` binds.
Exclude actual template spans from discovery; refuse templates embedded inside
an atomic native parameter name rather than rewriting its meaning. Existing
PostgreSQL/SQL Server behavior stays unchanged. Malformed/unsupported inputs
return static errors without SQL or values. No new secret persistence.

Acceptance uses bundled native prepare metadata to cross-check names/slots for
mixed/repeated/numbered/Unicode/Tcl names and literal/comment/identifier traps.
Prove actual values through the real-file driver and HTTP ordinary read,
Benchmark/Profile and definition rerun. Refuse gaps, cardinality mismatch,
parameterized batches, writes under read-only policy and template-slot collisions.
Exercise actual Vim parameter/definition review labels. Run formatting, strict
workspace Clippy and workspace tests; graduate ADR-138 and update inventories.
Native Windows/macOS host acceptance remains deferred.

## Acceptance — 2026-11-05

Implemented and graduated as ADR-138. Shared-layout acceptance covers repeated
names, numeric aliases, mixed contiguous slots, Unicode/Tcl names, quoting,
comments and identifier traps, invalid indexes/gaps, template exclusion and
atomic-name refusal. Differential tests compare slot count and native
name/index metadata with bundled SQLite, including dollar names versus
PostgreSQL dollar-quoted text, long numeric aliases and Tcl whitespace rules.

The real-file driver suite passes 16 tests. Added execution assertions prove
native value order/alias reuse, original SQL, refusal of parameterized batches
before writes, read-only enforcement and distinct syntax/parameter error codes.
The real-file HTTP/SDK acceptance now exercises named ordinary reads, count
mismatch, definition save/rerun, Benchmark and Profile with the original SQL.
A real WebSocket execution compiles a template beside a quoted Tcl-style bind
name and validates engine-aware history descriptors while returning exact values.
Restricted-policy tests retain read-only and schema authority around named tokens.

Actual Vim interaction acceptance proves bounded alias labels, one input per
native slot, native Tab/Enter behavior, unchanged ordinary SQL, cleanup of input
subscriptions, fresh definition values and explicit confirmed slot-order rerun.
Template tests prove native-slot reservation, name/identifier/quote boundaries,
atomic-name refusal and unchanged PostgreSQL dollar-quote behavior.

Linux validation passed: `cargo fmt --check`,
`cargo clippy --workspace --all-targets --locked --offline -- -D warnings`, and
`cargo test --workspace --locked --offline` (1,708 tests passed, five ignored).
The final full run includes typed static lexer errors preserving SyntaxError
versus InvalidParameterValue. No Driver signature, wire contract, schema migration,
CI workflow or environment-variable change. Native Windows/macOS host acceptance
remains deferred.
