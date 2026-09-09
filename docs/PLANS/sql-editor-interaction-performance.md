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

References: [CodeMirror completion validity](https://codemirror.net/examples/autocompletion/)
and [Zed text snapshots](https://zed.dev/blog/zed-decoded-rope-sumtree).
