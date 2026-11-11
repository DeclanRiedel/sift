# Export query terminal ownership

Work tracking (2026-11-11): **Done (scoped contract)**. Remaining product work: [current TODO](../TODO.md#ai-execution).

Status: implemented and validated on Linux; graduated to ADR-157.

## Contract

An ExportQuery operation describes the export observation, including encoding and
HTTP-body production. Opening a native cursor is not export success. Record one
terminal typed audit with the original actor and correlation: success only after
the encoder finishes and all produced chunks pass server pacing/quota admission;
failed observations retain stable codes and generic diagnostics. Authorized wire
errors may retain native details, but audit records must not copy them. HTTP-body
completion proves server production, not receipt or persistence by the client.

Use the existing bounded query-settlement admission across synchronous queries,
WebSocket queries and exports. Capture the audit owner before cursor startup, so
caller cancellation before headers still records an uncertain failed observation.
The HTTP future owns startup and the body owns encoding/pacing afterward. Their
synchronous audit-drop fallback works even when the body was never polled or is
abandoned while the native producer is silent; no detached body-draining task is
needed. Keep the permit and one shared logical shutdown guard until export
observation ends. Explicitly propagate that same guard into native execution,
whose independent owner retains it through original raw channel EOF. Observer
loss must neither abort native ownership nor release its admission/resources.

Require a native Done for every format, including text/JSON/HTML/XLSX. An early
channel close yields an error and must not append a successful trailer or file.
Parquet already requires Done. Preserve typed native errors inside the internal
I/O error, so audit can retain their stable code without parsing diagnostic text.
Encoding and delivery refusal are export failures even if native work completed;
they do not assert transaction rollback or client receipt. Existing cursor drop
cleanup, policy checks, result limits and bounded native supervision remain.

## Acceptance

Extend existing shutdown/export fixtures rather than adding smoke infrastructure.
Cover successful body completion, no premature audit at headers, native error
whose diagnostic echoes private input, missing terminal output, encoder failure,
body never polled, mid-stream body drop, silent producer, startup caller loss,
startup refusal before native dispatch and timeout followed by late native completion. Check one typed
ExportQuery audit, actor/correlation privacy and shutdown retention beyond Done
until raw EOF. Exercise byte pacing/quota refusal and drain interruption through
the existing pacing fixture. All formats retain ordinary output acceptance.

Run formatting, strict workspace/all-target Clippy and workspace tests under the
10% host CPU/RAM cap. Preserve wire protocol, locked Driver signatures and manual
CI. Native Windows/macOS acceptance stays deferred. Query-history/change-ledger
source attribution and provider-native export semantics remain separate work;
this change must not claim durable client delivery or rollback certainty.

## Completed evidence

Formatting and strict workspace/all-target Clippy passed. The complete workspace
suite passed 1,755 tests with six existing ignored cases. Rust/Cargo used the
shared 10% host CPU/RAM cap throughout validation.

The existing shutdown fixture covers ten cases: success, native error, missing
Done, encoder refusal, unpolled body, row delivery followed by body loss, startup
caller loss, invalid-connection startup refusal, timeout with late native return,
and server drain while the producer is silent. Each observes exactly one export
audit with original actor/correlation and excludes private SQL, binds, rows and
native diagnostic echoes. Success remains absent at headers. Native EOF tracking
survives both successful and failed observation settlement. Startup refusal proves
no driver dispatch; caller-loss settlement finishes while native startup remains
blocked with observation timeouts disabled. The missing-Done encoder check covers
all nine formats, and the existing byte-pacing fixture covers rate, retained-byte
quota and drain refusal. Ordinary CSV/TSV/JSON/HTML/Markdown/XLSX/SQL/Parquet
acceptance and the previous HTTP/WebSocket ownership fixtures remain green.

Two fixture assumptions were corrected during validation: audit result codes use
Code Display text rather than wire spelling, and MockDriver.execute_err produces
a page error after cursor publication. Invalid connection selection now exercises
actual pre-cursor refusal; the native page-error case remains independently covered.
