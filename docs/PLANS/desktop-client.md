# Plan — Desktop Client (GPUI-first)

Status: proposed. Owner: TBD. Companion docs: `docs/PLANS/metadata-store.md` (auth/tenant/secret), `docs/PLANS/collab-native.md` (rooms, CRDT, presence). Fills in milestone **C8** from the collab plan.

## 1. Decision

**The first (and, for now, only) sift client is a GPUI desktop app.** Rust throughout. Connects to a local sift server on loopback by default; can connect to a remote sift server over TLS + keypair auth.

**Web client is deferred but not precluded.** No wasm work in this plan. When a web client happens, it will be written in a Rust+wasm framework (Dioxus/Leptos most likely) against the same `sift-protocol` crate — not by porting GPUI. See §12.

Rationale: GPUI is desktop-native and performant; remote server support gives you Zed-style "connect from anywhere" without a browser. Rewriting the framework to reach the browser is a bigger project than building a second client against the existing wire protocol when the time comes.

## 2. Two invariants (load-bearing)

Everything downstream depends on these. Enforce at review.

### Invariant A — GPUI types never leak

GPUI is a dependency of `client-desktop` **only**. Never a dependency of `sift-protocol`, `sift-core`, `sift-server`, `sift-driver-*`, `sift-metadata`, `sift-doc`, `sift-client-sdk`. ADR-005 already says this — the invariant is *load-bearing* now, not aspirational, because the future web client depends on it holding.

Enforcement: a `cargo-deny` config or a CI check that greps for `use gpui` outside `crates/client-desktop/`.

### Invariant B — Every client-visible state is derivable from server responses

The client is a rendering surface, not a source of truth. Every piece of UI state must either:
- come from a server response (protocol type or protocol event), or
- be pure UI ephemera (scroll position, hover state, unpersisted local selection).

If the client needs to compute something the server doesn't expose, that is an **API gap** to fix on the server, not a client-local invariant. This is what preserves the future web client, and also what makes multi-client collab actually converge (the server is the referee).

## 3. Platform target order

1. **macOS first.** GPUI is most mature there; fastest path to a demoable build.
2. **Linux second.** GPUI Linux is functional. Developers care.
3. **Windows last.** GPUI Windows is catching up but behind.

Ship each platform when it's ready; don't gate features on cross-platform parity.

## 4. Crate layout

```
crates/client-desktop/
  Cargo.toml
  src/
    main.rs
    app.rs                    -- top-level GPUI app + window
    runtime.rs                -- tokio side runtime + bridge to GPUI
    transport/
      mod.rs                  -- ProtocolClient trait (HTTP + WS)
      http.rs                 -- built on sift-client-sdk
      ws.rs                   -- priority + stream channel handling
    auth/
      mod.rs                  -- credential source chooser
      loopback.rs             -- zero-auth local
      keypair.rs              -- Ed25519, private key in OS keychain
      token.rs                -- bearer for CI-like scenarios
    state/
      mod.rs                  -- ClientState (GPUI Model)
      rooms.rs
      documents.rs            -- mirrors server Documents via sift-doc
      presence.rs             -- remote cursor tracking
      results.rs              -- streamed query result pages
      history.rs
    views/
      window.rs
      sidebar.rs              -- rooms + documents tree
      editor.rs               -- SQL editor bound to a Document
      results.rs              -- result table view
      presence.rs             -- remote cursor overlay
      settings.rs
    input/
      keymap.rs               -- Zed-style command palette + bindings
    logging.rs
```

Dependencies:
- `sift-protocol` (wire types).
- `sift-client-sdk` (HTTP client + WS transport).
- `sift-doc` (CRDT document primitive, from the collab plan C2).
- `gpui`.
- `tokio` (side runtime).
- `keyring`.
- `arboard`, `dirs`, `serde`, `tracing`.

## 5. Runtime bridging

GPUI has its own executor; sift-server work is async on tokio (ADR-007). The client must run **both** and pass values across the boundary.

Design:
- Boot a `tokio::runtime::Runtime` on a dedicated thread (`runtime.rs`).
- Every HTTP/WS call happens on tokio.
- Results flow to GPUI via `mpsc::UnboundedSender<UiEvent>` consumed by a GPUI-side task using `cx.background_executor().spawn`.
- GPUI-side state updates happen on the main thread inside `cx.update(...)`.
- Never `.block_on()` on the GPUI main thread.

The `ProtocolClient` trait exposes async methods; internally each dispatches to the side runtime and yields a `oneshot::Receiver`. Views observe state via GPUI's model system.

## 6. State model

Server-driven, CRDT-mirrored.

```
ClientState (GPUI Model, main thread)
├── AuthContext            (principal, tenant memberships)
├── Rooms       DashMap<RoomId, RoomModel>
│   └── RoomModel
│       ├── members        Vec<PrincipalId>
│       ├── documents      DashMap<DocumentId, DocumentModel>
│       ├── presence       DashMap<PrincipalId, PresenceState>  (in-memory only)
│       └── result_streams DashMap<CursorId, ResultStream>
├── ConnectionProfiles     Vec<ConnectionProfile>               (server-cached, refreshable)
└── UiEphemera             (scroll positions, focus, selection)
```

- **`DocumentModel`** owns a `sift_doc::Document` (Loro-backed). Local edits apply immediately and are sent as `DocOp` on the priority channel. Remote `DocOpBroadcast` messages are applied to the same doc — Loro guarantees convergence.
- **`presence`** never persists. Cleared on room detach.
- **`result_streams`** back a virtualized result grid; consumed page-by-page with the ACK gate (§6.2 in the collab plan).

## 7. Auth on the client

Three modes, chosen per-server:

| Mode | When | How |
|---|---|---|
| **Loopback** | server is `127.0.0.1` and offers `loopback_bypass` | No credential sent |
| **Keypair** | remote server, personal use | Ed25519 keypair generated first-run; private key in OS keychain; sign nonce per connect |
| **Bearer token** | CI/scripts (rare on client) | Pasted into settings |

First-launch flow:
1. Client generates an Ed25519 keypair on first launch. Private key → keychain (namespace `sift.desktop.<host>`). Public key exportable via settings UI as a base64 string.
2. Local server on `127.0.0.1` → loopback bypass, no key registration needed.
3. Remote server → user pastes URL, client displays public key, user registers it once (over any existing auth, e.g. an OIDC browser flow initiated from the server's admin URL). Subsequent connects use keypair.

## 8. Editor design

- **Buffer**: a `sift_doc::Document` (Loro text CRDT). The editor view reads `.text()` for rendering and emits `DocumentOp`s on edit.
- **Selection & cursor**: local state only. Broadcast at ≤30 Hz (coalesced client-side) via the priority channel.
- **Remote cursors**: `PresenceBroadcast` messages update `presence` map; overlay renderer draws them at the mapped byte position with the remote user's display name.
- **Run query**: sends the current buffer's text via `Operation::Execute { document_id, tx? }`. Results stream to the whole room; each client renders independently.
- **Follow mode** (§10 refinement): if `following = Some(other)`, the client watches the other principal's `PresenceBroadcast` and syncs viewport/cursor to theirs.

Not in Phase 1: shared undo, rich text annotations, autocomplete.

## 9. Result view

- Consumes `ResultSetStream` pages from the WS stream channel.
- Virtualized table; renders visible window only.
- ACKs a page **only when the user's viewport reaches near it or after a short prefetch** — this keeps the initiator's ACK gate honest and lets slow observers backpressure their own memory without stalling the DB (collab plan §6.3).
- If lagged, client requests a resync-from-cursor.

## 10. Milestones

Sized for incremental review. Each is a runnable checkpoint.

| # | Deliverable | Depends on |
|---|---|---|
| **D1** | Crate skeleton, GPUI hello-window, tokio bridge, `ProtocolClient` calling `/v1/health` on a local server | none |
| **D2** | Loopback auth end-to-end: list tenants, list rooms via server routes | collab plan C1, C5, C6 |
| **D3** | Open a document; render its text; local editing generates `DocOp`s applied to a local Loro doc | collab plan C2, C3 |
| **D4** | Send `DocOp`s to server, receive `DocOpBroadcast`, apply to local doc — two windows on the same doc converge | collab plan C4 |
| **D5** | Presence: cursor/selection broadcast + remote cursor overlay | collab plan C3, C9 |
| **D6** | Execute query from a document; render streamed pages in a virtualized grid | collab plan C4 |
| **D7** | Keypair auth: keygen, keychain storage, register-key UI, remote-server connect | collab plan C7 |
| **D8** | Room list UI + create/join/leave; personal-room autoattach | collab plan C1 |
| **D9** | Connection-profile UI: list, create, edit, choose credential mode, `OpenConnectionFromProfile` | metadata plan (existing) |
| **D10** | Follow mode: viewport sync to another principal | D5 + collab C9 |
| **D11** | Query history view (server-backed) | collab C10 |
| **D12** | Command palette + keymap (Zed-style) | none, but nicer with D8+ |
| **D13** | Settings surface: servers, keys, appearance | ambient |
| **D14** | macOS notarization, Linux `.deb`/AppImage packaging | client stable |

**D1–D6 is "collab demoable."** Two macOS builds, both connected to the same local sift server, editing the same document, seeing each other's cursors and query results. That's the payoff moment — the earliest point where the whole architecture proves itself.

D7 unlocks "connect from anywhere." D8–D11 fill in the IDE surface. D12–D14 polish.

## 11. Dependencies on server-side collab plan

The client cannot start most milestones until the server-side collab primitives exist:

| Client milestone | Requires from `collab-native.md` |
|---|---|
| D2 | C1 (rooms schema), C5 (auth middleware), C6 (room + document HTTP routes) |
| D3 | C2 (`sift-doc` crate), C3 (protocol split) |
| D4 | C4 (RoomStore + broadcast) |
| D5 | C3 (priority channel), C9 (server-side coalescing) |
| D6 | C4 (broadcast result streams) |
| D7 | C7 (keypair auth) |
| D11 | C10 (room-scoped history) |

Reasonable interleave: server team ships C1–C4 while client team does D1 against `MockDriver`. As soon as C4 lands, D2–D6 unblock in sequence.

## 12. Web client — kept possible, not planned

Explicitly not in scope. But the following must remain true so a future web client is a straightforward project, not a rewrite:

1. **`sift-protocol` stays pure serde**, no `tokio`, no I/O, no OS deps (ADR-004). If it drifts, the web client dies before it's born.
2. **Invariant A** (§2) — GPUI never leaks. If a web client ever imports server types, those imports must be `no_std`-friendly.
3. **Invariant B** (§2) — every UI state derivable from server responses. No client-only invariants sneak in "just for GPUI."
4. **Auth is a menu.** The `principal_resolver` middleware (metadata plan §9) already accepts OIDC + session cookies alongside keypair + tokens. Web clients use OIDC + cookies; nothing new server-side.
5. **Loopback bypass is a *deployment mode*, not a *protocol feature*.** A web client never gets loopback bypass, but the protocol doesn't require it.

When a web client is built — probably Dioxus for direct protocol-crate reuse — it will reimplement §4–§9 of *this* doc in wasm, and share nothing but the wire.

## 13. Non-goals for the client's Phase 1

- **Language server integrations** (SQL LSPs). Later.
- **Rich-text annotations, inline decorators, gutter widgets.** Later.
- **Explain-plan visualizers.** Later; requires server-side C-suite plumbing.
- **Local plugin system.** Definitely later.
- **Auto-updater.** Manual updates for early releases.
- **Cross-workspace tabs / sessions on the client side.** Rooms are the persistent unit.
- **Any UI for voice/video.** Not this phase.

## 14. Risks and open questions

- **GPUI API churn.** GPUI is pre-1.0 and evolving. Track Zed's main; expect breakage. Mitigation: keep GPUI-touching code confined to `views/` and `app.rs`. State/logic stays framework-free.
- **Loro determinism across versions.** If `sift-doc` bumps Loro, on-disk snapshots need a migration path. Design `crdt_type` on the document row to also carry a version tag.
- **Tokio + GPUI executor mismatch.** Never call `.block_on` on the main thread. All async goes through `runtime.rs`'s bridge. This is a review-time invariant.
- **Keychain on Linux.** Some Linux desktops don't run a Secret Service daemon. Fall back to an age-encrypted file with a passphrase; document.
- **CRDT ↔ text editor cursor mapping.** Loro gives byte positions; the editor works in glyph positions. Need a mapping layer in `views/editor.rs`. Standard problem; well-solved in Zed.
- **Result view memory.** Virtualized grid + page ACKing keeps this bounded; still needs a "max in-memory pages per stream" config.
- **First-launch friction for remote servers.** Registering a public key is one step but requires an existing auth path. Deferred until D7; local dev works over loopback.
- **What does "personal room" mean when connecting to a remote server?** It's the user's private space *within a tenant*. Confirm this matches the collab plan's `kind='personal'` semantics.
- **Cross-window state sharing on one machine.** Two GPUI windows connecting to the same local server become two attachments to the same room — this is a feature, not a bug. Local sessions can validate collab without a second machine.

## 15. What this plan locks in

After D1–D6 ship:
- The **client architecture** commits to CRDT-mirrored, server-driven state with tokio-on-a-side-thread.
- The **auth story** on the client commits to loopback + keypair as the primary modes; token is a fallback.
- The **editor** commits to Loro as its buffer, not `String`.
- The **result view** commits to virtualized, page-ACKed rendering.

None of this precludes a future web client, a hosted service, or voice/video. All of it validates the collab-native pivot in code, not in a plan doc.
