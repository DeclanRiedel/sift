# Ordered CSV import review controls

Work tracking (2026-11-11): **Done (scoped contract)**. Remaining product work: [current TODO](../TODO.md#imports-and-transfers).

Implemented under ADR-133. The Vim CSV import review supports ordered column
selection, drop, reorder, copy, rename and reset to original columns. Keep original source indices separate from ordered
outputs so copies and omitted columns retain exact field text and record order.
Source bytes are immutable shared data; local column edits never change the file.

Render samples through the same output projection. Drop private columns from
the displayed samples as well as from import/quarantine values. Preserve quoted
multiline Unicode, NULL text and empty strings before existing conversion rules.
Bound review to 256 source/output columns, source headers to 4096 bytes, target
names to 128 bytes and distinct valid names, original/mapped CSV to 64 MiB.
Reject excess explicitly rather than silently omitting fields. Display snippets
are bounded; inference uses at most 200 rows and the grid at most 20 rows.

Prepare full CSV projection and native conversion on a background worker with a
writer that checks the output limit during emission. Do not clone full source
bytes or retain unbounded display strings on the UI thread. A generation and
frozen mapping/type/connection identity fence late preparations and callbacks.
Cancellation during local preparation dispatches no import. Once a server request
has started, preserve the existing local-cancel/inspect-target warning.

Existing-table CSV requests use projected target headers/values. New-table DDL
uses selected target names and per-output type choices, including copied fields.
SQLite native review uses the same mapped CSV before typed conversion; retain its
10,000-row/8-MiB/128-column limits and one-use server token. A changed selection,
type, source or native connection requires fresh review; prepared commands bind
instance, tenant, profile, session and connection, including parked CSV sources.
Native apply reuses only the exact reviewed typed rows and conversion choices.

Saving a projected review creates a CSV format-version-2 recipe with the ordered
column_projection option and target-keyed type choices. Upload the original file
when running the recipe; the server retains original-upload resume fingerprints.
Recipe sources must resolve unique exact headers as ADR-130 requires; positional
local review may omit ambiguous fields, but cannot save an ambiguous selected
header as a name-based recipe. Legacy rename-only recipes remain supported.

Vim command focus uses h/l selection, H/L movement, d drop, c copy, R reset,
i target-name editing and t type editing. Escape returns from a field to commands;
Tab navigates fields. Preserve v preview, x apply/DDL, n native mode, r recipe and
Esc cancel/close. Structural controls are disabled while preparing or dispatching.
All edits invalidate old dry-run/native reviews. Source changes cannot redirect
a prepared request. Existing audited import/bulk/recipe operations remain the
write boundary; no new protocol I/O, Driver signature or CI workflow is needed.

Acceptance covers exact projected bytes, bounded copies, sample privacy,
type/DDL and recipe consistency, ambiguous headers, Vim controls, stale native
reviews, cancellation and source replacement (ADR-133). Text/decimal expressions,
XLSX/Parquet projection and non-CSV mapping controls ship in later scoped
milestones. Broader formats/sources and native Windows/macOS remain open.
