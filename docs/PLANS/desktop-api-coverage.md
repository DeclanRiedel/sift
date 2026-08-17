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

- [~] `health`, `ready` — connection/handshake indicator (M2)
- [x] `open_session`, `open_connection_from_profile` — **connection picker built**: Connections dock rows connect on click; executor opens session + connection for the chosen tenant/profile (M3)
- [x] `connection_profiles` — profiles listed with a live status dot (disconnected / connecting / connected / failed) and connect/disconnect (M3)
- [x] `close_session` — used by the Disconnect action and on reconnect (M3)
- [ ] `open_session_for_tenant` — tenant-scoped session when multi-tenant (M2)
- [ ] `list_sessions` — session list panel (M4)
- [ ] `open_connection` (explicit spec) — ad-hoc connection dialog (M4)
- [ ] `ping_connection` — live connection health chip (M4)
- [ ] `close_connection`, `disconnect_connection_profile` — finer-grained disconnect (M4)
- [~] **footer/status bar** — connection target and execution outcome are live;
  transaction remains a placeholder. Exclusive, client-local left-panel and
  bottom-tool selection is landed; semantic search/diagnostics remain M3 work.

### Sign-in enough to connect — sign-in modal, account state

- [~] `whoami` — read by executor; surface the signed-in identity (M2)
- [ ] `password_login`, `refresh_session` — sign-in modal + refresh (M2)
- [ ] `logout`, `logout_all` — account menu (M2)
- [ ] `github_authorization_url`, `github_native_start`, `github_native_exchange`, `github_callback` — GitHub sign-in (M2)
- [ ] `exchange_ssh_proxy_capability` — SSH remote connect (M2/H)

### Execute — finish the run path

- [x] `execute` — Run Statement/Document → virtualized grid (M3)
- [ ] `stream_query` — streamed/paged cursor + live status (replaces bounded HTTP first page) (M3)
- [ ] `cancel` — cancel a running query (M3)
- [ ] `read_spilled_pages`, `delete_spilled_cursor` — paging/resume for large results (M3/M4)
- [ ] `execute_with_params` — parameterized run / bind-params UI (M3)

---

## P1 — Navigate the database

### Schema & catalog navigation — Schema tree, object details

- [ ] `schema` — lazy schema tree in the left dock (M4)
- [ ] `object_ddl` — object DDL view (M4)
- [ ] `providers` — engine/provider hints for rendering (M4)
- [ ] `search_schema` — schema search (M4)
- [ ] `search_data` — data search (M4)
- [ ] `catalog_graph` — dependency graph (M4/M5)

### Query history & saved queries — History tab, saved queries

- [ ] `history`, `history_page` — populate the History result tab (currently a placeholder) (M4)
- [ ] `saved_queries`, `saved_query`, `create_saved_query`, `update_saved_query`, `delete_saved_query` — saved queries panel (M4)

### Capability gating — command availability

- [ ] `available_operations` — capability-aware command enable/disable + disabled reasons across the UI (M4)

### Editor depth (polish of the built editor)

- [ ] find bar (model `find_matches` exists, no UI)
- [ ] gutter + line numbers, current-line highlight, vertical scroll, empty placeholder
- [ ] `explain` — Explain tab content (M3/M4)

---

## P2 — SQL intelligence (editor becomes real)

- [ ] `open_semantic_document`, `update_semantic_document`, `close_semantic_document` — editor↔semantic doc lifecycle (M3)
- [ ] `complete`, `complete_semantic_document` — completion popup (M3)
- [ ] `semantic_diagnostics`, `semantic_diagnostics_with_catalog` — inline diagnostics (M3)
- [ ] `format_semantic_document` — Format action (M3)
- [ ] `prepare_semantic_quick_fix` — quick-fix menu (M3)
- [ ] `find_semantic_usages` — usages panel (M3/M4)
- [ ] `prepare_semantic_refactor` — rename/refactor (M4)
- [ ] `select_semantic_statement` — server-side statement targeting (currently client-side) (M3)
- [ ] `capture_semantic_plan` — plan capture from the editor (M5)

---

## P3 — Write & transact

### Data editing — editable grid

- [ ] `preview_edits` — staged-edit preview with conflict display (M4)
- [ ] `apply_edits` — apply typed edits (M4)
- [ ] `bulk_insert` — bulk insert flow (M5)
- [ ] `import_csv` — CSV import (M5)

### Transactions & process control — transaction bar, process list

- [ ] `begin_transaction`, `commit_transaction`, `rollback_transaction` — transaction controls (M4)
- [ ] `execute_in_tx` — run inside an open transaction (M4)
- [ ] `list_transactions`, `preview_transaction` — transaction state panel (M4)
- [ ] `create_savepoint`, `rollback_to_savepoint`, `release_savepoint` — savepoint controls (M4)
- [ ] `list_processes`, `kill_process` — process/activity monitor (M4)

### Results depth (polish of the built grid)

- [ ] keyboard cell navigation; copy row / range / with-headers
- [ ] column resize / reorder (fixed 184px today); row numbers

---

## P4 — Collaborate & version

### Rooms & collaboration — presence, shared docs

- [~] `rooms`, presence stream — read into nav; Inspector shows participants (M2)
- [ ] Room WS (`attach`, `attach_with_presence`, `sync_document`, `submit_update`, `update_presence`, `pump`, `heartbeat`) — live Loro collab for the editor + presence cursors (M3)
- [ ] `documents`, `create_document`, `update_document_snapshot`, `delete_document` — document management (M3/M5)
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
- [ ] `set_repository_credential`, `fetch_repository`, `push_repository` — fetch/push + creds (M5)

---

## P5 — Advanced surfaces

### Catalog compare & migrations

- [ ] `compare_catalog_schemas`, `start_comparison`, `comparison`, `comparison_page`, `cancel_comparison`, `prepare_comparison_patch` — schema comparison (M5)
- [ ] `create_catalog_snapshot`, `catalog_snapshots`, `catalog_snapshot`, `delete_catalog_snapshot` — snapshot manager (M5)
- [ ] `preview_migration`, `apply_migration`, `migration_run`, `durable_migration_run`, `cancel_migration` — migration plan/apply + safety findings (M5)
- [ ] `catalog_diagram`, `preview_catalog_diagram_mutation` — ER diagram surface (M5)

### Plan captures

- [ ] `plan_capture`, `plan_captures`, `compare_plan_captures`, `delete_plan_capture` — plan-capture panel (M5)

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

- **Built (`[x]`):** `execute` → results grid. Plus non-SDK UI: editor, panes /
  docks / toolbar, command palette (filtering + dispatch), clickable workspace
  rows.
- **Plumbed (`[~]`), no dedicated UI:** `health`/`ready`, `open_session`,
  `open_connection_from_profile`, `whoami`, `connection_profiles`, `rooms`,
  `room_workspaces`/`workspace`, presence stream.
- **Next up (P0):** a real **connection picker** (choose profile → connect →
  connected state → disconnect), sign-in-to-connect, and streaming execute +
  cancel — then the status bar stops faking database/transaction/execution.
