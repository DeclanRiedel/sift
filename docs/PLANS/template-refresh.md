# Template refresh

Keep shipped examples and editor snippets aligned with current instance behavior.
Do not change local credentials or silently migrate existing user instances.

- [x] Check the reproducible example and generated instance templates against the current schema and lock.
- [x] Correct invalid identity completion placeholders and cover the inserted resource with validation.
- [x] Clarify development versus locked-instance environment settings and document desktop instance selection.
- [x] Correct quick-start instructions where CLI and demo resource names differ.
- [x] Run formatting, workspace lint and tests; commit the verified milestone.

Validation: `cargo fmt --all -- --check`, workspace Clippy with warnings denied,
and all enabled workspace tests passed. Desktop linking used the existing
temporary libxkbcommon-x11 development alias via `LIBRARY_PATH`; no repository
or system-library workaround was committed. The shipped demo lock remains
current and was not regenerated unnecessarily. No local credentials changed.
