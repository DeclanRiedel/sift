# Project-wide architecture and performance audit

This records the broad follow-up to the initial backend/frontend cleanup, not
a completion claim based on that initial three-change pass. Audit every area below, follow
cross-layer findings, implement concrete improvements, and commit verified
milestones. Runtime-only tracking lives in temporary JSON outside the repository.

## Audit coverage

- [x] Configuration: compare portable `sift.toml`, editor/schema help, examples,
  lock/apply, and effective runtime validation. Reject settings that validate
  but cannot start; document ownership and remove misleading examples.
- [x] Local and hosted instances: inspect authentication topology, lifecycle,
  admission limits, public endpoint handling, and shared server boundaries.
- [x] SSH networking: inspect bootstrap, process ownership, forwarding, endpoint
  validation, deadlines, reconnect, and cancellation versus local transport.
- [x] Multiplayer: inspect room attachment/presence lifecycle, CRDT sync and
  reassembly limits, reconnect/replay, permissions, and shared results.
- [x] SQL editor: inspect revision ownership, stale work, completion/diagnostic
  scheduling, cancellation, large-document work, and Vim interaction.
- [x] Visual layout: inventory modal sizes, scroll containment, content fit,
  long/error content, and small windows; use GPUI layout assertions and actual
  rendered inspection where the available environment supports it.
- [x] Broader backend/frontend hot paths: inspect metadata, cursor/results,
  task/resource lifetime, repeated computation, and architectural duplication.
- [x] Update architecture decisions when a stable boundary changes and reconcile
  product/docs checklists with actual behavior.
- [x] Complete final workspace formatting, Clippy, tests, diff review, and
  milestone commits; record any unverified external/live behavior explicitly.

## Working rules

Design each coupled fix before implementation. Keep findings concrete: trigger,
current behavior, intended behavior, affected boundaries, and validation.
Preserve secrets outside SQLite/logs and server-owned operations/audit. Keep
protocol types pure and CRDTs limited to query documents. API compatibility and
legacy interaction modes are not work priorities. Do not add unrelated smoke
scripts or CI workflows. Do not claim measured speedups or visual verification
without evidence.

## Findings and milestones

The sections below record reviewed findings, fixes, evidence, and deliberately
unverified external behavior. An audit checks the implementation; it is not a
claim that every future product-inventory feature is implemented.

### Configuration boundary

- [x] Require loopback binds for SSH manifests, matching runtime topology.
- [x] Intersect the two declared connection/query ceilings instead of letting
  format-v1 fields overwrite a stricter tenant policy. Document this in editor
  help and the operator guide; both fields remain usable without hidden priority.
- [x] Reject portable instance manifests in the development-config loader with
  an actionable `--instance-root` error, and clearly label the development template.
- [x] Share result/cursor/interval limit validation across manifest and runtime
  paths so non-instance startup cannot accept panic-inducing zero intervals.

Evidence: 19 instance-config tests and 226 server unit tests passed. Workspace
Clippy with warnings denied passed for the configuration milestone. Development
and manifest startup now use the same limits validator; timeout bounds also
agree. The example, operator guide, and configuration Wiki explain ownership
and intersecting ceilings.

### SSH helper lifecycle

- [x] Drain helper stderr from spawn, retaining only bounded diagnostics, so
  bootstrap cannot block on a full pipe before it emits readiness.
- [x] Bound readiness lines on initial connect and renewal, reject non-loopback
  forwarding URLs before sending credentials, and never echo token-bearing JSON
  in a parse error. Own the drain task through cancellation and early returns.
- [x] Bound control-master shutdown as well as startup and ordinary commands.

Evidence: two desktop output tests passed, covering IPv4/IPv6 loopback, invalid
and oversized readiness, redacted parse failures, and draining three times the
retention limit through a small pipe. Workspace Clippy passed. Live SSH hosts
were not contacted; endpoint identity pinning and capability exchange remain
in the existing connection path.

- [x] Follow-up: own the listener and its forwarding children, cap simultaneous
  SSH channels and queued errors, and propagate TCP half-close in both relay
  directions. A local EOF currently does not close SSH stdin, which can leave
  both sides waiting for completion.

Evidence: all five `sift-remote` tests passed, including bidirectional EOF
propagation with in-memory streams. Forwarding is capped at 128 channels and
one queued error; aborting its owner also drops all child relays. Workspace
Clippy passed.

### Multiplayer lifecycle and memory

- [x] Keep room creation/attachment/subscription atomic with idle eviction;
  evict after the last attachment too, regardless of drop order. Expiration
  must recheck a lease under its entry lock before removing refreshed presence.
- [x] Bound replica chunk counts, bytes, and concurrent transfers; count received
  chunks instead of scanning all slots each arrival. Reject inconsistent or
  incomplete transfers, and clear abandoned transfers on resync/reconnect.
- [x] Retain pending update IDs only. Reconnect already reconstructs missing
  updates from the CRDT version vector, so retaining payload copies is waste.

Evidence: nine SDK room tests and fourteen server room/authorization tests
passed, including concurrent subscription churn, reverse drop order, malicious
chunk counts, duplicate and out-of-order chunks, incomplete sync, and resync
cleanup. Workspace Clippy passed. Chunk limits are 4096 slots, 1 MiB per chunk,
four incomplete transfers, and 256 MiB retained payload per replica, matching
the server's default maximum retained CRDT history.

### Modal layout

- [x] Centralize preferred content widths for every modal variant and make
  content roots responsive instead of competing with the surrounding card.
- [x] Clamp cards to the available viewport, account for app-bar placement and
  padding, and provide overflow scrolling instead of clipping inaccessible
  controls. Preserve the dedicated expanded-result viewport.
- [x] Exercise modal layout at small and large window sizes, including wide
  repository, room, snippet, vault, and ledger surfaces and long form content.

Native screenshot capture is currently unavailable: this process cannot
authenticate to the running X display. Use GPUI's rendered layout tests and
record that limit rather than claiming pixel inspection.

Evidence: 406 UI tests and workspace Clippy passed. New rendered-layout tests
cover 64 modal variants at 1280×900, 800×600, and 480×400, plus footer/content
bounds on five wide surfaces. Repository commit details needed an explicit
flex height and wrapping actions to keep their footer inside the card. Long
populated content and editor overlays were exercised in the follow-up below.

Long-content follow-up: a rendered test supplies 8 KiB of snippet validation
errors in an 800×600 window, scrolls the modal, and verifies the footer remains
reachable. The error region is separately scrollable and capped at 120 px,
preventing overlap with its footer. Targeted test and workspace Clippy passed.

### SQL editor scheduling and response ownership

- [x] Correlate completions with their requested caret as well as text revision;
  moving without editing must cancel stale menus and pending responses.
- [x] Reject stale failures just like stale successful answers.
- [x] Own one debounce task per document/request class instead of detached
  timers; cancel superseded work and avoid cloning text for stale dispatches.
- [x] Review service queue coalescing and popup placement near viewport edges.

Evidence: UI suite passed (406 tests before the added caret-motion regression,
which also passed separately); desktop completion-burst regression and workspace
Clippy passed. Service batching keeps the latest completion position per tab,
preserving serial server-document updates. Old responses cannot consume a newer
caret request; moving away and back still cancels an outstanding menu.

Popup follow-up: completion, SQL/configuration hover, and star-expansion cards
share viewport-aware placement. Completion rows adapt to available height and
keep the selected row visible. All 408 UI tests and workspace Clippy passed,
including a scrolled 320×180 editor with its caret at the bottom-right edge.

### Live room authorization

- [x] Revalidate tenant membership as well as room membership on a live socket.
  Removing tenant membership leaves the explicit room-member row intact, so
  the old lease check can continue streaming room events to a removed member.
- [x] Move periodic SQLite authorization checks off Tokio worker threads.

Evidence: existing room-revocation test and a tenant-revocation variant passed
against a local HTTP/WebSocket server; workspace Clippy passed. The new check
joins current room, tenant, and principal state instead of trusting a cached
tenant list. No external server was contacted.

- [x] Follow-up: retain lease checks during active result/notification streams.
  The outer socket loop currently stops polling its lease while streaming or
  awaiting an ACK. Cancel and release active cursors on every streaming error.

Evidence: all four live local WebSocket lease tests passed, including logout
while an active cursor waits for an ACK. The socket closes and its cursor is
removed. Result and notification streams also observe graceful drain; JSON
sends have a 30-second write deadline. Workspace Clippy passed.

### Shared keyed mutation gates

- [x] Replace permanent per-workspace mutex entries with lifetime-owned gates.
  Reuse the schema-fetch gate's tested atomic last-owner cleanup in a small
  server-internal utility rather than duplicating the race-sensitive logic.
  Keep gates discoverable while any owner or waiter exists; remove idle entries.

Evidence: three schema-gate tests and the workspace-gate test passed, including
owned guards used across repository mutations. Workspace Clippy passed. The
owned guard unlocks before releasing its map ownership, retaining atomic cleanup.

### Query-performance summary

- [x] Define p95 as nearest-rank and select its order statistic without sorting
  every duration. The current floor-index calculation reports 20 ms for runs
  of 10, 20, and 100 ms, hiding the tail in a small history window.
- [x] Accumulate averages without i64 overflow and saturate total-row counters.

Evidence: the history-summary test passed with small samples, an empty history,
and repeated i64-maximum durations/row counts. Workspace Clippy passed.

### Local desktop activation

- [x] Enforce the advertised 10-second readiness deadline across handshake I/O,
  not merely 100 sleeps. A listener that accepts TCP without replying currently
  stalls startup indefinitely because the SDK has no HTTP response deadline.
- [x] Bound individual probes and avoid reparsing/rehashing the same instance
  manifest and lock on every 100-ms readiness poll. Use the already validated
  manager's state directory, and validate its advertised endpoint against the
  configured bind. Wildcard network binds connect through local loopback;
  explicit interface binds remain supported.

Evidence: the full workspace suite passed, including desktop lifecycle tests
and a stalled local TCP listener that never sends HTTP headers. Individual
probes now time out after one second. The supervisor also kills its own child
on final drop even if activation failed before acquiring a window lease.
Three local-supervisor tests additionally cover IPv4/IPv6 wildcard binds,
explicit interface binds, and rejecting a mismatched descriptor address/port.

### Document actor retention

- [x] Bound the idle actor cache and remove empty writer-lease sets. Every
  visited document currently retains its loaded CRDT until explicit deletion
  or server restart. Evict only map-owned actors, preserving active shared
  identity and durable reloads; serialize cache-miss loading with insertion.

Evidence: a regression loads 66 documents, retains a live owner, verifies idle
eviction/durable reconstruction, and checks writer-lease cleanup. The cache
targets 64 actors on misses; active owners may exceed that target and are never
evicted by trimming. This is a cache policy, not a global CRDT-memory quota.
The stale comment claiming manifest-configurable collaboration limits was also
corrected. Targeted test and workspace Clippy passed.

### Shared-result publication

- [x] Make per-room cap enforcement and publication atomic. Concurrent finished
  queries can all observe the same pre-insertion count and exceed the 32-result
  cap. Keep expensive serialization/encrypted spill outside the publication lock.

Evidence: three shared-result tests and workspace Clippy passed. Eight threads
publish 256 results into one room and retain exactly 32. Retired spill files
are dropped after releasing the publication lock, as well as keeping initial
serialization/spill outside it.

## Coverage and remaining verification limits

- Configuration ownership was traced from manifest/lock validation through
  applied-generation checks and runtime conversion. Startup rejects unapplied
  source drift; only opaque credential handles enter metadata. Runtime safety
  limits and configuration help now agree at the boundaries changed here.
- Local, network, and SSH transport were checked separately from personal/team
  deployment. Authentication middleware remains fail-closed; session/cursor
  ownership and managed connection policy remain server-owned. ADR-019/020
  now describe tenant revocation and the implemented SSH policy accurately.
- SDK reconnect still reattaches and reconstructs missing CRDT updates from
  version vectors. Results remain immutable independent reader views, with
  bounded retention and encrypted spill, not CRDT state.
- Existing editor tests exercise virtualized large documents and cached line
  layouts; new tests cover stale work, caret movement, coalescing, and popup
  bounds. No new speedup percentage is claimed: improvements remove repeated
  allocations/work or bound resource retention, rather than benchmark a workload.
- Native pixel screenshots remain unverified because X display authentication
  is unavailable. Layout tests are rendered GPUI geometry, not screenshots.
  They sample content/footer states; they do not prove every possible dynamic
  text payload fits every display/theme.
- Live external PostgreSQL, SQL Server, SSH, OAuth, and repository-provider
  infrastructure was not contacted. Local HTTP/WebSocket and in-memory relay
  tests cover the changed boundaries; feature-gated live suites remain separate.
- Future unchecked product inventory items (for example backup/restore,
  resumable transfer UI, and metrics export) were not silently treated as
  implemented. This audit's concrete findings are the checklist above.

## Validation environment

Final checks passed after all code changes:

- `cargo fmt --check`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `LIBRARY_PATH=/tmp/sift-refactor-HGLfxo cargo test --workspace --quiet`
- `git diff --check`

The workspace run included 409 workspace-UI tests and 32 desktop tests, plus
server, metadata, SDK, driver, shared-crate, integration, and doc-test targets.
Existing ignored/feature-gated live tests were not enabled. Milestone changes
were reviewed and committed locally; nothing was pushed or deployed.

Desktop test linking needs `libxkbcommon-x11.so`, but this machine only has its
versioned runtime library. Tests used an ephemeral linker alias via
`LIBRARY_PATH=/tmp/sift-refactor-HGLfxo`; no repository or system linker settings
were changed. Native display capture remained unavailable. Build artifacts have
left roughly 4 GiB free on the filesystem; no user files were deleted to make
space. Temporary JSON tracking stays outside Git under the same `/tmp` directory.

### Repository hosting I/O

- [x] Reuse one credential-free HTTP client per router lifetime; keep explicit
  timeouts and redirect denial. Currently each hosting handler rebuilds a pool.
- [x] Fetch independent PR/check summaries concurrently, respecting the existing
  repository network policy and keeping credentials request-scoped.
- [x] Bound provider response bodies before JSON decoding; upstream responses
  currently have no byte ceiling. Wipe temporary credential buffers on early
  errors and request cancellation as well as successful completion.

Evidence: three hosting tests and workspace Clippy passed. A local HTTP server
checks known-length and chunked bodies over 8 MiB, sanitized decode errors,
shared pool identity, and absent default credentials after an authenticated
request. No external repository provider was contacted. `Zeroizing` now owns
temporary hosting credential buffers, including early-return/cancellation paths.
