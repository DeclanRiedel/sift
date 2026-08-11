# Phase M1 — Native Application Shell Evidence

Recorded: 2026-08-11

Status: **complete.** M1 remains presentation-only: it performs no server,
schema, Git, semantic, or result I/O.

## Ownership and interaction

- `SiftApp` owns process-wide platform selection and the presentation store;
  `SiftWindow` owns one `WorkspaceShell`.
- The workspace owns pane entities, docks, modal, toast, tooltip, status bar,
  theme, and command-palette input. Each pane owns ordered item references and
  its focus handle.
- Typed actions back native application menus, conventional platform-aware key
  bindings, command-palette rows, and GPUI focus-context dispatch.
- Command availability carries a disabled reason. The palette renders disabled
  treatment and the reason rather than hiding unavailable work.
- Dirty-item close opens an explicit save/close choice. Save clears the dirty
  presentation marker, completes the pending close, and emits a toast; product
  saves remain an M2/M3 server operation.

## Components and appearance

- The initial `sift-ui` control system resolves neutral, accent, destructive,
  hover, active, focus-visible, selected, disabled, loading, and error states
  from semantic theme tokens.
- Light and dark themes remain complete and feature views do not introduce raw
  product palette values.
- The shell provides the integrated title area, persisted-size docks, pane
  tabs, result-panel placeholder, status bar, modal overlay, tooltip surface,
  and toast surface with accessibility roles on application and controls.

## Restore and platform behavior

- Presentation state is a versioned JSON contract containing window, dock,
  pane, and item references only. Tests verify that its encoded form excludes
  secret/query/result-shaped fields.
- Writes are serialized, use a private same-directory temporary file plus
  rename, and execute on GPUI's background executor. Window-bound changes
  update the retained restore bounds. Unix replacement is atomic; Windows uses
  the standard library's serialized remove-then-rename fallback.
- Invalid versions recover to defaults. Off-screen windows recenter on the
  primary display and saved logical dimensions scale without mutating the
  persisted geometry.
- Linux is the primary Nix-backed lane. CI also compiles `sift-desktop` with
  stable Rust on native macOS and Windows runners.

The current harness still lacks an X authorization cookie, so native visual
inspection and first-paint timing remain deferred to an authorized display.
GPUI visual test contexts construct and paint the shell while exercising the
real focus/action tree.

## Verification

```text
nix develop . --command cargo fmt --all --check
nix develop . --command cargo clippy --workspace --all-targets -- -D warnings
nix develop . --command cargo test --workspace
nix develop . --command cargo deny check
nix develop . --command scripts/check-client-dependency-firewall.sh
```
