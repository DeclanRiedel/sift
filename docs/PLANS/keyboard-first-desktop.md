# Keyboard-first desktop interaction language

Status: **active keyboard backlog, audited 2026-09-01.** Implemented foundations
and the remaining grid/edit-equivalence work are tracked below.

## Decision

Sift uses two related keyboard layers:

1. Focused components use Vim motions and operators for local content.
2. Application actions use `<leader> <family> <action>`.

`Space` is leader in Vim/UI normal mode. `Ctrl+K` provides the same language
from the standard keymap. Insert-mode Space stays literal. `:` in normal mode
always opens Sift's searchable command palette; Sift does not expose a separate
ModalKit Ex prompt.

Workspace chords use a transient IDE command state owned by the workspace, not
GPUI's timed multi-stroke replay. Once leader is pressed, every following key is
consumed by the IDE until a command completes or Escape cancels, so delayed or
invalid input can never mutate SQL. Status chrome displays the SQL Vim mode and
local input separately from the active `IDE <leader> …` sequence.

Vim is the only supported IDE and editor interaction mode. The Keymaps page
edits leader-command bindings; SQL and configuration editors start in Vim normal
mode. The status bar reports the current Vim mode without switching keymaps.

Families stay small and mnemonic:

- `f` find
- `g` go/focus
- `v` view/toggle
- `x` execute
- `t` tabs
- `r` results
- `e` edits/change sets
- `d` database
- `w` workspace
- `?` discovery

Exact defaults live in `CommandRegistry`. The desktop keymap implements the
available subset. `docs/keyboard-wiki/` separates available mappings from
planned component rollouts.

## Interaction invariants

- Escape moves one level toward normal mode and never mutates data.
- Focus remains visible and returns to its origin after a transient surface.
- Character bindings never capture literal input from text or cell editors.
- Every pointer action ultimately gets a command/action path.
- Disabled commands stay discoverable and state their reason.
- Mutation commands stage or preview; keybindings never bypass approval,
  capability checks, or optimistic conflict detection.
- Main clipboard backs editor and grid yanks/pastes across tabs.

## Modes

- **NORMAL:** navigate current surface and enter leader language.
- **INSERT:** edit SQL, text fields, or a cell.
- **VISUAL:** select text, cells, rows, or objects.
- **COMMAND:** resolve a leader sequence or search the command palette.

Status chrome exposes mode, focused surface, and pending leader prefix. Leader
prefixes display a compact which-key strip generated from the same vocabulary.

## Delivery order

### Monitor tab equivalence design

The Monitor header is one coherent pointer surface. Its tab order, stable
button ids, labels, and engine availability belong in one view model used by
both button rendering and Vim navigation. `<leader> d s` opens the Monitor;
`<leader> d h/l` move between available tabs through the command registry.
The movement calls the same view-selection path as a click, including its
loads and permission errors. It skips engine-disabled tabs and wraps at the
ends. Tests derive from the tab model and assert every rendered tab is
reachable for at least one supported engine, every available tab is reachable
from the dashboard command, and disabled tabs are skipped. This covers the
Monitor header only; controls inside each view remain separate audit work.

- [x] Route Vim-normal `:` to the command palette.
- [x] Add dynamic editor key contexts so normal-mode mappings cannot steal
      insert-mode characters.
- [x] Add Space leader, Ctrl+K fallback, core find/view/execute/tab/edit/
      database/workspace sequences, and which-key prefix hints.
- [x] Isolate leader input in a timeout-free IDE command state; never replay
      incomplete IDE keys into the focused editor.
- [x] Add a Keymaps page for Vim leader-command bindings.
- [x] Search command labels, stable command ids, and mnemonic sequences in one
      palette.
- [x] Remove advertised/default F-key dependencies.
- [x] Add standalone HTML/CSS default-key wiki and Nix runner.
- [x] Add a directional focus graph and Vim `Ctrl+W h/j/k/l` pane language;
      expose the same movement through `<leader> w h/j/k/l`.
- [x] Add `<leader> g c/e/i/r/p` surface focus and Connections NORMAL mode with
      `h/j/k/l`, `gg/G`, `/`, `Enter`, `r`, and `Escape`.
- [x] Give Inspector, result tabs, and Problems complete NORMAL selection state
      and their remaining local motions/operators. Inspector uses `h/l` views,
      `j/k`, `gg/G`, and Enter for field projection; result tabs use `H/L`; the
      read-only Problems item retains full Vim navigation and visual yank.
- [x] Add grid visual selection and system-clipboard `yc`, `yy`, `yh`, and `p`.
      Visual selection extends with Vim motions; `yc` copies selected cells,
      `yy` copies the focused row, `yh` copies selected headers, and `p`
      stages pasted values through the existing edit path. Visual `y` keeps
      the existing headers-and-values yank.
- [x] Add editable-result `i`, `dd`, `o`, undo/redo, Preview, Apply, Revert.
      In the focused grid, `dd` stages a row delete; `x` clears selected
      values, and visual `d` clears the range. `u`/Ctrl+R undo and redo cell
      staging; `g p` opens the staged-edit review with an audited preview and
      Apply, while `g u` discards all staged edits. Shift+U reverts one cell.
- [~] Add generated keyboard-equivalence tests proving every visible action has
      a command path. The app-bar menu is now checked from its generated menu
      model: every command item resolves through a Vim leader binding or the
      `:` palette. All default leader bindings are checked for collisions and
      reachability. Settings, Quit, transaction controls, theme, and results
      layout gained leader paths; clipboard actions became palette entries.
      The Monitor header now renders from one tab model and supports
      `<leader> d h/l` navigation across engine-available tabs; generated
      tests cover reachability, ordering, and skipped disabled tabs. Other
      pointer surfaces and context-local actions still need equivalent audits
      before this can be marked complete. External Wiki and License links are
      outside the command registry and remain to be covered.
- [x] Add versioned `keymaps.json` overrides with compact modal and full-file
      editors; validate command ids, leader syntax, and duplicate sequences.

## Development reference

Run the seeded desktop demo and wiki together:

```sh
nix run .#sift-desktop-demo-wiki
```

Defaults to `http://127.0.0.1:8787`. Override with
`SIFT_DESKTOP_DEMO_WIKI_BIND` and `SIFT_DESKTOP_DEMO_WIKI_PORT`.
