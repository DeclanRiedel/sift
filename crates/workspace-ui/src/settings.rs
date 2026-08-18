use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use toml_edit::{DocumentMut, Item, Value};

const SETTINGS_VERSION: u32 = 1;

const fn settings_version() -> u32 {
    SETTINGS_VERSION
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EditorMode {
    #[default]
    Standard,
    Vim,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct EditorSettings {
    pub default_mode: EditorMode,
}

impl Default for EditorSettings {
    fn default() -> Self {
        Self {
            default_mode: EditorMode::Standard,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct UserSettings {
    #[serde(default = "settings_version")]
    pub version: u32,
    pub editor: EditorSettings,
}

impl Default for UserSettings {
    fn default() -> Self {
        Self {
            version: SETTINGS_VERSION,
            editor: EditorSettings::default(),
        }
    }
}

impl UserSettings {
    pub fn decode(source: &str) -> Result<Self, String> {
        let settings: Self =
            toml::from_str(source).map_err(|error| format!("settings.toml is invalid: {error}"))?;
        if settings.version != SETTINGS_VERSION {
            return Err(format!(
                "settings.toml version {} is unsupported; expected {SETTINGS_VERSION}",
                settings.version
            ));
        }
        Ok(settings)
    }

    pub fn encode(&self) -> Result<String, String> {
        toml::to_string_pretty(self)
            .map_err(|error| format!("serializing settings.toml failed: {error}"))
    }
}

/// Crash-safe, OS-account-local storage for stable user preferences.
/// This file is intentionally separate from ephemeral presentation state.
#[derive(Debug, Clone)]
pub struct SettingsStore {
    path: PathBuf,
    write_lock: Arc<Mutex<()>>,
}

impl SettingsStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            write_lock: Arc::new(Mutex::new(())),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn load(&self) -> Result<UserSettings, String> {
        let source = std::fs::read_to_string(&self.path)
            .map_err(|error| format!("reading {} failed: {error}", self.path.display()))?;
        UserSettings::decode(&source)
    }

    pub fn read_text(&self) -> Result<String, String> {
        std::fs::read_to_string(&self.path)
            .map_err(|error| format!("reading {} failed: {error}", self.path.display()))
    }

    pub fn save(&self, settings: &UserSettings) -> Result<(), String> {
        self.write_validated(&settings.encode()?)
    }

    pub fn save_text(&self, source: &str) -> Result<UserSettings, String> {
        let settings = UserSettings::decode(source)?;
        self.write_validated(source)?;
        Ok(settings)
    }

    /// Update the UI-owned editor preference without discarding comments or
    /// unrelated settings written by the user.
    pub fn save_editor_mode(&self, mode: EditorMode) -> Result<UserSettings, String> {
        let _guard = self
            .write_lock
            .lock()
            .map_err(|_| "settings write lock poisoned".to_string())?;
        let source = std::fs::read_to_string(&self.path)
            .map_err(|error| format!("reading {} failed: {error}", self.path.display()))?;
        let mut document = source
            .parse::<DocumentMut>()
            .map_err(|error| format!("settings.toml is invalid: {error}"))?;
        let decor = document["editor"]["default_mode"]
            .as_value()
            .map(|value| value.decor().clone());
        let mut mode_value = Value::from(match mode {
            EditorMode::Standard => "standard",
            EditorMode::Vim => "vim",
        });
        if let Some(decor) = decor {
            *mode_value.decor_mut() = decor;
        }
        document["editor"]["default_mode"] = Item::Value(mode_value);
        let updated = document.to_string();
        let settings = UserSettings::decode(&updated)?;
        self.write_source(&updated)?;
        Ok(settings)
    }

    fn write_validated(&self, source: &str) -> Result<(), String> {
        let _guard = self
            .write_lock
            .lock()
            .map_err(|_| "settings write lock poisoned".to_string())?;
        self.write_source(source)
    }

    fn write_source(&self, source: &str) -> Result<(), String> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| format!("creating {} failed: {error}", parent.display()))?;
        }
        let temporary = self.path.with_extension("toml.tmp");
        let mut options = std::fs::OpenOptions::new();
        options.create(true).truncate(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        let mut file = options
            .open(&temporary)
            .map_err(|error| format!("opening {} failed: {error}", temporary.display()))?;
        file.write_all(source.as_bytes())
            .and_then(|()| file.sync_all())
            .map_err(|error| format!("writing {} failed: {error}", temporary.display()))?;
        drop(file);
        #[cfg(windows)]
        if self.path.exists() {
            std::fs::remove_file(&self.path)
                .map_err(|error| format!("replacing {} failed: {error}", self.path.display()))?;
        }
        std::fs::rename(&temporary, &self.path)
            .map_err(|error| format!("installing {} failed: {error}", self.path.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_round_trip_uses_readable_toml() {
        let settings = UserSettings {
            editor: EditorSettings {
                default_mode: EditorMode::Vim,
            },
            ..UserSettings::default()
        };
        let source = settings.encode().unwrap();
        assert!(source.contains("default_mode = \"vim\""));
        assert_eq!(UserSettings::decode(&source).unwrap(), settings);
    }

    #[test]
    fn store_rejects_invalid_text_without_replacing_the_file() {
        let directory = tempfile::tempdir().unwrap();
        let store = SettingsStore::new(directory.path().join("settings.toml"));
        store.save(&UserSettings::default()).unwrap();
        let original = store.read_text().unwrap();

        assert!(store.save_text("[editor\ndefault_mode = 3").is_err());
        assert_eq!(store.read_text().unwrap(), original);
    }

    #[test]
    fn ui_update_preserves_comments_and_unrelated_settings() {
        let directory = tempfile::tempdir().unwrap();
        let store = SettingsStore::new(directory.path().join("settings.toml"));
        let source = "# Personal settings\nversion = 1\ncustom = \"keep\"\n\n[editor]\ndefault_mode = \"standard\" # modal\n";
        store.write_validated(source).unwrap();

        let settings = store.save_editor_mode(EditorMode::Vim).unwrap();
        let updated = store.read_text().unwrap();

        assert_eq!(settings.editor.default_mode, EditorMode::Vim);
        assert!(updated.contains("# Personal settings"));
        assert!(updated.contains("custom = \"keep\""));
        assert!(updated.contains("# modal"));
    }
}
