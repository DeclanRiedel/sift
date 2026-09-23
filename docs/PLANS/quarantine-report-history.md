# Retained CSV quarantine reports

Status: design for the next Linux transfer slice. This applies to PostgreSQL,
SQL Server, and SQLite imports through the shared transfer API.

- Store new CSV quarantine artifacts with a dedicated content type so a history
  view cannot mistake arbitrary JSON exports for rejected-row reports. Existing
  `application/json` reports remain reachable from their current result or ID.
- List at most 100 newest, unexpired reports in one workspace. Check room read
  access and the existing `ReadTransferRecipe` operation before returning
  metadata; record the read in the operation audit. Never include artifact bytes
  in the list response.
- Keep the existing artifact download endpoint as the only content path. A
  selected history item opens the same report viewer and is checked against the
  active instance and workspace. Retention is seven days; expired items are
  omitted, and the server remains authoritative if an item expires after list.
- A full artifact browser, pagination beyond the newest 100 reports, pinning,
  and recovery of older generic-JSON quarantine artifacts remain separate work.

Acceptance: metadata tests cover workspace/room isolation, expiry, ordering,
content omission, and the bound. HTTP tests cover authorization and auditing;
desktop tests cover list selection and stale response refusal.
