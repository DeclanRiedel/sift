# sift — Lessons from Zed

> Companion to `DECISIONS.md` and `TECH_STACK.md`. Not binding ADRs; this is a
> study of what Zed does well, why it feels fast, and which of those ideas are
> worth stealing for a database IDE. Where an idea matures into a real decision,
> it should graduate into its own ADR.

Zed is already cited in ADR-006 (operation/command model). This doc goes wider:
rendering, process layout, collaboration, restart/update behaviour, and the
specific places where a SQL IDE should *not* copy a text editor.

---

## 1. How Zed actually works (the load-bearing parts)

### 1.1 Process and runtime layout

- **One editor process per window**, with the GPUI event loop on the main thread.
  GPUI is Zed's custom GPU-accelerated, retained-mode UI framework — no DOM, no
  CSS, no Chromium. Rendering goes straight to Metal (macOS) / Vulkan (Linux).
- **Out-of-process workers for heavy or untrusted work.** Language servers
  (LSP), text indexing, extensions, etc. each run as separate child processes
  communicating over JSON-RPC. The editor thread never blocks on syntax
  highlighting, indexing, or a wedged language server — the worst case is a
  degraded feature, not a frozen UI.
- **Async Rust bridged into the UI thread.** A tokio runtime runs in a
  background executor (`AsyncApp`); tasks post results back onto the GPUI
  thread. This is exactly the bridge ADR-007 anticipates for sift.
- **Local persistence via SQLite + on-disk snapshots** for recent projects,
  buffers, unsaved edits, window layout. Cold start reads these, not the FS
  tree, so the window can paint before the project has finished indexing.

### 1.2 Editor data model

- **Buffer** holds text in a rope-like sum-tree giving O(log n) edits and cheap
  slices. Immutable-ish; edits produce new revisions with cheap diffs.
- **MultiBuffer** composes multiple Buffers (and sub-ranges of them) into one
  view. This is what makes split views, multi-buffer search, and live log
  stitching natural.
- **Worktree / Project** layer owns file entries, FS watching, and (for remote
  projects) a replica of someone else's FS.

### 1.3 The Action system (ADR-006's inspiration)

- Every user gesture is an `Action` struct: name + payload. Actions are
  keybindable, searchable in the command palette, replayable, and check
  capabilities ("is this available in the focused view right now?").
- Actions are **dispatched locally and synchronously** to the focused view.
  This is the important nuance for sift: Zed Actions are a UI/controller
  concept, not a network concept. ADR-006 borrows the *naming/dispatch* shape
  but our Operations travel over the wire to a server. Same vocabulary,
  different layer.

---

## 2. Why restarting / updating Zed feels instant

This is the part worth studying, because a SQL IDE that reopens your tabs,
connections, and in-flight queries on relaunch is rare and delightful.

### 2.1 Cold start is cheap because there is almost nothing to boot

- **No Chromium, no JS bundle, no GC warmup.** First paint is bounded by GPU
  context init, not by a megabyte-sized web stack.
- **Retained-mode rendering.** GPUI keeps an element tree and diffs it; the
  first frame only needs the visible nodes, not the whole window's worth.
- **Native compilation, no interpreter.** Rust + LTO = no JIT warmup stall.

### 2.2 State is restored, not recomputed

- Window layout, open tabs, recent projects, unsaved buffer contents, keymap,
  theme — all persisted to disk and read on launch. The user perceives
  "instant reopen" because the app is genuinely restoring a snapshot, not
  rebuilding the world.
- Project indexing (file tree walk, symbol index) happens **after** the window
  paints, progressively. The app is usable immediately and gets smarter over
  the next few seconds.

### 2.3 Updates download in the background and swap on relaunch

- Zed's auto-updater fetches the new binary while you keep working; the change
  takes effect on the next restart, not by interrupting the session. There is
  no "applying update…" modal blocking the editor.
- Because restart is itself fast (above), the cost of "apply on next launch"
  is near-zero, so aggressive background updates are viable.

**sift adaptation.** Persist on every meaningful change: open tabs, per-tab
query text, scroll position in result sets, connection config (secrets in the
OS keychain, not plaintext), column widths/sort state, recent queries, command
palette history. On launch, restore layout first, reconnect lazily, refresh
schema metadata in the background. The user should see their workspace before
any DB round-trip completes.

---

## 3. How Zed sends updates to peers (collaboration model)

Zed is collaboration-native. The model is worth understanding because it tells
us both what to steal and what to skip.

### 3.1 The shared artifact is editable text, so they use CRDTs

- Each peer holds a **replica** of every shared buffer. Edits are encoded as
  operation-based CRDT ops (insert/delete at a logical position, not a byte
  offset), so concurrent edits from different peers reconcile without a
  central authority choosing a winner.
- A coordination service relays ops and bootstraps late joiners with a
  snapshot, but it does not arbitrate — reconciliation is deterministic on
  every peer. This is why collab editing in Zed doesn't have a "merge
  conflict" dialog: the data model makes conflicts structurally impossible.

### 3.2 Presence is separate and ephemeral

- Cursors, selections, follows, "who's typing" indicators are **not** part of
  the CRDT. They're broadcast as low-latency ephemeral messages and not
  persisted. Editing history is durable; presence is not.
- This separation matters: durable state uses the heavy reconcilable path;
  throwaway state uses the cheap fire-and-forget path. Mixing them is a
  classic collab performance bug.

### 3.3 Bulk state uses snapshots, not op replay from zero

- A peer joining late doesn't replay every edit since the beginning. It
  receives a recent snapshot, then the small stream of ops since that
  snapshot. This keeps join time bounded regardless of project age.

### 3.4 Network shape

- Central relay for bootstrapping and presence; peer-to-peer possible for
  streaming bulk ops once peers know each other. The protocol is custom and
  binary, not JSON, for the hot path.

### 3.5 What sift should and should not take from this

| Artifact in a SQL IDE | Analogue | Mechanism |
| --- | --- | --- |
| SQL query text in a shared editor tab | Shared buffer | **CRDT-worthy.** Multiple users editing the same query is exactly the problem CRDTs solve. |
| Result set from a query | A rendered view, not editable | **Not CRDT-worthy.** Share a reference (query + params + cursor position); each peer fetches from the server. |
| Open connection / session | Server-owned state | Broadcast presence only; the server is the single source of truth. |
| Cursor / selection in a query tab | Presence | Ephemeral broadcast, never persisted. |
| "User X is running query Y" | Event stream | Server emits events; clients subscribe. |

The headline: **only the SQL editor pane is a CRDT problem.** Everything else
in a database IDE — connections, results, schema, sessions — is naturally
server-authoritative and should not pay the CRDT tax. Zed's whole buffer layer
is a CRDT because every byte is editable; in sift, almost nothing is.

---

## 4. Ideas worth stealing, ranked

| # | Zed idea | sift adaptation | Priority |
| --- | --- | --- | --- |
| 1 | Retained-mode GPU UI (GPUI) | Already adopted (ADR-005). | Done |
| 2 | Out-of-process workers for heavy/untrusted work | Run each DB driver (and maybe each long query) in a side process or task sandbox; a wedged tiberius connection cannot freeze the server. | High |
| 3 | State snapshot on disk; restore before any I/O | Persist tabs, query text, layout, column widths, recent queries; paint window before reconnecting. | High |
| 4 | Background updater; apply on next launch | Same idea works verbatim for sift. Background-fetch the new server binary; swap on restart prompt. | Medium |
| 5 | Action system with capability checks | Already in ADR-006. Add capability checks so e.g. `RenameTable` is greyed out for read-only connections. | Medium |
| 6 | Ephemeral vs durable separation in collab | Presence/cursor = ephemeral; query text = CRDT-able; results/connections = server-authoritative. | Medium (phase 2+) |
| 7 | Late-join = snapshot + ops-since | For shared query tabs: send current text + recent edit log, not full history. | Medium (phase 2+) |
| 8 | Progressive post-paint indexing | Refresh schema metadata in the background after the workspace paints; cache aggressively; invalidate via LISTEN/NOTIFY (postgres) or polling. | High |
| 9 | MultiBuffer composition | "Pinned results" alongside a live query, or stitched log views — same composition trick. | Low |
| 10 | Worktree abstraction | Mirror as connection tree: server → database → schema → table/view/proc. Same lazy-expand, same FS-watch analogue (schema invalidation). | Medium |
| 11 | Pre-warmed processes / pools | Keep a warm connection pool per configured connection so first query after launch is immediate. | High |

---

## 5. What NOT to copy

- **CRDTs everywhere.** Tempting because Zed's collab is buttery, but only the
  SQL editor pane justifies it. Results, schema, and sessions are
  server-owned; CRDTs there buy nothing but complexity.
- **Local-first file ownership.** Zed's source of truth is a file the editor
  owns. sift's source of truth is a database the server talks to. Trying to
  pretend the client owns anything but transient UI state is how Navicat-style
  tools become brittle (see ADR-002).
- **Treating every artifact as editable text.** Result grids are not buffers.
  They need virtualization, server-side cursors, and backpressure — none of
  which a rope gives you. The result pane is the novel hard part; the editor
  pane is the part Zed already solved.
- **Replicating buffers to peers byte-for-byte.** For results, replicate a
  *reference* (query + cursor id + page window), not the data. Each peer
  streams its own pages from the server. Otherwise a 10M-row result gets
  serialized to every viewer.

---

## 6. Resulting ADR candidates

These are not decisions yet, just the follow-ups this study implies:

- **ADR-011 (candidate): result streaming via server-side cursors.** Keep DB
  cursors open server-side; clients page by cursor id; WS channel carries
  pages with backpressure tied to grid readiness. (Addresses the novel hard
  part flagged in §5.)
- **ADR-012 (candidate): workspace snapshot and restore.** Define the exact
  persisted-on-every-change set (tabs, query text, layout, column widths,
  recent queries, scroll positions) and the launch ordering (paint →
  reconnect → refresh schema).
- **ADR-013 (candidate): driver isolation.** Each driver crate (and possibly
  each long-running query) runs in a sandboxed task or side process so a
  wedged driver cannot take the server down. Mirrors Zed's out-of-process LSP.
- **ADR-014 (candidate): collaboration scope.** Phase-2 collaboration covers
  (a) shared SQL editor tabs via CRDT, (b) ephemeral presence, (c) shared
  session/connection state via server broadcast. Explicitly excludes result
  replication beyond references.
- **ADR-015 (candidate): background updater.** Background-fetch new server
  binary, apply on next launch, no in-session modal.

---

## 7. One-line summary

Steal Zed's **process discipline, restart model, and action system**; borrow
its **CRDT layer only for the SQL editor pane**; and accept that the result
grid is a problem Zed has never had to solve — which is where sift's snappiness
will be won or lost.
