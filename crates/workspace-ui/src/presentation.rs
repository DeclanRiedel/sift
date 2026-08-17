use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

const PRESENTATION_VERSION: u32 = 1;
const MIN_WINDOW_WIDTH: f32 = 720.0;
const MIN_WINDOW_HEIGHT: f32 = 480.0;

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ItemKind {
    Query,
    Schema,
    Welcome,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ItemPresentation {
    pub id: u64,
    pub kind: ItemKind,
    pub title: String,
    pub dirty: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PanePresentation {
    pub id: u64,
    pub items: Vec<ItemPresentation>,
    pub active_item: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkspacePresentation {
    pub left_dock: DockPresentation,
    pub right_dock: DockPresentation,
    pub bottom_dock: DockPresentation,
    pub panes: Vec<PanePresentation>,
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
    pub window: WindowPresentation,
    pub workspace: WorkspacePresentation,
}

impl Default for PresentationState {
    fn default() -> Self {
        Self {
            version: PRESENTATION_VERSION,
            dark_theme: true,
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
                    open: true,
                    size: 260.0,
                },
                panes: vec![PanePresentation {
                    id: 1,
                    items: vec![ItemPresentation {
                        id: 1,
                        kind: ItemKind::Query,
                        title: "query.sql".into(),
                        dirty: false,
                    }],
                    active_item: 0,
                }],
                active_pane: 0,
                workspace_id: None,
                instance_id: Some("local".into()),
            },
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

/// Small crash-safe store for presentation references only. Authoritative
/// workspace, query, schema, and result data never enter this file.
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
        let state = PresentationState::default();
        let bytes = state.encode().unwrap();
        assert_eq!(PresentationState::decode(&bytes), state);
        let json = String::from_utf8(bytes).unwrap();
        for forbidden in ["password", "result_rows", "query_text", "credential"] {
            assert!(!json.contains(forbidden));
        }
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
