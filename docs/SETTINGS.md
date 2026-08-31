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
```

The keyboard profile controls IDE commands independently from the editor's
default mode. Vim enables the leader language, Standard enables conventional
IDE shortcuts, and Hybrid enables both. Vim is the default profile.

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
`success_muted`, `editor_active_line`, `grid_stripe`, `syntax_keyword`,
`syntax_string`, `syntax_number`, and `syntax_comment`.

Ephemeral window and workspace layout remains in `presentation.json`. That file
is an internal recovery snapshot, not a supported settings interface.
