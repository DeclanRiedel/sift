# Desktop settings

Sift keeps stable, user-editable desktop preferences in `settings.toml`.
They are local to the OS account and are never synchronized through a Sift
server or shared room.

```toml
version = 1

[editor]
default_mode = "vim" # "standard" or "vim"

[appearance]
theme = "ayu-dark"

[keyboard]
profile = "vim" # "vim", "hybrid", or "standard"

[data]
selection_aggregates = false
query_results_placement = "right"

[ui]
recent_objects = true
navigation_hints = "always"

[repository]
grouping = "staging"
sort = "path"
view = "flat"
primary_action = "open_file"
commit_subject_limit = 72
commit_sign_off = false
# commit_author_name = "Ada Lovelace"
# commit_author_email = "ada@example.com"
```

The keyboard profile controls IDE commands independently from the editor's
default mode. Vim enables the leader language, Standard enables conventional
IDE shortcuts, and Hybrid enables both. Vim is the default profile.

## Complete settings reference

These are all supported `settings.toml` keys. Omitted sections and keys use the
listed defaults.

| Key | Values | Default | Effect |
| --- | --- | --- | --- |
| `version` | `1` | `1` | Settings schema version. |
| `editor.default_mode` | `vim`, `standard` | `standard` | Initial mode for new editable tabs. |
| `appearance.theme` | `ayu-dark`, `light`, or a custom theme id | `ayu-dark` | Active color theme. |
| `keyboard.profile` | `vim`, `hybrid`, `standard` | `vim` | Enabled IDE shortcut language. |
| `data.selection_aggregates` | Boolean | `false` | Shows sum and average for numeric Data-tab selections. Cell count remains visible when off. |
| `data.query_results_placement` | `right`, `bottom` | `right` | Places query results beside the SQL editor or below it. |
| `ui.recent_objects` | Boolean | `true` | Remembers and shows up to five recently opened database objects. When off, objects are neither shown nor collected. |
| `ui.navigation_hints` | `always`, `hold`, `hidden` | `always` | Shows navigation shortcut hints, reveals them only while Alt is held, or keeps them hidden. |
| `repository.grouping` | `staging`, `file_state` | `staging` | Groups source-control changes by staging state or file state. |
| `repository.sort` | `path`, `file_name` | `path` | Sorts source-control paths by full path or file name. |
| `repository.view` | `flat`, `tree` | `flat` | Displays repository changes as a flat list or folder tree. |
| `repository.primary_action` | `open_file`, `open_diff` | `open_file` | Chooses the default action when activating a changed path. |
| `repository.commit_subject_limit` | Positive integer | `72` | Subject-length guide; values below one behave as one. |
| `repository.commit_author_name` | String; omit for none | omitted | Explicit commit author name. |
| `repository.commit_author_email` | String; omit for none | omitted | Explicit commit author email. |
| `repository.commit_sign_off` | Boolean | `false` | Adds a `Signed-off-by` trailer when absent. |

The in-app Settings panel exposes the common Vim, appearance, query/result
layout, Data aggregate, recent-object, and navigation-hint controls. **Open
settings.toml** exposes every option above.

IDE leader bindings live beside it in `keymaps.json`:

```json
{
  "version": 1,
  "bindings": {
    "workspace.focus-connections": "<leader> g c",
    "query.execute-statement": "<leader> x s"
  }
}
```

Open **Keymaps** to edit bindings in a compact table, or choose **Open JSON**
for the complete file. Saving either surface validates command ids, leader
syntax, and duplicate sequences before replacing the file. An empty binding
disables that command. Changes apply immediately to command resolution, the
palette, and which-key hints.

Open the profile menu, choose **Settings**, then use **Open settings.toml** to
edit the file in Sift. Saving validates the complete document before replacing
the file; invalid TOML stays unsaved and the previous settings remain active.

Default locations:

- Linux: `$XDG_CONFIG_HOME/sift/settings.toml`, otherwise
  `$HOME/.config/sift/settings.toml`
- macOS: `$HOME/Library/Application Support/Sift/settings.toml`
- Windows: `%LOCALAPPDATA%\Sift\settings.toml`

`keymaps.json` uses the same directory on every platform.

## Themes

`ayu-dark` is Sift's default. It combines Ayu Dark's warm syntax colors with
neutral charcoal surfaces and a darker editor background. `light` is also
built in as a restrained warm-grey palette without pure-white surfaces.

Custom themes are TOML files in a `themes` directory beside `settings.toml`.
Select one using its file name without `.toml`:

```toml
# themes/my-theme.toml
version = 1
name = "My Theme"
appearance = "dark" # "dark" or "light"

[colors]
background = "#080b10"
accent = "#e6b450"
syntax_keyword = "#ff8f40"
```

```toml
# settings.toml
[appearance]
theme = "my-theme"
```

Colors use `#rrggbb` or `#rrggbbaa`. Every color is optional: omitted values
inherit from the built-in palette for the chosen appearance. Unknown color
names and malformed values are rejected when settings are saved or the app
starts using the theme.

Available color names are `background`, `surface`, `panel`, `toolbar`,
`elevated_surface`, `hovered_surface`, `selected_surface`, `active_surface`,
`scrim`, `border`, `subtle_border`, `strong_border`, `text`, `muted_text`,
`disabled_text`, `accent`, `accent_muted`, `accent_hover`,
`drop_target_background`, `drop_target_border`, `on_accent`, `focus_ring`,
`danger`, `danger_muted`, `warning`, `warning_muted`, `success`,
`success_muted`, `staged`, `staged_muted`, `editor_active_line`, `grid_stripe`, `syntax_keyword`,
`syntax_string`, `syntax_number`, and `syntax_comment`.

Open **Settings → Manage themes…** to choose a built-in theme, edit the current
theme, or import/export a TOML file. Editing opens the theme in a normal Sift
tab; saving validates the document and applies it immediately when it is the
selected theme. Built-ins are copied to a new custom file before editing, and
imports receive a unique file name rather than replacing an existing theme.

Ephemeral window and workspace layout remains in `presentation.json`. That file
is an internal recovery snapshot, not a supported settings interface.
