# Desktop API → UI Coverage & Build Priority

Every public `sift-client-sdk` `Client` capability mapped to the desktop UI that
should expose it, ordered by build priority. This is the working "what next" for
the Phase M client.

Legend: `[x]` wired to a working UI · `[~]` plumbing/model exists, no dedicated
UI · `[ ]` not started.

**Ordering principle:** a database IDE is only useful once you can connect to a
database, see what's in it, and run SQL against it. So the priority is the
daily-driver spine first — **connections → navigation → query intelligence** —
then writing/transactions, then collaboration/versioning, then advanced
surfaces, then admin/platform. Within P0, the connection experience comes first.

## Priority at a glance

- **P0 — Connect & run:** connection management UI, sign-in-to-connect, streaming execute + cancel. *The spine.*
- **P1 — Navigate:** schema tree, object DDL, search, history/saved queries, capability gating, editor depth.
- **P2 — SQL intelligence:** completion, diagnostics, formatting, quick-fixes, usages.
- **P3 — Write & transact:** data editing, transactions, savepoints, process control.
- **P4 — Collaborate & version:** rooms/presence/live docs, virtual workspaces, projections, Git.
- **P5 — Advanced surfaces:** diagrams, compare/migrations, runs/schedules, transfers, plan captures, extensions, DDL sources, notifications.
- **P6 — Admin & platform:** admin console, tenants/invites, tokens/keys, policy, audit/approvals.

---

## P0 — Connect & run (the spine) 🎯 start here

### Connections & sessions — connection picker, connected indicator, disconnect

- [x] `health`, `ready` — instance lifecycle gates negotiation on both probes
  and surfaces failed readiness as a degraded state (M2)
- [x] `open_session`, `open_connection_from_profile` — **connection picker built**: Connections dock rows connect on click; executor opens session + connection for the chosen tenant/profile (M3)
- [x] `connection_profiles` — profiles listed with a live status dot (disconnected / connecting / connected / failed) and connect/disconnect (M3)
- [x] `close_session` — used by the Disconnect action and on reconnect (M3)
- [x] `open_session_for_tenant` — every profile and ad-hoc query context opens
  a tenant-scoped session (M2)
- [x] `list_sessions` — Account lists live sessions and their physical
  connections, with refresh and close controls (M4)
- [x] `open_connection` (explicit spec) — connection-URL dialog offers
  **Connect once** without creating a durable profile (M4)
- [x] `ping_connection` — periodic connection health chip with latency, failure
  classification, last-success time, and explicit reconnect (M4)
- [x] `close_connection`, `disconnect_connection_profile` — Account closes an
  individual physical connection or session; profile actions disconnect that
  profile across sessions (M4)
- [x] **footer/status bar** — connection target, execution outcome, and live
  transaction status are wired. Exclusive, client-local left-panel and
  bottom-tool selection is landed.

### Sign-in enough to connect — sign-in modal, account state

- [x] `whoami` — signed-in identity and account/session controls are surfaced
  in the app bar account popover (M2)
- [x] `password_login`, `refresh_session` — hosted sign-in modal and explicit
      session refresh (M2)
- [x] `logout`, `logout_all` — account menu (M2)
- [x] `github_authorization_url`, `github_native_start`, `github_native_exchange`, `github_callback` — GitHub native sign-in (M2)
- [x] `exchange_ssh_proxy_capability` — desktop SSH profiles own the
  `sift-remote` helper, consume its short-lived grant, follow renewals, and
  preserve OpenSSH host-key/authentication policy (M2/H)

### Execute — finish the run path

- [x] `execute` — Run Statement/Document → virtualized grid (M3)
- [x] `stream_query` — streamed/paged cursor + live status (M3)
- [x] `cancel` — cancel a running query (M3)
- [x] `read_spilled_pages`, `delete_spilled_cursor` — paging/resume for large results (M3/M4)
- [x] `execute_with_params` — detected-parameter dialog with typed/null bindings (M3)

---

## P1 — Navigate the database

### Schema & catalog navigation — Schema tree, object details

- [x] `schema` — schema tree in the Connections dock (M4)
- [x] `object_ddl` — canonical object DDL replaces the immediate catalog-derived preview (M4)
- [ ] `providers` — engine/provider hints for rendering (M4)
- [x] `search_schema` — filtered schema search in the Connections dock (M4)
- [x] `search_data` — bounded table-data search modal with per-row field previews (M4)
- [x] `catalog_graph` — dependency/usages section in the object inspector (M4/M5)

### Query history & saved queries — History tab, saved queries

- [x] `history_page` — paginated History result tab with query status, timing, row count, and rerun (M4)
- [x] `saved_queries`, `saved_query`, `create_saved_query`, `update_saved_query`, `delete_saved_query` — saved queries panel (M4)

### Capability gating — command availability

- [x] `available_operations` — server capability-aware command enable/disable with provider/policy reasons (M4)

### Editor depth (polish of the built editor)

- [x] find/replace bar with case sensitivity, match navigation, and highlights
- [x] gutter with line numbers, active-line emphasis, diagnostic markers, and scrolling
- [x] `explain` — estimated and analyzed plans with normalized tree + raw copy (M3/M4)

---

## P2 — SQL intelligence (editor becomes real)

- [x] `open_semantic_document`, `update_semantic_document`, `close_semantic_document` — editor↔semantic doc lifecycle (M3)
- [x] `complete`, `complete_semantic_document` — completion popup (M3)
- [x] `semantic_diagnostics`, `semantic_diagnostics_with_catalog` — inline diagnostics (M3)
- [x] `format_semantic_document` — Format action (M3)
- [x] `prepare_semantic_quick_fix` — quick-fix menu (M3)
- [x] `find_semantic_usages` — usages panel (M3/M4)
- [ ] `prepare_semantic_refactor` — rename/refactor (M4)
- [ ] `select_semantic_statement` — server-side statement targeting (currently client-side) (M3)
- [ ] `capture_semantic_plan` — plan capture from the editor (M5)

---

## P3 — Write & transact

### Data editing — editable grid

- [x] `preview_edits` — multi-cell staged-edit preview with conflict display (M4)
- [x] `apply_edits` — apply typed edits with staged-edit revert controls (M4)
- [ ] `bulk_insert` — bulk insert flow (M5)
- [ ] `import_csv` — CSV import (M5)

### Transactions & process control — transaction bar, process list

- [x] `begin_transaction`, `commit_transaction`, `rollback_transaction` — transaction controls (M4)
- [x] `execute_in_tx` — run inside an open transaction (M4)
- [ ] `list_transactions`, `preview_transaction` — transaction state panel (M4)
- [ ] `create_savepoint`, `rollback_to_savepoint`, `release_savepoint` — savepoint controls (M4)
- [ ] `list_processes`, `kill_process` — process/activity monitor (M4)

### Results depth (polish of the built grid)

- [x] keyboard cell navigation; copy cell / row / column / range / all, with headers
- [x] column resize / reorder; row numbers

---

## P4 — Collaborate & version

### Rooms & collaboration — presence, shared docs

- [~] `rooms`, presence stream — read into nav; Inspector shows participants (M2)
- [~] Room WS (`attach`, `attach_with_presence`, `sync_document`, `submit_update`, `update_presence`, `pump`, `heartbeat`) — live Loro editor sync, durable ACKs, reconnect, and presence are wired; presence cursors and one shared room transport remain (M3)
- [~] `documents`, `create_document`, `update_document_snapshot`, `delete_document` — list/create/open are wired; rename/reorder/delete UI remains (M3/M5)
- [ ] `create_room`, `delete_room`, `bind_room_connection`, `unbind_room_connection` — room management (M5)
- [ ] `room_members`, `add_room_member`, `remove_room_member`, `join_room`, `leave_room` — membership (M5)
- [ ] `room_results`, `room_result`, `room_result_pages` — shared result references (M5)

### Virtual workspaces — workspace tree, checkpoints

- [~] `room_workspaces`, `workspace` — restored/opened workspace (read-only nav); rows now open on click (M2)
- [ ] `create_workspace`, `update_workspace`, `delete_workspace` — workspace management (M5)
- [ ] `workspace_nodes`, `create_workspace_node`, `move_workspace_node`, `delete_workspace_node`, `mutate_workspace_batch` — workspace file tree (M5)
- [ ] `workspace_checkpoints`, `create_workspace_checkpoint`, `restore_workspace_checkpoint` — checkpoint history (M5)
- [ ] `workspace_projection`, `bind_workspace_projection`, `delete_workspace_projection`, `plan_workspace_projection`, `apply_workspace_projection` — projection reconciliation (M5)

### Git / VCS — source control panel

- [ ] `workspace_repository`, `bind_workspace_repository`, `delete_workspace_repository` — repo binding (M5)
- [ ] `repository_status`, `repository_diff`, `repository_branches` — status/diff/branches (M5)
- [ ] `stage_repository_paths`, `unstage_repository_paths`, `commit_repository` — stage/commit (M5)
- [x] `set_repository_credential`, `delete_repository_credential`,
      `test_repository_credential`, remote list/add/edit/remove,
      `fetch_repository`, `push_repository` — explicit remotes + credentials (G7)

---

## P5 — Advanced surfaces

### Catalog compare & migrations

- [ ] `compare_catalog_schemas`, `start_comparison`, `comparison`, `comparison_page`, `cancel_comparison`, `prepare_comparison_patch` — schema comparison (M5)
- [ ] `create_catalog_snapshot`, `catalog_snapshots`, `catalog_snapshot`, `delete_catalog_snapshot` — snapshot manager (M5)
- [ ] `preview_migration`, `apply_migration`, `migration_run`, `durable_migration_run`, `cancel_migration` — migration plan/apply + safety findings (M5)
- [x] `catalog_diagram`, `preview_catalog_diagram_mutation` — bounded relationship
      diagram with table-designer mutation previews and baseline-comparison handoff (M5)

### Plan captures

- [x] `plan_capture`, `plan_captures`, `compare_plan_captures`,
  `delete_plan_capture` — parameter-aware capture, paginated fingerprint-filtered
  listing, two-capture comparison, and deletion are wired (M5)

### Runs & scheduling — Runs dock

- [~] run/schedule feature flags — surfaced as nav tags only (M2)
- [ ] `run_configurations` (+create/get/update/delete/validate) — run config CRUD (M5)
- [ ] `start_run`, `run`, `run_steps`, `run_logs`, `cancel_run`, `rerun` — live run + logs (M5)
- [ ] `run_schedules` (+create/get/update/delete/enable/disable), `schedule_occurrences`, `resume_schedule_occurrence` — schedules (M5)

### Transfer recipes / import-export

- [ ] `transfer_recipes` (+create/get/update/delete/validate), `execute_transfer_recipe` — transfer recipes with progress/cancel (M5)
- [ ] `workspace_artifact`, `stream_workspace_artifact` — artifact download/preview (M5)
- [ ] `export_query`, `stream_export_query` — export result to file (M5)

### DDL sources

- [ ] `ddl_sources` (+create/get/update/delete/refresh) — DDL source panel (M5)

### Extensions (Phase I)

- [ ] `extensions`, `extension`, `extension_diagnostics` — extension list/detail (M5)
- [ ] `validate_extension`, `install_extension`, `uninstall_extension`, `purge_extension` — install lifecycle (M5)
- [ ] `select_extension`, `grant_extension`, `allow_extension_tenant`, `rollback_extension` — enable/grant/rollback (M5)
- [ ] `invoke_extension` — render declarative commands/forms/tables/panels (M5)

### Notifications

- [ ] `listen_notifications` — toast/inbox for server-pushed notifications (M4/M5)

---

## P6 — Admin & platform

### Connection profiles & credentials — connection editor

- [x] Personal/team server vaults in Collaboration, including vault-backed
  connections, capability grants, masked version history, rotation, and
  controlled generic-secret reveal —
  [design](collaborative-connection-vaults.md)
- [~] `upsert_connection_profile`, `delete_connection_profile` — create and
  Save & Connect are available in the Connections dock; edit/delete remain for
  the full profile editor (M4)
- [ ] `set_connection_credential` — credential entry (M4)
- [ ] `connection_policy`, `update_connection_policy` — policy editor (M5)

### Admin & tenant management — admin console

- [ ] `admin_create_principal`, `admin_set_principal_disabled`, `admin_principal_identities`, `admin_link_password_identity`, `admin_unlink_identity` — principal admin (M5/M6)
- [ ] `admin_auth_sessions`, `admin_revoke_auth_session`, `admin_issue_password_reset` — session admin (M6)
- [ ] `create_tenant_invitation`, `tenant_invitations`, `revoke_tenant_invitation`, `accept_tenant_invitation` — tenant invites (M5)
- [ ] `tenant_usage`, `set_tenant_limits`, `clear_tenant_limits` — usage/limits panel (M5)
- [ ] `github_allowlist`, `create_github_allowlist_entry`, `revoke_github_allowlist_entry` — GitHub allowlist (M5)

### Keys, tokens, account — account settings

- [ ] `change_password`, `reset_password` — account settings (M6)
- [ ] `principal_keys`, `register_principal_key`, `revoke_principal_key`, `issue_key_challenge`, `authenticate_key` — key management + key auth (M6)
- [ ] `auth_tokens`, `issue_token`, `revoke_token` — API token management (M6)

### Operations, audit, approvals, governed tools

- [ ] `operations`, `audit` — operations catalog + audit log (M5)
- [ ] `operation_audit`, `operation_audit_page` — per-operation audit detail (M5)
- [ ] `create_operation_approval`, `approve_operation` — approval flow for destructive ops (M4/M5)
- [ ] `governed_tools`, `invoke_tool` — governed tool invocation (M5)
- [ ] `openapi` — dev/help surface (optional)

---

## Transport plumbing (no dedicated UI)

Not user-facing — reconnect/websocket machinery behind the above: `connect`,
`connect_session_websocket`, `connect_room_websocket`,
`reauthenticate_session_websocket`, `reauthenticate_room_websocket`, WebSocket
`send`/`next`/`heartbeat`/`reauthenticate`, `submit`, notification transport.

---

## Rollup

- **P0 is implemented:** health/readiness, hosted and SSH sign-in, profile and
  one-shot connections, tenant sessions, session/connection teardown,
  streaming execution, paging, cancellation, and typed parameters all have
  working desktop paths.
- **Partial (`[~]`) items now begin at P4:** collaboration documents,
  presence, and read-only virtual-workspace navigation still have follow-up UI
  work described in their sections.
- **Next unchecked API/UI work:** P1 provider rendering refinements and P2
  semantic refactor/statement/plan-capture depth; priority remains governed by
  the current product backlog rather than the historical milestone labels.
