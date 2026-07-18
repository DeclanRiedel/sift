# TODO

## Open

- investigate: autocomplete API to see if scoring makes sense
  - Looked at `crates/completion/src/rank.rs`: scoring is context-weighted
    (per-context base bonuses + prefix match, stable sort by score then label).
    Coherent as-is; revisit only if ranking quality complaints surface.

## Done

- **Granular SQL Server error mapping.** `ms_err` in
  `crates/driver-sqlserver/src/lib.rs` previously collapsed every
  `tiberius::error::Error` to `Code::DriverInternal`. It now classifies by SQL
  Server error number (via `mssql_error_code`) and by transport variant
  (Io/Tls → `ConnectionFailed`, Conversion → `InvalidParameterValue`, …),
  matching the granularity of the Postgres driver's `pg_err`. Covered by unit
  tests.

## Considered — no change needed

- **Break up files (e.g. `column.rs` engine facets into new files).**
  `crates/protocol/src/column.rs` is ~138 lines and cohesive; the facet structs
  are tightly coupled to `EngineColumnFacets`. Splitting adds import ceremony
  for no benefit. The genuinely large files (`server/src/http.rs`,
  `session.rs`, `driver-sqlserver/src/lib.rs`, `cursors.rs`) are a separate,
  higher-risk refactor to schedule deliberately if churn there becomes painful.

- **Driver naming / "map IDs to drivers with prefixes 'pg' 'ms'".** The driver
  registry (`server/src/registry.rs`) is already keyed by the typed `Engine`
  enum, not string prefixes; string prefixes would be a regression. `ms_err`
  kept its name to parallel `pg_err`.

- **Completion API should key off the room's active engines.** Completion is
  already scoped per-connection: `SessionStore::complete` derives the engine
  from `entry.driver.engine()`, so a request never mixes engine vocabularies.
  Rooms don't blend connection engine IDs, so no change is required here.
