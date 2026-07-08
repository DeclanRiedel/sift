# Phase B Completion — Decisions & Plan

Closes out the remaining Phase B items after the reliability list, driver
isolation (ADR-013), and versioning (ADR-016) landed. Decisions below are
locked (confirmed 2026-07-07); the plan is the agreed implementation shape.

## Status — all done

- [x] Secret backends: encrypted-file + OS-keychain (`207eb67`), dev-env
      wiring (`c9cce1b`).
- [x] SQL fingerprinting + audit sanitization, configurable `store_sql`
      (`53a6acc`); ADR-009 updated.
- [x] HTTP result byte cap (+ configurable row cap) and constant-time bearer
      comparison (`a5434c2`).

Every commit passed `cargo fmt`, `clippy --workspace --all-targets -D
warnings`, and `cargo test --workspace`. The `os-keychain` feature is off by
default; it is a pure-Rust build (zbus-based Secret Service on Linux, no system
libdbus) verified with `cargo check -p sift-server --features os-keychain`, and
gated only because it needs a running credential service at runtime.

## Locked decisions

1. **Secret backends: both.** Ship an encrypted-file backend (tested,
   headless-friendly) *and* an OS-keychain backend, selected by
   `metadata.secret_backend` (`memory` | `file` | `keychain`). Honors ADR-008
   (secrets never in the SQLite metadata db — both stores live outside it).

2. **Encrypted-file key source: keyfile path only.** The file store reads a
   32-byte key from a file whose path comes from config
   (`metadata.secret_key_file`, settable via env). No raw key in an env var —
   env vars leak via `/proc`, crash dumps, child procs, CI logs; a `0600`
   keyfile keeps material on disk-with-perms. Missing/unreadable keyfile =
   secrets unavailable with a clear startup error. The nix dev shell generates
   a git-ignored dev keyfile and exports its path.

3. **SQL fingerprinting: configurable.**
   - The operation **audit/replay trail always** stores a fingerprint, never
     raw SQL or bind values. This covers both the in-memory `/v1/operations`
     view and the optional JSONL operation log — today both serialize
     `Operation::ExecuteQuery` with full `sql` + `params`, which is the live
     bind-value leak. Sanitize the `Operation` before it is stored.
   - **`query_history` keeps raw SQL by default** (`metadata.store_sql = true`)
     because showing/re-running past SQL is a user-facing feature; it already
     omits bind values. Setting `metadata.store_sql = false` stores the
     fingerprint there too for privacy-sensitive deployments.
   - Touches ADR-009 (operation audit as a first-class *replay* contract):
     raw-SQL replay for queries is intentionally dropped in favor of a
     fingerprint. Update ADR-009 to record that the audit trail is a
     sanitized record, not a verbatim replay source, for query bodies.

4. **HTTP result cap: add a configurable byte cap.** Keep the existing
   row-count cap (10k) and add a total-bytes cap so 10k wide rows can't OOM
   the HTTP `execute` path. Both configurable under `config.limits`; exceeding
   either returns `Code::ResultTooLarge`.

## Implementation plan

### A. Secret backends (`metadata` crate)
- New deps: an AEAD cipher (`chacha20poly1305`, RustCrypto — small, pure-Rust,
  audited) for the file store; `keyring` for the OS backend. `rand_core` is
  already a workspace dep for key/nonce generation.
- `FileSecretStore`: implements the existing `SecretStore` trait
  (`put/get/delete(namespace, handle, secret)`). On-disk format: a single
  encrypted blob (a serialized `HashMap<(ns,handle), bytes>`), AEAD-sealed with
  a per-write random nonce, atomically replaced (`write tmp + rename`). Loads +
  decrypts on open; re-seals on each mutation. Key comes from the keyfile.
- `OsKeychainSecretStore`: `keyring` entries under service `sift`, account
  `"{namespace}:{handle}"`, using keyring's binary `set_secret`/`get_secret`.
  Pure-Rust build (zbus Secret Service on Linux; no system libdbus).
- `main.rs::build_metadata_store`: match `secret_backend` → construct the
  chosen store; `file` requires `secret_key_file`; unknown value errors as
  today.
- Never log secret bytes (already the rule; add a redaction note/test).
- Tests: `FileSecretStore` round-trip put/get/delete/overwrite with a temp
  keyfile + wrong-key-fails-to-decrypt. Keychain tests `#[ignore]` (no Secret
  Service in headless CI) + documented manual verification.

### B. SQL fingerprinting
- `fingerprint(sql) -> String`: normalize (trim, collapse internal whitespace,
  lowercase) then hash (SHA-256 hex; add `sha2`). Document that this is a
  coarse grouping key, not literal-stripping normalization.
- `sanitize_operation(Operation) -> Operation`: replace
  `ExecuteQuery.request.sql` with its fingerprint and clear `params`; pass
  other variants through. Apply in `push_operation_full` before building the
  `OperationAuditEntry`, so both the in-memory ring and the JSONL writer only
  ever see sanitized operations.
- Config `metadata.store_sql: bool` (default `true`). Thread into the execute
  history path: when false, `NewQueryHistory.sql_text = fingerprint(sql)`.
- Tests: an executed parameterized query leaves no raw SQL/params in
  `/v1/operations` or the JSONL log; `query_history` keeps raw SQL when
  `store_sql=true` and the fingerprint when false.

### C. Result byte cap
- `config.limits.max_http_result_bytes` (default 16 MiB) +
  `max_http_result_rows` (default 10_000, replacing the const).
- Thread limits into `SessionStore` (atomic, like `request_timeout`) and into
  `drain_stream`; accumulate an approximate per-row byte size (sum of value
  sizes — text/blob lengths + fixed widths) and error with `ResultTooLarge`
  when either cap trips.
- Tests: a result over the byte cap (few huge rows, under the row cap) returns
  `ResultTooLarge`.

### D. Config + nix
- `config.rs`: add `metadata.secret_key_file`, `metadata.store_sql`, and a
  `LimitsConfig { max_http_result_rows, max_http_result_bytes }`.
- `flake.nix` devShell: generate `.sift/dev-secret.key` (0600, git-ignored) if
  absent and export its path so `secret_backend = "file"` works out of the box
  in dev.

## Low-stakes items — will just do (no decision needed)
- **Constant-time bearer-token comparison** in `auth_middleware` (currently
  plain `==`, a timing oracle). Use a constant-time compare.
- Mark the now-complete "Audit granularity" backlog item done.

## Deferred (with reason)
- **Graceful-shutdown straggler-cursor cancellation** (global cursor registry)
  and explicit pool close — per ADR-018, the per-query timeout + connection
  close is the current backstop. Revisit if a real need appears.
- **`Reused`/`Reopened` distinction** in the `ping` result after a reconnect —
  cosmetic; not needed for correctness.
- **Vault secret backend** — hosted-phase concern (ADR-019 territory).
