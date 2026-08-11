# Desktop API → UI Coverage

Every public `sift-client-sdk` `Client` capability mapped to the desktop UI
surface that should expose it, with current status. This is the "what's left"
map for the Phase M client.

Legend: `[x]` wired to a working UI · `[~]` plumbing/model exists, UI incomplete
· `[ ]` not started. Milestone column is the Phase M milestone that owns it.

Reality check: today only query execution, the results surface, the editor, and
the read-only lifecycle navigation are user-facing. Transport plumbing (session
open, connection-from-profile, whoami) runs headless inside the executor with no
UI of its own. Everything else is unbuilt.

---

## 1. Connection & session lifecycle — status bar, Connections dock, connection picker

- [~] `health`, `ready` — drives the connection indicator + handshake (M2)
- [~] `open_session`, `open_connection_from_profile` — opened headless by the query executor; no session/connection UI yet (M3)
- [ ] `open_session_for_tenant` — tenant-scoped session picker (M2/M4)
- [ ] `list_sessions`, `close_session` — session manager UI (M4)
- [ ] `open_connection` (explicit spec) — ad-hoc connection dialog (M4)
- [ ] `ping_connection` — connection health chip in the dock (M4)
- [ ] `close_connection` — disconnect action in the dock (M4)

## 2. Auth & identity — sign-in flow, account menu, credentials

- [~] `whoami` — read by the executor; not shown (git/account UI removed until wired) (M2)
- [ ] `password_login`, `refresh_session` — sign-in modal + session refresh (M2)
- [ ] `logout`, `logout_all` — account menu (M2)
- [ ] `change_password`, `reset_password` — account settings (M6)
- [ ] `github_authorization_url`, `github_native_start`, `github_native_exchange`, `github_callback` — GitHub sign-in flow (M2)
- [ ] `github_allowlist`, `create_github_allowlist_entry`, `revoke_github_allowlist_entry` — admin allowlist screen (M5)
- [ ] `principal_keys`, `register_principal_key`, `revoke_principal_key`, `issue_key_challenge`, `authenticate_key` — key management + key auth (M6)
- [ ] `exchange_ssh_proxy_capability` — SSH remote-connect flow (M2/H)

## 3. Admin & tenant management — admin console

- [ ] `admin_create_principal`, `admin_set_principal_disabled`, `admin_principal_identities`, `admin_link_password_identity`, `admin_unlink_identity` — principal admin (M5/M6)
- [ ] `admin_auth_sessions`, `admin_revoke_auth_session`, `admin_issue_password_reset` — session admin (M6)
- [ ] `create_tenant_invitation`, `tenant_invitations`, `revoke_tenant_invitation`, `accept_tenant_invitation` — tenant invites (M5)
- [ ] `tenant_usage`, `set_tenant_limits`, `clear_tenant_limits` — tenant usage/limits panel (M5)

## 4. Query execution & results — editor + results grid ✅ (core built)

- [x] `execute` — Run Statement/Document → virtualized results grid (M3)
- [ ] `execute_with_params` — parameterized run (bind-params UI) (M3)
- [ ] `execute_in_tx` — run inside an open transaction (M4)
- [ ] `stream_query` — streamed/paged cursor with live status (replaces bounded HTTP first page) (M3)
- [ ] `cancel` — cancel a running query (M3)
- [ ] `read_spilled_pages`, `delete_spilled_cursor` — paging/resume for large results (M3/M4)
- [ ] `explain` — Explain tab content (M3/M4)
- [ ] `export_query`, `stream_export_query` — export result to file (M5)

## 5. SQL intelligence (semantic) — editor completions, diagnostics, actions

- [ ] `open_semantic_document`, `update_semantic_document`, `close_semantic_document` — editor↔semantic doc lifecycle (M3)
- [ ] `complete`, `complete_semantic_document` — completion popup (M3)
- [ ] `semantic_diagnostics`, `semantic_diagnostics_with_catalog` — inline diagnostics/underlines (M3)
- [ ] `format_semantic_document` — Format action (M3)
- [ ] `prepare_semantic_quick_fix` — quick-fix menu (M3)
- [ ] `find_semantic_usages` — usages panel (M3/M4)
- [ ] `prepare_semantic_refactor` — rename/refactor (M4)
- [ ] `select_semantic_statement` — statement-under-caret targeting via server (currently client-side) (M3)
- [ ] `capture_semantic_plan` — plan capture from the editor (M5)

## 6. Schema & catalog navigation — Schema tree, object details, diagrams

- [ ] `schema` — lazy schema tree in the left dock (M4)
- [ ] `object_ddl` — object DDL view (M4)
- [ ] `catalog_graph` — dependency graph (M4/M5)
- [ ] `catalog_diagram`, `preview_catalog_diagram_mutation` — ER diagram surface (M5)
- [ ] `search_schema` — schema search (M4)
- [ ] `search_data` — data search (M4)
- [ ] `providers` — engine/provider hints for rendering (M4)

## 7. Data editing — editable grid, edit staging

- [ ] `preview_edits` — staged-edit preview with conflict display (M4)
- [ ] `apply_edits` — apply typed edits (M4)
- [ ] `bulk_insert` — bulk insert flow (M5)
- [ ] `import_csv` — CSV import (M5)

## 8. Transactions & process control — transaction bar, process list

- [ ] `begin_transaction`, `commit_transaction`, `rollback_transaction` — transaction controls (M4)
- [ ] `list_transactions`, `preview_transaction` — transaction state panel (M4)
- [ ] `create_savepoint`, `rollback_to_savepoint`, `release_savepoint` — savepoint controls (M4)
- [ ] `list_processes`, `kill_process` — process/activity monitor (M4)

## 9. Catalog compare & migrations — diff/migration surfaces

- [ ] `compare_catalog_schemas`, `start_comparison`, `comparison`, `comparison_page`, `cancel_comparison`, `prepare_comparison_patch` — schema comparison UI (M5)
- [ ] `create_catalog_snapshot`, `catalog_snapshots`, `catalog_snapshot`, `delete_catalog_snapshot` — snapshot manager (M5)
- [ ] `preview_migration`, `apply_migration`, `migration_run`, `durable_migration_run`, `cancel_migration` — migration plan/apply with safety findings (M5)

## 10. Plan captures — plan history & comparison

- [ ] `plan_capture`, `plan_captures`, `compare_plan_captures`, `delete_plan_capture` — plan-capture panel (M5)

## 11. Rooms & collaboration — presence, shared docs, room results

- [~] `rooms` — read into the Connections/Workspaces nav (read-only) (M2)
- [ ] `create_room`, `delete_room` — room management (M5)
- [ ] `bind_room_connection`, `unbind_room_connection` — room connection binding (M5)
- [ ] `room_members`, `add_room_member`, `remove_room_member` — membership UI (M5)
- [ ] `join_room`, `leave_room` — join/leave controls (M2/M5)
- [ ] `room_results`, `room_result`, `room_result_pages` — shared result references (M5)
- [ ] `documents`, `create_document`, `update_document_snapshot`, `delete_document` — document management (M3/M5)
- [ ] Room WS (`attach`, `attach_with_presence`, `sync_document`, `submit_update`, `update_presence`, `pump`, `heartbeat`) — live Loro collab for the editor + presence cursors (M3)
- [~] presence stream (via `stream_room_presence` helper) — Inspector shows participant count/follow (M2)

## 12. Virtual workspaces — workspace tree, checkpoints

- [~] `room_workspaces`, `workspace` — restored/opened workspace (read-only nav) (M2)
- [ ] `create_workspace`, `update_workspace`, `delete_workspace` — workspace management (M5)
- [ ] `workspace_nodes`, `create_workspace_node`, `move_workspace_node`, `delete_workspace_node`, `mutate_workspace_batch` — workspace file tree (M5)
- [ ] `workspace_checkpoints`, `create_workspace_checkpoint`, `restore_workspace_checkpoint` — checkpoint history (M5)

## 13. Workspace projections — projection reconciliation

- [ ] `workspace_projection`, `bind_workspace_projection`, `delete_workspace_projection`, `plan_workspace_projection`, `apply_workspace_projection` — projection UI (M5)

## 14. Git / VCS — source control panel

- [ ] `workspace_repository`, `bind_workspace_repository`, `delete_workspace_repository` — repo binding (M5)
- [ ] `repository_status`, `repository_diff`, `repository_branches` — status/diff/branches (M5)
- [ ] `stage_repository_paths`, `unstage_repository_paths`, `commit_repository` — stage/commit (M5)
- [ ] `set_repository_credential`, `fetch_repository`, `push_repository` — fetch/push + creds (M5)

## 15. Runs & scheduling — Runs dock, run detail, schedules

- [~] run/schedule feature flags — surfaced as nav tags only (M2)
- [ ] `run_configurations`, `create_run_configuration`, `run_configuration`, `update_run_configuration`, `delete_run_configuration`, `validate_run_configuration` — run config CRUD (M5)
- [ ] `start_run`, `run`, `run_steps`, `run_logs`, `cancel_run`, `rerun` — live run + logs (M5)
- [ ] `run_schedules`, `create_run_schedule`, `run_schedule`, `update_run_schedule`, `delete_run_schedule`, `enable_run_schedule`, `disable_run_schedule` — schedule CRUD (M5)
- [ ] `schedule_occurrences`, `resume_schedule_occurrence` — occurrence history (M5)

## 16. Transfer recipes / import-export — transfer UI, artifacts

- [ ] `transfer_recipes`, `create_transfer_recipe`, `transfer_recipe`, `update_transfer_recipe`, `delete_transfer_recipe`, `validate_transfer_recipe` — recipe CRUD (M5)
- [ ] `execute_transfer_recipe` — run transfer with progress/cancel (M5)
- [ ] `workspace_artifact`, `stream_workspace_artifact` — artifact download/preview (M5)

## 17. DDL sources — DDL source manager

- [ ] `ddl_sources`, `create_ddl_source`, `ddl_source`, `update_ddl_source`, `delete_ddl_source`, `refresh_ddl_source` — DDL source panel (M5)

## 18. Connection profiles & credentials — connection editor

- [~] `connection_profiles` — profile names read into the Connections dock; first profile used by the executor (M2/M3)
- [ ] `upsert_connection_profile`, `delete_connection_profile` — profile editor (M4)
- [ ] `set_connection_credential` — credential entry (M4)
- [ ] `connection_policy`, `update_connection_policy` — policy editor (M5)
- [ ] `disconnect_connection_profile` — force-disconnect (M4)

## 19. Query history & saved queries — History tab, saved queries

- [ ] `history`, `history_page` — History result tab (currently a placeholder) (M4)
- [ ] `saved_queries`, `saved_query`, `create_saved_query`, `update_saved_query`, `delete_saved_query` — saved queries panel (M4)

## 20. Operations, audit, approvals, capabilities — command availability, audit log, approvals

- [ ] `available_operations` — capability-aware command enable/disable + disabled reasons (M4)
- [ ] `operations`, `audit` — operations catalog + audit log (M5)
- [ ] `operation_audit`, `operation_audit_page` — per-operation audit detail (M5)
- [ ] `create_operation_approval`, `approve_operation` — approval flow for destructive ops (M4/M5)
- [ ] `governed_tools`, `invoke_tool` — governed tool invocation (M5)
- [ ] `auth_tokens`, `issue_token`, `revoke_token` — API token management (M6)
- [ ] `openapi` — dev/help surface (optional)

## 21. Extensions (Phase I) — declarative contributions

- [ ] `extensions`, `extension`, `extension_diagnostics` — extension list/detail (M5)
- [ ] `validate_extension`, `install_extension`, `uninstall_extension`, `purge_extension` — install lifecycle (M5)
- [ ] `select_extension`, `grant_extension`, `allow_extension_tenant`, `rollback_extension` — enable/grant/rollback (M5)
- [ ] `invoke_extension` — declarative command/form/table/panel rendering (M5)
- [ ] `providers` — provider registry for extension/render dispatch (M4/M5)

## 22. Notifications

- [ ] `listen_notifications` — toast/inbox for server-pushed notifications (M4/M5)

## 23. Transport plumbing (no dedicated UI)

Not user-facing surfaces — the SDK/reconnect machinery behind the above:
`connect`, `connect_session_websocket`, `connect_room_websocket`,
`reauthenticate_session_websocket`, `reauthenticate_room_websocket`,
WebSocket `send`/`next`/`heartbeat`/`reauthenticate`, `submit`,
`listen_notifications` transport. Exercised indirectly by the features above.

---

## Rollup

- **Built (`[x]`):** query `execute` → results grid. (Plus non-SDK UI: editor,
  panes/docks/toolbar, command palette.)
- **Plumbed (`[~]`), no dedicated UI:** `health`/`ready`, `open_session`,
  `open_connection_from_profile`, `whoami`, `connection_profiles`, `rooms`,
  `room_workspaces`/`workspace`, presence stream.
- **Not started (`[ ]`):** everything else — the bulk of M3 (streaming/cancel,
  semantic intelligence, live collab), all of M4 (schema nav, data editing,
  transactions, saved queries/history, capability gating), and all of M5
  (diagrams, migrations, git, runs, transfers, extensions, admin).
