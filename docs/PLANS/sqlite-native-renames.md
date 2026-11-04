# SQLite native table rename milestone

Design before implementation, 2026-10-27. Extend the existing audited schema
comparison and migration lifecycle with explicit table renames, preserving data
and an exact inverse. Do not rebuild a table or change file/root authority.

Only native-proven simple ordinary `main` tables qualify. Reuse the verified
canonical CREATE statement and existing column checks. Normalize only the
quoted table-name prefix for rename compatibility; retain column declarations,
metadata and all other catalog properties. A caller must accept the rename
mapping explicitly. No structure-changing rename or database/schema move is
inferred.

Initially require unfiltered, untruncated captures of main/temp with only simple
tables and columns. Refuse views, triggers, constraints, indexes, virtual tables,
unknown nodes and incomplete dependency projections. This keeps native rename's
implicit dependency rewrites out of the first contract. Empty temp is allowed;
objects outside main and definitions lacking native proof are refused.

Render quoted ALTER TABLE main.old RENAME TO new and its inverse inside the
existing forced transactional group. Existing live-revision checks, test rollback,
production confirmation, read-only policy and supervised cancellation apply.
Native acceptance must preserve row values, roll validation back, apply the
rename, reject a stale plan and refuse dependency-bearing shapes. Broader
column rename/drop/alter and the designer remain separate milestones.

Reference: https://www.sqlite.org/lang_altertable.html

## Acceptance (2026-11-04)

The real-file HTTP fixture passes validation rollback and native rename/apply
with quoted names, unchanged rowids/values, stale-index rejection, and explicit
view/kind-filter refusal. Shared projection now keeps ancestors for selected
view columns instead of returning dangling graph references. All four SQLite
HTTP provider tests, formatting, strict workspace Clippy and the full workspace
suite pass. ADR-118 records the graduated boundary; broader migration work
remains partial.
