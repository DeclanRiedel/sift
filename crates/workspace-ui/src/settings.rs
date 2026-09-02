use std::collections::{BTreeMap, HashSet};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use sift_ui::{Theme, ThemeConfig};
use toml_edit::{DocumentMut, Item, Value};

const SETTINGS_VERSION: u32 = 1;
const KEYMAPS_VERSION: u32 = 1;
const QUERY_BINDINGS_VERSION: u32 = 1;

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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AppearanceSettings {
    /// Built-in theme id or the stem of a TOML file in `themes/`.
    pub theme: String,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QueryResultsPlacement {
    Bottom,
    #[default]
    Right,
}

impl QueryResultsPlacement {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Bottom => "bottom",
            Self::Right => "right",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DataSettings {
    pub selection_aggregates: bool,
    pub query_results_placement: QueryResultsPlacement,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct UiSettings {
    pub recent_objects: bool,
}

impl Default for UiSettings {
    fn default() -> Self {
        Self {
            recent_objects: true,
        }
    }
}

impl Default for DataSettings {
    fn default() -> Self {
        Self {
            selection_aggregates: false,
            query_results_placement: QueryResultsPlacement::Right,
        }
    }
}

impl Default for AppearanceSettings {
    fn default() -> Self {
        Self {
            theme: "ayu-dark".into(),
        }
    }
}

impl Default for EditorSettings {
    fn default() -> Self {
        Self {
            default_mode: EditorMode::Standard,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeyboardProfile {
    #[default]
    Vim,
    Hybrid,
    Standard,
}

impl KeyboardProfile {
    pub const fn vim_enabled(self) -> bool {
        matches!(self, Self::Vim | Self::Hybrid)
    }

    pub const fn standard_enabled(self) -> bool {
        matches!(self, Self::Standard | Self::Hybrid)
    }

    const fn as_str(self) -> &'static str {
        match self {
            Self::Vim => "vim",
            Self::Hybrid => "hybrid",
            Self::Standard => "standard",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct KeyboardSettings {
    pub profile: KeyboardProfile,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RepositoryGrouping {
    #[default]
    Staging,
    FileState,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RepositorySort {
    #[default]
    Path,
    FileName,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RepositoryView {
    #[default]
    Flat,
    Tree,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RepositoryPrimaryAction {
    #[default]
    OpenFile,
    OpenDiff,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RepositorySettings {
    pub grouping: RepositoryGrouping,
    pub sort: RepositorySort,
    pub view: RepositoryView,
    pub primary_action: RepositoryPrimaryAction,
    pub commit_subject_limit: usize,
    pub commit_author_name: Option<String>,
    pub commit_author_email: Option<String>,
    pub commit_sign_off: bool,
}

impl Default for RepositorySettings {
    fn default() -> Self {
        Self {
            grouping: RepositoryGrouping::default(),
            sort: RepositorySort::default(),
            view: RepositoryView::default(),
            primary_action: RepositoryPrimaryAction::default(),
            commit_subject_limit: 72,
            commit_author_name: None,
            commit_author_email: None,
            commit_sign_off: false,
        }
    }
}

impl Default for KeyboardSettings {
    fn default() -> Self {
        Self {
            profile: KeyboardProfile::Vim,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeymapSettings {
    #[serde(default = "keymaps_version")]
    pub version: u32,
    #[serde(default)]
    pub bindings: BTreeMap<String, String>,
}

const fn keymaps_version() -> u32 {
    KEYMAPS_VERSION
}

impl Default for KeymapSettings {
    fn default() -> Self {
        Self {
            version: KEYMAPS_VERSION,
            bindings: BTreeMap::new(),
        }
    }
}

impl KeymapSettings {
    pub fn decode(source: &str) -> Result<Self, String> {
        let keymaps: Self = serde_json::from_str(source)
            .map_err(|error| format!("keymaps.json is invalid: {error}"))?;
        if keymaps.version != KEYMAPS_VERSION {
            return Err(format!(
                "keymaps.json version {} is unsupported; expected {KEYMAPS_VERSION}",
                keymaps.version
            ));
        }
        let mut sequences = HashSet::new();
        for (command, sequence) in &keymaps.bindings {
            if command.trim().is_empty() {
                return Err("keymaps.json contains an empty command id".into());
            }
            validate_leader_sequence(sequence)?;
            if !sequence.is_empty() && !sequences.insert(sequence) {
                return Err(format!("keymaps.json assigns {sequence:?} more than once"));
            }
        }
        Ok(keymaps)
    }

    pub fn encode(&self) -> Result<String, String> {
        serde_json::to_string_pretty(self)
            .map(|source| format!("{source}\n"))
            .map_err(|error| format!("serializing keymaps.json failed: {error}"))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct QueryBindingSettings {
    version: u32,
    bindings: BTreeMap<String, Vec<String>>,
}

impl Default for QueryBindingSettings {
    fn default() -> Self {
        Self {
            version: QUERY_BINDINGS_VERSION,
            bindings: BTreeMap::new(),
        }
    }
}

fn validate_leader_sequence(sequence: &str) -> Result<(), String> {
    if sequence.is_empty() {
        return Ok(());
    }
    let tokens = sequence.split_whitespace().collect::<Vec<_>>();
    if tokens.len() < 2 || tokens.first() != Some(&"<leader>") {
        return Err(format!(
            "keymap sequence {sequence:?} must look like \"<leader> h\" or \"<leader> g c\""
        ));
    }
    if tokens[1..].iter().any(|token| {
        token.len() != 1
            || !token
                .chars()
                .all(|character| character.is_ascii_alphanumeric())
    }) {
        return Err(format!(
            "keymap sequence {sequence:?} may only contain single ASCII letters or digits after <leader>"
        ));
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct UserSettings {
    #[serde(default = "settings_version")]
    pub version: u32,
    pub editor: EditorSettings,
    pub appearance: AppearanceSettings,
    pub keyboard: KeyboardSettings,
    pub data: DataSettings,
    pub ui: UiSettings,
    pub repository: RepositorySettings,
}

impl Default for UserSettings {
    fn default() -> Self {
        Self {
            version: SETTINGS_VERSION,
            editor: EditorSettings::default(),
            appearance: AppearanceSettings::default(),
            keyboard: KeyboardSettings::default(),
            data: DataSettings::default(),
            ui: UiSettings::default(),
            repository: RepositorySettings::default(),
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

    pub fn keymaps_path(&self) -> PathBuf {
        self.path.with_file_name("keymaps.json")
    }

    pub fn query_bindings_path(&self) -> PathBuf {
        self.path.with_file_name("query-bindings.json")
    }

    pub fn themes_dir(&self) -> PathBuf {
        self.path.with_file_name("themes")
    }

    pub fn theme_path(&self, name: &str) -> Result<PathBuf, String> {
        if name.is_empty()
            || !name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        {
            return Err(
                "theme id may contain only ASCII letters, numbers, hyphens, and underscores".into(),
            );
        }
        Ok(self.themes_dir().join(format!("{name}.toml")))
    }

    pub fn load_theme(&self, name: &str) -> Result<Theme, String> {
        if let Some(theme) = Theme::builtin(name) {
            return Ok(theme);
        }
        let path = self.theme_path(name)?;
        let source = std::fs::read_to_string(&path)
            .map_err(|error| format!("reading theme {} failed: {error}", path.display()))?;
        ThemeConfig::decode(&source)
            .map(|config| config.theme())
            .map_err(|error| format!("{}: {error}", path.display()))
    }

    pub fn read_theme_text(&self, name: &str) -> Result<String, String> {
        if let Some(source) = Theme::builtin_source(name) {
            return Ok(source.to_owned());
        }
        let path = self.theme_path(name)?;
        std::fs::read_to_string(&path)
            .map_err(|error| format!("reading theme {} failed: {error}", path.display()))
    }

    pub fn save_theme_text(&self, name: &str, source: &str) -> Result<Theme, String> {
        if Theme::builtin(name).is_some() {
            return Err("built-in themes must be copied before editing".into());
        }
        let theme = ThemeConfig::decode(source)?.theme();
        let path = self.theme_path(name)?;
        let _guard = self
            .write_lock
            .lock()
            .map_err(|_| "settings write lock poisoned".to_string())?;
        write_atomic(&path, source, "toml")?;
        Ok(theme)
    }

    /// Return an editable custom theme id, copying a built-in without
    /// overwriting any existing user theme.
    pub fn make_theme_editable(&self, name: &str) -> Result<String, String> {
        if Theme::builtin(name).is_none() {
            self.load_theme(name)?;
            return Ok(name.to_owned());
        }
        let source = self.read_theme_text(name)?;
        let base = format!("{name}-custom");
        let id = self.unique_theme_id(&base)?;
        self.save_theme_text(&id, &source)?;
        Ok(id)
    }

    pub fn import_theme_file(&self, source_path: &Path) -> Result<String, String> {
        let source = std::fs::read_to_string(source_path)
            .map_err(|error| format!("reading theme {} failed: {error}", source_path.display()))?;
        ThemeConfig::decode(&source)?;
        let stem = source_path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or("imported-theme");
        let base = normalize_theme_id(stem);
        let id = self.unique_theme_id(&base)?;
        self.save_theme_text(&id, &source)?;
        Ok(id)
    }

    pub fn export_theme_file(&self, name: &str, destination: &Path) -> Result<(), String> {
        let source = self.read_theme_text(name)?;
        ThemeConfig::decode(&source)?;
        write_atomic(destination, &source, "toml")
    }

    pub fn list_custom_themes(&self) -> Result<Vec<String>, String> {
        let entries = match std::fs::read_dir(self.themes_dir()) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(format!("reading themes directory failed: {error}")),
        };
        let mut themes = entries
            .filter_map(Result::ok)
            .filter_map(|entry| {
                let path = entry.path();
                (path.extension().and_then(|extension| extension.to_str()) == Some("toml"))
                    .then(|| path.file_stem()?.to_str().map(str::to_owned))
                    .flatten()
            })
            .filter(|name| self.theme_path(name).is_ok() && Theme::builtin(name).is_none())
            .collect::<Vec<_>>();
        themes.sort();
        themes.dedup();
        Ok(themes)
    }

    fn unique_theme_id(&self, base: &str) -> Result<String, String> {
        for suffix in 1..=10_000 {
            let candidate = if suffix == 1 {
                base.to_owned()
            } else {
                format!("{base}-{suffix}")
            };
            if Theme::builtin(&candidate).is_none() && !self.theme_path(&candidate)?.exists() {
                return Ok(candidate);
            }
        }
        Err("could not allocate a unique theme id".into())
    }

    pub fn load_query_bindings(&self) -> Result<BTreeMap<String, Vec<String>>, String> {
        let path = self.query_bindings_path();
        let source = match std::fs::read_to_string(&path) {
            Ok(source) => source,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(BTreeMap::new());
            }
            Err(error) => return Err(format!("reading {} failed: {error}", path.display())),
        };
        let stored: QueryBindingSettings = serde_json::from_str(&source)
            .map_err(|error| format!("{} is invalid: {error}", path.display()))?;
        if stored.version != QUERY_BINDINGS_VERSION {
            return Err(format!(
                "{} version {} is unsupported; expected {QUERY_BINDINGS_VERSION}",
                path.display(),
                stored.version
            ));
        }
        Ok(stored.bindings)
    }

    pub fn save_query_bindings(
        &self,
        bindings: &BTreeMap<String, Vec<String>>,
    ) -> Result<(), String> {
        let _guard = self
            .write_lock
            .lock()
            .map_err(|_| "settings write lock poisoned".to_string())?;
        let stored = QueryBindingSettings {
            version: QUERY_BINDINGS_VERSION,
            bindings: bindings.clone(),
        };
        let source = serde_json::to_string_pretty(&stored)
            .map(|source| format!("{source}\n"))
            .map_err(|error| format!("serializing query bindings failed: {error}"))?;
        write_atomic(&self.query_bindings_path(), &source, "json")
    }

    pub fn load_keymaps(&self) -> Result<KeymapSettings, String> {
        let path = self.keymaps_path();
        match std::fs::read_to_string(&path) {
            Ok(source) => KeymapSettings::decode(&source),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let keymaps = KeymapSettings::default();
                self.save_keymaps(&keymaps)?;
                Ok(keymaps)
            }
            Err(error) => Err(format!("reading {} failed: {error}", path.display())),
        }
    }

    pub fn read_keymaps_text(&self) -> Result<String, String> {
        self.load_keymaps()?.encode()
    }

    pub fn save_keymaps(&self, keymaps: &KeymapSettings) -> Result<(), String> {
        self.save_keymaps_text(&keymaps.encode()?).map(|_| ())
    }

    pub fn save_keymaps_text(&self, source: &str) -> Result<KeymapSettings, String> {
        let keymaps = KeymapSettings::decode(source)?;
        let _guard = self
            .write_lock
            .lock()
            .map_err(|_| "settings write lock poisoned".to_string())?;
        write_atomic(&self.keymaps_path(), source, "json")?;
        Ok(keymaps)
    }

    pub fn load(&self) -> Result<UserSettings, String> {
        let source = std::fs::read_to_string(&self.path)
            .map_err(|error| format!("reading {} failed: {error}", self.path.display()))?;
        let settings = UserSettings::decode(&source)?;
        self.load_theme(&settings.appearance.theme)?;
        Ok(settings)
    }

    pub fn read_text(&self) -> Result<String, String> {
        std::fs::read_to_string(&self.path)
            .map_err(|error| format!("reading {} failed: {error}", self.path.display()))
    }

    pub fn save(&self, settings: &UserSettings) -> Result<(), String> {
        self.load_theme(&settings.appearance.theme)?;
        self.write_validated(&settings.encode()?)
    }

    pub fn save_text(&self, source: &str) -> Result<UserSettings, String> {
        let settings = UserSettings::decode(source)?;
        self.load_theme(&settings.appearance.theme)?;
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

    /// Update the IDE keymap profile while preserving hand-written settings.
    pub fn save_keyboard_profile(&self, profile: KeyboardProfile) -> Result<UserSettings, String> {
        let _guard = self
            .write_lock
            .lock()
            .map_err(|_| "settings write lock poisoned".to_string())?;
        let source = std::fs::read_to_string(&self.path)
            .map_err(|error| format!("reading {} failed: {error}", self.path.display()))?;
        let mut document = source
            .parse::<DocumentMut>()
            .map_err(|error| format!("settings.toml is invalid: {error}"))?;
        let decor = document["keyboard"]["profile"]
            .as_value()
            .map(|value| value.decor().clone());
        let mut profile_value = Value::from(profile.as_str());
        if let Some(decor) = decor {
            *profile_value.decor_mut() = decor;
        }
        document["keyboard"]["profile"] = Item::Value(profile_value);
        let updated = document.to_string();
        let settings = UserSettings::decode(&updated)?;
        self.write_source(&updated)?;
        Ok(settings)
    }

    /// Update result selection aggregates while preserving hand-written settings.
    pub fn save_selection_aggregates(&self, visible: bool) -> Result<UserSettings, String> {
        let _guard = self
            .write_lock
            .lock()
            .map_err(|_| "settings write lock poisoned".to_string())?;
        let source = std::fs::read_to_string(&self.path)
            .map_err(|error| format!("reading {} failed: {error}", self.path.display()))?;
        let mut document = source
            .parse::<DocumentMut>()
            .map_err(|error| format!("settings.toml is invalid: {error}"))?;
        let decor = document["data"]["selection_aggregates"]
            .as_value()
            .map(|value| value.decor().clone());
        let mut visible_value = Value::from(visible);
        if let Some(decor) = decor {
            *visible_value.decor_mut() = decor;
        }
        document["data"]["selection_aggregates"] = Item::Value(visible_value);
        let updated = document.to_string();
        let settings = UserSettings::decode(&updated)?;
        self.write_source(&updated)?;
        Ok(settings)
    }

    /// Update query/result placement while preserving hand-written settings.
    pub fn save_query_results_placement(
        &self,
        placement: QueryResultsPlacement,
    ) -> Result<UserSettings, String> {
        let _guard = self
            .write_lock
            .lock()
            .map_err(|_| "settings write lock poisoned".to_string())?;
        let source = std::fs::read_to_string(&self.path)
            .map_err(|error| format!("reading {} failed: {error}", self.path.display()))?;
        let mut document = source
            .parse::<DocumentMut>()
            .map_err(|error| format!("settings.toml is invalid: {error}"))?;
        let decor = document["data"]["query_results_placement"]
            .as_value()
            .map(|value| value.decor().clone());
        let mut placement_value = Value::from(placement.as_str());
        if let Some(decor) = decor {
            *placement_value.decor_mut() = decor;
        }
        document["data"]["query_results_placement"] = Item::Value(placement_value);
        let updated = document.to_string();
        let settings = UserSettings::decode(&updated)?;
        self.write_source(&updated)?;
        Ok(settings)
    }

    /// Update recent-object tracking while preserving hand-written settings.
    pub fn save_recent_objects(&self, enabled: bool) -> Result<UserSettings, String> {
        let _guard = self
            .write_lock
            .lock()
            .map_err(|_| "settings write lock poisoned".to_string())?;
        let source = std::fs::read_to_string(&self.path)
            .map_err(|error| format!("reading {} failed: {error}", self.path.display()))?;
        let mut document = source
            .parse::<DocumentMut>()
            .map_err(|error| format!("settings.toml is invalid: {error}"))?;
        if document.get("ui").is_none() {
            document["ui"] = Item::Table(toml_edit::Table::new());
        }
        let decor = document["ui"]["recent_objects"]
            .as_value()
            .map(|value| value.decor().clone());
        let mut enabled_value = Value::from(enabled);
        if let Some(decor) = decor {
            *enabled_value.decor_mut() = decor;
        }
        document["ui"]["recent_objects"] = Item::Value(enabled_value);
        let updated = document.to_string();
        let settings = UserSettings::decode(&updated)?;
        self.write_source(&updated)?;
        Ok(settings)
    }

    /// Update the selected theme while preserving hand-written settings.
    pub fn save_theme(&self, theme: &str) -> Result<UserSettings, String> {
        self.load_theme(theme)?;
        let _guard = self
            .write_lock
            .lock()
            .map_err(|_| "settings write lock poisoned".to_string())?;
        let source = std::fs::read_to_string(&self.path)
            .map_err(|error| format!("reading {} failed: {error}", self.path.display()))?;
        let mut document = source
            .parse::<DocumentMut>()
            .map_err(|error| format!("settings.toml is invalid: {error}"))?;
        if document.get("appearance").is_none() {
            document["appearance"] = Item::Table(toml_edit::Table::new());
        }
        let decor = document
            .get("appearance")
            .and_then(Item::as_table)
            .and_then(|appearance| appearance.get("theme"))
            .and_then(Item::as_value)
            .map(|value| value.decor().clone());
        let mut theme_value = Value::from(theme);
        if let Some(decor) = decor {
            *theme_value.decor_mut() = decor;
        }
        document["appearance"]
            .as_table_mut()
            .expect("appearance was created as a table")
            .insert("theme", Item::Value(theme_value));
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
        write_atomic(&self.path, source, "toml")
    }
}

fn normalize_theme_id(value: &str) -> String {
    let mut normalized = String::new();
    let mut separator = false;
    for character in value.chars() {
        if character.is_ascii_alphanumeric() {
            normalized.push(character.to_ascii_lowercase());
            separator = false;
        } else if !normalized.is_empty() && !separator {
            normalized.push('-');
            separator = true;
        }
    }
    while normalized.ends_with('-') {
        normalized.pop();
    }
    if normalized.is_empty() {
        "imported-theme".into()
    } else {
        normalized
    }
}

fn write_atomic(path: &Path, source: &str, extension: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("creating {} failed: {error}", parent.display()))?;
    }
    let temporary = path.with_extension(format!("{extension}.tmp"));
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
    if path.exists() {
        std::fs::remove_file(path)
            .map_err(|error| format!("replacing {} failed: {error}", path.display()))?;
    }
    std::fs::rename(&temporary, path)
        .map_err(|error| format!("installing {} failed: {error}", path.display()))
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
        assert!(source.contains("profile = \"vim\""));
        assert!(source.contains("theme = \"ayu-dark\""));
        assert!(source.contains("selection_aggregates = false"));
        assert!(source.contains("query_results_placement = \"right\""));
        assert!(source.contains("recent_objects = true"));
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

    #[test]
    fn keyboard_profile_update_supports_all_three_states() {
        let directory = tempfile::tempdir().unwrap();
        let store = SettingsStore::new(directory.path().join("settings.toml"));
        store.save(&UserSettings::default()).unwrap();

        for profile in [
            KeyboardProfile::Vim,
            KeyboardProfile::Hybrid,
            KeyboardProfile::Standard,
        ] {
            let settings = store.save_keyboard_profile(profile).unwrap();
            assert_eq!(settings.keyboard.profile, profile);
        }
    }

    #[test]
    fn selection_aggregate_update_preserves_unrelated_settings() {
        let directory = tempfile::tempdir().unwrap();
        let store = SettingsStore::new(directory.path().join("settings.toml"));
        store.save(&UserSettings::default()).unwrap();

        let settings = store.save_selection_aggregates(true).unwrap();

        assert!(settings.data.selection_aggregates);
        assert_eq!(settings.appearance.theme, "ayu-dark");
        assert!(store
            .read_text()
            .unwrap()
            .contains("selection_aggregates = true"));
    }

    #[test]
    fn result_placement_update_preserves_unrelated_settings() {
        let directory = tempfile::tempdir().unwrap();
        let store = SettingsStore::new(directory.path().join("settings.toml"));
        store.save(&UserSettings::default()).unwrap();

        let settings = store
            .save_query_results_placement(QueryResultsPlacement::Bottom)
            .unwrap();

        assert_eq!(
            settings.data.query_results_placement,
            QueryResultsPlacement::Bottom
        );
        assert_eq!(settings.appearance.theme, "ayu-dark");
        assert!(store
            .read_text()
            .unwrap()
            .contains("query_results_placement = \"bottom\""));
    }

    #[test]
    fn recent_objects_update_preserves_unrelated_settings() {
        let directory = tempfile::tempdir().unwrap();
        let store = SettingsStore::new(directory.path().join("settings.toml"));
        store.save(&UserSettings::default()).unwrap();

        let settings = store.save_recent_objects(false).unwrap();

        assert!(!settings.ui.recent_objects);
        assert_eq!(settings.appearance.theme, "ayu-dark");
        assert!(store
            .read_text()
            .unwrap()
            .contains("recent_objects = false"));
    }

    #[test]
    fn keymaps_json_round_trips_and_rejects_duplicate_sequences() {
        let directory = tempfile::tempdir().unwrap();
        let store = SettingsStore::new(directory.path().join("settings.toml"));
        let mut keymaps = KeymapSettings::default();
        keymaps
            .bindings
            .insert("workspace.focus-connections".into(), "<leader> g c".into());
        keymaps
            .bindings
            .insert("workspace.previous-tab".into(), "<leader> h".into());

        store.save_keymaps(&keymaps).unwrap();
        assert_eq!(store.load_keymaps().unwrap(), keymaps);
        assert_eq!(store.keymaps_path(), directory.path().join("keymaps.json"));

        let invalid = r#"{
          "version": 1,
          "bindings": {
            "workspace.focus-connections": "<leader> g c",
            "workspace.focus-editor": "<leader> g c"
          }
        }"#;
        assert!(store.save_keymaps_text(invalid).is_err());
        assert_eq!(store.load_keymaps().unwrap(), keymaps);
    }

    #[test]
    fn query_bindings_survive_a_new_store_instance() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.toml");
        let store = SettingsStore::new(&path);
        let bindings =
            BTreeMap::from([("query-fingerprint".into(), vec!["42".into(), "open".into()])]);

        store.save_query_bindings(&bindings).unwrap();

        assert_eq!(
            SettingsStore::new(path).load_query_bindings().unwrap(),
            bindings
        );
    }

    #[test]
    fn custom_theme_loads_from_the_themes_directory() {
        let directory = tempfile::tempdir().unwrap();
        let store = SettingsStore::new(directory.path().join("settings.toml"));
        std::fs::create_dir(store.themes_dir()).unwrap();
        std::fs::write(
            store.theme_path("personal").unwrap(),
            "version = 1\nname = \"Personal\"\nappearance = \"dark\"\n[colors]\naccent = \"#ff0000\"\n",
        )
        .unwrap();

        let theme = store.load_theme("personal").unwrap();
        assert_eq!(theme.colors.accent, gpui::rgb(0xff0000).into());
        assert!(store.load_theme("../escape").is_err());
    }

    #[test]
    fn settings_reject_a_missing_theme_without_replacing_the_file() {
        let directory = tempfile::tempdir().unwrap();
        let store = SettingsStore::new(directory.path().join("settings.toml"));
        store.save(&UserSettings::default()).unwrap();
        let original = store.read_text().unwrap();
        let invalid = original.replace("theme = \"ayu-dark\"", "theme = \"missing\"");

        assert!(store.save_text(&invalid).is_err());
        assert_eq!(store.read_text().unwrap(), original);
    }

    #[test]
    fn themes_copy_import_and_export_without_overwriting() {
        let directory = tempfile::tempdir().unwrap();
        let store = SettingsStore::new(directory.path().join("settings.toml"));
        store.save(&UserSettings::default()).unwrap();

        let editable = store.make_theme_editable("ayu-dark").unwrap();
        assert_eq!(editable, "ayu-dark-custom");
        assert!(store.theme_path(&editable).unwrap().exists());
        assert_eq!(
            store.make_theme_editable("ayu-dark").unwrap(),
            "ayu-dark-custom-2"
        );

        let exported = directory.path().join("Shared Theme.toml");
        store.export_theme_file(&editable, &exported).unwrap();
        let imported = store.import_theme_file(&exported).unwrap();
        assert_eq!(imported, "shared-theme");
        assert_eq!(
            store.list_custom_themes().unwrap(),
            vec![
                "ayu-dark-custom".to_string(),
                "ayu-dark-custom-2".to_string(),
                "shared-theme".to_string(),
            ]
        );
        assert_eq!(
            store.load_theme(&imported).unwrap(),
            store.load_theme(&editable).unwrap()
        );
    }
}
