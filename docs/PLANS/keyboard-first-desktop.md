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

The Keymaps page keeps IDE and editor choices explicit. Its IDE profile is
tri-state: Vim enables only the leader language, Hybrid enables leader and
conventional IDE shortcuts, and Standard disables leader commands. The SQL
editor default remains a separate Vim/Standard choice.

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

- [x] Route Vim-normal `:` to the command palette.
- [x] Add dynamic editor key contexts so normal-mode mappings cannot steal
      insert-mode characters.
- [x] Add Space leader, Ctrl+K fallback, core find/view/execute/tab/edit/
      database/workspace sequences, and which-key prefix hints.
- [x] Isolate leader input in a timeout-free IDE command state; never replay
      incomplete IDE keys into the focused editor.
- [x] Add a Keymaps page with Vim, Hybrid, and Standard IDE profiles, separate
      from the SQL editor's default mode.
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
- [ ] Add grid visual selection and system-clipboard `yc`, `yy`, `yh`, and `p`.
- [ ] Add editable-result `i`, `dd`, `o`, undo/redo, Preview, Apply, Revert.
- [ ] Add generated keyboard-equivalence tests proving every visible action has
      a command path.
- [x] Add versioned `keymaps.json` overrides with compact modal and full-file
      editors; validate command ids, leader syntax, and duplicate sequences.

## Development reference

Run the seeded desktop demo and wiki together:

```sh
nix run .#sift-desktop-demo-wiki
```

Defaults to `http://127.0.0.1:8787`. Override with
`SIFT_DESKTOP_DEMO_WIKI_BIND` and `SIFT_DESKTOP_DEMO_WIKI_PORT`.
