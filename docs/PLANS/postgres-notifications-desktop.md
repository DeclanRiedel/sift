# PostgreSQL notification listener in the desktop

Status: implemented for Linux desktop. The existing `Listen`
operation, PostgreSQL driver stream, session WebSocket, and SDK subscription are
the transport. No `Driver` trait or protocol change is needed.

## Scope

- Add an explicit **Listen to channel** action for an open PostgreSQL
  connection. Accept one channel per listener initially. Use the server's
  identifier rules before subscribing; show validation and permission failures
  in the listener view. Never infer or run `LISTEN` from SQL editor text.
- Show a dedicated read-only, Vim-navigable listener view with channel,
  connection identity, receive time, and payload. Keep payload as data: no SQL
  execution, interpolation, automatic clipboard writes, or generic toast.
  Existing application notifications have a different lifetime and scope.
- Keep at most 200 notifications and 1 MiB of payload text per listener. Reject
  or truncate an oversized individual payload visibly; bound rendering and
  clipboard export. Counters show dropped entries. No durable storage or room
  broadcast. Closing the view or connection releases the subscription.
- Bind each stream event to instance, session, physical connection, and a
  listener generation. Refuse stale events after disconnect, reconnect, target
  switch, listener restart, or view closure. Surface stream failure and offer
  explicit restart; never silently reconnect to a different database.
- A dedicated WebSocket may stay open while listening. Stop must drop its SDK
  stream and release server and driver resources promptly, including when no
  notification arrives. Audit the existing `Listen` operation; do not add an
  unaudited polling route.

## Acceptance

- Driver/server regression: dropping a quiet subscriber frees the dedicated
  PostgreSQL listener without closing the main query connection. Concurrent
  listeners remain independent; `UNLISTEN` affects only subscribed channels.
- Desktop tests: channel validation, bounded history, stale event refusal,
  stop/restart, failure display, and Vim selection/copy. Exercise a live
  PostgreSQL notification round trip with the opt-in provider suite.
- Run `cargo fmt`, strict workspace Clippy, and workspace tests. Keep the
  product inventory partial until the desktop path and live round trip pass.
