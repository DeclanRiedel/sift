# SQL editor interaction performance

This note records the OSS comparison used for Sift's automatic completion and
diagnostic scheduling. It supplements ADR-053; it is not a second source of
product requirements.

## Primary-source comparison

- [DBeaver SQL Assist](https://github.com/dbeaver/dbeaver/wiki/SQL-Assist-and-Auto-Complete)
  separates semantic and legacy completion engines, keeps manual completion,
  and warns that combining engines costs more on large queries.
- [DBeaver SQL editor preferences](https://github.com/dbeaver/dbeaver/wiki/Admin-Manage-Preferences)
  expose automatic activation, activation-on-keystroke, activation delay, and
  Tab acceptance as separate controls. This is evidence that activation policy
  belongs in the client and must not be confused with candidate generation.
- [pgAdmin preferences](https://github.com/pgadmin-org/pgadmin4/blob/master/docs/en_US/preferences.rst)
  make autocomplete-on-keypress optional while retaining Ctrl/Cmd+Space. This
  provides a recovery path when automatic activation is undesirable or a SQL
  dialect construct is not recognized.
- [sqls](https://github.com/sqls-server/sqls) requires an explicit database
  connection for database-aware completion and exposes switching the selected
  connection/database as editor operations. This supports binding semantic
  work to a named target instead of whichever connection happens to be global.

## Sift decisions

1. The desktop uses a cheap activation guard only. It excludes strings and
   comments, and activates after useful clause boundaries, qualifier dots, or
   two identifier characters. The dialect parser on the server remains the
   sole owner of SQL context and correctness.
2. Automatic completion waits 180 ms and is revision-cancelled. Ctrl+Space is
   immediate. A typing burst therefore produces at most one admitted request.
3. Diagnostics wait for 1200 ms of idle time. Editing hides prior-revision
   markers immediately; incomplete SQL is not painted as a persistent error.
4. Query tabs carry a credential-free semantic target: instance, tenant,
   connection profile, provider, and database when known. The executor rejects
   a target mismatch rather than returning candidates from the wrong catalog.
5. Connection setup loads a shallow schema before queued semantic work runs.
   The server schema cache is keyed by connection-spec hash, not physical
   connection id, so the metadata lane warms the dictionary used by the
   semantic lane. Cache misses are single-flight and retain the existing TTL
   and invalidation policy.
6. Column hydration remains best effort and object-scoped. It never expands
   every table deeply in response to typing.

## Performance invariants

- No driver call or SQL parse runs on the GPUI input path.
- No stale semantic response may open or repaint UI for a newer revision.
- Automatic activation remains cancellable independently of diagnostics.
- A wrong-profile catalog is an error, never a fallback.
- Manual completion continues to work when automatic activation declines a
  context.

## Interaction refinement (2026-09-09)

Measure with the existing GPUI `frame_budget` harness under `release-dev`, the
same optimized profile used by the desktop demo. Its timings exclude GPU
submission, compositor and physical input/display latency; they are not an
end-to-end typing latency claim.

Implementation order and constraints:

1. Separate cursor/mode changes from diagnostics changes. Do not format the
   global Problems document when none is open, or refresh it for cursor motion.
   Status and Vim mode must still update normally.
2. Mirror ordinary single-cursor Insert text as a known splice, retaining
   ModalKit's command/undo state. Complex commands, selections, IME, and
   multi-cursor edits retain the authoritative snapshot path. Verify actual
   buffer length/cursor changes before taking this path: ModalKit represents
   Replace mode as Insert with a different insertion style. Avoid platform and
   unnamed-register clipboard copies on ordinary Insert character events.
3. Reuse visible completion candidates only for a proven identifier extension.
   The server remains authoritative and refreshes the bounded, potentially
   incomplete list in the background. Never reuse across punctuation, cursor
   movement, target changes, or external edits. Do not pretend a capped response
   is a complete catalog cache.
4. Preserve viewport rendering and bound cache invalidation where syntax and
   layout dependencies permit. Cache relative wrap ranges by source line and
   exact text, invalidated by width/font/style changes. Avoid new parsing or
   database I/O on input.

Implemented refinements also retain glyph layouts for unchanged visible lines
whose visual row positions remain stable. Server completion context is forwarded
to snippet enrichment instead of re-tokenizing SQL in the workspace. Connection
retargeting, disconnect/failure, and schema refresh invalidate editor semantic
epochs, previews, and pending requests without changing query text.

References: [CodeMirror completion validity](https://codemirror.net/examples/autocompletion/)
and [Zed text snapshots](https://zed.dev/blog/zed-decoded-rope-sumtree).

## Rapid completion acceptance

The ordinary typing/backspace benchmark does not cover repeated Tab acceptance.
The additional `vim_rapid_completion_large_document` fixture replays
`sel<Tab> * fro<Tab>` on an 8,000-line buffer, with fixture reset outside timing.
It covers local keyword menus and acceptance, not network or compositor latency.

- Completion acceptance splices the existing ModalKit buffer using a black-hole
  deletion and literal transcription. It does not rebuild default bindings or
  copy the whole document. The canonical document retains one undo edit per
  completion. Snippet tabstop navigation only updates the existing cursor.
- A bounded shared keyword table supplies immediate previews for plain SQL
  tokens. Strings, comments, quoted/qualified identifiers and table/alias slots
  decline these previews; dialect-specific literal syntax falls back to the
  server. Server completion still refreshes after 180 ms and owns SQL context,
  dialect-specific suggestions, catalog candidates and ranking.
- Tab with an outstanding request flushes the debounce and records acceptance
  for exactly that revision/caret. It does not insert indentation. Editing,
  moving, Escape or semantic invalidation cancels that intent; an empty result
  reports that no completion is available. With no pending completion, Tab
  retains its existing indentation behaviour.
- Completion takes priority over queued diagnostics. The worker re-coalesces
  between jobs and after synchronization; read-only analysis yields to newly
  arrived controls. Document synchronization remains serial and non-interruptible
  so an HTTP update cannot leave an unknown committed server revision. Dropping
  a read future does not promise cancellation of server-side computation.
