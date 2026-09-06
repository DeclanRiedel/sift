# Windows frontend / NixOS backend audit

Target: Windows 11 desktop connected through HTTP over SSH forwarding to
`declan@vostro`. Keep server ownership of configuration and operations.

## Milestones

- [x] Commit the tested native Windows development and hosting baseline.
- [x] Use Sift's own window bar on Windows, embed its icon, and keep Wiki under Help.
- [x] Make connection creation and feature visibility follow the connected server.
- [x] Make connection and other dialog failures discoverable and copyable.
- [ ] Verify the native frontend against vostro and record bounded benchmarks.

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
