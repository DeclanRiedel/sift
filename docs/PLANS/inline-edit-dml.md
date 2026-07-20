# Design — Inline-edit → DML generation (Phase D)

> Status: **implemented.** The contract and implementation turn
> a set of result-grid edits into safe, minimal, parameterized DML with a
> preview step and transactional apply + conflict detection.
>
> Implies **ADR-023 (candidate): inline-edit conflict & row-identity model.**
> Everything else here is server-side composition that preserves the ADR-017
> driver-trait lock — no new `Driver` method, same shape as `ddl.rs` /
> `export.rs`.

## Goal

The GUI shows a result grid. A user edits cells, adds rows, deletes rows. The
server must:

1. **Resolve row identity** — map each edited grid row back to a real table row.
2. **Generate DML** — minimal, parameterized `INSERT` / `UPDATE` / `DELETE`.
3. **Preview** — return the exact statements + bind values (never auto-run).
4. **Apply** — execute the whole edit set in one transaction, detect concurrent
   modification, and roll back atomically on any conflict or error.

Non-goals: multi-table edits, joins, computed-view writes, bulk paste of
thousands of rows (that is `BulkInsert` / CSV import), and schema edits (that is
DDL). One edit set targets exactly one base table on one connection.

## Where it lives

- **Protocol types** → `crates/protocol/src/edit.rs` (pure serde, new module).
- **Server logic** → `crates/server/src/edit.rs` (composition, mirrors
  `ddl.rs`). Uses `Driver::schema(Deep)` for identity + column metadata and
  `Driver::execute` for preview validation / apply.
- **Audit** → two new `Operation` variants (additive; protocol-version bump per
  ADR-016): `PreviewEdits { session, connection }` and
  `ApplyEdits { session, connection }`. SQL is fingerprinted, never stored raw
  (existing audit-redaction contract).

## Protocol shape (`sift-protocol`)

```text
EditSet {
  table: ObjectPath,          // schema-qualified base table (kind = Table)
  edits: Vec<RowEdit>,
}

RowEdit =
  | Insert { values: Vec<CellEdit> }
  | Update { key: RowKey, changes: Vec<CellEdit>, expected: Vec<CellEdit> }
  | Delete { key: RowKey, expected: Vec<CellEdit> }

CellEdit { column: String, value: Value }   // Value = existing cell union
RowKey  { columns: Vec<CellEdit> }          // the identity columns + values
```

- `expected` carries the **original** values of the columns the client last saw
  (the optimistic-concurrency baseline). For `Update` it is the pre-edit value
  of each changed column; for `Delete` it may be empty (key-only) or the full
  row, per the conflict policy chosen below.
- `Value` is reused verbatim, so typing and JSON/binary encoding are already
  solved. Binds go through the existing positional `params: Vec<Value>`.

### Preview / Apply requests + responses

```text
PreviewEditsRequest { connection, edit_set: EditSet }
ApplyEditsRequest   { connection, edit_set: EditSet, tx: Option<TxHandleRef> }

EditPlan {
  statements: Vec<EditStatement>,   // ordered; 1+ per RowEdit
  identity: IdentitySource,         // PrimaryKey | UniqueIndex{name} | (error)
}
EditStatement {
  edit_index: usize,                // which RowEdit produced it
  kind: Insert|Update|Delete,
  sql: String,                      // parameterized, engine-quoted
  params: Vec<Value>,
}

ApplyEditsResult {
  applied: Vec<EditOutcome>,        // per statement: affected_rows, returned key
  committed: bool,
}
EditError = Conflict { edit_index, detail } | Validation { .. } | Driver { .. }
```

Preview is pure generation + static validation — it does **not** touch the DB
except (optionally) to confirm identity columns exist in the deep schema
snapshot it already fetches. Apply is the only path that executes.

## ADR-023 — row identity & conflict model (the decisions)

**1. Row identity source, in priority order:**
   1. **Primary key** — columns with `ColumnMetadata.primary_key == true`
      (composite PK = all of them). This is the default and the only always-safe
      key.
   2. **Unique index** — if no PK, fall back to a single non-nullable UNIQUE
      index from the deep schema snapshot (`indexes` where `unique == true` and
      no column nullable). Report which one via `IdentitySource::UniqueIndex`.
   3. **No stable key → reject.** Return `Code::EditNoRowIdentity` with a
      message. We do **not** silently fall back to full-row-match WHERE clauses
      (ambiguous: matches N duplicate rows, corrupts data). A read-only grid is
      the correct GUI behavior here.

   `ctid` (PG) / `%%physloc%%` (MSSQL) are explicitly rejected as identity —
   they are not stable across `VACUUM` / page moves and would silently target
   the wrong row.

**2. Conflict detection — optimistic, value-based:**
   - Every `UPDATE` / `DELETE` WHERE clause is `PK = ? [AND …]` **plus**
     `original_col = ?` for each column in `expected`. So the statement only
     hits the row if it still holds the values the user last saw.
   - After execute, assert `affected_rows == 1`. `0` → `Conflict` (row changed
     or vanished under the user); `>1` → `Validation` error (identity wasn't
     unique — should be impossible given decision 1, so it's a hard stop).
   - `NULL` in `expected` generates `col IS NULL`, not `col = NULL`.
   - Rejected alternative: rowversion/xmin snapshots. Cleaner in theory but
     `xmin` isn't in `ColumnMetadata` today and MSSQL needs an explicit
     `rowversion` column; value-based works on every table with zero schema
     prerequisites. Revisit if wide rows make the WHERE clause unwieldy.

**3. Column exclusions on write:**
   - Identity / auto-increment columns (`auto_increment == true`, PG
     `is_identity`) are **omitted** from `INSERT` column lists and never
     `UPDATE`d; the DB assigns them. `INSERT … RETURNING <pk>` (PG) /
     `OUTPUT inserted.<pk>` (MSSQL) returns the assigned key so the grid can
     refresh the new row.
   - Generated/computed columns are not represented in metadata yet and can
     still reach generated DML. They will error at the database. Modeling and
     excluding them remains tracked in `ddl-gaps.md`.

**4. Statement ordering within an edit set:** deletes, then updates, then
   inserts — avoids a delete+insert of the same PK colliding, and lets an
   insert reuse a key a delete just freed. All inside one transaction.

## Apply flow (transactional, reuses existing machinery)

1. `Driver::schema(Deep)` on the target table → column metadata + identity.
   Cache-friendly (goes through `SchemaCache`).
2. Generate + validate the `EditPlan` (same code path as preview).
3. `begin_transaction` (existing) — or use the caller-supplied `tx`.
4. For each `EditStatement` in order: `execute` under the tx, check
   `affected_rows`. First conflict/error → `rollback_transaction`, return the
   `EditError` tagged with `edit_index`.
5. All succeed → `commit_transaction` (unless the caller owns the `tx`, in which
   case leave it open and let them commit — mirrors `ExecuteRequestHttp.tx`).
6. Return `ApplyEditsResult` with per-statement `affected_rows` + returned keys.

Isolation, timeout, cancel, and audit all come for free: apply runs through the
same `SessionStore` execute path that already has spawn+timeout+cancel (ADR-013)
and correlation-id/audit wiring.

## Routes

```
POST /v1/sessions/:id/connections/:conn_id/edits/preview   → EditPlan
POST /v1/sessions/:id/connections/:conn_id/edits/apply      → ApplyEditsResult
```

Both bodies are `{ edit_set, tx? }`. Preview is idempotent and side-effect free;
apply mutates. OpenAPI entries added alongside (and this is one more reason to
land the Phase J "OpenAPI from typed schemas" item — the hand-authored blob will
need four new schema objects).

## Engine specifics

- **Quoting:** reuse the existing identifier-quoting helpers in each driver's
  schema/DDL path (PG `"ident"`, MSSQL `[ident]`) — do not hand-roll.
- **Confirmation of assigned keys:** PG `RETURNING`, MSSQL `OUTPUT inserted.*`.
  The `is_pure_dml` / affected-rows routing already distinguishes these (it was
  hardened in the quality pass), so returned rows surface correctly.
- **Type coercion:** `Value` → bind is already engine-mapped by the drivers;
  the one open driver gap (NULL params typed as `TEXT`, tracked in
  `quality-pass-findings-v2.md`) affects `expected` NULLs — mitigated here
  because NULL comparisons are emitted as `IS NULL` literals, not binds.

## Test plan

- Unit (server, MockDriver): PK single/composite key generation; unique-index
  fallback; no-identity rejection; identity/auto-increment omission from
  INSERT; NULL-in-expected → `IS NULL`; delete/update/insert ordering.
- Live PG (`live-pg`): round-trip an update with a concurrent out-of-band change
  → asserts `Conflict` and rollback; insert with serial PK → `RETURNING` key;
  composite-PK update; delete with stale `expected` → conflict.
- Live MSSQL (`live-mssql`): same matrix, `OUTPUT` path, `[bracket]` quoting.
- Preview never executes: assert MockDriver `execute` call count is 0 on
  preview.

## Open questions (resolve during implementation)

- Should preview optionally do a **dry-run count** (`SELECT count(*) WHERE
  <conflict predicate>`) so the GUI can warn "3 of 10 rows changed" before
  apply? Adds N read queries to preview; default off, opt-in flag.
- Batch size ceiling for one edit set (protect the tx) — propose a config limit
  mirroring `config.limits`, reject oversized sets with `ResultTooLarge`-style
  code.
- Whether to expose `IdentitySource` in the *preview* so the GUI can gray out
  editing when a grid isn't backed by a single base table (recommended: yes).
