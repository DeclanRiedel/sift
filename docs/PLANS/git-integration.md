# Git Integration Build Checklist

Status: active implementation plan  
Owner: desktop + workspace + server  
Created: 2026-08-28  
Zed reference reviewed: `zed-industries/zed@01acd0ee8e906dd0ec8b526fe08da94444a5e2af`

This file is the source-of-truth checklist for Sift's Git integration. Update it
in the same commit as each implementation change. Mark an item complete only
when its user-visible behavior, focused tests, and relevant workspace gates are
complete. Add the implementing commit after the item when useful.

`docs/PLANS/desktop-api-coverage.md` continues to track raw API coverage. This
plan tracks the complete product workflow.

## Product goal

Git makes database work reviewable, recoverable, collaborative, and portable.
A Sift workspace remains the server-owned source of product identity; Git is a
versioned projection of that workspace, not an ambient client-side repository.

The normal loop should become:

1. Open or create a workspace SQL file.
2. Edit collaboratively.
3. Review working changes and database impact.
4. Stage files or hunks.
5. Create a checkpoint-bound commit.
6. Validate or run the committed database work.
7. Fetch/push through explicit server policy when network Git is enabled.

## Zed patterns to adopt

Reviewed source:

- [`crates/git`](https://github.com/zed-industries/zed/tree/01acd0ee8e906dd0ec8b526fe08da94444a5e2af/crates/git)
  separates typed Git state and operations from presentation.
- [`crates/git_ui`](https://github.com/zed-industries/zed/tree/01acd0ee8e906dd0ec8b526fe08da94444a5e2af/crates/git_ui)
  owns panel, diff, branch, history, conflict, commit, and remote UI.
- [`git_panel.rs`](https://github.com/zed-industries/zed/blob/01acd0ee8e906dd0ec8b526fe08da94444a5e2af/crates/git_ui/src/git_panel.rs)
  derives a flattened visible model, supports grouping/sorting/tree modes, and
  virtualizes long status/history lists.
- [`project_diff.rs`](https://github.com/zed-industries/zed/blob/01acd0ee8e906dd0ec8b526fe08da94444a5e2af/crates/git_ui/src/project_diff.rs)
  presents a project-wide diff with next/previous hunk and staging actions.
- [`commit_modal.rs`](https://github.com/zed-industries/zed/blob/01acd0ee8e906dd0ec8b526fe08da94444a5e2af/crates/git_ui/src/commit_modal.rs)
  reuses commit state between compact and expanded editors.
- [`conflict_view.rs`](https://github.com/zed-industries/zed/blob/01acd0ee8e906dd0ec8b526fe08da94444a5e2af/crates/git_ui/src/conflict_view.rs)
  embeds ours/theirs/both resolution controls in the editor.
- [`branch_picker.rs`](https://github.com/zed-industries/zed/blob/01acd0ee8e906dd0ec8b526fe08da94444a5e2af/crates/git_ui/src/branch_picker.rs)
  uses a searchable picker instead of a large permanent branch tree.
- [`git_panel_settings.rs`](https://github.com/zed-industries/zed/blob/01acd0ee8e906dd0ec8b526fe08da94444a5e2af/crates/git_ui/src/git_panel_settings.rs)
  keeps panel grouping, sorting, tree, stats, badge, and click behavior explicit.

Adopt these principles:

- One typed repository model feeds every Git surface.
- Repository changes are event-driven and coalesced.
- Panel rows are a flattened, cached, virtualized projection.
- Diff review is a first-class editor surface, not a small panel preview.
- File, hunk, and global staging actions share one command language.
- Compact and expanded commit editors share one draft.
- Branches and repositories use searchable pickers.
- Conflict resolution lives next to conflicted text.
- Remote output becomes structured, actionable UI.
- Every action has command-palette and keyboard coverage.

## Sift-specific adaptations

Do not copy Zed's client-filesystem authority. Sift differs in load-bearing
ways:

- Git executes beside the server-owned projection, including remote servers.
- Public requests carry workspace ids, binding ids, revisions, and normalized
  relative paths; never arbitrary absolute paths.
- SQL text remains owned by the room's Loro document. Git, projection state,
  results, schema, and sessions are not CRDTs.
- Repository index and branch are shared room state in v1. Mutations are
  serialized, revision-guarded, broadcast, and audited.
- Credentials come from `SecretStore` for one operation. No ambient credential
  helper, URL secret, inherited secret environment, SQLite secret, or log data.
- Hooks, shell commands, external diff, pagers, fsmonitor, unsafe protocols,
  and interactive prompts remain disabled.
- Sift checkpoints replace most stash/recovery use cases. Git stash is deferred
  until a clear product need remains.
- No force push or automatic merge/rebase in v1.
- Git data must link to database context without committing result rows or
  secret values.
- Git records reviewed intent; the Database Change Ledger records who actually
  executed a mutation, where, when, and with what outcome.
- Manual cell values, before/after rows, and query results never enter Git
  automatically. An explicit generated DML artifact is reviewed as data-bearing
  content before it can be committed.

## Current foundation

- [x] Server-owned virtual workspaces and revisioned trees.
- [x] Optional confined filesystem projections.
- [x] Typed repository binding and capability discovery.
- [x] Hardened bundled system-Git adapter behind `VcsRepository`.
- [x] Bounded typed status, diff-stat, and branch parsing.
- [x] Root-confined whole-path stage and unstage.
- [x] Checkpoint-bound local commit API.
- [x] One-operation credential channel for fetch and push.
- [x] Network Git operator gate.
- [x] SDK and OpenAPI coverage for current VCS endpoints.
- [x] Demo repository initialization on `main`.
- [x] Demo workspace advertises `git: true`.
- [x] Desktop auto-selects the sole Git-enabled workspace.
- [x] Basic desktop status rows, manual refresh, and whole-file stage toggle.
- [x] Git adapter confinement, malformed-output, credential-redaction, and
      checkpoint/concurrency coverage.

## Milestone G1 — Daily local Git loop

### Repository model and refresh

- [x] Add a desktop repository projection separate from `WorkspaceShell` view
      rendering.
- [x] Carry binding, branch, HEAD, upstream, status, loading, operation, and
      error state in that projection.
- [x] Subscribe every Git surface to the same projection.
- [x] Coalesce status requests and discard stale revision results.
- [ ] Refresh after editor save, projection reconciliation, stage, unstage,
      commit, branch, and remote operations.
- [x] Add bounded periodic fallback refresh only while a Git surface is open.
- [x] Add server-side projection/repository change events for immediate refresh.
- [x] Prevent refresh loops after entities or workspaces are dropped.
- [x] Handle detached HEAD as a stable state, not an endless loading state.
- [x] Distinguish missing repo, unavailable root, untrusted ownership, disabled
      Git, stale binding, and command failure.

### Git panel structure

- [x] Split panel into header, change list, commit area, and operation footer.
- [x] Show repository/workspace name and current branch in header.
- [x] Show ahead/behind counts and detached-HEAD state.
- [x] Group by staging state: conflicts, staged, unstaged, untracked.
- [x] Support alternate grouping by file state.
- [x] Support flat and folder-tree views.
- [x] Sort by path or filename.
- [x] Show file-state icon and staged/partial/conflict control.
- [x] Show additions/deletions when diff stats are available.
- [x] Show changed-file count badge on Git tab.
- [x] Persist grouping, sorting, tree mode, and primary click behavior.
- [x] Flatten and cache visible rows outside render.
- [x] Render long file lists with `uniform_list`.
- [x] Preserve selection by stable path across refreshes.
- [x] Add clear empty, loading, truncated, and error states.
- [x] Add refresh and overflow icon buttons with tooltips.

### File-level actions

- [x] Open changed file from primary click.
- [x] Open file diff from primary click or explicit action.
- [x] Context menu: open file, open diff, stage, unstage, discard, copy path.
- [x] Stage one path.
- [x] Unstage one path.
- [x] Stage all tracked changes.
- [x] Stage all including untracked files with explicit labeling.
- [x] Unstage all.
- [x] Discard one worktree path with confirmation and checkpoint.
- [x] Restore one deleted path with confirmation and checkpoint.
- [x] Disable invalid actions for conflicts and in-flight operations.
- [x] Display optimistic pending state without pretending success.

## Milestone G2 — Project diff and hunk workflow

- [x] Add a reusable diff model supporting HEAD→index, index→worktree, and
      HEAD→worktree.
- [x] Extend API from diff statistics to bounded textual hunks.
- [x] Carry stable file/hunk identities and truncation metadata.
- [x] Add project-wide diff tab covering all changed files.
- [x] Add single-file diff tab.
- [x] Support unified diff first; side-by-side after unified workflow is solid.
- [x] Syntax-highlight SQL, TOML, JSON, Markdown, and plain text diffs.
- [x] Render binary, renamed, copied, deleted, and type-changed files safely.
- [x] Navigate previous/next file.
- [x] Navigate previous/next hunk with `[c` and `]c`.
- [x] Stage selected hunk.
- [x] Unstage selected hunk.
- [x] Stage and move to next hunk.
- [x] Unstage and move to next hunk.
- [x] Stage or unstage selected lines where patch boundaries permit.
- [x] Revert selected hunk with confirmation and checkpoint.
- [x] Copy hunk or patch.
- [x] Toggle whitespace visibility.
- [x] Preserve diff position across repository refresh.
- [x] Lazy-load content only for visible/selected diff files.
- [x] Bound large diffs by bytes, lines, files, and render time.

## Milestone G3 — Commit workflow

- [x] Add compact commit editor at panel bottom.
- [x] Add expanded commit editor tab/modal using the same draft entity.
- [x] Persist draft per workspace, never in repository files.
- [x] Show subject-length guidance and configurable limit.
- [x] Validate non-empty message and staged content.
- [x] Show author name/email and configuration action.
- [x] Derive safe default identity from authenticated principal when possible.
- [x] Commit staged changes through existing checkpoint-bound API.
- [x] Show exact checkpoint and resulting commit hash.
- [x] Clear draft only after confirmed success.
- [x] Show recently created commit below editor.
- [x] Add audited "uncommit" as checkpointed soft reset after server contract is
      designed.
- [x] Add amend only after revision/precondition and collaboration semantics are
      designed.
- [x] Add optional sign-off after identity UX exists.
- [x] Keep `--no-verify`; repository hooks remain unsupported.
- [x] Offer initial commit in newly initialized repositories.
- [x] Seed desktop-demo with a clean initial commit once commit UI is usable.

## Milestone G4 — Workspace files and projection reconciliation

- [x] Render the complete virtual workspace file tree in desktop.
- [x] Open workspace `.sql` documents in query tabs.
- [x] Create, rename, move, and delete workspace files/folders.
- [x] Save query tab changes into the canonical room document.
- [x] Materialize the matching document revision into the projection.
- [x] Show editor dirty, virtual-tree dirty, projection dirty, and Git dirty as
      distinct states.
- [x] Save all workspace documents.
- [x] Create named checkpoint.
- [x] Browse checkpoint history.
- [x] Restore checkpoint as a new audited head.
- [x] Plan filesystem reconciliation before applying it.
- [x] Show virtual-only, filesystem-only, identical, and both-changed entries.
- [x] Require explicit conflict resolution when both sides changed.
- [x] Never overwrite collaborative text from a watcher event.
- [x] Refresh Git only after projection revision commits.
- [x] Preserve node identity across file moves and Git renames.
- [x] Handle external checkout changes while desktop is connected.

## Milestone G5 — Branches and history

### Branches

- [ ] Add searchable branch picker.
- [ ] List local and remote branches with upstream/ahead/behind state.
- [ ] Create branch from HEAD.
- [ ] Create branch from selected commit or checkpoint.
- [ ] Switch branch with clean-worktree guard.
- [ ] Offer checkpointed reconciliation when switching with changes.
- [ ] Rename local branch.
- [ ] Delete merged local branch with confirmation.
- [ ] Require stronger confirmation for unmerged branch deletion.
- [ ] Set or change upstream.
- [ ] Preserve detached HEAD and unborn branch states.
- [ ] Broadcast branch/HEAD changes to collaborators.
- [ ] Do not add automatic merge or rebase in this milestone.

### History

- [ ] Add virtualized commit-history tab.
- [ ] Render compact commit graph, refs, author, date, subject, and short hash.
- [ ] Load history incrementally by cursor/page.
- [ ] Load history from detached HEAD when no current branch exists.
- [ ] Search by message, author, ref, and hash.
- [ ] Open commit detail.
- [ ] List commit files and stats.
- [ ] Compare commit to parent.
- [ ] Compare two selected commits.
- [ ] Open historical file read-only.
- [ ] Restore historical file through a new audited workspace mutation.
- [ ] Revert commit with preview and checkpoint.
- [ ] Copy commit hash/message/permalink.

## Milestone G6 — Conflicts and recovery

- [ ] Model merge/rebase/cherry-pick/revert operation state explicitly.
- [ ] List conflicted files before ordinary changes.
- [ ] Add conflict indicator and next/previous conflict navigation.
- [ ] Parse conflict regions without trusting marker-shaped user text blindly.
- [ ] Highlight ours, theirs, and common/base regions.
- [ ] Add use-ours, use-theirs, use-both, and manual-edit controls beside text.
- [ ] Mark file resolved only after all regions are resolved and saved.
- [ ] Continue or abort supported repository operation explicitly.
- [ ] Create checkpoint before conflict resolution begins.
- [ ] Recover operation state after server/client restart.
- [ ] Keep repository usable when one client disconnects mid-operation.
- [ ] Explain unsupported or corrupt repository states without refresh loops.
- [ ] Add repair/rebind workflow for missing or moved projections.

## Milestone G7 — Remotes and credentials

- [ ] Add repository binding/initialization UI.
- [ ] Add clone-to-configured-projection workflow.
- [ ] List/add/edit/remove remotes through typed APIs.
- [ ] Add credential creation, replacement, test, and removal UI.
- [ ] Support PAT/basic credentials through `SecretStore` first.
- [ ] Design SSH-key support without ambient agent leakage.
- [ ] Support per-principal credentials where deployment policy requires them.
- [ ] Fetch selected remote.
- [ ] Show fetched ref changes and actionable remote output.
- [ ] Push current branch.
- [ ] Set upstream on first push.
- [ ] Explain authentication, non-fast-forward, protected-branch, and network
      policy failures distinctly.
- [ ] Add explicit pull strategy only after fetch + branch integration is solid.
- [ ] Preview merge/rebase effects before pull mutates shared worktree.
- [ ] Keep force push disabled.
- [ ] Keep background network operations opt-in and visible.
- [ ] Add provider-neutral repository/commit/file permalinks.

## Milestone G8 — Collaboration semantics

- [ ] Document shared repository/index/branch behavior in product UI.
- [ ] Serialize repository mutations server-side per binding.
- [ ] Show actor and in-flight operation to attached clients.
- [ ] Broadcast status, branch, HEAD, credential-presence, and operation changes.
- [ ] Revision-guard every mutating desktop command.
- [ ] Rebase optimistic UI on authoritative operation results.
- [ ] Enforce room role matrix for view, stage, commit, branch, fetch, and push.
- [ ] Audit every user-visible Git action as an `Operation`.
- [ ] Notify collaborators about commits, branch switches, and remote updates.
- [ ] Prevent edits arriving after checkpoint from entering an in-flight commit.
- [ ] Leave later edits visible as new uncommitted work.
- [ ] Define disconnect/crash ownership transfer for active operations.
- [ ] Evaluate per-user branches/worktrees only after shared workflow graduates.

## Milestone G9 — Database-aware Git

### Database Change Ledger policy

The Change Ledger, not Git author metadata, is authoritative for "who did
what." Git identifies who authored a reviewed artifact. The ledger identifies
the authenticated principal who executed it against a particular database and
whether the operation committed, failed, conflicted, or rolled back.

Default ledger records contain actor, timestamp, tenant/room, connection and
database target, operation kind, affected object, row count, SQL fingerprint,
row-identity fingerprint where safe, transaction/correlation ids, workspace
revision, checkpoint, Git commit, source workflow, and outcome. They do not
contain raw cell values, before/after rows, query results, credentials, or
secret-bearing SQL.

Changes made outside Sift cannot be attributed to a Sift principal. Optional
Postgres/SQL Server native audit or CDC ingestion may report external changes,
but must label their identity source and confidence explicitly.

- [x] Every user-visible server action has a typed, audited `Operation`.
- [ ] Define typed change-ledger projection over relevant audit operations.
- [ ] Cover manual grid insert, update, and delete.
- [ ] Cover direct DML execution without storing raw SQL or parameter values.
- [ ] Cover DDL preview/apply and schema-designer mutations.
- [ ] Cover migration application and rollback.
- [ ] Cover CSV/import and bulk mutation workflows.
- [ ] Record authenticated actor, target, operation, fingerprints, row count,
      transaction/correlation ids, and terminal outcome.
- [ ] Attach workspace revision, checkpoint, file path, and Git commit when
      execution came from a versioned artifact.
- [ ] Distinguish authored-by, approved-by, and executed-by identities.
- [ ] Add database/table/user/operation/date filters.
- [ ] Link Git commit detail to its executions.
- [ ] Link schema/table/grid surfaces to their relevant ledger entries.
- [ ] Add exportable audit report with permission and retention enforcement.
- [ ] Make ledger storage append-only and tamper-evident.
- [ ] Add configurable retention and optional external audit sink.
- [ ] Keep raw values excluded by default and verify redaction in tests.
- [ ] Design optional encrypted before/after capture as a separate compliance
      mode with explicit enablement, access, retention, and deletion policy.
- [ ] Label native database audit/CDC events as external and preserve their
      database-native identity separately from Sift identity.

### Versioned database workflow

- [ ] Link commit to workspace checkpoint and tree revision in UI.
- [ ] Link query execution to workspace file and commit when available.
- [ ] Link explain-plan capture to file/commit revision.
- [ ] Generate migration SQL into a workspace path.
- [ ] Commit schema-diff plans and rollback scripts together.
- [ ] Compare repository DDL source against a live catalog snapshot.
- [ ] Show database objects affected by staged SQL/DDL.
- [ ] Run formatter and semantic diagnostics against staged SQL.
- [ ] Add pre-commit validation through bounded typed Sift operations, not hooks.
- [ ] Validate migrations against an explicitly selected test database.
- [ ] Show last successful run for the current commit.
- [ ] Store run configurations and migration metadata without secret values.
- [ ] Prevent result rows, credentials, connection strings with passwords, and
      secret-shaped values from entering generated commits.

## Milestone G10 — Hosting-provider integration

- [ ] Add provider-neutral hosting-provider trait and typed repository identity.
- [ ] Detect GitHub/GitLab/Bitbucket-style HTTPS remotes safely.
- [ ] Open repository, branch, commit, and file in browser.
- [ ] GitHub repository picker using linked identity.
- [ ] Show pull-request association for branch/commit.
- [ ] Show CI/check status.
- [ ] Create pull request from current pushed branch.
- [ ] Open pull request in browser.
- [ ] Add review comments only after core local review workflow graduates.
- [ ] Keep hosting authentication separate from database and Git transport
      credentials.

## Milestone G11 — Performance and polish

- [ ] Measure status latency for 1k, 10k, and 100k-path repositories.
- [ ] Measure first Git-panel frame and steady-state refresh cost.
- [ ] Keep status/history panel rows virtualized.
- [ ] Avoid repository scans during editor typing and cursor blink.
- [ ] Coalesce filesystem events and repository refreshes.
- [ ] Cancel stale diff/history requests.
- [ ] Cache unchanged diff metadata by HEAD/index/worktree identity.
- [ ] Keep binary and huge-file work bounded.
- [ ] Preserve focus and selection through refresh.
- [ ] Add app-bar/status-bar branch indicator and changed-count badge.
- [ ] Add notifications for commit/fetch/push/conflict outcomes.
- [ ] Add command-palette entries for every Git action.
- [ ] Add Vim navigation for panel rows, files, hunks, history, and pickers.
- [ ] Add context menus without making them the only action path.
- [ ] Persist panel size, selected row, selected diff, and view mode per workspace.
- [ ] Add accessible names, disabled reasons, and keyboard focus coverage.

## Milestone G12 — Operations, compatibility, and graduation

- [ ] Expose adapter executable/version and health diagnostics.
- [ ] Configure time, output, file, history, and diff limits in instance policy.
- [ ] Define checkout backup policy: protected data versus disposable projection.
- [ ] Backup and restore bindings and credential handles without secret leakage.
- [ ] Verify local, SSH-remote, network-hosted, and container behavior.
- [ ] Verify read-only projection behavior.
- [ ] Verify Git-disabled and network-disabled capability degradation.
- [ ] Verify cross-platform path, case-folding, executable, and line-ending rules.
- [ ] Verify repository ownership/trust failures have actionable diagnostics.
- [ ] Verify malformed Git output always fails closed.
- [ ] Verify no hooks, shell, pager, external diff, fsmonitor, protocol extension,
      or ambient credential helper executes.
- [ ] Verify secret values never enter SQLite, logs, audit, URLs, arguments, or
      inherited environment.
- [ ] Verify stale/concurrent stage, commit, branch, fetch, and push operations.
- [ ] Verify crash recovery during each mutating operation.
- [ ] Keep `cargo fmt`, workspace clippy, and workspace tests green.
- [ ] Record measured performance and security evidence in ADR-034 graduation.

## Explicit non-goals

- Arbitrary Git CLI or shell execution from the product UI.
- Repository hooks or `--no-verify` behavior that executes hooks.
- Ambient desktop credential helpers on a remote/server checkout.
- Force push in v1.
- Automatic merge/rebase/conflict resolution.
- Git as the canonical owner of live collaborative SQL text.
- CRDT-backed Git index, branches, results, schema, or sessions.
- Committing query result data or secret material automatically.
- Copying Zed's terminal fallback; Sift must provide typed safe operations or
  clearly state that an operation is unsupported.

## Next implementation slice

- [x] G1.1: extract desktop repository projection from `WorkspaceShell`.
- [x] G1.2: group staged/unstaged/untracked/conflicted rows.
- [x] G1.3: virtualize the Git status list and preserve stable selection.
- [x] G1.4: add stage-all, unstage-all, refresh, and open-file commands.
- [x] G1.5: add compact commit editor and wire checkpoint-bound commit.
- [x] G2.1: design bounded textual-diff/hunk protocol before implementing diff
      UI.
