# Windows frontend / NixOS backend audit

Target: Windows 11 desktop connected through HTTP over SSH forwarding to
`declan@vostro`. Keep server ownership of configuration and operations.

## Milestones

- [x] Commit the tested native Windows development and hosting baseline.
- [x] Use Sift's own window bar on Windows, embed its icon, and keep Wiki under Help.
- [x] Make connection creation and feature visibility follow the connected server.
- [x] Make connection and other dialog failures discoverable and copyable.
- [x] Verify the native frontend against vostro and record bounded benchmarks.

## Findings and intended behavior

- Windows currently requests a native titlebar in addition to Sift's bar.
  Keep resizing, dragging, and Sift's window controls working without duplicate chrome.
- The toolbar adds a development Wiki button despite an existing Help entry.
- The window icon helper excludes Windows; its backend loads an executable resource.
- Add connection unconditionally opens manifest editing. A standalone server needs
  the existing connection wizard; manifest-managed servers retain configuration editing.
- Database operation capabilities are loaded only after opening a database.
  Coarse server features must be known during handshake, before any connections exist.
  Advertise enabled manifest, workspace, and Git features through additive handshake
  capability identifiers. Do not infer configuration support from a remote profile ID.
  Hide unavailable feature entry points; keep palette entries disabled with a reason.
  Existing authorization and contextual operation checks remain authoritative.
- SSH destination and HTTP endpoint fields need distinct guidance and early validation.
  This cross-OS setup uses `http://127.0.0.1:17474`, not an HTTPS URL in an SSH field.
- Instance-manager failures currently remain inside the dialog. Route failures to the
  existing notification/problem history and add a copy action to error presentation.
  Preserve redaction and retain useful errors after the dialog closes.

## Verification

Use existing behavior tests for empty connections, missing/disabled features,
connection failure reporting, and window/menu behavior. Run required workspace
formatting, Clippy, and tests on both hosts. Build normal desktop binaries separately
from test targets. Measure idle/active process memory, CPU, and bounded API latency;
distinguish frontend memory, backend memory, and tunnel overhead. No database workload
claim without a configured disposable database. Keep CI manual-only.

## Results — 2026-09-06

Both hosts passed `cargo fmt --all -- --check`,
`cargo clippy --workspace --all-targets --locked -- -D warnings`,
`cargo test --workspace --locked`, and a normal `cargo build --workspace --locked`.

| Host | Tests passed | Ignored |
| --- | ---: | ---: |
| Windows 11, Rust 1.98.1 MSVC | 1,198 | 1 |
| vostro / NixOS, flake Rust 1.96.1 | 1,203 | 1 |

Behavior checks cover clicking Add connection without a manifest, enabled and
disabled feature controls, disabled palette reasons, rejecting URLs in the SSH
field, error history retention, copying inline errors without dismissing the
dialog, copying notifications without dismissing them, and repeating a failure
after clearing it. Existing Git tests now explicitly negotiate Git support.

The rebuilt Windows frontend connected to port 17474, remained responsive to
native window messages, and exited with code 0 after closing its window. The
native top inset changed from 31 pixels to 0. Extracting the executable's icon
returned the existing Sift artwork. Its native stdout/stderr logs were empty.
The user's original window was left running; relaunch to see the changes.

### Bounded measurements

Development builds, no configured database, no query workload. The Windows
validation window used fresh presentation state; the original user session was
left open. Builds had finished before each host's measurement. Memory is a
process snapshot, excludes GPU allocations, and is not peak memory. CPU is a
30-second idle sample shortly after startup, expressed as a percentage of one
logical CPU. Different Task Manager memory columns are not directly comparable.

| Process | Working set / RSS (MiB) | Private memory (MiB) | Idle CPU |
| --- | ---: | ---: | ---: |
| Windows desktop | 72.63 | 97.67 | 5.036% |
| Windows SSH tunnel | 10.98 | 3.16 | 1.505% |
| NixOS backend | 73.38 | 73.27 | 0.067% |

Each endpoint used five warmups followed by 50 sequential successful requests.
Windows used .NET HttpClient through SSH; NixOS used Python urllib over loopback.
These timings include client/transport overhead and do not isolate handler time.

| Route | Windows → SSH → vostro median / p95 | vostro loopback median / p95 |
| --- | ---: | ---: |
| `GET /v1/ready` | 30.86 / 40.44 ms | 0.91 / 1.09 ms |
| `POST /v1/handshake` | 62.20 / 92.92 ms | 0.73 / 0.79 ms |

### Running setup

The updated backend remains in the isolated worktree
`/home/declan/personal/sift-windows-validation`, under the transient user service
`sift-dev-backend.service`. It listens on vostro's loopback port 7474. The Windows
SSH forward remains available at `http://127.0.0.1:17474`. This is a development
session, not a boot-persistent deployment. The original NixOS checkout and NixOS
configuration were not changed.

From the Windows repository, launch the rebuilt frontend with:

```powershell
./scripts/dev.ps1 desktop --server-url http://127.0.0.1:17474 --server-name vostro
```

Milestones: `5712d95` (Windows tooling), `7485934` (chrome/icon/Wiki),
`2579f78` (server capabilities), `dde76b2` (UI behavior and errors), and
`fa6a56d` (Git fixtures). CI remains manual-only.
