# Phase M0 — GPUI Feasibility Evidence

Recorded: 2026-08-11

Status: **complete.** This file records the M0
baseline against ADR-040 and `phase-m-gpui-desktop.md`; hard product budgets are
graduated in M6 after representative hardware and release builds exist.

## Reference environment

- Linux x86_64 Nix development shell.
- Rust stable 1.96.1 in the Nix shell. Server/shared packages retain their
  declared Rust 1.80 minimum; GPUI client crates intentionally track the latest
  stable toolchain required by the pinned upstream revision.
- GPUI/Zed revision
  `d71f1461045c098dc6ca6b1b5adcf1b8949722e8`.
- GPUI Linux features: X11, Wayland, font-kit, and Vulkan/WGPU rendering.

The dev shell now declares fontconfig, FreeType, libxkbcommon, Vulkan loader,
Wayland, X11, and XCB dependencies. Both GPUI window backends compile from the
same workspace dependency selection.

## Boundary evidence

- `sift-api-types` is pure serde/schemars data over `sift-protocol`; it has no
  storage, runtime, network, or OS dependency.
- `sift-client-sdk` no longer depends on `sift-metadata` directly or
  transitively.
- `scripts/check-client-dependency-firewall.sh` rejects metadata, server, and
  driver crates from the SDK and all desktop/UI crate graphs.
- The server consumes the moved request DTOs and explicitly maps the three
  metadata-owned enums and API-token record at its HTTP boundary.
- Secret-bearing moved request types retain redacted `Debug` behavior, covered
  by a contract test.

## GPUI capability evidence

| Capability | Evidence |
| --- | --- |
| Application/window composition | `sift-desktop` opens a GPUI window rooted at `FeasibilityWorkspace`. |
| Entity ownership | Workspace owns the query-input entity and the cancellable probe task. |
| Typed actions and focus context | Theme, refresh, focus, clipboard, and editing actions bind through `SiftWorkspace` / `SiftTextInput`. |
| Async-to-UI bridge | Deterministic `#[gpui::test]` advances the GPUI clock and observes the weak entity update. |
| Cancellation on owner drop | The probe is retained as a GPUI `Task`; replacing or dropping it cancels the prior task by GPUI ownership. |
| IME/text input | `EntityInputHandler` implements selection, marked text, UTF-16 mapping, composition replacement, bounds, and point lookup. |
| Unicode correctness | Test round-trips a non-BMP character between UTF-8 byte and UTF-16 platform offsets. |
| Clipboard | Copy, cut, paste, and select-all are typed actions; pasted line breaks are normalized for the M0 single-line control. |
| Accessibility | Root and input expose application/text-input roles and accessible labels. |
| Virtualization | A GPUI `uniform_list` projects 100,000 logical result rows while constructing only the requested visible range. |
| Theme boundary | Original light/dark Sift semantic tokens live in `sift-ui`; feature views contain no raw palette. |
| Headless testing | Six API/UI/workspace tests run without a display, including two real GPUI window contexts. |

## Initial measurements

These are cold development-build observations, not product budgets:

- first GPUI dependency check in the prepared Nix shell: 2 minutes 23 seconds;
- first code-generating desktop build: 3 minutes 12 seconds;
- first workspace-wide build and test with the GPUI graph: 5 minutes 52
  seconds;
- incremental API/UI/workspace test run after compilation: 4.31 seconds;
- logical virtual result cardinality: 100,000 rows with visible-range
  construction through `uniform_list`.

The harness display exported `DISPLAY=:0.0` without an X authorization cookie,
so a native launch correctly reached GPUI's X11 initialization and then failed
before window creation with an authorization error. This is an execution
environment limitation, not a renderer/build failure. Window composition,
input registration, painting, focus, and async work are exercised through
GPUI's simulated window contexts; native first-paint timing remains an M1
acceptance measurement on an authorized display.

## Commands

```text
nix develop . --command cargo fmt --all --check
nix develop . --command cargo check -p sift-desktop --all-targets
nix develop . --command cargo test -p sift-api-types -p sift-ui -p sift-workspace-ui
nix develop . --command scripts/check-client-dependency-firewall.sh
nix develop . --command cargo clippy --workspace --all-targets -- -D warnings
nix develop . --command cargo test --workspace
nix develop . --command cargo deny check
```

All commands above pass. Cargo-deny records exact git-source permissions for
the pinned GPUI graph, narrow compatible-license exceptions for Zed's three
GPL tracing support crates, and maintenance-only advisory exceptions tied to
the pin. Two GPUI support manifests omit a license field upstream; cargo-deny
reports those as warnings while the dependency policy passes.
