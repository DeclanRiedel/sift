# Shared-room infrastructure and guide

- [x] Release room subscriptions on socket EOF and cancel room handling during
  server drain, including blocked sends/sync work.
- [x] Replace per-waiter shutdown polling with race-safe broadcast notification.
- [x] Verify room disconnect/drain cleanup and shutdown notification behavior.
- [x] Enforce tenant access on Join; isolate room-administration members from
  the active People panel, clear stale selection details, refresh bindings after
  mutations, and scroll long member lists.
- [x] Add a linked Shared Rooms wiki guide grounded in actual desktop commands,
  membership rules, connection ownership, sync, results, and recovery behavior.
- [ ] Run formatting, workspace Clippy/tests, and commit verified milestones.

Design: keep room attachment, writer leases, and subscription owned by the
socket future. Dropping that future releases them; shutdown must not wait for
the next presence tick or socket write. Shutdown is one-way state, so register
a notification before checking the flag to avoid missed wakeups. Documentation
must distinguish a shared server/room from peer-to-peer desktop connections and
must not promise persistence for process-local results.

Targeted evidence: six shutdown tests, two room disconnect/drain tests, two
membership-revocation tests, and the UI room-state isolation regression pass.
The wiki has 50 valid local links/anchors. No external infrastructure was used;
wiki layout was inspected in source, not verified with native screenshots.
