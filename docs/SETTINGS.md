# Desktop settings

Sift keeps stable, user-editable desktop preferences in `settings.toml`.
They are local to the OS account and are never synchronized through a Sift
server or shared room.

```toml
version = 1

[editor]
default_mode = "vim" # "standard" or "vim"

[keyboard]
profile = "vim" # "vim", "hybrid", or "standard"
```

The keyboard profile controls IDE commands independently from the SQL editor's
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

Ephemeral window and workspace layout remains in `presentation.json`. That file
is an internal recovery snapshot, not a supported settings interface.
