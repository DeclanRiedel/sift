use std::collections::HashMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

const PRESENTATION_VERSION: u32 = 1;
const MIN_WINDOW_WIDTH: f32 = 720.0;
const MIN_WINDOW_HEIGHT: f32 = 480.0;

const fn is_false(value: &bool) -> bool {
    !*value
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl Rect {
    fn intersects(self, other: Self) -> bool {
        self.x < other.x + other.width
            && self.x + self.width > other.x
            && self.y < other.y + other.height
            && self.y + self.height > other.y
    }

    pub fn physical_size(self, scale_factor: f32) -> (u32, u32) {
        let scale_factor = scale_factor.max(0.1);
        (
            (self.width * scale_factor).round() as u32,
            (self.height * scale_factor).round() as u32,
        )
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WindowPresentation {
    pub bounds: Rect,
    pub maximized: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DockPresentation {
    pub open: bool,
    pub size: f32,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LeftPanel {
    #[default]
    Connections,
    Git,
    Collaboration,
    QueryOutline,
    SavedQueries,
}

impl LeftPanel {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Connections => "Connections",
            Self::Git => "Git",
            Self::Collaboration => "Collab",
            Self::QueryOutline => "Query Outline",
            Self::SavedQueries => "Saved Queries",
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BottomTool {
    #[default]
    #[serde(alias = "problems")]
    Console,
    Monitor,
    Automations,
}

impl BottomTool {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Console => "Console",
            Self::Monitor => "Monitor",
            Self::Automations => "Automations",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ItemKind {
    Query,
    Configuration,
    Problems,
    Notifications,
    GitDiff,
    Schema,
    Welcome,
}

/// Durable identity needed to reopen a database-backed tab. Contains only
/// public object/profile references; credentials and result data stay out of
/// presentation state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DatabaseObjectSource {
    pub instance_id: String,
    pub tenant_id: i64,
    pub profile_id: i64,
    pub profile_name: String,
    pub provider_id: sift_protocol::ProviderId,
    pub catalog: Option<String>,
    pub schema: String,
    pub object: String,
    pub object_kind: sift_protocol::ObjectKind,
    #[serde(default)]
    pub last_refreshed_at_ms: Option<u64>,
}

/// Stable server references for a room-owned query document. SQL and CRDT
/// bytes deliberately stay out of presentation state; reconnect resolves this
/// identity through the room API and WebSocket.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoomDocumentSource {
    pub instance_id: String,
    pub room_id: i64,
    pub document_id: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ItemSource {
    DatabaseObject(DatabaseObjectSource),
    RoomDocument(RoomDocumentSource),
}

/// What a finished run left behind, durable across restarts.
///
/// This is deliberately a *reference*, not a result: row and cell values are
/// server-owned product data and never enter client-local presentation state
/// (ADR-034). Restoring one tells the user what the tab last produced and lets
/// them re-run it; it never pretends the rows are still available.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResultReference {
    /// Server cursor the run streamed from, when it had one. Retained for
    /// correlation with server-side history, not as a promise it still exists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor_id: Option<u64>,
    /// Rows the client actually received, after the retention bound.
    pub row_count: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub affected_rows: Option<u64>,
    /// True when the run had more rows than were retained or delivered.
    #[serde(default, skip_serializing_if = "is_false")]
    pub has_more: bool,
    pub completed_at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ItemPresentation {
    pub id: u64,
    pub kind: ItemKind,
    pub title: String,
    pub dirty: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<ItemSource>,
    /// Reference to this tab's last completed run. Never its rows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_result: Option<ResultReference>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PanePresentation {
    pub id: u64,
    pub items: Vec<ItemPresentation>,
    pub active_item: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PaneAxis {
    Horizontal,
    Vertical,
}

/// Recursive, client-local pane geometry. Pane contents remain in
/// `WorkspacePresentation::panes`; leaves reference them by stable id.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PaneLayoutPresentation {
    Pane {
        pane_id: u64,
    },
    Split {
        axis: PaneAxis,
        children: Vec<PaneLayoutPresentation>,
        flexes: Vec<f32>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkspacePresentation {
    pub left_dock: DockPresentation,
    pub right_dock: DockPresentation,
    pub bottom_dock: DockPresentation,
    #[serde(default)]
    pub left_panel: LeftPanel,
    #[serde(default)]
    pub bottom_tool: BottomTool,
    pub panes: Vec<PanePresentation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pane_layout: Option<PaneLayoutPresentation>,
    /// Legacy flat horizontal proportions, retained only to migrate older
    /// presentation files into `pane_layout`.
    #[serde(default)]
    pub pane_flexes: Vec<f32>,
    pub active_pane: usize,
    #[serde(default)]
    pub workspace_id: Option<i64>,
    #[serde(default)]
    pub instance_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PresentationState {
    pub version: u32,
    pub dark_theme: bool,
    #[serde(default, rename = "vim_mode_default", skip_serializing_if = "is_false")]
    pub legacy_vim_mode_default: bool,
    pub window: WindowPresentation,
    pub workspace: WorkspacePresentation,
    /// Last presentation for each Sift server. `workspace` remains the active
    /// entry for backwards compatibility; inactive entries let server
    /// switching restore distinct IDE layouts instead of leaking one server's
    /// tabs into another.
    #[serde(default)]
    pub instance_workspaces: HashMap<String, WorkspacePresentation>,
    /// Client-local commit drafts keyed by workspace id. These never enter a
    /// workspace projection or repository file.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub repository_commit_drafts: HashMap<i64, String>,
}

impl Default for PresentationState {
    fn default() -> Self {
        Self {
            version: PRESENTATION_VERSION,
            dark_theme: true,
            legacy_vim_mode_default: false,
            window: WindowPresentation {
                bounds: Rect {
                    x: 100.0,
                    y: 80.0,
                    width: 1280.0,
                    height: 800.0,
                },
                maximized: false,
            },
            workspace: WorkspacePresentation {
                left_dock: DockPresentation {
                    open: true,
                    size: 232.0,
                },
                right_dock: DockPresentation {
                    open: true,
                    size: 224.0,
                },
                bottom_dock: DockPresentation {
                    open: false,
                    size: 260.0,
                },
                left_panel: LeftPanel::Connections,
                bottom_tool: BottomTool::Console,
                panes: vec![PanePresentation {
                    id: 1,
                    items: vec![ItemPresentation {
                        id: 1,
                        kind: ItemKind::Query,
                        title: "query.sql".into(),
                        dirty: false,
                        source: None,
                        last_result: None,
                    }],
                    active_item: 0,
                }],
                pane_layout: Some(PaneLayoutPresentation::Pane { pane_id: 1 }),
                pane_flexes: vec![1.0],
                active_pane: 0,
                workspace_id: None,
                instance_id: Some("local".into()),
            },
            instance_workspaces: HashMap::new(),
            repository_commit_drafts: HashMap::new(),
        }
    }
}

impl PresentationState {
    pub fn recover_for_displays(mut self, displays: &[Rect]) -> Self {
        let fallback = displays.first().copied().unwrap_or(Rect {
            x: 0.0,
            y: 0.0,
            width: 1280.0,
            height: 800.0,
        });
        self.window.bounds.width = self
            .window
            .bounds
            .width
            .clamp(MIN_WINDOW_WIDTH, fallback.width.max(MIN_WINDOW_WIDTH));
        self.window.bounds.height = self
            .window
            .bounds
            .height
            .clamp(MIN_WINDOW_HEIGHT, fallback.height.max(MIN_WINDOW_HEIGHT));
        if !displays
            .iter()
            .any(|display| self.window.bounds.intersects(*display))
        {
            self.window.bounds.x = fallback.x + (fallback.width - self.window.bounds.width) / 2.0;
            self.window.bounds.y = fallback.y + (fallback.height - self.window.bounds.height) / 2.0;
            self.window.maximized = false;
        }
        self
    }

    pub fn decode(bytes: &[u8]) -> Self {
        serde_json::from_slice::<Self>(bytes)
            .ok()
            .filter(|state| state.version == PRESENTATION_VERSION)
            .unwrap_or_default()
    }

    pub fn encode(&self) -> Result<Vec<u8>, serde_json::Error> {
        serde_json::to_vec_pretty(self)
    }
}

/// Small crash-safe client-local store for presentation references only.
/// Authoritative workspace, query, schema, and result data never enter this
/// file. Desktop composition chooses an OS-account-local path; no server or
/// shared room reads or writes this store.
#[derive(Debug, Clone)]
pub struct PresentationStore {
    path: PathBuf,
    write_lock: Arc<Mutex<()>>,
}

impl PresentationStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            write_lock: Arc::new(Mutex::new(())),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn load(&self) -> PresentationState {
        std::fs::read(&self.path)
            .map(|bytes| PresentationState::decode(&bytes))
            .unwrap_or_default()
    }

    pub fn save(&self, state: &PresentationState) -> std::io::Result<()> {
        let _guard = self
            .write_lock
            .lock()
            .map_err(|_| std::io::Error::other("presentation write lock poisoned"))?;
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let temporary = self.path.with_extension("json.tmp");
        let bytes = state
            .encode()
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
        let mut options = std::fs::OpenOptions::new();
        options.create(true).truncate(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        let mut file = options.open(&temporary)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        drop(file);
        // `rename` replaces atomically on Unix. Windows' standard-library
        // implementation does not replace an existing destination, so the
        // serialized writer removes only this exact presentation file first.
        #[cfg(windows)]
        if self.path.exists() {
            std::fs::remove_file(&self.path)?;
        }
        std::fs::rename(temporary, &self.path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presentation_round_trip_contains_references_not_product_data() {
        let mut state = PresentationState::default();
        let mut remote = state.workspace.clone();
        remote.instance_id = Some("hosted:team".into());
        remote.workspace_id = Some(42);
        remote.panes[0].items[0].source = Some(ItemSource::DatabaseObject(DatabaseObjectSource {
            instance_id: "hosted:team".into(),
            tenant_id: 7,
            profile_id: 9,
            profile_name: "Warehouse".into(),
            provider_id: sift_protocol::ProviderId::new("sift/postgres").unwrap(),
            catalog: Some("analytics".into()),
            schema: "public".into(),
            object: "events".into(),
            object_kind: sift_protocol::ObjectKind::View,
            last_refreshed_at_ms: Some(123),
        }));
        state
            .instance_workspaces
            .insert("hosted:team".into(), remote);
        let bytes = state.encode().unwrap();
        assert_eq!(PresentationState::decode(&bytes), state);
        let json = String::from_utf8(bytes).unwrap();
        assert!(!json.contains("vim_mode_default"));
        for forbidden in ["password", "result_rows", "query_text", "credential"] {
            assert!(!json.contains(forbidden));
        }
    }

    #[test]
    fn a_result_reference_survives_restart_without_carrying_rows() {
        let mut state = PresentationState::default();
        state.workspace.panes[0].items[0].last_result = Some(ResultReference {
            cursor_id: Some(31),
            row_count: 10_000,
            affected_rows: None,
            has_more: true,
            completed_at_ms: 1_700_000_000_000,
        });

        let encoded = String::from_utf8(state.encode().unwrap()).unwrap();
        let decoded = PresentationState::decode(encoded.as_bytes());

        assert_eq!(decoded, state);
        // The reference is metadata about a run, never the run's output.
        for forbidden in ["rows\":[", "values", "columns"] {
            assert!(
                !encoded.contains(forbidden),
                "{forbidden} leaked into presentation state"
            );
        }
    }

    #[test]
    fn a_tab_without_a_recorded_run_omits_the_reference_entirely() {
        let state = PresentationState::default();
        let encoded = String::from_utf8(state.encode().unwrap()).unwrap();
        assert!(!encoded.contains("last_result"));
        assert!(PresentationState::decode(encoded.as_bytes())
            .workspace
            .panes[0]
            .items[0]
            .last_result
            .is_none());
    }

    #[test]
    fn room_query_persists_identity_without_sql_or_snapshot() {
        let mut state = PresentationState::default();
        state.workspace.panes[0].items[0].source =
            Some(ItemSource::RoomDocument(RoomDocumentSource {
                instance_id: "hosted:team".into(),
                room_id: 7,
                document_id: 11,
            }));

        let encoded = String::from_utf8(state.encode().unwrap()).unwrap();
        let decoded = PresentationState::decode(encoded.as_bytes());

        assert!(!encoded.contains("select "));
        assert_eq!(
            decoded.workspace.panes[0].items[0].source,
            state.workspace.panes[0].items[0].source
        );
    }

    #[test]
    fn legacy_vim_preference_remains_available_for_settings_migration() {
        let state = PresentationState::default();
        let mut json = serde_json::to_value(state).unwrap();
        json.as_object_mut()
            .unwrap()
            .insert("vim_mode_default".into(), true.into());

        let decoded = PresentationState::decode(&serde_json::to_vec(&json).unwrap());
        assert!(decoded.legacy_vim_mode_default);
    }

    #[test]
    fn older_presentation_defaults_new_local_preferences() {
        let state = PresentationState::default();
        let mut json = serde_json::to_value(state).unwrap();
        json.as_object_mut().unwrap().remove("vim_mode_default");
        let workspace = json
            .get_mut("workspace")
            .and_then(serde_json::Value::as_object_mut)
            .unwrap();
        workspace.remove("left_panel");
        workspace.remove("bottom_tool");
        workspace.remove("pane_layout");
        workspace.remove("pane_flexes");

        let decoded = PresentationState::decode(&serde_json::to_vec(&json).unwrap());
        assert_eq!(decoded.workspace.left_panel, LeftPanel::Connections);
        assert_eq!(decoded.workspace.bottom_tool, BottomTool::Console);
        assert!(decoded.workspace.pane_layout.is_none());
        assert!(decoded.workspace.pane_flexes.is_empty());
        assert!(!decoded.legacy_vim_mode_default);
    }

    #[test]
    fn legacy_problems_tool_restores_as_console() {
        let state = PresentationState::default();
        let mut json = serde_json::to_value(state).unwrap();
        json.get_mut("workspace")
            .and_then(serde_json::Value::as_object_mut)
            .unwrap()
            .insert("bottom_tool".into(), "problems".into());

        let decoded = PresentationState::decode(&serde_json::to_vec(&json).unwrap());
        assert_eq!(decoded.workspace.bottom_tool, BottomTool::Console);
    }

    #[test]
    fn off_screen_window_recovers_to_primary_display() {
        let mut state = PresentationState::default();
        state.window.bounds.x = 8_000.0;
        state.window.bounds.y = -4_000.0;
        state.window.maximized = true;
        let display = Rect {
            x: 0.0,
            y: 0.0,
            width: 1920.0,
            height: 1080.0,
        };
        let recovered = state.recover_for_displays(&[display]);
        assert!(recovered.window.bounds.intersects(display));
        assert!(!recovered.window.maximized);
    }

    #[test]
    fn unknown_presentation_version_falls_back_safely() {
        let decoded = PresentationState::decode(br#"{"version":999}"#);
        assert_eq!(decoded, PresentationState::default());
    }

    #[test]
    fn logical_window_size_scales_without_changing_saved_geometry() {
        let bounds = PresentationState::default().window.bounds;
        assert_eq!(bounds.physical_size(1.0), (1280, 800));
        assert_eq!(bounds.physical_size(1.5), (1920, 1200));
        assert_eq!(bounds, PresentationState::default().window.bounds);
    }

    #[test]
    fn store_atomically_restores_presentation_state() {
        let directory = tempfile::tempdir().unwrap();
        let store = PresentationStore::new(directory.path().join("presentation.json"));
        let defaults = PresentationState::default();
        let state = PresentationState {
            dark_theme: false,
            workspace: WorkspacePresentation {
                left_dock: DockPresentation {
                    open: false,
                    ..defaults.workspace.left_dock.clone()
                },
                ..defaults.workspace.clone()
            },
            ..defaults
        };
        store.save(&state).unwrap();
        assert_eq!(store.load(), state);
        assert!(!store.path().with_extension("json.tmp").exists());
    }
}
