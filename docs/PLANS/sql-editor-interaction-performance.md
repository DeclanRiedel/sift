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
3. Diagnostics wait for 650 ms of idle time. Editing hides prior-revision
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
