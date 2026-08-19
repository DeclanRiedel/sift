use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::sync::Arc;

use gpui::{
    actions, deferred, div, img, prelude::*, px, uniform_list, App, Context, CursorStyle,
    DefiniteLength, Entity, EventEmitter, FocusHandle, Focusable, IntoElement, MouseButton,
    PathPromptOptions, ResizeEdge, Role, ScrollStrategy, SharedString, Subscription, Task,
    UniformListScrollHandle, Window, WindowBounds, WindowControlArea,
};
use sift_api_types::RoomId;
use sift_ui::{
    database_logo, icon, ActiveTheme, Badge, Button, ButtonTone, Clickable, Disableable,
    ErrorBanner, Field, IconButton, IconName, KeyBinding, SectionLabel, TextInput, Theme,
    ThemeMetrics, Toggleable, Tone, Tooltip,
};

use crate::editor::{
    EditorEvent, EditorKeymap, EditorLanguage, QueryDocument, QueryEditor, VimMode,
    EDITOR_GUTTER_WIDTH,
};
use crate::results::{ResultPlacement, ResultState, ResultsView};
use crate::settings::{EditorMode, SettingsStore, UserSettings};

use crate::presentation::{
    BottomTool, DatabaseObjectSource, ItemKind, ItemPresentation, ItemSource, LeftPanel,
    PanePresentation, PresentationState, PresentationStore, WindowPresentation,
    WorkspacePresentation,
};
use crate::{
    ConnectionNavEntry, LifecycleEvent, LifecycleProjection, PresenceEvent, RoomPresenceProjection,
    WorkspaceNavEntry,
};

mod app_bar;
mod bottom_tools;
mod commands;
mod dock_layout;
mod docks;
mod items;
mod status_bar;

pub use commands::{CommandContext, CommandDefinition, CommandId, CommandRegistry, CommandSpec};
pub use docks::{Dock, DockDefinition, DockId, DockPlacement, DockRegistry};
pub use items::{ItemDefinition, ItemRegistry, ItemRuntimeKind};
pub use status_bar::StatusBar;

use app_bar::AppBarMenu;

const PALETTE_VISIBLE_ROWS: usize = 10;
const PALETTE_ROW_HEIGHT: f32 = 30.0;
const DOCK_RESIZE_HANDLE_SIZE: f32 = 7.0;
const PANE_RESIZE_HANDLE_SIZE: f32 = 7.0;
const PANE_MIN_WIDTH: f32 = 180.0;
const RESULT_RESIZE_HANDLE_SIZE: f32 = 7.0;
const RESULT_MIN_EXTENT: f32 = 140.0;
const EDITOR_MIN_EXTENT: f32 = 160.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SqlProblemSeverity {
    Error,
    Warning,
}

impl SqlProblemSeverity {
    const fn label(self) -> &'static str {
        match self {
            Self::Error => "Error",
            Self::Warning => "Warning",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SqlProblem {
    item_id: Option<u64>,
    title: String,
    severity: SqlProblemSeverity,
    message: String,
}

fn quote_identifier(identifier: &str) -> String {
    format!("\"{}\"", identifier.replace('"', "\"\""))
}

fn table_preview_sql(
    provider_id: &sift_protocol::ProviderId,
    schema: &str,
    object: &str,
) -> String {
    let qualified = format!("{}.{}", quote_identifier(schema), quote_identifier(object));
    if provider_id.as_str() == "sift/sql-server" {
        format!("SELECT TOP (100) * FROM {qualified};")
    } else {
        format!("SELECT * FROM {qualified} LIMIT 100;")
    }
}

fn schema_object_kind_label(kind: sift_protocol::ObjectKind) -> &'static str {
    use sift_protocol::ObjectKind;
    match kind {
        ObjectKind::Table => "table",
        ObjectKind::View => "view",
        ObjectKind::MaterializedView => "materialized",
        ObjectKind::ForeignTable => "foreign",
        ObjectKind::PartitionedTable => "partitioned",
        ObjectKind::TableValuedFunction => "table function",
        ObjectKind::ScalarFunction => "function",
        ObjectKind::Procedure => "procedure",
        ObjectKind::Synonym => "synonym",
        ObjectKind::Sequence => "sequence",
        ObjectKind::Trigger => "trigger",
        ObjectKind::Type => "type",
        ObjectKind::Extension => "extension",
    }
}

#[derive(Debug, Clone, Copy)]
struct DockResizeDrag {
    dock: DockId,
}

#[derive(Debug, Clone, Copy)]
struct PaneResizeDrag {
    boundary: usize,
}

fn valid_pane_flexes(mut flexes: Vec<f32>, pane_count: usize) -> Vec<f32> {
    if flexes.len() != pane_count || flexes.iter().any(|flex| !flex.is_finite() || *flex <= 0.0) {
        flexes = vec![1.0; pane_count];
    }
    flexes
}

fn resize_pane_flexes(flexes: &mut [f32], boundary: usize, pointer_x: f32, available_width: f32) {
    if boundary + 1 >= flexes.len() || available_width <= 0.0 {
        return;
    }
    let total = flexes.iter().sum::<f32>();
    let pair_total = flexes[boundary] + flexes[boundary + 1];
    let prefix = flexes[..boundary].iter().sum::<f32>();
    let minimum = (PANE_MIN_WIDTH / available_width * total).min(pair_total / 2.0);
    let requested_left = pointer_x.clamp(0.0, available_width) / available_width * total - prefix;
    let left = requested_left.clamp(minimum, pair_total - minimum);
    flexes[boundary] = left;
    flexes[boundary + 1] = pair_total - left;
}

#[derive(Debug, Clone, Copy)]
struct ResultResizeDrag {
    item_id: u64,
    placement: ResultPlacement,
}

/// A pane tab being dragged. `pane_id` distinguishes a same-pane reorder from
/// a cross-pane move; `item_id` is stable across the drag.
#[derive(Debug, Clone, Copy)]
struct TabDrag {
    pane_id: u64,
    item_id: u64,
}

/// An item detached from one pane, ready to attach to another. Editors, their
/// results surface, and clean-point text move with the tab.
struct TabTransfer {
    item: ItemPresentation,
    editor: Entity<QueryEditor>,
    results: Option<Entity<ResultsView>>,
    clean_text: String,
}

fn optional_u32_field(
    input: &Entity<TextInput>,
    label: &str,
    cx: &App,
) -> Result<Option<u32>, String> {
    let value = input.read(cx).text().trim();
    if value.is_empty() {
        return Ok(None);
    }
    value
        .parse::<u32>()
        .ok()
        .filter(|value| *value > 0)
        .map(Some)
        .ok_or_else(|| format!("{label} must be a positive whole number"))
}

fn optional_pool_min_field(input: &Entity<TextInput>, cx: &App) -> Result<Option<u32>, String> {
    let value = input.read(cx).text().trim();
    if value.is_empty() {
        return Ok(None);
    }
    value
        .parse::<u32>()
        .map(Some)
        .map_err(|_| "Minimum pool size must be a non-negative whole number".into())
}

actions!(
    sift_shell,
    [
        OpenCommandPalette,
        OpenServerConnection,
        DismissModal,
        PaletteUp,
        PaletteDown,
        PaletteConfirm,
        SplitPane,
        FocusNextPane,
        CloseActivePane,
        CloseActiveItem,
        SaveActiveItem,
        ToggleLeftDock,
        ToggleRightDock,
        ToggleBottomDock
    ]
);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Modal {
    CommandPalette,
    ServerPicker,
    ServerConnection,
    InstanceSetup,
    Settings,
    Account,
    DatabaseConnection,
    ConfirmDeleteConnection(ConnectionNavEntry),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DatabaseWizardStep {
    Provider,
    Details,
    Review,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SavedServerProfile {
    pub id: String,
    pub name: String,
    pub base_url: String,
    #[serde(default, skip_serializing)]
    pub has_saved_token: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SavedInstanceRoot {
    pub manifest_id: String,
    pub name: String,
    pub root: std::path::PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstanceCredentialKind {
    GithubOauthClientSecret,
    Postgres,
    SqlServer,
}

impl InstanceCredentialKind {
    fn label(self) -> &'static str {
        match self {
            Self::GithubOauthClientSecret => "GitHub OAuth client secret",
            Self::Postgres => "PostgreSQL password",
            Self::SqlServer => "SQL Server password",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstanceCredentialPresentation {
    pub slot: String,
    pub consumer: String,
    pub kind: InstanceCredentialKind,
    pub readiness: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstancePlanPresentation {
    pub root: std::path::PathBuf,
    pub manifest_id: String,
    pub name: String,
    pub deployment: String,
    pub bind: String,
    pub configuration_digest: String,
    pub lock_digest: String,
    pub principals: usize,
    pub tenants: usize,
    pub memberships: usize,
    pub connections: usize,
    pub extensions: usize,
    pub warnings: Vec<String>,
    pub credentials: Vec<InstanceCredentialPresentation>,
    pub current_generation: Option<u64>,
    pub generation_count: usize,
    pub drifted: bool,
    pub last_apply: Option<String>,
    pub destroy_confirmation_required: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstanceConfigurationPresentation {
    pub root: Option<std::path::PathBuf>,
    pub manifest: String,
    pub source_revision: Option<String>,
    pub name: String,
    pub is_new: bool,
}

#[derive(Clone)]
pub enum InstanceCommand {
    UseLocal,
    Connect {
        profile_id: Option<String>,
        name: String,
        base_url: String,
        bearer_token: Option<String>,
        remember_token: bool,
    },
    Forget {
        profile_id: String,
    },
    ForgetRoot {
        root: std::path::PathBuf,
    },
    InspectRoot {
        root: std::path::PathBuf,
    },
    ApplyRoot {
        root: std::path::PathBuf,
        allow_destroy: bool,
    },
    ImportRootCredential {
        root: std::path::PathBuf,
        slot: String,
        kind: InstanceCredentialKind,
        secret: String,
    },
    StartRoot {
        root: std::path::PathBuf,
    },
    PrepareRootConfiguration {
        root: std::path::PathBuf,
    },
    OpenRootConfiguration {
        root: std::path::PathBuf,
    },
    OpenCurrentConfiguration,
    SaveConfiguration {
        root: Option<std::path::PathBuf>,
        manifest: String,
        expected_source_revision: Option<String>,
        is_new: bool,
    },
    SignInWithPassword {
        username: String,
        password: String,
    },
    SignInWithGithub,
    SignOut {
        everywhere: bool,
    },
}

#[derive(Debug, Clone)]
pub enum InstanceManagerEvent {
    Profiles(Vec<SavedServerProfile>),
    Roots(Vec<SavedInstanceRoot>),
    InstancePlan(Box<InstancePlanPresentation>),
    InstanceConfiguration(Box<InstanceConfigurationPresentation>),
    InstanceOperationPending { message: String },
    Testing,
    Connected { name: String },
    Failed { message: String },
    AuthenticationPending,
    GithubAuthorization { url: String },
    Authenticated { display_name: String },
    SignedOut,
    AuthenticationFailed { message: String },
}

/// Intent carried by a toast so outcomes read at a glance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToastTone {
    Info,
    Success,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Toast {
    pub id: u64,
    pub message: String,
    pub tone: ToastTone,
}

/// Events a pane emits upward to the workspace. A pane never mutates sibling
/// panes or the workspace's pane list directly; it asks its owner instead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PaneEvent {
    /// The pane was interacted with and should become the active pane.
    FocusRequested,
    /// The pane should be removed (its close control was used, or it emptied).
    CloseRequested,
    /// A tab-local close control was used; the workspace owns dirty handling.
    CloseItemRequested { item_id: u64 },
    /// A tab-local dirty prompt chose to discard the item.
    DiscardItemRequested { item_id: u64 },
    /// A configuration tab's dirty prompt requested a save.
    SaveItemRequested { item_id: u64 },
    /// A tab from another pane was dropped onto this pane.
    MoveItemRequested { item_id: u64 },
    /// Active editor state changed. Cursor-only changes do not dirty the tab.
    EditorStateChanged { item_id: u64, dirty: Option<bool> },
    /// An editor requested the workspace-level command palette.
    OpenCommandPaletteRequested,
    /// A query item asked to run SQL; the workspace dispatches it to execution.
    ExecuteRequested { item_id: u64, sql: String },
    /// A database-backed snapshot requested a live refresh. Workspace owns
    /// connection selection and may reconnect before executing.
    RefreshDatabaseItemRequested { item_id: u64 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum DatabaseItemState {
    Live,
    Offline,
    Reconnecting,
    Failed(String),
}

#[derive(Debug, Clone)]
struct PendingDatabaseExecution {
    item_id: u64,
    sql: String,
    source: DatabaseObjectSource,
}

#[derive(Debug, Clone)]
struct DatabaseObjectTarget {
    connection: ConnectionNavEntry,
    catalog: String,
    schema: String,
    object: String,
    object_kind: sift_protocol::ObjectKind,
}

/// Live state of the desktop's database connection. Owned by the shell,
/// updated from executor events.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectionStatus {
    Disconnected,
    Connecting { profile_id: i64 },
    Connected { profile_id: i64, name: String },
    Failed { profile_id: i64, reason: String },
}

#[derive(Debug, Clone)]
enum ConnectionSchemaState {
    Unavailable,
    Loading {
        profile_id: i64,
    },
    Ready {
        profile_id: i64,
        snapshot: Box<sift_protocol::SchemaSnapshot>,
    },
    Failed {
        profile_id: i64,
        message: String,
    },
}

/// Shell → executor. The executor owns the SDK client, session, and
/// connection; the shell only reports intent (connect / disconnect / run).
#[derive(Clone)]
pub enum ExecutorCommand {
    Connect {
        tenant_id: i64,
        profile_id: i64,
        name: String,
    },
    Disconnect,
    RefreshSchema,
    Execute {
        item_id: u64,
        sql: String,
    },
    CreateConnectionProfile {
        tenant_id: i64,
        name: String,
        provider_id: sift_protocol::ProviderId,
        configuration: serde_json::Value,
        credentials: Option<serde_json::Value>,
    },
    DeleteConnectionProfile {
        tenant_id: i64,
        profile_id: i64,
    },
}

/// Executor → shell. Connection-state changes and query outcomes share one
/// channel so ordering (connect before its run's result) is preserved.
#[derive(Debug, Clone)]
pub enum ExecutorEvent {
    Connection(ConnectionStatus),
    SchemaLoaded {
        profile_id: i64,
        snapshot: Box<sift_protocol::SchemaSnapshot>,
    },
    SchemaLoadFailed {
        profile_id: i64,
        message: String,
    },
    Execution {
        item_id: u64,
        state: ResultState,
    },
    ProfileCreated {
        entry: ConnectionNavEntry,
        connection_error: Option<String>,
    },
    ProfileCreationFailed(String),
    ProfileDeleted {
        tenant_id: i64,
        profile_id: i64,
    },
    ProfileDeletionFailed(String),
}

/// A pane owns its ordered items and focus handle. The workspace owns panes;
/// items never reach sideways into sibling panes.
pub struct Pane {
    id: u64,
    items: Vec<ItemPresentation>,
    active_item: usize,
    backward_items: Vec<u64>,
    forward_items: Vec<u64>,
    focus_handle: FocusHandle,
    /// Live editor per SQL or configuration item. Editor contents are not
    /// persisted in the local layout; their owning service rehydrates them.
    editors: HashMap<u64, Entity<QueryEditor>>,
    /// Text at the last clean point for each editor. Comparing content rather
    /// than latching a boolean means undoing back to the clean text is clean.
    clean_documents: HashMap<u64, String>,
    /// Live editor subscriptions, keyed by item. Stored (not detached) so a
    /// tab moved to another pane unsubscribes instead of double-reporting.
    editor_subscriptions: HashMap<u64, Subscription>,
    /// The Data/Messages/Explain/History surface owned by each query item.
    results: HashMap<u64, Entity<ResultsView>>,
    database_item_states: HashMap<u64, DatabaseItemState>,
    /// Transient wrapper sizes while dragging. Keeping these on the pane avoids
    /// invalidating and repainting the result grid for every pointer event.
    live_result_extents: HashMap<u64, f32>,
    /// Mouse-move events may outpace display refresh. One pane invalidation per
    /// frame keeps result resizing responsive without redundant layout passes.
    result_resize_frame_pending: bool,
    /// Insertion index a dragged tab would land at, if a drag is hovering.
    tab_drop_index: Option<usize>,
    /// Item whose tab is currently being dragged from this pane. Used to dim
    /// the source tab; cleared lazily when no drag is active anymore.
    dragging_item: Option<u64>,
    /// Dirty-close confirmation belongs to its tab and is rendered inline.
    pending_close_item: Option<u64>,
}

impl Pane {
    fn from_presentation(
        pane: PanePresentation,
        vim_mode_default: bool,
        cx: &mut Context<Self>,
    ) -> Self {
        let PanePresentation {
            id,
            mut items,
            active_item,
        } = pane;
        let mut editors = HashMap::new();
        let mut clean_documents = HashMap::new();
        let mut editor_subscriptions = HashMap::new();
        let mut results = HashMap::new();
        let mut database_item_states = HashMap::new();
        for item in items
            .iter_mut()
            .filter(|item| ItemRegistry::definition(&item.kind).runtime.is_editor())
        {
            // Editor text is rehydrated independently of presentation state.
            // A persisted dirty bit without its document would create a false
            // close warning for an unchanged, empty editor.
            item.dirty = false;
            let id = item.id;
            let restored_text = match item.source.as_ref() {
                Some(ItemSource::DatabaseObject(source)) => {
                    table_preview_sql(&source.provider_id, &source.schema, &source.object)
                }
                None => String::new(),
            };
            let document = QueryDocument::with_random_peer(&restored_text);
            let language = if item.kind == ItemKind::Configuration {
                EditorLanguage::Toml
            } else {
                EditorLanguage::Sql
            };
            let keymap = if vim_mode_default {
                EditorKeymap::Vim
            } else {
                EditorKeymap::Standard
            };
            let editor = cx.new(|cx| {
                QueryEditor::new(document, cx)
                    .with_language(language)
                    .with_keymap(keymap)
            });
            editor_subscriptions.insert(
                id,
                cx.subscribe(&editor, move |pane, _, event, cx| {
                    pane.on_editor_event(id, event, cx);
                }),
            );
            editors.insert(id, editor);
            clean_documents.insert(id, restored_text);
            if item.kind == ItemKind::Query {
                results.insert(id, cx.new(ResultsView::new));
            }
            if matches!(item.source, Some(ItemSource::DatabaseObject(_))) {
                database_item_states.insert(id, DatabaseItemState::Offline);
            }
        }
        Self {
            id,
            items,
            active_item,
            backward_items: Vec::new(),
            forward_items: Vec::new(),
            focus_handle: cx.focus_handle(),
            editors,
            clean_documents,
            editor_subscriptions,
            results,
            database_item_states,
            live_result_extents: HashMap::new(),
            result_resize_frame_pending: false,
            tab_drop_index: None,
            dragging_item: None,
            pending_close_item: None,
        }
    }

    fn on_editor_event(&mut self, item_id: u64, event: &EditorEvent, cx: &mut Context<Self>) {
        match event {
            EditorEvent::DocumentChanged => {
                let dirty = self.editors.get(&item_id).is_some_and(|editor| {
                    self.clean_documents
                        .get(&item_id)
                        .is_none_or(|clean| editor.read(cx).document().text() != clean)
                });
                cx.emit(PaneEvent::EditorStateChanged {
                    item_id,
                    dirty: Some(dirty),
                });
            }
            EditorEvent::CursorChanged | EditorEvent::VimStateChanged => {
                cx.emit(PaneEvent::EditorStateChanged {
                    item_id,
                    dirty: None,
                })
            }
            EditorEvent::OpenCommandPalette => cx.emit(PaneEvent::OpenCommandPaletteRequested),
            EditorEvent::Execute { sql } => {
                // Show the pending state immediately, then ask the workspace to
                // dispatch the run. The workspace owns the executor channel.
                if let Some(result) = self.results.get(&item_id) {
                    result.update(cx, |result, cx| result.set_pending(cx));
                }
                cx.emit(PaneEvent::ExecuteRequested {
                    item_id,
                    sql: sql.clone(),
                });
            }
        }
    }

    /// Apply an execution outcome to the query item's results surface.
    /// Returns whether this pane owns the item.
    fn set_result(&mut self, item_id: u64, state: ResultState, cx: &mut Context<Self>) -> bool {
        match self.results.get(&item_id) {
            Some(result) => {
                result.update(cx, |result, cx| result.set_state(state, cx));
                true
            }
            None => false,
        }
    }

    fn snapshot(&self) -> PanePresentation {
        let items = self
            .items
            .iter()
            .filter(|item| item.kind != ItemKind::Configuration)
            .cloned()
            .collect::<Vec<_>>();
        let active_id = self.active_item().map(|item| item.id);
        let active_item = active_id
            .and_then(|active| items.iter().position(|item| item.id == active))
            .unwrap_or_else(|| items.len().saturating_sub(1));
        PanePresentation {
            id: self.id,
            items,
            active_item,
        }
    }

    fn active_item(&self) -> Option<&ItemPresentation> {
        self.items.get(self.active_item)
    }

    fn active_item_mut(&mut self) -> Option<&mut ItemPresentation> {
        self.items.get_mut(self.active_item)
    }

    /// The focus handle that should receive keyboard input for this pane: the
    /// active query item's editor when there is one, else the pane itself. This
    /// keeps the `SiftEditor` key context active so editing keys route.
    fn active_focus_handle(&self, cx: &App) -> FocusHandle {
        self.active_item()
            .filter(|item| ItemRegistry::definition(&item.kind).runtime.is_editor())
            .and_then(|item| self.editors.get(&item.id))
            .map(|editor| editor.focus_handle(cx))
            .unwrap_or_else(|| self.focus_handle.clone())
    }

    fn active_cursor_position(&self, cx: &App) -> Option<(usize, usize)> {
        let item = self.active_item()?;
        self.editors
            .get(&item.id)
            .map(|editor| editor.read(cx).cursor_position())
    }

    fn active_editor_mode(&self, cx: &App) -> Option<(EditorKeymap, VimMode, String)> {
        let item = self.active_item()?;
        self.editors.get(&item.id).map(|editor| {
            let editor = editor.read(cx);
            (
                editor.keymap(),
                editor.vim_mode(),
                editor.vim_entered().to_owned(),
            )
        })
    }

    fn contains_item(&self, item_id: u64) -> bool {
        self.items.iter().any(|item| item.id == item_id)
    }

    fn editor(&self, item_id: u64) -> Option<Entity<QueryEditor>> {
        self.editors.get(&item_id).cloned()
    }

    fn database_source(&self, item_id: u64) -> Option<DatabaseObjectSource> {
        self.items
            .iter()
            .find(|item| item.id == item_id)
            .and_then(|item| item.source.as_ref())
            .map(|source| match source {
                ItemSource::DatabaseObject(source) => source.clone(),
            })
    }

    fn set_database_item_state(&mut self, item_id: u64, state: DatabaseItemState) {
        if self.database_source(item_id).is_some() {
            self.database_item_states.insert(item_id, state);
        }
    }

    fn set_all_database_item_states(&mut self, state: DatabaseItemState) {
        let ids = self
            .items
            .iter()
            .filter(|item| matches!(item.source, Some(ItemSource::DatabaseObject(_))))
            .map(|item| item.id)
            .collect::<Vec<_>>();
        for item_id in ids {
            self.database_item_states.insert(item_id, state.clone());
        }
    }

    fn mark_database_item_refreshed(&mut self, item_id: u64) {
        let refreshed_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()
            .and_then(|duration| u64::try_from(duration.as_millis()).ok());
        let Some(item) = self.items.iter_mut().find(|item| item.id == item_id) else {
            return;
        };
        let Some(ItemSource::DatabaseObject(source)) = item.source.as_mut() else {
            return;
        };
        source.last_refreshed_at_ms = refreshed_at;
        self.database_item_states
            .insert(item_id, DatabaseItemState::Live);
    }

    fn can_navigate_backward(&self) -> bool {
        self.backward_items
            .iter()
            .any(|id| self.items.iter().any(|item| item.id == *id))
    }

    fn can_navigate_forward(&self) -> bool {
        self.forward_items
            .iter()
            .any(|id| self.items.iter().any(|item| item.id == *id))
    }

    fn activate_item(&mut self, index: usize, record_history: bool) {
        if index >= self.items.len() || index == self.active_item {
            return;
        }
        if record_history {
            if let Some(current) = self.active_item().map(|item| item.id) {
                self.backward_items.push(current);
            }
            self.forward_items.clear();
        }
        self.active_item = index;
    }

    fn navigate_backward(&mut self) {
        while let Some(id) = self.backward_items.pop() {
            let Some(index) = self.items.iter().position(|item| item.id == id) else {
                continue;
            };
            if let Some(current) = self.active_item().map(|item| item.id) {
                self.forward_items.push(current);
            }
            self.active_item = index;
            break;
        }
    }

    fn navigate_forward(&mut self) {
        while let Some(id) = self.forward_items.pop() {
            let Some(index) = self.items.iter().position(|item| item.id == id) else {
                continue;
            };
            if let Some(current) = self.active_item().map(|item| item.id) {
                self.backward_items.push(current);
            }
            self.active_item = index;
            break;
        }
    }

    fn forget_item(&mut self, item_id: u64) {
        self.backward_items.retain(|id| *id != item_id);
        self.forward_items.retain(|id| *id != item_id);
        self.clean_documents.remove(&item_id);
        self.editor_subscriptions.remove(&item_id);
        self.live_result_extents.remove(&item_id);
        if self.pending_close_item == Some(item_id) {
            self.pending_close_item = None;
        }
    }

    fn replace_new_pane_placeholder(&mut self) -> bool {
        let Some(placeholder_id) = self.items.first().and_then(|item| {
            (self.items.len() == 1
                && item.kind == ItemKind::Welcome
                && item.title == "New pane"
                && !item.dirty)
                .then_some(item.id)
        }) else {
            return false;
        };

        self.items.clear();
        self.active_item = 0;
        self.editors.remove(&placeholder_id);
        self.results.remove(&placeholder_id);
        self.forget_item(placeholder_id);
        true
    }

    fn mark_clean(&mut self, item_id: u64, cx: &App) {
        if let Some(editor) = self.editors.get(&item_id) {
            self.clean_documents
                .insert(item_id, editor.read(cx).document().text().to_owned());
        }
        if let Some(item) = self.items.iter_mut().find(|item| item.id == item_id) {
            item.dirty = false;
        }
    }

    fn open_configuration(
        &mut self,
        item: ItemPresentation,
        editor: Entity<QueryEditor>,
        cx: &mut Context<Self>,
    ) {
        let item_id = item.id;
        if let Some(index) = self
            .items
            .iter()
            .position(|candidate| candidate.id == item_id)
        {
            self.items[index] = item;
            self.activate_item(index, true);
        } else {
            if !self.replace_new_pane_placeholder() {
                if let Some(current) = self.active_item().map(|item| item.id) {
                    self.backward_items.push(current);
                    self.forward_items.clear();
                }
            }
            self.items.push(item);
            self.active_item = self.items.len() - 1;
        }
        self.editor_subscriptions.insert(
            item_id,
            cx.subscribe(&editor, move |pane, _, event, cx| {
                pane.on_editor_event(item_id, event, cx);
            }),
        );
        self.clean_documents
            .insert(item_id, editor.read(cx).document().text().to_owned());
        self.editors.insert(item_id, editor);
        self.results.remove(&item_id);
        self.database_item_states.remove(&item_id);
        self.pending_close_item = None;
        cx.notify();
    }

    fn open_query(
        &mut self,
        item: ItemPresentation,
        editor: Entity<QueryEditor>,
        results: Entity<ResultsView>,
        cx: &mut Context<Self>,
    ) {
        let item_id = item.id;
        if !self.replace_new_pane_placeholder() {
            if let Some(current) = self.active_item().map(|item| item.id) {
                self.backward_items.push(current);
                self.forward_items.clear();
            }
        }
        self.items.push(item);
        self.active_item = self.items.len() - 1;
        self.editor_subscriptions.insert(
            item_id,
            cx.subscribe(&editor, move |pane, _, event, cx| {
                pane.on_editor_event(item_id, event, cx);
            }),
        );
        self.clean_documents
            .insert(item_id, editor.read(cx).document().text().to_owned());
        self.editors.insert(item_id, editor);
        self.results.insert(item_id, results);
        if matches!(
            self.items.last().and_then(|item| item.source.as_ref()),
            Some(ItemSource::DatabaseObject(_))
        ) {
            self.database_item_states
                .insert(item_id, DatabaseItemState::Offline);
        }
        cx.notify();
    }

    fn resize_results(
        &mut self,
        event: &gpui::DragMoveEvent<ResultResizeDrag>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let drag = *event.drag(cx);
        if !self.results.contains_key(&drag.item_id) {
            return;
        }
        let width: f32 = event.bounds.size.width.into();
        let height: f32 = event.bounds.size.height.into();
        let pointer_x: f32 = (event.event.position.x - event.bounds.left()).into();
        let pointer_y: f32 = (event.event.position.y - event.bounds.top()).into();
        let extent = match drag.placement {
            ResultPlacement::Bottom => (height - pointer_y)
                .max(RESULT_MIN_EXTENT)
                .min((height - EDITOR_MIN_EXTENT).max(RESULT_MIN_EXTENT)),
            ResultPlacement::Right => (width - pointer_x)
                .max(RESULT_MIN_EXTENT)
                .min((width - EDITOR_MIN_EXTENT).max(RESULT_MIN_EXTENT)),
        };
        self.queue_result_resize(drag.item_id, extent, window, cx);
    }

    fn queue_result_resize(
        &mut self,
        item_id: u64,
        extent: f32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Sub-pixel wrapper changes still trigger full layout. Snap them away,
        // and skip pointer events that resolve to the current visible extent.
        let extent = extent.round();
        let current = self.live_result_extents.get(&item_id).copied().or_else(|| {
            self.results
                .get(&item_id)
                .map(|result| result.read(cx).extent())
        });
        if current == Some(extent) {
            return;
        }
        self.live_result_extents.insert(item_id, extent);

        if self.result_resize_frame_pending {
            return;
        }
        self.result_resize_frame_pending = true;
        cx.notify();
        cx.on_next_frame(window, |pane, _, _| {
            pane.result_resize_frame_pending = false;
        });
    }

    fn finish_results_resize(
        &mut self,
        drag: &ResultResizeDrag,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(extent) = self.live_result_extents.remove(&drag.item_id) else {
            return;
        };
        if let Some(results) = self.results.get(&drag.item_id) {
            results.update(cx, |results, cx| results.set_extent(extent, cx));
        }
    }

    /// Pointer passed over a tab while dragging: remember where the tab would
    /// insert. Left half inserts before the tab, right half after it.
    fn tab_drag_hover(
        &mut self,
        index: usize,
        event: &gpui::DragMoveEvent<TabDrag>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // `on_drag_move` fires on every listener, hovered or not, so the tab
        // under the pointer has to claim the drop itself. Without this the
        // last-painted tab always wins and tabs only ever move rightwards.
        if !event.bounds.contains(&event.event.position) {
            return;
        }
        let pointer_x = event.event.position.x - event.bounds.left();
        let drop_index = if pointer_x > event.bounds.size.width / 2. {
            index + 1
        } else {
            index
        };
        if self.tab_drop_index != Some(drop_index) {
            self.tab_drop_index = Some(drop_index);
            cx.notify();
        }
    }

    /// A dragged tab was released over this pane: reorder locally, or ask the
    /// workspace to move it here from its originating pane.
    fn finish_tab_drag(&mut self, drag: &TabDrag, _: &mut Window, cx: &mut Context<Self>) {
        self.dragging_item = None;
        let to = self.tab_drop_index.take().unwrap_or(self.items.len());
        if drag.pane_id != self.id {
            cx.emit(PaneEvent::MoveItemRequested {
                item_id: drag.item_id,
            });
        } else {
            self.move_item(drag.item_id, to);
        }
        cx.emit(PaneEvent::FocusRequested);
        cx.notify();
    }

    /// Reorder an item to `to`, an insertion index in the current order.
    /// The active item follows the dragged tab.
    fn move_item(&mut self, item_id: u64, to: usize) {
        let Some(from) = self.items.iter().position(|item| item.id == item_id) else {
            return;
        };
        if to > self.items.len() || from == to || from + 1 == to {
            return;
        }
        let item = self.items.remove(from);
        let to = if from < to { to - 1 } else { to };
        self.items.insert(to, item);
        self.active_item = to;
    }

    /// Detach an item so another pane can adopt it. Caller persists layout.
    fn take_item(&mut self, item_id: u64) -> Option<TabTransfer> {
        let index = self.items.iter().position(|item| item.id == item_id)?;
        let item = self.items.remove(index);
        let editor = self.editors.remove(&item_id)?;
        let results = self.results.remove(&item_id);
        let clean_text = self.clean_documents.remove(&item_id).unwrap_or_default();
        self.forget_item(item_id);
        self.active_item = self.active_item.min(self.items.len().saturating_sub(1));
        Some(TabTransfer {
            item,
            editor,
            results,
            clean_text,
        })
    }

    /// Adopt a tab detached from another pane, appending it as the active tab.
    fn receive_item(&mut self, transfer: TabTransfer, cx: &mut Context<Self>) {
        let item_id = transfer.item.id;
        let editor = transfer.editor;
        self.editor_subscriptions.insert(
            item_id,
            cx.subscribe(&editor, move |pane, _, event, cx| {
                pane.on_editor_event(item_id, event, cx);
            }),
        );
        self.clean_documents.insert(item_id, transfer.clean_text);
        self.editors.insert(item_id, editor);
        if let Some(results) = transfer.results {
            self.results.insert(item_id, results);
        }
        self.items.push(transfer.item);
        self.active_item = self.items.len() - 1;
        self.pending_close_item = None;
        cx.notify();
    }
}

impl Focusable for Pane {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl EventEmitter<PaneEvent> for Pane {}

fn pane_border_color(theme: &Theme, is_focused: bool) -> gpui::Hsla {
    if is_focused {
        theme.colors.accent
    } else {
        theme.colors.subtle_border
    }
}

impl Pane {}

impl gpui::Render for Pane {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let colors = theme.colors;
        let is_focused = self.active_focus_handle(cx).is_focused(window)
            || self.focus_handle.contains_focused(window, cx);
        let active = self.active_item().cloned();
        let database_notice = active.as_ref().and_then(|item| {
            let ItemSource::DatabaseObject(_) = item.source.as_ref()?;
            let state = self.database_item_states.get(&item.id)?.clone();
            (!matches!(state, DatabaseItemState::Live)).then_some((item.id, state))
        });
        let pending_close = active
            .as_ref()
            .filter(|item| self.pending_close_item == Some(item.id))
            .cloned();
        let has_items = !self.items.is_empty();
        let can_go_back = self.can_navigate_backward();
        let can_go_forward = self.can_navigate_forward();
        let pane_id = self.id;
        div()
            .id(("pane", self.id as usize))
            .relative()
            .key_context("SiftPane")
            .track_focus(&self.focus_handle)
            // Clicking anywhere in the pane makes it the active pane. The
            // workspace owns the pane list, so we ask rather than reach across.
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|_, _, _, cx| cx.emit(PaneEvent::FocusRequested)),
            )
            .on_drag_move::<ResultResizeDrag>(cx.listener(Self::resize_results))
            .on_drop::<ResultResizeDrag>(cx.listener(Self::finish_results_resize))
            // A tab dropped anywhere else in the pane (not only its tab bar)
            // still lands here; empty panes accept cross-pane drops this way.
            .on_drop::<TabDrag>(cx.listener(Self::finish_tab_drag))
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .border_t_1()
            .border_color(pane_border_color(&theme, is_focused))
            .bg(colors.background)
            .children(has_items.then(|| {
                div()
                    .debug_selector(|| "pane-tab-bar".into())
                    .h(theme.metrics.tab_height)
                    .flex_none()
                    .flex()
                    .items_stretch()
                    .relative()
                    .bg(colors.toolbar)
                    .child(
                        div()
                            .w(EDITOR_GUTTER_WIDTH)
                            .h_full()
                            .flex_none()
                            .flex()
                            .items_center()
                            .border_r_1()
                            .border_color(colors.subtle_border)
                            .children(
                                [
                                    (
                                        "pane-go-back",
                                        IconName::ChevronLeft,
                                        "Go back",
                                        can_go_back,
                                        true,
                                    ),
                                    (
                                        "pane-go-forward",
                                        IconName::ChevronRight,
                                        "Go forward",
                                        can_go_forward,
                                        false,
                                    ),
                                ]
                                .into_iter()
                                .map(
                                    |(id, icon_name, label, enabled, backward)| {
                                        div()
                                            .id((id, pane_id as usize))
                                            .role(Role::Button)
                                            .aria_label(label)
                                            .w(EDITOR_GUTTER_WIDTH / 2.)
                                            .h_full()
                                            .flex_none()
                                            .flex()
                                            .items_center()
                                            .justify_center()
                                            .text_color(if enabled {
                                                colors.muted_text
                                            } else {
                                                colors.disabled_text
                                            })
                                            .when(enabled, |button| {
                                                button
                                                    .hover(|button| {
                                                        button
                                                            .bg(colors.hovered_surface)
                                                            .text_color(colors.text)
                                                    })
                                                    .on_click(cx.listener(
                                                        move |pane, _, window, cx| {
                                                            if backward {
                                                                pane.navigate_backward();
                                                            } else {
                                                                pane.navigate_forward();
                                                            }
                                                            pane.active_focus_handle(cx)
                                                                .focus(window, cx);
                                                            cx.emit(PaneEvent::FocusRequested);
                                                            cx.notify();
                                                        },
                                                    ))
                                            })
                                            .child(icon(
                                                icon_name,
                                                if enabled {
                                                    colors.muted_text
                                                } else {
                                                    colors.disabled_text
                                                },
                                                12.,
                                            ))
                                            .tooltip(move |_, cx| {
                                                cx.new(|_| Tooltip::new(label)).into()
                                            })
                                    },
                                ),
                            ),
                    )
                    .child(
                        div()
                            .absolute()
                            .left_0()
                            .right_0()
                            .bottom_0()
                            .h(px(1.))
                            .bg(colors.subtle_border),
                    )
                    .child(
                        div()
                            .relative()
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .overflow_x_hidden()
                            .child(
                                div()
                                    .id(("tabs-scroll", self.id as usize))
                                    .flex()
                                    .h_full()
                                    .overflow_x_scroll()
                                    .children(self.items.iter().enumerate().map(
                                        |(index, item)| {
                                            let selected = index == self.active_item;
                                            let item_id = item.id;
                                            let tab_debug = format!("tab-{item_id}");
                                            let dragging_over_left =
                                                self.tab_drop_index == Some(index);
                                            let dragging_over_right = self.tab_drop_index
                                                == Some(index + 1)
                                                && index + 1 == self.items.len();
                                            div()
                                                .id(("tab", item.id as usize))
                                                .debug_selector(move || tab_debug.clone())
                                                .relative()
                                                .flex_none()
                                                .flex()
                                                .items_center()
                                                .h_full()
                                                .min_w(px(110.))
                                                .max_w(px(240.))
                                                .border_r_1()
                                                .border_color(colors.subtle_border)
                                                .on_drag(
                                                    TabDrag { pane_id, item_id },
                                                    |_, _, _, cx| cx.new(|_| gpui::Empty),
                                                )
                                                .on_drag_move::<TabDrag>(cx.listener(
                                                    move |pane, event, window, cx| {
                                                        pane.tab_drag_hover(
                                                            index, event, window, cx,
                                                        );
                                                    },
                                                ))
                                                .when(dragging_over_left, |tab| {
                                                    tab.child(
                                                        div()
                                                            .absolute()
                                                            .left_0()
                                                            .top_0()
                                                            .bottom_0()
                                                            .w(px(2.))
                                                            .bg(colors.accent),
                                                    )
                                                })
                                                .when(dragging_over_right, |tab| {
                                                    tab.child(
                                                        div()
                                                            .absolute()
                                                            .right_0()
                                                            .top_0()
                                                            .bottom_0()
                                                            .w(px(2.))
                                                            .bg(colors.accent),
                                                    )
                                                })
                                                .when(selected, |tab| {
                                                    tab.bg(colors.background)
                                                        .text_color(colors.text)
                                                })
                                                .child(
                                                    div()
                                                        .id(("tab-label", item.id as usize))
                                                        .flex()
                                                        .flex_1()
                                                        .min_w_0()
                                                        .items_center()
                                                        .h_full()
                                                        .pl_2()
                                                        .pr_1()
                                                        .when(!selected, |label| {
                                                            label.text_color(colors.muted_text)
                                                        })
                                                        .hover(|label| {
                                                            label.text_color(colors.text)
                                                        })
                                                        .on_click(cx.listener(
                                                            move |pane, _, window, cx| {
                                                                pane.activate_item(index, true);
                                                                pane.active_focus_handle(cx)
                                                                    .focus(window, cx);
                                                                cx.emit(PaneEvent::FocusRequested);
                                                                cx.notify();
                                                            },
                                                        ))
                                                        .child(
                                                            div()
                                                                .flex_1()
                                                                .min_w_0()
                                                                .truncate()
                                                                .child(item.title.clone()),
                                                        ),
                                                )
                                                .child(
                                                    IconButton::new(
                                                        ("tab-close", item.id as usize),
                                                        IconName::Close,
                                                        format!("Close tab {}", item.title),
                                                    )
                                                    .square(px(22.))
                                                    .icon_size(12.)
                                                    .tooltip(format!("Close tab {}", item.title))
                                                    .on_click(cx.listener(move |_, _, _, cx| {
                                                        cx.emit(PaneEvent::CloseItemRequested {
                                                            item_id,
                                                        });
                                                    })),
                                                )
                                        },
                                    )),
                            ),
                    )
            }))
            .children(pending_close.map(|item| {
                let item_id = item.id;
                let is_configuration = item.kind == ItemKind::Configuration;
                div()
                    .id(("dirty-close-strip", item_id as usize))
                    .h(theme.metrics.toolbar_height)
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_3()
                    .border_b_1()
                    .border_color(colors.warning)
                    .bg(colors.warning_muted)
                    .text_sm()
                    .child(icon(IconName::Warning, colors.warning, 14.))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .child(format!("Discard changes to {}?", item.title)),
                    )
                    .child(
                        Button::new(("keep-editing", item_id as usize), "Keep editing")
                            .tone(ButtonTone::Ghost)
                            .on_click(cx.listener(move |pane, _, window, cx| {
                                pane.pending_close_item = None;
                                pane.active_focus_handle(cx).focus(window, cx);
                                cx.notify();
                            })),
                    )
                    .children(is_configuration.then(|| {
                        Button::new(("save-dirty-item", item_id as usize), "Save")
                            .tone(ButtonTone::Accent)
                            .on_click(cx.listener(move |_, _, _, cx| {
                                cx.emit(PaneEvent::SaveItemRequested { item_id });
                            }))
                    }))
                    .child(
                        Button::new(("discard-dirty-item", item_id as usize), "Discard")
                            .tone(ButtonTone::DangerGhost)
                            .on_click(cx.listener(move |_, _, _, cx| {
                                cx.emit(PaneEvent::DiscardItemRequested { item_id });
                            })),
                    )
            }))
            .child({
                let body = div().flex_1().min_h_0().flex().flex_col();
                match active {
                    Some(item) if item.kind == ItemKind::Configuration => {
                        match self.editors.get(&item.id) {
                            Some(editor) => {
                                body.child(div().flex_1().min_h_0().child(editor.clone()))
                            }
                            None => body.child(
                                div()
                                    .size_full()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .text_color(colors.muted_text)
                                    .child("Configuration editor is unavailable"),
                            ),
                        }
                    }
                    Some(item)
                        if ItemRegistry::definition(&item.kind).runtime
                            == ItemRuntimeKind::Query =>
                    {
                        match (self.editors.get(&item.id), self.results.get(&item.id)) {
                            (Some(editor), Some(result)) => {
                                let placement = result.read(cx).placement();
                                let extent = self
                                    .live_result_extents
                                    .get(&item.id)
                                    .copied()
                                    .unwrap_or_else(|| result.read(cx).extent());
                                let item_id = item.id;
                                let resize_hitbox = div()
                                    .id(("resize-query-results", item_id as usize))
                                    .debug_selector(move || {
                                        format!("resize-query-results-{item_id}")
                                    })
                                    .absolute()
                                    .flex_none()
                                    .cursor(match placement {
                                        ResultPlacement::Bottom => CursorStyle::ResizeUpDown,
                                        ResultPlacement::Right => CursorStyle::ResizeLeftRight,
                                    })
                                    .block_mouse_except_scroll()
                                    .when(placement == ResultPlacement::Bottom, |handle| {
                                        handle
                                            .left_0()
                                            .top(px(-(RESULT_RESIZE_HANDLE_SIZE - 1.0) / 2.0))
                                            .w_full()
                                            .h(px(RESULT_RESIZE_HANDLE_SIZE))
                                    })
                                    .when(placement == ResultPlacement::Right, |handle| {
                                        handle
                                            .top_0()
                                            .left(px(-(RESULT_RESIZE_HANDLE_SIZE - 1.0) / 2.0))
                                            .h_full()
                                            .w(px(RESULT_RESIZE_HANDLE_SIZE))
                                    })
                                    .on_drag(
                                        ResultResizeDrag { item_id, placement },
                                        |_, _, _, cx| cx.new(|_| gpui::Empty),
                                    )
                                    .tooltip(move |_, cx| {
                                        cx.new(|_| Tooltip::new("Drag to resize results")).into()
                                    });
                                let handle = div()
                                    .id(("query-results-separator", item_id as usize))
                                    .debug_selector(move || {
                                        format!("query-results-separator-{item_id}")
                                    })
                                    .relative()
                                    .flex_none()
                                    .bg(colors.subtle_border)
                                    .when(placement == ResultPlacement::Bottom, |handle| {
                                        handle.w_full().h(px(1.0))
                                    })
                                    .when(placement == ResultPlacement::Right, |handle| {
                                        handle.h_full().w(px(1.0))
                                    })
                                    .child(resize_hitbox);
                                let split = match placement {
                                    ResultPlacement::Bottom => div()
                                        .size_full()
                                        .min_h_0()
                                        .flex()
                                        .flex_col()
                                        .child(div().flex_1().min_h_0().child(editor.clone()))
                                        .child(handle)
                                        .child(
                                            div()
                                                .h(px(extent))
                                                .flex_none()
                                                .flex()
                                                .min_h_0()
                                                .child(result.clone()),
                                        )
                                        .into_any_element(),
                                    ResultPlacement::Right => div()
                                        .size_full()
                                        .min_w_0()
                                        .flex()
                                        .child(div().flex_1().min_w_0().child(editor.clone()))
                                        .child(handle)
                                        .child(
                                            div()
                                                .w(px(extent))
                                                .flex_none()
                                                .flex()
                                                .min_w_0()
                                                .child(result.clone()),
                                        )
                                        .into_any_element(),
                                };
                                body.child(split)
                            }
                            _ => body.child(
                                div()
                                    .size_full()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .p_4()
                                    .text_color(colors.muted_text)
                                    .child(format!("Query editor · {}", item.title)),
                            ),
                        }
                    }
                    Some(item) => {
                        let definition = ItemRegistry::definition(&item.kind);
                        let message = definition.placeholder_prefix.map_or_else(
                            || definition.empty_message.to_owned(),
                            |prefix| format!("{prefix} · {}", item.title),
                        );
                        body.child(
                            div()
                                .size_full()
                                .flex()
                                .items_center()
                                .justify_center()
                                .p_4()
                                .child(
                                    div()
                                        .max_w(px(420.))
                                        .min_w_0()
                                        .text_center()
                                        .text_color(colors.muted_text)
                                        .whitespace_normal()
                                        .child(message),
                                ),
                        )
                    }
                    None => body.child(
                        div()
                            .size_full()
                            .flex()
                            .items_center()
                            .justify_center()
                            .p_4()
                            .text_center()
                            .text_color(colors.muted_text)
                            .child("No open items"),
                    ),
                }
            })
            .children(database_notice.map(|(item_id, state)| {
                let (tint, can_retry) = match state {
                    DatabaseItemState::Live => unreachable!(),
                    DatabaseItemState::Offline => (colors.warning_muted, true),
                    DatabaseItemState::Reconnecting => (colors.warning_muted, false),
                    DatabaseItemState::Failed(_) => (colors.danger_muted, true),
                };
                div()
                    .id(("database-snapshot-overlay", item_id as usize))
                    .debug_selector(|| "database-snapshot-overlay".into())
                    .absolute()
                    .left_0()
                    .right_0()
                    .top(theme.metrics.tab_height)
                    .bottom_0()
                    .bg(tint)
                    .when(can_retry, |overlay| {
                        overlay
                            .cursor_pointer()
                            .on_click(cx.listener(move |_, _, _, cx| {
                                cx.emit(PaneEvent::RefreshDatabaseItemRequested { item_id });
                            }))
                    })
            }))
    }
}

pub struct WorkspaceShell {
    focus_handle: FocusHandle,
    query_input: Entity<TextInput>,
    server_name_input: Entity<TextInput>,
    server_url_input: Entity<TextInput>,
    server_token_input: Entity<TextInput>,
    account_username_input: Entity<TextInput>,
    account_password_input: Entity<TextInput>,
    database_name_input: Entity<TextInput>,
    database_host_input: Entity<TextInput>,
    database_port_input: Entity<TextInput>,
    database_catalog_input: Entity<TextInput>,
    database_user_input: Entity<TextInput>,
    database_password_input: Entity<TextInput>,
    database_search_path_input: Entity<TextInput>,
    database_application_name_input: Entity<TextInput>,
    database_timeout_input: Entity<TextInput>,
    database_pool_min_input: Entity<TextInput>,
    database_pool_max_input: Entity<TextInput>,
    instance_secret_input: Entity<TextInput>,
    instance_configuration_editor: Entity<QueryEditor>,
    palette_selected: usize,
    palette_scroll_handle: UniformListScrollHandle,
    dark_theme: bool,
    settings: UserSettings,
    settings_store: Option<Arc<SettingsStore>>,
    settings_item: Option<u64>,
    window_presentation: WindowPresentation,
    panes: Vec<Entity<Pane>>,
    workspace_sessions: HashMap<String, WorkspaceSession>,
    workspace_presentations: HashMap<String, WorkspacePresentation>,
    pane_flexes: Vec<f32>,
    workspace_resize_frame_pending: bool,
    active_pane: usize,
    selected_workspace_id: Option<i64>,
    selected_instance_id: Option<String>,
    left_dock: Dock,
    right_dock: Dock,
    bottom_dock: Dock,
    active_left_panel: LeftPanel,
    active_bottom_tool: BottomTool,
    modal: Option<Modal>,
    app_bar_expanded: bool,
    app_bar_menu: Option<AppBarMenu>,
    toasts: Vec<Toast>,
    next_toast_id: u64,
    status: StatusBar,
    sql_problems: Vec<SqlProblem>,
    lifecycle: LifecycleProjection,
    presence: RoomPresenceProjection,
    _lifecycle_task: Option<Task<()>>,
    _presence_task: Option<Task<()>>,
    _executor_task: Option<Task<()>>,
    _instance_task: Option<Task<()>>,
    executor_sender: Option<tokio::sync::mpsc::UnboundedSender<ExecutorCommand>>,
    pending_database_execution: Option<PendingDatabaseExecution>,
    instance_sender: Option<tokio::sync::mpsc::UnboundedSender<InstanceCommand>>,
    saved_servers: Vec<SavedServerProfile>,
    instance_roots: Vec<SavedInstanceRoot>,
    instance_plan: Option<InstancePlanPresentation>,
    instance_configuration: Option<InstanceConfigurationPresentation>,
    instance_configuration_item: Option<u64>,
    selected_instance_credential: Option<String>,
    instance_operation_pending: bool,
    instance_operation_error: Option<String>,
    selected_server_profile: Option<String>,
    remember_server_token: bool,
    server_connection_pending: bool,
    server_connection_error: Option<String>,
    account_pending: bool,
    account_error: Option<String>,
    connection_status: ConnectionStatus,
    connection_schema: ConnectionSchemaState,
    expanded_tenants: HashSet<i64>,
    expanded_connections: HashSet<i64>,
    expanded_rooms: HashSet<i64>,
    expanded_catalogs: HashSet<(i64, String)>,
    expanded_schemas: HashSet<(i64, String, String)>,
    selected_database_tenant: Option<i64>,
    selected_database_provider: Option<String>,
    selected_database_ssl_mode: Option<String>,
    database_wizard_step: DatabaseWizardStep,
    database_connection_pending: bool,
    database_connection_error: Option<String>,
    store: Option<Arc<PresentationStore>>,
    _bounds_subscription: Subscription,
    next_id: u64,
}

/// Live, server-scoped IDE state. Pane entities stay alive while another
/// server is active, preserving editor text, dirty state, results, and focus
/// history without putting product data in `presentation.json`.
struct WorkspaceSession {
    panes: Vec<Entity<Pane>>,
    pane_flexes: Vec<f32>,
    active_pane: usize,
    selected_workspace_id: Option<i64>,
    left_dock: Dock,
    right_dock: Dock,
    bottom_dock: Dock,
    active_left_panel: LeftPanel,
    active_bottom_tool: BottomTool,
    sql_problems: Vec<SqlProblem>,
    expanded_tenants: HashSet<i64>,
    expanded_connections: HashSet<i64>,
    expanded_rooms: HashSet<i64>,
    expanded_catalogs: HashSet<(i64, String)>,
    expanded_schemas: HashSet<(i64, String, String)>,
}

impl WorkspaceSession {
    fn snapshot(&self, instance_id: String, cx: &App) -> WorkspacePresentation {
        WorkspacePresentation {
            left_dock: self.left_dock.presentation.clone(),
            right_dock: self.right_dock.presentation.clone(),
            bottom_dock: self.bottom_dock.presentation.clone(),
            left_panel: self.active_left_panel,
            bottom_tool: self.active_bottom_tool,
            panes: self
                .panes
                .iter()
                .map(|pane| pane.read(cx).snapshot())
                .collect(),
            pane_flexes: self.pane_flexes.clone(),
            active_pane: self.active_pane,
            workspace_id: self.selected_workspace_id,
            instance_id: Some(instance_id),
        }
    }
}

impl WorkspaceShell {
    pub fn new(
        state: PresentationState,
        settings: UserSettings,
        store: Option<Arc<PresentationStore>>,
        settings_store: Option<Arc<SettingsStore>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let window_presentation = state.window.clone();
        let vim_mode_default = settings.editor.default_mode == EditorMode::Vim;
        // Install the process-wide theme first so every child entity reads the
        // same palette through `ActiveTheme` during construction and render.
        let theme = if state.dark_theme {
            Theme::dark()
        } else {
            Theme::light()
        };
        sift_ui::init_theme(theme, cx);
        let mut workspace_presentations = state.instance_workspaces;
        let workspace = if state.workspace.panes.is_empty() {
            PresentationState::default().workspace
        } else {
            state.workspace
        };
        let selected_workspace_id = workspace.workspace_id;
        let selected_instance_id = workspace.instance_id.clone();
        if let Some(instance_id) = &selected_instance_id {
            workspace_presentations.remove(instance_id);
        }
        let panes = workspace
            .panes
            .into_iter()
            .map(|pane| cx.new(|cx| Pane::from_presentation(pane, vim_mode_default, cx)))
            .collect::<Vec<_>>();
        for pane in &panes {
            cx.subscribe_in(pane, window, Self::on_pane_event).detach();
        }
        let active_pane = workspace.active_pane.min(panes.len().saturating_sub(1));
        let next_id = panes
            .iter()
            .flat_map(|pane| {
                pane.read(cx)
                    .items
                    .iter()
                    .map(|item| item.id)
                    .chain(std::iter::once(pane.read(cx).id))
            })
            .max()
            .unwrap_or(0)
            + 1;
        let query_input = cx.new(|cx| TextInput::new("", "Search commands…", cx));
        let server_name_input = cx.new(|cx| TextInput::new("", "Display name", cx));
        let server_url_input = cx.new(|cx| TextInput::new("", "http://192.168.1.20:7474", cx));
        let server_token_input =
            cx.new(|cx| TextInput::new("", "Bearer token (or use saved token)", cx).masked());
        let account_username_input =
            cx.new(|cx| TextInput::new("", "Username", cx).aria_label("Account username"));
        let account_password_input = cx.new(|cx| {
            TextInput::new("", "Password", cx)
                .aria_label("Account password")
                .masked()
        });
        let database_name_input =
            cx.new(|cx| TextInput::new("", "Display name", cx).aria_label("Connection name"));
        let database_host_input =
            cx.new(|cx| TextInput::new("", "Database host", cx).aria_label("Host"));
        let database_port_input = cx.new(|cx| TextInput::new("", "Port", cx));
        let database_catalog_input =
            cx.new(|cx| TextInput::new("", "Database (optional)", cx).aria_label("Database"));
        let database_user_input = cx.new(|cx| TextInput::new("", "Username", cx));
        let database_password_input = cx.new(|cx| {
            TextInput::new("", "Password (optional)", cx)
                .aria_label("Password")
                .masked()
        });
        let database_search_path_input =
            cx.new(|cx| TextInput::new("", "public, reporting", cx).aria_label("Search path"));
        let database_application_name_input =
            cx.new(|cx| TextInput::new("", "sift", cx).aria_label("Database application name"));
        let database_timeout_input = cx.new(|cx| {
            TextInput::new("", "Server default", cx).aria_label("Connection timeout in seconds")
        });
        let database_pool_min_input =
            cx.new(|cx| TextInput::new("", "0", cx).aria_label("Minimum pool size"));
        let database_pool_max_input =
            cx.new(|cx| TextInput::new("", "Server default", cx).aria_label("Maximum pool size"));
        let instance_secret_input = cx.new(|cx| {
            TextInput::new("", "Required secret", cx)
                .aria_label("Instance credential value")
                .masked()
        });
        let instance_configuration_editor = cx.new(|cx| {
            QueryEditor::new(QueryDocument::with_random_peer(""), cx)
                .with_language(EditorLanguage::Toml)
                .with_keymap(if vim_mode_default {
                    EditorKeymap::Vim
                } else {
                    EditorKeymap::Standard
                })
        });
        // Re-render the palette as the search text changes so its list filters.
        cx.observe(&query_input, |shell, _, cx| {
            shell.palette_selected = 0;
            shell
                .palette_scroll_handle
                .scroll_to_item(0, ScrollStrategy::Top);
            cx.notify();
        })
        .detach();
        panes[active_pane]
            .read(cx)
            .active_focus_handle(cx)
            .focus(window, cx);
        let bounds_subscription = cx.observe_window_bounds(window, |shell, window, cx| {
            let bounds = window.window_bounds();
            shell.capture_window_bounds(bounds);
            let size = bounds.get_bounds().size;
            shell.fit_docks_to_viewport(size.width.into(), size.height.into());
            shell.persist(cx);
        });
        let mut left_dock = DockRegistry::create(DockId::Left, workspace.left_dock.clone());
        let mut right_dock = DockRegistry::create(DockId::Inspector, workspace.right_dock.clone());
        let mut bottom_dock = DockRegistry::create(DockId::Bottom, workspace.bottom_dock.clone());
        let viewport = window.window_bounds().get_bounds().size;
        let side_sizes = dock_layout::fit_side_docks(
            viewport.width.into(),
            left_dock.presentation.size,
            right_dock.presentation.size,
            left_dock.presentation.open,
            right_dock.presentation.open,
        );
        left_dock.presentation.size = side_sizes.left;
        right_dock.presentation.size = side_sizes.right;
        let vertical_chrome: f32 =
            (theme.metrics.toolbar_height + theme.metrics.status_height).into();
        bottom_dock.presentation.size = dock_layout::fit_bottom_dock(
            f32::from(viewport.height) - vertical_chrome,
            bottom_dock.presentation.size,
        );
        let pane_flexes = valid_pane_flexes(workspace.pane_flexes, panes.len());
        Self {
            focus_handle: cx.focus_handle(),
            query_input,
            server_name_input,
            server_url_input,
            server_token_input,
            account_username_input,
            account_password_input,
            database_name_input,
            database_host_input,
            database_port_input,
            database_catalog_input,
            database_user_input,
            database_password_input,
            database_search_path_input,
            database_application_name_input,
            database_timeout_input,
            database_pool_min_input,
            database_pool_max_input,
            instance_secret_input,
            instance_configuration_editor,
            palette_selected: 0,
            palette_scroll_handle: UniformListScrollHandle::new(),
            dark_theme: state.dark_theme,
            next_toast_id: 1,
            settings,
            settings_store,
            settings_item: None,
            window_presentation,
            panes,
            workspace_sessions: HashMap::new(),
            workspace_presentations,
            pane_flexes,
            workspace_resize_frame_pending: false,
            active_pane,
            selected_workspace_id,
            selected_instance_id,
            left_dock,
            right_dock,
            bottom_dock,
            active_left_panel: workspace.left_panel,
            active_bottom_tool: workspace.bottom_tool,
            modal: None,
            app_bar_expanded: false,
            app_bar_menu: None,
            toasts: Vec::new(),
            status: StatusBar::default(),
            sql_problems: Vec::new(),
            lifecycle: LifecycleProjection::default(),
            presence: RoomPresenceProjection::default(),
            _lifecycle_task: None,
            _presence_task: None,
            _executor_task: None,
            _instance_task: None,
            executor_sender: None,
            pending_database_execution: None,
            instance_sender: None,
            saved_servers: Vec::new(),
            instance_roots: Vec::new(),
            instance_plan: None,
            instance_configuration: None,
            instance_configuration_item: None,
            selected_instance_credential: None,
            instance_operation_pending: false,
            instance_operation_error: None,
            selected_server_profile: None,
            remember_server_token: true,
            server_connection_pending: false,
            server_connection_error: None,
            account_pending: false,
            account_error: None,
            connection_status: ConnectionStatus::Disconnected,
            connection_schema: ConnectionSchemaState::Unavailable,
            expanded_tenants: HashSet::new(),
            expanded_connections: HashSet::new(),
            expanded_rooms: HashSet::new(),
            expanded_catalogs: HashSet::new(),
            expanded_schemas: HashSet::new(),
            selected_database_tenant: None,
            selected_database_provider: None,
            selected_database_ssl_mode: Some("prefer".into()),
            database_wizard_step: DatabaseWizardStep::Provider,
            database_connection_pending: false,
            database_connection_error: None,
            store,
            _bounds_subscription: bounds_subscription,
            next_id,
        }
    }

    pub fn command_specs(&self, cx: &App) -> Vec<CommandSpec> {
        CommandRegistry::palette(self.command_context(cx))
    }

    fn command_context(&self, cx: &App) -> CommandContext {
        let has_item = self
            .panes
            .get(self.active_pane)
            .is_some_and(|pane| pane.read(cx).active_item().is_some());
        CommandContext {
            has_active_item: has_item,
            pane_count: self.panes.len(),
        }
    }

    pub fn pane_count(&self) -> usize {
        self.panes.len()
    }

    /// Label for the workspace switcher: the open workspace's name, or a local
    /// fallback before any server workspace is selected.
    fn workspace_label(&self) -> String {
        self.selected_workspace_id
            .and_then(|selected| {
                self.lifecycle
                    .tenants
                    .iter()
                    .flat_map(|tenant| &tenant.rooms)
                    .flat_map(|room| &room.workspaces)
                    .find(|workspace| workspace.id == selected)
                    .map(|workspace| workspace.name.clone())
            })
            .unwrap_or_else(|| "Local workspace".into())
    }

    fn workspace_context_label(&self) -> String {
        let workspace = self.workspace_label();
        self.selected_workspace_id
            .and_then(|selected| {
                self.lifecycle
                    .tenants
                    .iter()
                    .flat_map(|tenant| &tenant.rooms)
                    .find(|room| room.workspaces.iter().any(|entry| entry.id == selected))
                    .map(|room| format!("{} / {workspace}", room.name))
            })
            .unwrap_or(workspace)
    }

    fn active_server_name(&self) -> String {
        self.lifecycle
            .selected_instance
            .as_ref()
            .map(|instance| instance.name.clone())
            .unwrap_or_else(|| "Local Sift".into())
    }

    fn account_initials(&self) -> String {
        let Some(identity) = &self.lifecycle.identity else {
            return "?".into();
        };
        let initials = identity
            .principal
            .display_name
            .split_whitespace()
            .filter_map(|part| part.chars().next())
            .take(2)
            .flat_map(char::to_uppercase)
            .collect::<String>();
        if initials.is_empty() {
            "?".into()
        } else {
            initials
        }
    }

    fn render_account_icon(&self, size: f32, cx: &App) -> gpui::AnyElement {
        let colors = cx.theme().colors;
        match &self.lifecycle.identity {
            Some(identity) => div()
                .relative()
                .size(px(size))
                .flex()
                .items_center()
                .justify_center()
                .overflow_hidden()
                .rounded_sm()
                .bg(colors.accent_muted)
                .text_xs()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .child(self.account_initials())
                .children(identity.principal.avatar_url.clone().map(|url| {
                    img(url)
                        .absolute()
                        .inset_0()
                        .size_full()
                        .object_fit(gpui::ObjectFit::Cover)
                }))
                .into_any_element(),
            None => icon(IconName::User, colors.muted_text, size * 0.72),
        }
    }

    pub fn active_pane(&self) -> usize {
        self.active_pane
    }

    pub fn modal(&self) -> Option<&Modal> {
        self.modal.as_ref()
    }

    pub fn active_item_dirty(&self, cx: &App) -> Option<bool> {
        self.panes
            .get(self.active_pane)
            .and_then(|pane| pane.read(cx).active_item().map(|item| item.dirty))
    }

    pub fn active_item_count(&self, cx: &App) -> usize {
        self.panes
            .get(self.active_pane)
            .map_or(0, |pane| pane.read(cx).items.len())
    }

    pub fn lifecycle(&self) -> &LifecycleProjection {
        &self.lifecycle
    }

    pub fn attach_lifecycle(
        &mut self,
        mut receiver: tokio::sync::mpsc::UnboundedReceiver<LifecycleEvent>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self._lifecycle_task = Some(cx.spawn_in(window, async move |shell, cx| {
            while let Some(event) = receiver.recv().await {
                if shell
                    .update_in(cx, |shell, window, cx| {
                        let mut instance_changed = false;
                        if let LifecycleEvent::TenantLoaded(tenant) = &event {
                            shell.expanded_tenants.insert(tenant.id.0);
                            shell
                                .expanded_rooms
                                .extend(tenant.rooms.iter().map(|room| room.id.0));
                        }
                        if let LifecycleEvent::Selected(instance) = &event {
                            instance_changed =
                                shell.selected_instance_id.as_deref() != Some(instance.id.as_str());
                            if instance_changed {
                                shell.switch_instance_workspace(&instance.id, window, cx);
                            }
                        }
                        shell.lifecycle.apply(event);
                        shell.status.connection = shell.lifecycle.status_label();
                        shell.reconcile_restored_workspace(cx);
                        if instance_changed {
                            shell.persist(cx);
                        }
                        cx.notify();
                    })
                    .is_err()
                {
                    break;
                }
            }
        }));
    }

    fn switch_instance_workspace(
        &mut self,
        instance_id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.selected_instance_id.as_deref() == Some(instance_id) {
            return;
        }

        let target = self
            .workspace_sessions
            .remove(instance_id)
            .unwrap_or_else(|| {
                let mut presentation = self
                    .workspace_presentations
                    .remove(instance_id)
                    .unwrap_or_else(|| PresentationState::default().workspace);
                presentation.instance_id = Some(instance_id.to_owned());
                Self::session_from_presentation(presentation, self.vim_mode_default(), window, cx)
            });

        let outgoing = WorkspaceSession {
            panes: std::mem::replace(&mut self.panes, target.panes),
            pane_flexes: std::mem::replace(&mut self.pane_flexes, target.pane_flexes),
            active_pane: std::mem::replace(&mut self.active_pane, target.active_pane),
            selected_workspace_id: std::mem::replace(
                &mut self.selected_workspace_id,
                target.selected_workspace_id,
            ),
            left_dock: std::mem::replace(&mut self.left_dock, target.left_dock),
            right_dock: std::mem::replace(&mut self.right_dock, target.right_dock),
            bottom_dock: std::mem::replace(&mut self.bottom_dock, target.bottom_dock),
            active_left_panel: std::mem::replace(
                &mut self.active_left_panel,
                target.active_left_panel,
            ),
            active_bottom_tool: std::mem::replace(
                &mut self.active_bottom_tool,
                target.active_bottom_tool,
            ),
            sql_problems: std::mem::replace(&mut self.sql_problems, target.sql_problems),
            expanded_tenants: std::mem::replace(
                &mut self.expanded_tenants,
                target.expanded_tenants,
            ),
            expanded_connections: std::mem::replace(
                &mut self.expanded_connections,
                target.expanded_connections,
            ),
            expanded_rooms: std::mem::replace(&mut self.expanded_rooms, target.expanded_rooms),
            expanded_catalogs: std::mem::replace(
                &mut self.expanded_catalogs,
                target.expanded_catalogs,
            ),
            expanded_schemas: std::mem::replace(
                &mut self.expanded_schemas,
                target.expanded_schemas,
            ),
        };
        if let Some(previous) = self.selected_instance_id.replace(instance_id.to_owned()) {
            self.workspace_sessions.insert(previous, outgoing);
        }

        if let Some(sender) = &self.executor_sender {
            let _ = sender.send(ExecutorCommand::Disconnect);
        }
        self.connection_status = ConnectionStatus::Disconnected;
        self.connection_schema = ConnectionSchemaState::Unavailable;
        self.pending_database_execution = None;
        for pane in &self.panes {
            pane.update(cx, |pane, _| {
                pane.set_all_database_item_states(DatabaseItemState::Offline)
            });
        }
        self.presence.apply(PresenceEvent::Left);
        self.modal = None;
        self.app_bar_menu = None;
        self.active_pane = self.active_pane.min(self.panes.len().saturating_sub(1));
        self.next_id = self.next_id.max(
            self.panes
                .iter()
                .flat_map(|pane| {
                    pane.read(cx)
                        .items
                        .iter()
                        .map(|item| item.id)
                        .chain(std::iter::once(pane.read(cx).id))
                })
                .max()
                .unwrap_or(0)
                + 1,
        );
        if let Some(pane) = self.panes.get(self.active_pane) {
            pane.read(cx).active_focus_handle(cx).focus(window, cx);
        }
    }

    fn session_from_presentation(
        workspace: WorkspacePresentation,
        vim_mode_default: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> WorkspaceSession {
        let workspace = if workspace.panes.is_empty() {
            PresentationState::default().workspace
        } else {
            workspace
        };
        let panes = workspace
            .panes
            .into_iter()
            .map(|pane| cx.new(|cx| Pane::from_presentation(pane, vim_mode_default, cx)))
            .collect::<Vec<_>>();
        for pane in &panes {
            cx.subscribe_in(pane, window, Self::on_pane_event).detach();
        }
        let active_pane = workspace.active_pane.min(panes.len().saturating_sub(1));
        let pane_flexes = valid_pane_flexes(workspace.pane_flexes, panes.len());
        WorkspaceSession {
            panes,
            pane_flexes,
            active_pane,
            selected_workspace_id: workspace.workspace_id,
            left_dock: DockRegistry::create(DockId::Left, workspace.left_dock),
            right_dock: DockRegistry::create(DockId::Inspector, workspace.right_dock),
            bottom_dock: DockRegistry::create(DockId::Bottom, workspace.bottom_dock),
            active_left_panel: workspace.left_panel,
            active_bottom_tool: workspace.bottom_tool,
            sql_problems: Vec::new(),
            expanded_tenants: HashSet::new(),
            expanded_connections: HashSet::new(),
            expanded_rooms: HashSet::new(),
            expanded_catalogs: HashSet::new(),
            expanded_schemas: HashSet::new(),
        }
    }

    pub fn attach_presence(
        &mut self,
        mut receiver: tokio::sync::mpsc::UnboundedReceiver<PresenceEvent>,
        cx: &mut Context<Self>,
    ) {
        self._presence_task = Some(cx.spawn(async move |shell, cx| {
            while let Some(event) = receiver.recv().await {
                if shell
                    .update(cx, |shell, cx| {
                        shell.presence.apply(event);
                        cx.notify();
                    })
                    .is_err()
                {
                    break;
                }
            }
        }));
    }

    /// Attach the executor: `sender` carries connect/disconnect/run commands to
    /// the executor task, `receiver` delivers connection-state and query
    /// outcomes back onto the UI thread.
    pub fn attach_executor(
        &mut self,
        sender: tokio::sync::mpsc::UnboundedSender<ExecutorCommand>,
        mut receiver: tokio::sync::mpsc::UnboundedReceiver<ExecutorEvent>,
        cx: &mut Context<Self>,
    ) {
        self.executor_sender = Some(sender);
        self._executor_task = Some(cx.spawn(async move |shell, cx| {
            while let Some(event) = receiver.recv().await {
                if shell
                    .update(cx, |shell, cx| shell.on_executor_event(event, cx))
                    .is_err()
                {
                    break;
                }
            }
        }));
    }

    pub fn attach_instance_manager(
        &mut self,
        sender: tokio::sync::mpsc::UnboundedSender<InstanceCommand>,
        mut receiver: tokio::sync::mpsc::UnboundedReceiver<InstanceManagerEvent>,
        profiles: Vec<SavedServerProfile>,
        cx: &mut Context<Self>,
    ) {
        self.instance_sender = Some(sender);
        self.saved_servers = profiles;
        self._instance_task = Some(cx.spawn(async move |shell, cx| {
            while let Some(event) = receiver.recv().await {
                if shell
                    .update(cx, |shell, cx| shell.on_instance_manager_event(event, cx))
                    .is_err()
                {
                    break;
                }
            }
        }));
    }

    fn on_instance_manager_event(&mut self, event: InstanceManagerEvent, cx: &mut Context<Self>) {
        match event {
            InstanceManagerEvent::Profiles(profiles) => self.saved_servers = profiles,
            InstanceManagerEvent::Roots(roots) => {
                self.instance_roots = roots;
                self.instance_operation_pending = false;
                self.instance_operation_error = None;
            }
            InstanceManagerEvent::InstancePlan(plan) => {
                self.instance_operation_pending = false;
                self.instance_operation_error = None;
                self.instance_plan = Some(*plan);
                self.modal = Some(Modal::InstanceSetup);
            }
            InstanceManagerEvent::InstanceConfiguration(configuration) => {
                self.instance_operation_pending = false;
                self.instance_operation_error = None;
                let editor = cx.new(|cx| {
                    QueryEditor::new(QueryDocument::with_random_peer(&configuration.manifest), cx)
                        .with_language(EditorLanguage::Toml)
                        .with_keymap(if self.vim_mode_default() {
                            EditorKeymap::Vim
                        } else {
                            EditorKeymap::Standard
                        })
                });
                self.instance_configuration_editor = editor.clone();
                let item_id = self.instance_configuration_item.unwrap_or_else(|| {
                    let item_id = self.next_id;
                    self.next_id += 1;
                    item_id
                });
                let pane_index = self
                    .panes
                    .iter()
                    .position(|pane| pane.read(cx).contains_item(item_id))
                    .unwrap_or(self.active_pane);
                if let Some(pane) = self.panes.get(pane_index) {
                    pane.update(cx, |pane, cx| {
                        pane.open_configuration(
                            ItemPresentation {
                                id: item_id,
                                kind: ItemKind::Configuration,
                                title: "sift.toml".into(),
                                dirty: false,
                                source: None,
                            },
                            editor,
                            cx,
                        )
                    });
                    self.active_pane = pane_index;
                }
                self.instance_configuration_item = Some(item_id);
                self.instance_configuration = Some(*configuration);
                self.modal = None;
            }
            InstanceManagerEvent::InstanceOperationPending { message } => {
                self.instance_operation_pending = true;
                self.instance_operation_error = None;
                self.show_toast(message, cx);
            }
            InstanceManagerEvent::Testing => {
                self.server_connection_pending = true;
                self.server_connection_error = None;
            }
            InstanceManagerEvent::Connected { name } => {
                self.server_connection_pending = false;
                self.server_connection_error = None;
                self.modal = None;
                self.server_token_input
                    .update(cx, |input, cx| input.set_text("", cx));
                self.show_success_toast(format!("Connected to {name}"), cx);
            }
            InstanceManagerEvent::Failed { message } => {
                self.server_connection_pending = false;
                self.instance_operation_pending = false;
                self.instance_operation_error = Some(message.clone());
                self.server_connection_error = Some(message);
            }
            InstanceManagerEvent::AuthenticationPending => {
                self.account_pending = true;
                self.account_error = None;
            }
            InstanceManagerEvent::GithubAuthorization { url } => {
                cx.open_url(&url);
                self.show_toast("Complete sign in in your browser".into(), cx);
            }
            InstanceManagerEvent::Authenticated { display_name } => {
                self.account_pending = false;
                self.account_error = None;
                self.modal = None;
                self.account_password_input
                    .update(cx, |input, cx| input.set_text("", cx));
                self.show_success_toast(format!("Signed in as {display_name}"), cx);
            }
            InstanceManagerEvent::SignedOut => {
                self.account_pending = false;
                self.account_error = None;
                self.modal = None;
                self.show_toast("Signed out".into(), cx);
            }
            InstanceManagerEvent::AuthenticationFailed { message } => {
                self.account_pending = false;
                self.account_error = Some(message);
            }
        }
        cx.notify();
    }

    fn on_executor_event(&mut self, event: ExecutorEvent, cx: &mut Context<Self>) {
        match event {
            ExecutorEvent::Connection(status) => {
                if let ConnectionStatus::Connected { profile_id, .. } = &status {
                    self.expanded_connections.insert(*profile_id);
                }
                self.status.database = match &status {
                    ConnectionStatus::Connected { name, .. } => name.clone(),
                    ConnectionStatus::Connecting { .. } => "Connecting…".into(),
                    ConnectionStatus::Failed { .. } => "Connection failed".into(),
                    ConnectionStatus::Disconnected => "No database".into(),
                };
                self.status.current_error = match &status {
                    ConnectionStatus::Failed { reason, .. } => Some(reason.clone()),
                    _ => None,
                };
                self.status.diagnostic_count = usize::from(self.status.current_error.is_some());
                if matches!(
                    status,
                    ConnectionStatus::Disconnected | ConnectionStatus::Failed { .. }
                ) {
                    self.connection_schema = ConnectionSchemaState::Unavailable;
                }
                self.connection_status = status.clone();
                self.sync_database_item_states(cx);
                match status {
                    ConnectionStatus::Connected { profile_id, .. } => {
                        let pending = self.pending_database_execution.take().filter(|pending| {
                            pending.source.profile_id == profile_id
                                && pending.source.instance_id
                                    == self.selected_instance_id.as_deref().unwrap_or_default()
                        });
                        if let Some(pending) = pending {
                            self.send_execution(pending.item_id, pending.sql, cx);
                        }
                    }
                    ConnectionStatus::Failed { profile_id, .. } => {
                        if self
                            .pending_database_execution
                            .as_ref()
                            .is_some_and(|pending| pending.source.profile_id == profile_id)
                        {
                            self.pending_database_execution = None;
                        }
                    }
                    ConnectionStatus::Disconnected | ConnectionStatus::Connecting { .. } => {}
                }
                cx.notify();
            }
            ExecutorEvent::SchemaLoaded {
                profile_id,
                snapshot,
            } => {
                self.expanded_connections.insert(profile_id);
                if matches!(
                    self.connection_schema,
                    ConnectionSchemaState::Failed {
                        profile_id: failed,
                        ..
                    } if failed == profile_id
                ) {
                    self.status.current_error = None;
                    self.status.diagnostic_count = 0;
                }
                self.expanded_catalogs
                    .retain(|(expanded_profile, _)| *expanded_profile != profile_id);
                self.expanded_schemas
                    .retain(|(expanded_profile, _, _)| *expanded_profile != profile_id);
                if snapshot.trees.len() == 1 {
                    self.expanded_catalogs
                        .insert((profile_id, snapshot.trees[0].name.clone()));
                }
                for catalog in &snapshot.trees {
                    for schema in &catalog.schemas {
                        if catalog.schemas.len() == 1
                            || matches!(schema.name.as_str(), "lab" | "public" | "dbo")
                        {
                            self.expanded_schemas.insert((
                                profile_id,
                                catalog.name.clone(),
                                schema.name.clone(),
                            ));
                        }
                    }
                }
                self.connection_schema = ConnectionSchemaState::Ready {
                    profile_id,
                    snapshot,
                };
                cx.notify();
            }
            ExecutorEvent::SchemaLoadFailed {
                profile_id,
                message,
            } => {
                self.connection_schema = ConnectionSchemaState::Failed {
                    profile_id,
                    message: message.clone(),
                };
                self.status.current_error = Some(message);
                self.status.diagnostic_count = 1;
                cx.notify();
            }
            ExecutorEvent::Execution { item_id, state } => self.route_result(item_id, state, cx),
            ExecutorEvent::ProfileCreated {
                entry,
                connection_error,
            } => {
                if let Some(tenant) = self
                    .lifecycle
                    .tenants
                    .iter_mut()
                    .find(|tenant| tenant.id.0 == entry.tenant_id)
                {
                    tenant.connections.push(entry.clone());
                    tenant
                        .connections
                        .sort_by(|left, right| left.name.cmp(&right.name));
                }
                self.database_connection_pending = false;
                self.database_connection_error = None;
                self.modal = None;
                self.database_password_input
                    .update(cx, |input, cx| input.set_text("", cx));
                match connection_error {
                    Some(error) => {
                        self.show_error_toast(
                            format!("Added {}, but connection failed: {error}", entry.name),
                            cx,
                        );
                    }
                    None => {
                        self.show_success_toast(
                            format!("Added and connected to {}", entry.name),
                            cx,
                        );
                    }
                }
            }
            ExecutorEvent::ProfileCreationFailed(message) => {
                self.database_connection_pending = false;
                self.database_connection_error = Some(message.clone());
                self.status.current_error = Some(message);
                self.status.diagnostic_count = 1;
                cx.notify();
            }
            ExecutorEvent::ProfileDeleted {
                tenant_id,
                profile_id,
            } => {
                if let Some(tenant) = self
                    .lifecycle
                    .tenants
                    .iter_mut()
                    .find(|tenant| tenant.id.0 == tenant_id)
                {
                    tenant.connections.retain(|entry| entry.id != profile_id);
                }
                if matches!(
                    self.connection_status,
                    ConnectionStatus::Connected { profile_id: current, .. }
                        | ConnectionStatus::Connecting { profile_id: current }
                        | ConnectionStatus::Failed { profile_id: current, .. }
                        if current == profile_id
                ) {
                    self.connection_status = ConnectionStatus::Disconnected;
                    self.connection_schema = ConnectionSchemaState::Unavailable;
                    self.status.database = "No database".into();
                }
                self.show_toast("Connection deleted".into(), cx);
            }
            ExecutorEvent::ProfileDeletionFailed(message) => {
                self.status.current_error = Some(message.clone());
                self.status.diagnostic_count = 1;
                self.show_error_toast(message, cx);
            }
        }
    }

    fn show_toast(&mut self, message: String, cx: &mut Context<Self>) {
        self.push_toast(message, ToastTone::Info, cx);
    }

    fn show_success_toast(&mut self, message: String, cx: &mut Context<Self>) {
        self.push_toast(message, ToastTone::Success, cx);
    }

    fn show_error_toast(&mut self, message: String, cx: &mut Context<Self>) {
        self.push_toast(message, ToastTone::Error, cx);
    }

    /// Queue a toast. The newest toast replaces the oldest beyond three
    /// visible at once, and each auto-dismisses independently.
    fn push_toast(&mut self, message: String, tone: ToastTone, cx: &mut Context<Self>) {
        let id = self.next_toast_id;
        self.next_toast_id += 1;
        if self.toasts.len() >= 3 {
            self.toasts.remove(0);
        }
        self.toasts.push(Toast { id, message, tone });
        cx.spawn(async move |shell, cx| {
            cx.background_executor()
                .timer(std::time::Duration::from_secs(3))
                .await;
            let _ = shell.update(cx, |shell, cx| {
                shell.toasts.retain(|toast| toast.id != id);
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn dismiss_toast(&mut self, cx: &mut Context<Self>) {
        self.toasts.clear();
        cx.notify();
    }

    pub fn connection_status(&self) -> &ConnectionStatus {
        &self.connection_status
    }

    /// Ask the executor to open `entry`. Optimistically shows connecting.
    fn connect(&mut self, entry: &ConnectionNavEntry, cx: &mut Context<Self>) {
        let Some(sender) = &self.executor_sender else {
            return;
        };
        self.pending_database_execution = None;
        if sender
            .send(ExecutorCommand::Connect {
                tenant_id: entry.tenant_id,
                profile_id: entry.id,
                name: entry.name.clone(),
            })
            .is_ok()
        {
            self.connection_status = ConnectionStatus::Connecting {
                profile_id: entry.id,
            };
            self.connection_schema = ConnectionSchemaState::Loading {
                profile_id: entry.id,
            };
            self.sync_database_item_states(cx);
            cx.notify();
        }
    }

    fn database_source(&self, item_id: u64, cx: &App) -> Option<DatabaseObjectSource> {
        self.panes
            .iter()
            .find_map(|pane| pane.read(cx).database_source(item_id))
    }

    fn sync_database_item_states(&mut self, cx: &mut Context<Self>) {
        let status = self.connection_status.clone();
        for pane in &self.panes {
            pane.update(cx, |pane, _| {
                let sources = pane
                    .items
                    .iter()
                    .filter_map(|item| {
                        pane.database_source(item.id)
                            .map(|source| (item.id, source))
                    })
                    .collect::<Vec<_>>();
                for (item_id, source) in sources {
                    let state = match &status {
                        ConnectionStatus::Connected { profile_id, .. }
                            if *profile_id == source.profile_id =>
                        {
                            DatabaseItemState::Live
                        }
                        ConnectionStatus::Connecting { profile_id }
                            if *profile_id == source.profile_id =>
                        {
                            DatabaseItemState::Reconnecting
                        }
                        ConnectionStatus::Failed { profile_id, reason }
                            if *profile_id == source.profile_id =>
                        {
                            DatabaseItemState::Failed(reason.clone())
                        }
                        _ => DatabaseItemState::Offline,
                    };
                    pane.set_database_item_state(item_id, state);
                }
            });
        }
    }

    fn execute_database_item(&mut self, item_id: u64, sql: String, cx: &mut Context<Self>) {
        let Some(source) = self.database_source(item_id, cx) else {
            self.send_execution(item_id, sql, cx);
            return;
        };
        if self.selected_instance_id.as_deref() != Some(source.instance_id.as_str()) {
            self.route_result(
                item_id,
                ResultState::Unavailable("Tab belongs to another Sift server".into()),
                cx,
            );
            return;
        }
        if matches!(
            self.connection_status,
            ConnectionStatus::Connected { profile_id, .. } if profile_id == source.profile_id
        ) {
            self.sync_database_item_states(cx);
            self.send_execution(item_id, sql, cx);
            return;
        }

        self.pending_database_execution = Some(PendingDatabaseExecution {
            item_id,
            sql,
            source: source.clone(),
        });
        let Some(sender) = &self.executor_sender else {
            self.pending_database_execution = None;
            self.route_result(
                item_id,
                ResultState::Unavailable("Database connection manager is unavailable".into()),
                cx,
            );
            return;
        };
        let needs_connect = !matches!(
            self.connection_status,
            ConnectionStatus::Connecting { profile_id } if profile_id == source.profile_id
        );
        if needs_connect
            && sender
                .send(ExecutorCommand::Connect {
                    tenant_id: source.tenant_id,
                    profile_id: source.profile_id,
                    name: source.profile_name,
                })
                .is_err()
        {
            self.pending_database_execution = None;
            self.connection_status = ConnectionStatus::Failed {
                profile_id: source.profile_id,
                reason: "Database connection manager stopped".into(),
            };
            self.sync_database_item_states(cx);
            cx.notify();
            return;
        }
        self.connection_status = ConnectionStatus::Connecting {
            profile_id: source.profile_id,
        };
        self.status.database = "Connecting…".into();
        self.sync_database_item_states(cx);
        cx.notify();
    }

    fn send_execution(&mut self, item_id: u64, sql: String, cx: &mut Context<Self>) {
        let Some(sender) = &self.executor_sender else {
            self.route_result(
                item_id,
                ResultState::Unavailable("Not connected to a database.".into()),
                cx,
            );
            return;
        };
        if sender
            .send(ExecutorCommand::Execute { item_id, sql })
            .is_ok()
        {
            self.status.execution = "Running…".into();
            self.clear_sql_problems(item_id);
            cx.notify();
        }
    }

    fn refresh_database_item(&mut self, item_id: u64, cx: &mut Context<Self>) {
        let sql = self.panes.iter().find_map(|pane| {
            let pane = pane.read(cx);
            pane.editor(item_id)
                .map(|editor| editor.read(cx).document().text().to_owned())
        });
        if let Some(sql) = sql {
            self.execute_database_item(item_id, sql, cx);
        }
    }

    fn refresh_connection_schema(&mut self, cx: &mut Context<Self>) {
        let ConnectionStatus::Connected { profile_id, .. } = &self.connection_status else {
            return;
        };
        let profile_id = *profile_id;
        let Some(sender) = &self.executor_sender else {
            return;
        };
        if sender.send(ExecutorCommand::RefreshSchema).is_ok() {
            self.connection_schema = ConnectionSchemaState::Loading { profile_id };
            cx.notify();
        }
    }

    fn disconnect(&mut self, cx: &mut Context<Self>) {
        if let Some(sender) = &self.executor_sender {
            let _ = sender.send(ExecutorCommand::Disconnect);
        }
        self.connection_status = ConnectionStatus::Disconnected;
        self.connection_schema = ConnectionSchemaState::Unavailable;
        self.status.database = "No database".into();
        self.sync_database_item_states(cx);
        cx.notify();
    }

    fn toggle_catalog_schema(&mut self, profile_id: i64, catalog: String, cx: &mut Context<Self>) {
        let key = (profile_id, catalog);
        if !self.expanded_catalogs.remove(&key) {
            self.expanded_catalogs.insert(key);
        }
        cx.notify();
    }

    fn toggle_tenant(&mut self, tenant_id: i64, cx: &mut Context<Self>) {
        if !self.expanded_tenants.remove(&tenant_id) {
            self.expanded_tenants.insert(tenant_id);
        }
        cx.notify();
    }

    fn toggle_connection(&mut self, profile_id: i64, cx: &mut Context<Self>) {
        if !self.expanded_connections.remove(&profile_id) {
            self.expanded_connections.insert(profile_id);
        }
        cx.notify();
    }

    fn toggle_room(&mut self, room_id: i64, cx: &mut Context<Self>) {
        if !self.expanded_rooms.remove(&room_id) {
            self.expanded_rooms.insert(room_id);
        }
        cx.notify();
    }

    fn toggle_database_schema(
        &mut self,
        profile_id: i64,
        catalog: String,
        schema: String,
        cx: &mut Context<Self>,
    ) {
        let key = (profile_id, catalog, schema);
        if !self.expanded_schemas.remove(&key) {
            self.expanded_schemas.insert(key);
        }
        cx.notify();
    }

    fn open_table_preview(
        &mut self,
        target: DatabaseObjectTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let DatabaseObjectTarget {
            connection,
            catalog,
            schema,
            object,
            object_kind,
        } = target;
        let source = DatabaseObjectSource {
            instance_id: self
                .selected_instance_id
                .clone()
                .unwrap_or_else(|| "local".into()),
            tenant_id: connection.tenant_id,
            profile_id: connection.id,
            profile_name: connection.name.clone(),
            provider_id: connection.provider_id.clone(),
            catalog: Some(catalog),
            schema: schema.clone(),
            object: object.clone(),
            object_kind,
            last_refreshed_at_ms: None,
        };
        let title = format!("{schema}.{object}");
        if self.focus_open_database_item(&source, window, cx) {
            return;
        }
        let item_id = self.next_id;
        self.next_id += 1;
        let sql = table_preview_sql(&source.provider_id, &schema, &object);
        let keymap = if self.vim_mode_default() {
            EditorKeymap::Vim
        } else {
            EditorKeymap::Standard
        };
        let editor = cx.new(|cx| {
            QueryEditor::new(QueryDocument::with_random_peer(&sql), cx).with_keymap(keymap)
        });
        let results = cx.new(ResultsView::new);
        results.update(cx, |results, cx| results.set_pending(cx));
        if let Some(pane) = self.panes.get(self.active_pane) {
            pane.update(cx, |pane, cx| {
                pane.open_query(
                    ItemPresentation {
                        id: item_id,
                        kind: ItemKind::Query,
                        title,
                        dirty: false,
                        source: Some(ItemSource::DatabaseObject(source)),
                    },
                    editor,
                    results,
                    cx,
                )
            });
        }
        self.execute_database_item(item_id, sql, cx);
        self.focus_active_pane(window, cx);
        self.persist(cx);
        cx.notify();
    }

    fn focus_open_database_item(
        &mut self,
        source: &DatabaseObjectSource,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let found = self
            .panes
            .iter()
            .enumerate()
            .find_map(|(pane_index, pane)| {
                pane.read(cx)
                    .items
                    .iter()
                    .enumerate()
                    .find(|(_, item)| {
                        let Some(ItemSource::DatabaseObject(existing)) = item.source.as_ref()
                        else {
                            return false;
                        };
                        existing.instance_id == source.instance_id
                            && existing.profile_id == source.profile_id
                            && existing.catalog == source.catalog
                            && existing.schema == source.schema
                            && existing.object == source.object
                            && existing.object_kind == source.object_kind
                    })
                    .map(|(item_index, _)| (pane_index, item_index))
            });
        let Some((pane_index, item_index)) = found else {
            return false;
        };
        self.active_pane = pane_index;
        self.panes[pane_index].update(cx, |pane, _| pane.activate_item(item_index, true));
        self.focus_active_pane(window, cx);
        cx.notify();
        true
    }

    /// Focus an already-open semantic item instead of creating a duplicate.
    /// Dynamic resources currently use their kind and stable display title as
    /// identity; this can graduate to an explicit resource key when more query
    /// item sources are introduced.
    fn focus_open_item(
        &mut self,
        kind: ItemKind,
        title: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let existing = self
            .panes
            .iter()
            .enumerate()
            .find_map(|(pane_index, pane)| {
                pane.read(cx)
                    .items
                    .iter()
                    .position(|item| item.kind == kind && item.title == title)
                    .map(|item_index| (pane_index, item_index))
            });
        let Some((pane_index, item_index)) = existing else {
            return false;
        };
        self.active_pane = pane_index;
        self.panes[pane_index].update(cx, |pane, _| pane.activate_item(item_index, true));
        self.focus_active_pane(window, cx);
        cx.notify();
        true
    }

    fn open_user_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(store) = self.settings_store.clone() else {
            self.show_toast(
                "The settings file is unavailable in this session".into(),
                cx,
            );
            return;
        };
        let source = match store.read_text() {
            Ok(source) => source,
            Err(error) => {
                self.show_toast(error, cx);
                return;
            }
        };
        self.modal = None;
        if self.focus_open_item(ItemKind::Configuration, "settings.toml", window, cx) {
            return;
        }

        let item_id = self.next_id;
        self.next_id += 1;
        let keymap = if self.vim_mode_default() {
            EditorKeymap::Vim
        } else {
            EditorKeymap::Standard
        };
        let editor = cx.new(|cx| {
            QueryEditor::new(QueryDocument::with_random_peer(&source), cx)
                .with_language(EditorLanguage::Toml)
                .with_keymap(keymap)
        });
        if let Some(pane) = self.panes.get(self.active_pane) {
            pane.update(cx, |pane, cx| {
                pane.open_configuration(
                    ItemPresentation {
                        id: item_id,
                        kind: ItemKind::Configuration,
                        title: "settings.toml".into(),
                        dirty: false,
                        source: None,
                    },
                    editor,
                    cx,
                )
            });
        }
        self.settings_item = Some(item_id);
        self.focus_active_pane(window, cx);
        cx.notify();
    }

    fn request_delete_connection(&mut self, entry: &ConnectionNavEntry, cx: &mut Context<Self>) {
        self.modal = Some(Modal::ConfirmDeleteConnection(entry.clone()));
        cx.notify();
    }

    fn confirm_delete_connection(&mut self, entry: &ConnectionNavEntry, cx: &mut Context<Self>) {
        let Some(sender) = &self.executor_sender else {
            self.show_toast("Database connection manager is unavailable".into(), cx);
            return;
        };
        if sender
            .send(ExecutorCommand::DeleteConnectionProfile {
                tenant_id: entry.tenant_id,
                profile_id: entry.id,
            })
            .is_err()
        {
            self.show_toast("Database connection manager stopped".into(), cx);
        } else {
            self.modal = None;
            cx.notify();
        }
    }

    fn open_database_connection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.selected_database_tenant = self.lifecycle.tenants.first().map(|tenant| tenant.id.0);
        self.selected_database_provider = None;
        self.database_wizard_step = DatabaseWizardStep::Provider;
        self.database_name_input
            .update(cx, |input, cx| input.set_text("", cx));
        self.database_host_input
            .update(cx, |input, cx| input.set_text("localhost", cx));
        self.database_port_input
            .update(cx, |input, cx| input.set_text("", cx));
        self.database_catalog_input
            .update(cx, |input, cx| input.set_text("", cx));
        self.database_user_input
            .update(cx, |input, cx| input.set_text("", cx));
        self.database_password_input
            .update(cx, |input, cx| input.set_text("", cx));
        for input in [
            &self.database_search_path_input,
            &self.database_application_name_input,
            &self.database_timeout_input,
            &self.database_pool_min_input,
            &self.database_pool_max_input,
        ] {
            input.update(cx, |input, cx| input.set_text("", cx));
        }
        self.selected_database_ssl_mode = None;
        self.database_connection_error = None;
        self.database_connection_pending = false;
        self.modal = Some(Modal::DatabaseConnection);
        self.focus_handle.focus(window, cx);
        cx.notify();
    }

    fn select_database_provider(&mut self, provider_id: String, cx: &mut Context<Self>) {
        let port = match provider_id.as_str() {
            "sift/postgres" => "5432",
            "sift/sql-server" => "1433",
            _ => "",
        };
        self.selected_database_provider = Some(provider_id);
        self.selected_database_ssl_mode = Some(
            match self.selected_database_provider.as_deref() {
                Some("sift/sql-server") => "require",
                _ => "prefer",
            }
            .into(),
        );
        self.database_port_input
            .update(cx, |input, cx| input.set_text(port, cx));
        self.configure_database_tab_order(cx);
        cx.notify();
    }

    fn configure_database_tab_order(&self, cx: &mut Context<Self>) {
        let mut fields = vec![
            self.database_name_input.clone(),
            self.database_catalog_input.clone(),
            self.database_host_input.clone(),
            self.database_port_input.clone(),
            self.database_user_input.clone(),
            self.database_password_input.clone(),
        ];
        if self.selected_database_provider.as_deref() == Some("sift/postgres") {
            fields.push(self.database_search_path_input.clone());
            fields.push(self.database_application_name_input.clone());
        }
        fields.push(self.database_timeout_input.clone());
        fields.push(self.database_pool_min_input.clone());
        if self.selected_database_provider.as_deref() == Some("sift/postgres") {
            fields.push(self.database_pool_max_input.clone());
        }

        let handles = fields
            .iter()
            .map(|field| field.focus_handle(cx))
            .collect::<Vec<_>>();
        for (index, field) in fields.into_iter().enumerate() {
            let previous = index.checked_sub(1).map(|index| handles[index].clone());
            let next = handles.get(index + 1).cloned();
            field.update(cx, |input, cx| input.set_tab_targets(previous, next, cx));
        }
    }

    fn database_form_error(&self, cx: &App) -> Option<String> {
        if self.selected_database_tenant.is_none() {
            return Some("Select a workspace".into());
        }
        if self.selected_database_provider.is_none() {
            return Some("Select a database type".into());
        }
        let required = [
            ("Connection name", &self.database_name_input),
            ("Host", &self.database_host_input),
            ("Username", &self.database_user_input),
        ];
        if let Some((label, _)) = required
            .into_iter()
            .find(|(_, input)| input.read(cx).text().trim().is_empty())
        {
            return Some(format!("{label} is required"));
        }
        let port = self.database_port_input.read(cx).text().trim();
        if !port.is_empty() && !matches!(port.parse::<u16>(), Ok(value) if value > 0) {
            return Some("Port must be between 1 and 65535".into());
        }
        for (input, label) in [
            (&self.database_timeout_input, "Connection timeout"),
            (&self.database_pool_max_input, "Maximum pool size"),
        ] {
            if let Err(error) = optional_u32_field(input, label, cx) {
                return Some(error);
            }
        }
        let pool_min = match optional_pool_min_field(&self.database_pool_min_input, cx) {
            Ok(value) => value,
            Err(error) => return Some(error),
        };
        let pool_max = optional_u32_field(&self.database_pool_max_input, "Maximum pool size", cx)
            .ok()
            .flatten();
        if matches!((pool_min, pool_max), (Some(min), Some(max)) if min > max) {
            return Some("Minimum pool size cannot exceed maximum pool size".into());
        }
        None
    }

    fn database_wizard_next(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.database_connection_error = None;
        match self.database_wizard_step {
            DatabaseWizardStep::Provider => {
                if self.selected_database_provider.is_none() {
                    self.database_connection_error =
                        Some("Choose a database type to continue".into());
                } else {
                    self.database_wizard_step = DatabaseWizardStep::Details;
                    self.database_name_input.focus_handle(cx).focus(window, cx);
                }
            }
            DatabaseWizardStep::Details => {
                if let Some(error) = self.database_form_error(cx) {
                    self.database_connection_error = Some(error);
                } else {
                    self.database_wizard_step = DatabaseWizardStep::Review;
                    self.focus_handle.focus(window, cx);
                }
            }
            DatabaseWizardStep::Review => {}
        }
        cx.notify();
    }

    fn database_wizard_back(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.database_connection_error = None;
        self.database_wizard_step = match self.database_wizard_step {
            DatabaseWizardStep::Provider => DatabaseWizardStep::Provider,
            DatabaseWizardStep::Details => DatabaseWizardStep::Provider,
            DatabaseWizardStep::Review => DatabaseWizardStep::Details,
        };
        if self.database_wizard_step == DatabaseWizardStep::Details {
            self.database_name_input.focus_handle(cx).focus(window, cx);
        } else {
            self.focus_handle.focus(window, cx);
        }
        cx.notify();
    }

    fn submit_database_connection(&mut self, cx: &mut Context<Self>) {
        if self.database_connection_pending {
            return;
        }
        let Some(sender) = &self.executor_sender else {
            self.database_connection_error =
                Some("Database connection manager is unavailable".into());
            cx.notify();
            return;
        };
        if let Some(error) = self.database_form_error(cx) {
            self.database_connection_error = Some(error);
            cx.notify();
            return;
        }
        let tenant_id = self
            .selected_database_tenant
            .expect("form validation checked tenant");
        let Some(provider_id) = self
            .selected_database_provider
            .as_deref()
            .and_then(|id| sift_protocol::ProviderId::new(id).ok())
        else {
            self.database_connection_error = Some("Select an available database provider".into());
            cx.notify();
            return;
        };
        let name = self.database_name_input.read(cx).text().trim().to_owned();
        let host = self.database_host_input.read(cx).text().trim().to_owned();
        let user = self.database_user_input.read(cx).text().trim().to_owned();
        let port_text = self.database_port_input.read(cx).text().trim().to_owned();
        let port = if port_text.is_empty() {
            None
        } else {
            match port_text.parse::<u16>() {
                Ok(port) if port > 0 => Some(port),
                _ => {
                    self.database_connection_error =
                        Some("Port must be between 1 and 65535".into());
                    cx.notify();
                    return;
                }
            }
        };
        let database = self
            .database_catalog_input
            .read(cx)
            .text()
            .trim()
            .to_owned();
        let password = self.database_password_input.read(cx).text().to_owned();
        let mut configuration = serde_json::Map::from_iter([
            ("host".into(), serde_json::Value::String(host)),
            ("user".into(), serde_json::Value::String(user)),
            ("password".into(), serde_json::Value::Null),
        ]);
        if let Some(port) = port {
            configuration.insert("port".into(), serde_json::json!(port));
        }
        if !database.is_empty() {
            configuration.insert("database".into(), serde_json::Value::String(database));
        }
        if let Some(security_mode) = &self.selected_database_ssl_mode {
            if provider_id.as_str() == "sift/sql-server" {
                let (encrypt, trust_server_certificate) = match security_mode.as_str() {
                    "disable" => (false, false),
                    "trust_server_certificate" => (true, true),
                    _ => (true, false),
                };
                let mut engine = serde_json::Map::from_iter([
                    ("engine".into(), serde_json::json!("sql_server")),
                    ("mars".into(), serde_json::json!(false)),
                    ("encrypt".into(), serde_json::json!(encrypt)),
                    (
                        "trust_server_certificate".into(),
                        serde_json::json!(trust_server_certificate),
                    ),
                ]);
                if let Some(timeout) =
                    optional_u32_field(&self.database_timeout_input, "Connection timeout", cx)
                        .expect("form validation checked timeout")
                {
                    engine.insert("connect_timeout_secs".into(), serde_json::json!(timeout));
                }
                if let Some(pool_min) = optional_pool_min_field(&self.database_pool_min_input, cx)
                    .expect("form validation checked pool minimum")
                {
                    engine.insert("pool_min_size".into(), serde_json::json!(pool_min));
                }
                configuration.insert("engine_specific".into(), engine.into());
            } else {
                configuration.insert(
                    "ssl_mode".into(),
                    serde_json::Value::String(security_mode.clone()),
                );
                let mut engine =
                    serde_json::Map::from_iter([("engine".into(), serde_json::json!("postgres"))]);
                let search_path = self.database_search_path_input.read(cx).text().trim();
                if !search_path.is_empty() {
                    engine.insert(
                        "search_path".into(),
                        serde_json::Value::Array(
                            search_path
                                .split(',')
                                .map(str::trim)
                                .filter(|value| !value.is_empty())
                                .map(|value| serde_json::Value::String(value.to_owned()))
                                .collect(),
                        ),
                    );
                }
                let application_name = self.database_application_name_input.read(cx).text().trim();
                if !application_name.is_empty() {
                    engine.insert(
                        "application_name".into(),
                        serde_json::Value::String(application_name.to_owned()),
                    );
                }
                for (key, input, label) in [
                    (
                        "connect_timeout_secs",
                        &self.database_timeout_input,
                        "Connection timeout",
                    ),
                    (
                        "pool_max_size",
                        &self.database_pool_max_input,
                        "Maximum pool size",
                    ),
                ] {
                    if let Some(value) = optional_u32_field(input, label, cx)
                        .expect("form validation checked numeric fields")
                    {
                        engine.insert(key.into(), serde_json::json!(value));
                    }
                }
                if let Some(pool_min) = optional_pool_min_field(&self.database_pool_min_input, cx)
                    .expect("form validation checked pool minimum")
                {
                    engine.insert("pool_min_size".into(), serde_json::json!(pool_min));
                }
                configuration.insert("engine_specific".into(), engine.into());
            }
        }
        let credentials = (!password.is_empty()).then(|| serde_json::json!({"password": password}));
        if sender
            .send(ExecutorCommand::CreateConnectionProfile {
                tenant_id,
                name,
                provider_id,
                configuration: serde_json::Value::Object(configuration),
                credentials,
            })
            .is_err()
        {
            self.database_connection_error = Some("Database connection manager stopped".into());
        } else {
            self.database_connection_pending = true;
            self.database_connection_error = None;
        }
        cx.notify();
    }

    /// Deliver an execution outcome to whichever pane owns the query item.
    fn route_result(&mut self, item_id: u64, state: ResultState, cx: &mut Context<Self>) {
        let refreshed = matches!(state, ResultState::Ready(_));
        self.status.execution = match &state {
            ResultState::Ready(_) | ResultState::Idle => "Ready".into(),
            _ => state.status_label(),
        };
        self.replace_sql_problems(item_id, &state, cx);
        for pane in &self.panes {
            if pane.update(cx, |pane, cx| {
                let routed = pane.set_result(item_id, state.clone(), cx);
                if routed && refreshed {
                    pane.mark_database_item_refreshed(item_id);
                }
                routed
            }) {
                break;
            }
        }
        cx.notify();
    }

    fn query_item_title(&self, item_id: u64, cx: &App) -> String {
        self.panes
            .iter()
            .find_map(|pane| {
                pane.read(cx)
                    .items
                    .iter()
                    .find(|item| item.id == item_id)
                    .map(|item| item.title.clone())
            })
            .unwrap_or_else(|| format!("Query {item_id}"))
    }

    fn replace_sql_problems(&mut self, item_id: u64, state: &ResultState, cx: &App) {
        self.sql_problems
            .retain(|problem| problem.item_id != Some(item_id));
        let title = self.query_item_title(item_id, cx);
        let mut push = |severity, message: String| {
            self.sql_problems.push(SqlProblem {
                item_id: Some(item_id),
                title: title.clone(),
                severity,
                message,
            });
        };
        match state {
            ResultState::Unavailable(message) | ResultState::Failed(message) => {
                push(SqlProblemSeverity::Error, message.clone());
            }
            ResultState::TimedOut => {
                push(SqlProblemSeverity::Error, "Query timed out".into());
            }
            ResultState::OutcomeUnknown => {
                push(SqlProblemSeverity::Error, "Query outcome is unknown".into());
            }
            ResultState::Ready(data) => {
                for warning in &data.warnings {
                    push(SqlProblemSeverity::Warning, warning.message.clone());
                }
            }
            ResultState::Idle | ResultState::Pending | ResultState::Cancelled => {}
        }
        self.sync_sql_problem_status();
    }

    fn clear_sql_problems(&mut self, item_id: u64) {
        self.sql_problems
            .retain(|problem| problem.item_id != Some(item_id));
        self.sync_sql_problem_status();
    }

    fn sync_sql_problem_status(&mut self) {
        self.status.diagnostic_count = self.sql_problems.len();
        self.status.current_error = self
            .sql_problems
            .iter()
            .rev()
            .find(|problem| problem.severity == SqlProblemSeverity::Error)
            .map(|problem| problem.message.clone());
    }

    fn visible_problems(&self) -> Vec<SqlProblem> {
        if !self.sql_problems.is_empty() {
            return self.sql_problems.clone();
        }
        self.status
            .current_error
            .as_ref()
            .map(|message| {
                vec![SqlProblem {
                    item_id: None,
                    title: "Workspace".into(),
                    severity: SqlProblemSeverity::Error,
                    message: message.clone(),
                }]
            })
            .unwrap_or_default()
    }

    fn copy_problem(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(problem) = self.visible_problems().get(index).cloned() else {
            return;
        };
        cx.write_to_clipboard(gpui::ClipboardItem::new_string(problem.message));
        self.show_toast("Copied problem".into(), cx);
    }

    fn copy_all_problems(&mut self, cx: &mut Context<Self>) {
        let problems = self.visible_problems();
        if problems.is_empty() {
            return;
        }
        let text = problems
            .iter()
            .map(|problem| {
                format!(
                    "[{}] {}: {}",
                    problem.severity.label(),
                    problem.title,
                    problem.message
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        cx.write_to_clipboard(gpui::ClipboardItem::new_string(text));
        self.show_toast("Copied all problems".into(), cx);
    }

    fn selected_workspace(&self) -> Option<&WorkspaceNavEntry> {
        let selected = self.selected_workspace_id?;
        self.lifecycle
            .tenants
            .iter()
            .flat_map(|tenant| &tenant.rooms)
            .flat_map(|room| &room.workspaces)
            .find(|workspace| workspace.id == selected)
    }

    fn select_left_panel(&mut self, panel: LeftPanel, cx: &mut Context<Self>) {
        if self.left_dock.presentation.open && self.active_left_panel == panel {
            self.left_dock.presentation.open = false;
        } else {
            self.active_left_panel = panel;
            self.left_dock.presentation.open = true;
        }
        self.fit_side_docks_to_width(self.window_presentation.bounds.width);
        self.persist(cx);
        cx.notify();
    }

    fn select_bottom_tool(&mut self, tool: BottomTool, cx: &mut Context<Self>) {
        if self.bottom_dock.presentation.open && self.active_bottom_tool == tool {
            self.bottom_dock.presentation.open = false;
        } else {
            self.active_bottom_tool = tool;
            self.bottom_dock.presentation.open = true;
        }
        self.persist(cx);
        cx.notify();
    }

    fn close_inspector(&mut self, cx: &mut Context<Self>) {
        self.right_dock.presentation.open = false;
        self.persist(cx);
        cx.notify();
    }

    fn show_project_search(&mut self, cx: &mut Context<Self>) {
        self.show_toast("Project search is not wired to the desktop yet".into(), cx);
    }

    fn show_diagnostics(&mut self, cx: &mut Context<Self>) {
        self.select_bottom_tool(BottomTool::Problems, cx);
    }

    fn copy_current_error(&mut self, cx: &mut Context<Self>) {
        let Some(error) = self.status.current_error.clone() else {
            return;
        };
        cx.write_to_clipboard(gpui::ClipboardItem::new_string(error));
        self.show_toast("Copied current diagnostic".into(), cx);
    }

    fn active_cursor_position(&self, cx: &App) -> Option<(usize, usize)> {
        self.panes
            .get(self.active_pane)
            .and_then(|pane| pane.read(cx).active_cursor_position(cx))
    }

    fn active_editor_mode(&self, cx: &App) -> Option<(EditorKeymap, VimMode, String)> {
        self.panes
            .get(self.active_pane)
            .and_then(|pane| pane.read(cx).active_editor_mode(cx))
    }

    fn toggle_active_editor_keymap(&mut self, cx: &mut Context<Self>) {
        let editor = self.panes.get(self.active_pane).and_then(|pane| {
            let pane = pane.read(cx);
            pane.active_item().and_then(|item| pane.editor(item.id))
        });
        if let Some(editor) = editor {
            editor.update(cx, |editor, cx| editor.toggle_keymap(cx));
        }
    }

    fn vim_mode_default(&self) -> bool {
        self.settings.editor.default_mode == EditorMode::Vim
    }

    /// Swap the process-wide theme and persist the preference. Views read the
    /// palette through `ActiveTheme`, so the refresh is automatic.
    fn toggle_theme(&mut self, cx: &mut Context<Self>) {
        self.dark_theme = !self.dark_theme;
        let theme = if self.dark_theme {
            Theme::dark()
        } else {
            Theme::light()
        };
        sift_ui::set_theme(theme, cx);
        self.persist(cx);
        cx.notify();
    }

    fn toggle_vim_mode_default(&mut self, cx: &mut Context<Self>) {
        let settings_is_open = self.settings_item.is_some_and(|item_id| {
            self.panes
                .iter()
                .any(|pane| pane.read(cx).contains_item(item_id))
        });
        if settings_is_open {
            self.show_toast(
                "Save or close settings.toml before changing this preference here".into(),
                cx,
            );
            return;
        }
        let mut settings = self.settings.clone();
        settings.editor.default_mode = if self.vim_mode_default() {
            EditorMode::Standard
        } else {
            EditorMode::Vim
        };
        if let Some(store) = &self.settings_store {
            settings = match store.save_editor_mode(settings.editor.default_mode) {
                Ok(settings) => settings,
                Err(error) => {
                    self.show_toast(error, cx);
                    return;
                }
            };
        }
        self.settings = settings;
        cx.notify();
    }

    pub fn open_workspace(&mut self, workspace: &WorkspaceNavEntry, cx: &mut Context<Self>) {
        self.selected_workspace_id = Some(workspace.id);
        self.presence.join(RoomId(workspace.room_id));
        self.persist(cx);
        cx.notify();
    }

    pub fn follow_participant(&mut self, attachment_id: i64, cx: &mut Context<Self>) -> bool {
        let followed = self.presence.follow(attachment_id);
        if followed {
            cx.notify();
        }
        followed
    }

    fn reconcile_restored_workspace(&mut self, cx: &mut Context<Self>) {
        if self.lifecycle.phase != crate::ConnectionPhase::Ready {
            return;
        }
        let Some(selected) = self.selected_workspace_id else {
            return;
        };
        let exists = self
            .lifecycle
            .tenants
            .iter()
            .flat_map(|tenant| &tenant.rooms)
            .flat_map(|room| &room.workspaces)
            .any(|workspace| workspace.id == selected);
        if !exists {
            self.selected_workspace_id = None;
            self.show_toast("Restored workspace is no longer available".into(), cx);
            self.persist(cx);
        }
    }

    pub fn mark_active_item_dirty(&mut self, dirty: bool, cx: &mut Context<Self>) {
        if let Some(pane) = self.panes.get(self.active_pane) {
            pane.update(cx, |pane, _| {
                if let Some(item) = pane.active_item_mut() {
                    item.dirty = dirty;
                }
            });
        }
    }

    pub fn snapshot(&self, cx: &App) -> PresentationState {
        let mut instance_workspaces = self.workspace_presentations.clone();
        instance_workspaces.extend(
            self.workspace_sessions
                .iter()
                .map(|(instance_id, session)| {
                    (
                        instance_id.clone(),
                        session.snapshot(instance_id.clone(), cx),
                    )
                }),
        );
        PresentationState {
            dark_theme: self.dark_theme,
            window: self.window_presentation.clone(),
            workspace: WorkspacePresentation {
                left_dock: self.left_dock.presentation.clone(),
                right_dock: self.right_dock.presentation.clone(),
                bottom_dock: self.bottom_dock.presentation.clone(),
                left_panel: self.active_left_panel,
                bottom_tool: self.active_bottom_tool,
                panes: self
                    .panes
                    .iter()
                    .map(|pane| pane.read(cx).snapshot())
                    .collect(),
                pane_flexes: self.pane_flexes.clone(),
                active_pane: self.active_pane,
                workspace_id: self.selected_workspace_id,
                instance_id: self.selected_instance_id.clone(),
            },
            instance_workspaces,
            ..PresentationState::default()
        }
    }

    fn persist(&self, cx: &mut Context<Self>) {
        let Some(store) = self.store.clone() else {
            return;
        };
        let state = self.snapshot(cx);
        cx.background_spawn(async move {
            let _ = store.save(&state);
        })
        .detach();
    }

    fn fit_side_docks_to_width(&mut self, width: f32) {
        let sizes = dock_layout::fit_side_docks(
            width,
            self.left_dock.presentation.size,
            self.right_dock.presentation.size,
            self.left_dock.presentation.open,
            self.right_dock.presentation.open,
        );
        self.left_dock.presentation.size = sizes.left;
        self.right_dock.presentation.size = sizes.right;
    }

    fn fit_docks_to_viewport(&mut self, width: f32, height: f32) {
        self.fit_side_docks_to_width(width);

        let vertical_chrome: f32 =
            (ThemeMetrics::default().toolbar_height + ThemeMetrics::default().status_height).into();
        self.bottom_dock.presentation.size = dock_layout::fit_bottom_dock(
            (height - vertical_chrome).max(0.0),
            self.bottom_dock.presentation.size,
        );
    }

    fn resize_dock(
        &mut self,
        event: &gpui::DragMoveEvent<DockResizeDrag>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let dock = event.drag(cx).dock;
        let width: f32 = event.bounds.size.width.into();
        let height: f32 = event.bounds.size.height.into();
        let pointer_x: f32 = (event.event.position.x - event.bounds.left()).into();
        let pointer_y: f32 = (event.event.position.y - event.bounds.top()).into();
        let previous_sizes = (
            self.left_dock.presentation.size,
            self.right_dock.presentation.size,
            self.bottom_dock.presentation.size,
        );

        match dock {
            DockId::Left | DockId::Inspector => {
                let requested = match dock {
                    DockId::Left => pointer_x,
                    DockId::Inspector => width - pointer_x,
                    DockId::Bottom => unreachable!(),
                };
                let sizes = dock_layout::resize_side_dock(
                    width,
                    dock_layout::SideDockSizes {
                        left: self.left_dock.presentation.size,
                        right: self.right_dock.presentation.size,
                    },
                    self.left_dock.presentation.open,
                    self.right_dock.presentation.open,
                    dock,
                    requested,
                );
                self.left_dock.presentation.size = sizes.left.round();
                self.right_dock.presentation.size = sizes.right.round();
            }
            DockId::Bottom => {
                self.bottom_dock.presentation.size =
                    dock_layout::fit_bottom_dock(height, height - pointer_y).round();
            }
        }
        let current_sizes = (
            self.left_dock.presentation.size,
            self.right_dock.presentation.size,
            self.bottom_dock.presentation.size,
        );
        if current_sizes != previous_sizes {
            self.queue_workspace_resize(window, cx);
        }
    }

    fn finish_dock_resize(&mut self, _: &DockResizeDrag, _: &mut Window, cx: &mut Context<Self>) {
        self.persist(cx);
    }

    fn resize_panes(
        &mut self,
        event: &gpui::DragMoveEvent<PaneResizeDrag>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let width: f32 = event.bounds.size.width.into();
        let pointer_x: f32 = f32::from(event.event.position.x - event.bounds.left()).round();
        let boundary = event.drag(cx).boundary;
        let previous_pair = self
            .pane_flexes
            .get(boundary..=boundary.saturating_add(1))
            .map(|pair| (pair[0], pair[1]));
        resize_pane_flexes(&mut self.pane_flexes, boundary, pointer_x, width);
        let current_pair = self
            .pane_flexes
            .get(boundary..=boundary.saturating_add(1))
            .map(|pair| (pair[0], pair[1]));
        if current_pair != previous_pair {
            self.queue_workspace_resize(window, cx);
        }
    }

    fn queue_workspace_resize(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.workspace_resize_frame_pending {
            return;
        }
        self.workspace_resize_frame_pending = true;
        cx.notify();
        cx.on_next_frame(window, |shell, _, _| {
            shell.workspace_resize_frame_pending = false;
        });
    }

    fn finish_pane_resize(&mut self, _: &PaneResizeDrag, _: &mut Window, cx: &mut Context<Self>) {
        self.persist(cx);
    }

    fn capture_window_bounds(&mut self, window_bounds: WindowBounds) {
        let maximized = matches!(window_bounds, WindowBounds::Maximized(_));
        let bounds = window_bounds.get_bounds();
        self.window_presentation.bounds = crate::presentation::Rect {
            x: bounds.origin.x.into(),
            y: bounds.origin.y.into(),
            width: bounds.size.width.into(),
            height: bounds.size.height.into(),
        };
        self.window_presentation.maximized = maximized;
    }

    fn split_pane(&mut self, _: &SplitPane, window: &mut Window, cx: &mut Context<Self>) {
        let id = self.next_id;
        self.next_id += 1;
        let source_has_items = self
            .panes
            .get(self.active_pane)
            .is_some_and(|pane| !pane.read(cx).items.is_empty());
        let pane = cx.new(|cx| {
            Pane::from_presentation(
                PanePresentation {
                    id,
                    items: source_has_items
                        .then(|| ItemPresentation {
                            id,
                            kind: ItemKind::Welcome,
                            title: "New pane".into(),
                            dirty: false,
                            source: None,
                        })
                        .into_iter()
                        .collect(),
                    active_item: 0,
                },
                self.vim_mode_default(),
                cx,
            )
        });
        cx.subscribe_in(&pane, window, Self::on_pane_event).detach();
        let new_flex = self
            .pane_flexes
            .get_mut(self.active_pane)
            .map(|source| {
                *source /= 2.0;
                *source
            })
            .unwrap_or(1.0);
        self.panes.push(pane);
        self.pane_flexes.push(new_flex);
        self.active_pane = self.panes.len() - 1;
        self.focus_active_pane(window, cx);
        self.persist(cx);
        cx.notify();
    }

    /// Move keyboard focus to the active pane's editor (or the pane itself).
    /// Called whenever the active pane changes or a modal is dismissed so focus
    /// is never orphaned — an orphaned focus silently drops all keybindings.
    fn focus_active_pane(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(pane) = self.panes.get(self.active_pane) {
            pane.read(cx).active_focus_handle(cx).focus(window, cx);
        }
    }

    #[cfg(test)]
    fn active_editor_focused(&self, window: &Window, cx: &App) -> bool {
        self.panes
            .get(self.active_pane)
            .is_some_and(|pane| pane.read(cx).active_focus_handle(cx).is_focused(window))
    }

    #[cfg(test)]
    fn active_results_focused(&self, window: &Window, cx: &App) -> bool {
        self.panes.get(self.active_pane).is_some_and(|pane| {
            let pane = pane.read(cx);
            pane.active_item()
                .and_then(|item| pane.results.get(&item.id))
                .is_some_and(|results| results.focus_handle(cx).is_focused(window))
        })
    }

    fn on_pane_event(
        &mut self,
        emitter: &Entity<Pane>,
        event: &PaneEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(index) = self.panes.iter().position(|pane| pane == emitter) else {
            return;
        };
        match event {
            PaneEvent::FocusRequested => {
                self.active_pane = index;
                // A child control may already have claimed focus while this
                // bubbled. Preserve it (notably the result grid); otherwise a
                // pane click restores the active editor after a modal.
                if !emitter.read(cx).focus_handle.contains_focused(window, cx) {
                    self.focus_active_pane(window, cx);
                }
                cx.notify();
            }
            PaneEvent::CloseRequested => {
                self.active_pane = index;
                self.close_pane_at(index, window, cx);
            }
            PaneEvent::CloseItemRequested { item_id } => {
                self.active_pane = index;
                emitter.update(cx, |pane, _| {
                    if let Some(item_index) = pane.items.iter().position(|item| item.id == *item_id)
                    {
                        pane.activate_item(item_index, false);
                    }
                });
                self.close_active_item(&CloseActiveItem, window, cx);
            }
            PaneEvent::DiscardItemRequested { item_id } => {
                self.active_pane = index;
                emitter.update(cx, |pane, _| {
                    if let Some(item_index) = pane.items.iter().position(|item| item.id == *item_id)
                    {
                        pane.activate_item(item_index, false);
                    }
                });
                self.remove_active_item(window, cx);
            }
            PaneEvent::SaveItemRequested { item_id } => {
                self.active_pane = index;
                emitter.update(cx, |pane, _| {
                    if let Some(item_index) = pane.items.iter().position(|item| item.id == *item_id)
                    {
                        pane.activate_item(item_index, false);
                    }
                });
                self.save_active_item(&SaveActiveItem, window, cx);
            }
            PaneEvent::MoveItemRequested { item_id } => {
                // The emitter is the drop target; find the pane that owns the
                // tab and transfer editor, results, and clean point with it.
                let source = self
                    .panes
                    .iter()
                    .position(|pane| pane != emitter && pane.read(cx).contains_item(*item_id));
                let Some(source) = source else {
                    return;
                };
                let transfer = self.panes[source].update(cx, |pane, _| pane.take_item(*item_id));
                let Some(transfer) = transfer else {
                    return;
                };
                emitter.update(cx, |pane, cx| pane.receive_item(transfer, cx));
                self.active_pane = index;
                self.focus_active_pane(window, cx);
                self.persist(cx);
                cx.notify();
            }
            PaneEvent::EditorStateChanged { item_id, dirty } => {
                if let Some(dirty) = dirty {
                    emitter.update(cx, |pane, _| {
                        if let Some(item) = pane.items.iter_mut().find(|item| item.id == *item_id) {
                            item.dirty = *dirty;
                        }
                        if !*dirty && pane.pending_close_item == Some(*item_id) {
                            pane.pending_close_item = None;
                        }
                    });
                }
                cx.notify();
            }
            PaneEvent::OpenCommandPaletteRequested => {
                self.active_pane = index;
                self.open_command_palette(&OpenCommandPalette, window, cx);
            }
            PaneEvent::ExecuteRequested { item_id, sql } => {
                self.execute_database_item(*item_id, sql.clone(), cx)
            }
            PaneEvent::RefreshDatabaseItemRequested { item_id } => {
                self.refresh_database_item(*item_id, cx)
            }
        }
    }

    /// Remove a pane and keep the workspace non-empty. The final pane is never
    /// destroyed; it degrades to an empty pane the user can reuse.
    fn close_pane_at(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if self.panes.len() <= 1 || index >= self.panes.len() {
            self.persist(cx);
            cx.notify();
            return;
        }
        let removed_item_ids = self.panes[index]
            .read(cx)
            .items
            .iter()
            .map(|item| item.id)
            .collect::<HashSet<_>>();
        self.panes.remove(index);
        self.sql_problems.retain(|problem| {
            problem
                .item_id
                .is_none_or(|item_id| !removed_item_ids.contains(&item_id))
        });
        self.sync_sql_problem_status();
        let removed_flex = self.pane_flexes.remove(index);
        if index > 0 {
            self.pane_flexes[index - 1] += removed_flex;
        } else if let Some(first) = self.pane_flexes.first_mut() {
            *first += removed_flex;
        }
        self.active_pane = self.active_pane.min(self.panes.len() - 1);
        self.focus_active_pane(window, cx);
        self.persist(cx);
        cx.notify();
    }

    fn close_active_pane(
        &mut self,
        _: &CloseActivePane,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_pane_at(self.active_pane, window, cx);
    }

    fn focus_next_pane(&mut self, _: &FocusNextPane, window: &mut Window, cx: &mut Context<Self>) {
        self.active_pane = (self.active_pane + 1) % self.panes.len();
        self.focus_active_pane(window, cx);
        cx.notify();
    }

    fn close_active_item(
        &mut self,
        _: &CloseActiveItem,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(pane) = self.panes.get(self.active_pane) else {
            return;
        };
        if let Some(item) = pane.read(cx).active_item() {
            if item.dirty {
                let item_id = item.id;
                pane.update(cx, |pane, _| {
                    pane.pending_close_item = Some(item_id);
                });
                cx.notify();
                return;
            }
        }
        self.remove_active_item(window, cx);
    }

    fn remove_active_item(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let removed_item_id = self.panes.get(self.active_pane).and_then(|pane| {
            pane.update(cx, |pane, _| {
                if !pane.items.is_empty() {
                    let removed = pane.items.remove(pane.active_item);
                    pane.forget_item(removed.id);
                    pane.active_item = pane.active_item.min(pane.items.len().saturating_sub(1));
                    Some(removed.id)
                } else {
                    None
                }
            })
        });
        if let Some(item_id) = removed_item_id {
            self.clear_sql_problems(item_id);
        }
        // A pane emptied by its last close collapses so panes never accumulate
        // as un-closeable ghosts. The final pane always survives.
        let emptied = self
            .panes
            .get(self.active_pane)
            .is_some_and(|pane| pane.read(cx).items.is_empty());
        if emptied && self.panes.len() > 1 {
            self.close_pane_at(self.active_pane, window, cx);
        } else {
            // The closed item's editor may have held focus; move it to the new
            // active item so keyboard input keeps routing.
            self.focus_active_pane(window, cx);
            self.persist(cx);
            cx.notify();
        }
    }

    fn save_active_item(
        &mut self,
        _: &SaveActiveItem,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let active_configuration = self.panes.get(self.active_pane).and_then(|pane| {
            let pane = pane.read(cx);
            pane.active_item()
                .filter(|item| item.kind == ItemKind::Configuration)
                .and_then(|item| pane.editor(item.id).map(|editor| (item.id, editor)))
        });
        if let Some((item_id, editor)) = active_configuration {
            if self.settings_item == Some(item_id) {
                let Some(store) = self.settings_store.clone() else {
                    self.show_toast(
                        "The settings file is unavailable in this session".into(),
                        cx,
                    );
                    return;
                };
                match store.save_text(editor.read(cx).document().text()) {
                    Ok(settings) => {
                        self.settings = settings;
                        if let Some(pane) = self.panes.get(self.active_pane) {
                            pane.update(cx, |pane, cx| pane.mark_clean(item_id, cx));
                        }
                        self.show_toast("Saved settings.toml".into(), cx);
                    }
                    Err(error) => self.show_toast(error, cx),
                }
                self.focus_active_pane(window, cx);
                cx.notify();
                return;
            }
            self.instance_configuration_editor = editor;
            self.save_instance_configuration(cx);
            return;
        }
        if let Some(pane) = self.panes.get(self.active_pane) {
            pane.update(cx, |pane, cx| {
                if let Some(item_id) = pane.active_item().map(|item| item.id) {
                    pane.mark_clean(item_id, cx);
                    pane.pending_close_item = None;
                }
            });
        }
        self.show_toast("Presentation saved".into(), cx);
        self.persist(cx);
        self.focus_active_pane(window, cx);
        cx.notify();
    }

    fn open_command_palette(
        &mut self,
        _: &OpenCommandPalette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.modal = Some(Modal::CommandPalette);
        self.palette_selected = 0;
        self.palette_scroll_handle
            .scroll_to_item(0, ScrollStrategy::Top);
        self.query_input.focus_handle(cx).focus(window, cx);
        cx.notify();
    }

    fn open_server_connection(
        &mut self,
        _: &OpenServerConnection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_app_bar_modal(Modal::ServerConnection, cx);
        self.server_connection_error = None;
        self.server_connection_pending = false;
        self.server_name_input.focus_handle(cx).focus(window, cx);
        cx.notify();
    }

    fn prompt_for_instance_root(&mut self, cx: &mut Context<Self>) {
        let prompt = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Open Existing Sift Instance".into()),
        });
        self.instance_operation_pending = true;
        self.instance_operation_error = None;
        cx.spawn(async move |shell, cx| {
            let result = prompt.await;
            let _ = shell.update(cx, |shell, cx| match result {
                Ok(Ok(Some(paths))) => {
                    if let Some(root) = paths.into_iter().next() {
                        shell.inspect_instance_root(root, cx);
                    }
                }
                Ok(Ok(None)) | Err(_) => {
                    shell.instance_operation_pending = false;
                    cx.notify();
                }
                Ok(Err(error)) => {
                    shell.instance_operation_pending = false;
                    shell.instance_operation_error =
                        Some(format!("opening folder picker: {error}"));
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn prompt_for_new_instance_root(&mut self, cx: &mut Context<Self>) {
        let prompt = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Choose an empty folder for the Sift instance".into()),
        });
        self.instance_operation_pending = true;
        self.instance_operation_error = None;
        cx.spawn(async move |shell, cx| {
            let result = prompt.await;
            let _ = shell.update(cx, |shell, cx| match result {
                Ok(Ok(Some(paths))) => {
                    if let Some(root) = paths.into_iter().next() {
                        shell.send_instance_command(
                            InstanceCommand::PrepareRootConfiguration { root },
                            cx,
                        );
                    }
                }
                Ok(Ok(None)) | Err(_) => {
                    shell.instance_operation_pending = false;
                    cx.notify();
                }
                Ok(Err(error)) => {
                    shell.instance_operation_pending = false;
                    shell.instance_operation_error =
                        Some(format!("opening folder picker: {error}"));
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn send_instance_command(&mut self, command: InstanceCommand, cx: &mut Context<Self>) {
        let Some(sender) = &self.instance_sender else {
            self.instance_operation_pending = false;
            self.instance_operation_error = Some("Instance manager is unavailable".into());
            cx.notify();
            return;
        };
        self.instance_operation_pending = true;
        self.instance_operation_error = None;
        if sender.send(command).is_err() {
            self.instance_operation_pending = false;
            self.instance_operation_error = Some("Instance manager stopped".into());
        }
        cx.notify();
    }

    fn open_root_configuration(&mut self, root: std::path::PathBuf, cx: &mut Context<Self>) {
        self.dismiss_app_bar_overlays(cx);
        self.send_instance_command(InstanceCommand::OpenRootConfiguration { root }, cx);
    }

    fn open_current_configuration(&mut self, cx: &mut Context<Self>) {
        self.dismiss_app_bar_overlays(cx);
        self.send_instance_command(InstanceCommand::OpenCurrentConfiguration, cx);
    }

    fn forget_instance_root(&mut self, root: std::path::PathBuf, cx: &mut Context<Self>) {
        self.send_instance_command(InstanceCommand::ForgetRoot { root }, cx);
    }

    fn save_instance_configuration(&mut self, cx: &mut Context<Self>) {
        let Some(configuration) = self.instance_configuration.clone() else {
            return;
        };
        let manifest = self
            .instance_configuration_editor
            .read(cx)
            .document()
            .text()
            .to_owned();
        self.send_instance_command(
            InstanceCommand::SaveConfiguration {
                root: configuration.root,
                manifest,
                expected_source_revision: configuration.source_revision,
                is_new: configuration.is_new,
            },
            cx,
        );
    }

    fn inspect_instance_root(&mut self, root: std::path::PathBuf, cx: &mut Context<Self>) {
        let Some(sender) = &self.instance_sender else {
            self.instance_operation_pending = false;
            self.instance_operation_error = Some("Instance manager is unavailable".into());
            cx.notify();
            return;
        };
        self.instance_operation_pending = true;
        self.instance_operation_error = None;
        if sender.send(InstanceCommand::InspectRoot { root }).is_err() {
            self.instance_operation_pending = false;
            self.instance_operation_error = Some("Instance manager stopped".into());
        }
        cx.notify();
    }

    fn connect_instance_root(&mut self, root: std::path::PathBuf, cx: &mut Context<Self>) {
        if self.server_connection_pending {
            return;
        }
        let Some(sender) = &self.instance_sender else {
            self.server_connection_error = Some("Desktop connection manager is unavailable".into());
            cx.notify();
            return;
        };
        if sender.send(InstanceCommand::StartRoot { root }).is_err() {
            self.server_connection_error = Some("Desktop connection manager stopped".into());
        } else {
            self.server_connection_pending = true;
            self.server_connection_error = None;
        }
        cx.notify();
    }

    fn apply_instance_root(&mut self, allow_destroy: bool, cx: &mut Context<Self>) {
        let (Some(sender), Some(plan)) = (&self.instance_sender, &self.instance_plan) else {
            return;
        };
        self.instance_operation_pending = true;
        self.instance_operation_error = None;
        if sender
            .send(InstanceCommand::ApplyRoot {
                root: plan.root.clone(),
                allow_destroy,
            })
            .is_err()
        {
            self.instance_operation_pending = false;
            self.instance_operation_error = Some("Instance manager stopped".into());
        }
        cx.notify();
    }

    fn import_instance_credential(&mut self, cx: &mut Context<Self>) {
        let Some(slot) = self.selected_instance_credential.clone() else {
            return;
        };
        let Some(plan) = &self.instance_plan else {
            return;
        };
        let Some(credential) = plan.credentials.iter().find(|item| item.slot == slot) else {
            return;
        };
        let secret = self.instance_secret_input.read(cx).text().to_owned();
        if secret.is_empty() {
            self.instance_operation_error = Some("Credential value is required".into());
            cx.notify();
            return;
        }
        let Some(sender) = &self.instance_sender else {
            return;
        };
        self.instance_operation_pending = true;
        self.instance_operation_error = None;
        if sender
            .send(InstanceCommand::ImportRootCredential {
                root: plan.root.clone(),
                slot,
                kind: credential.kind,
                secret,
            })
            .is_err()
        {
            self.instance_operation_pending = false;
            self.instance_operation_error = Some("Instance manager stopped".into());
        } else {
            self.instance_secret_input
                .update(cx, |input, cx| input.set_text("", cx));
        }
        cx.notify();
    }

    fn start_instance_root(&mut self, cx: &mut Context<Self>) {
        let (Some(sender), Some(plan)) = (&self.instance_sender, &self.instance_plan) else {
            return;
        };
        self.instance_operation_pending = true;
        self.instance_operation_error = None;
        if sender
            .send(InstanceCommand::StartRoot {
                root: plan.root.clone(),
            })
            .is_err()
        {
            self.instance_operation_pending = false;
            self.instance_operation_error = Some("Instance manager stopped".into());
        }
        cx.notify();
    }

    fn toggle_app_bar_modal(&mut self, modal: Modal, cx: &mut Context<Self>) {
        if self.modal.as_ref() == Some(&modal) {
            self.close_app_bar_modal(cx);
            cx.notify();
        } else {
            match &modal {
                Modal::ServerPicker => self.server_connection_error = None,
                Modal::Account => self.account_error = None,
                _ => {}
            }
            self.open_app_bar_modal(modal, cx);
        }
    }

    fn sign_in_with_password(&mut self, cx: &mut Context<Self>) {
        if self.account_pending {
            return;
        }
        let username = self
            .account_username_input
            .read(cx)
            .text()
            .trim()
            .to_owned();
        let password = self.account_password_input.read(cx).text().to_owned();
        if username.is_empty() || password.is_empty() {
            self.account_error = Some("Username and password are required".into());
            cx.notify();
            return;
        }
        let Some(sender) = &self.instance_sender else {
            self.account_error = Some("Desktop account manager is unavailable".into());
            cx.notify();
            return;
        };
        if sender
            .send(InstanceCommand::SignInWithPassword { username, password })
            .is_err()
        {
            self.account_error = Some("Desktop account manager stopped".into());
        } else {
            self.account_pending = true;
            self.account_error = None;
        }
        cx.notify();
    }

    fn sign_in_with_github(&mut self, cx: &mut Context<Self>) {
        if self.account_pending {
            return;
        }
        let Some(sender) = &self.instance_sender else {
            self.account_error = Some("Desktop account manager is unavailable".into());
            cx.notify();
            return;
        };
        if sender.send(InstanceCommand::SignInWithGithub).is_err() {
            self.account_error = Some("Desktop account manager stopped".into());
        } else {
            self.account_pending = true;
            self.account_error = None;
        }
        cx.notify();
    }

    fn sign_out(&mut self, everywhere: bool, cx: &mut Context<Self>) {
        if self.account_pending {
            return;
        }
        let Some(sender) = &self.instance_sender else {
            self.account_error = Some("Desktop account manager is unavailable".into());
            cx.notify();
            return;
        };
        if sender
            .send(InstanceCommand::SignOut { everywhere })
            .is_err()
        {
            self.account_error = Some("Desktop account manager stopped".into());
        } else {
            self.account_pending = true;
            self.account_error = None;
        }
        cx.notify();
    }

    fn connect_saved_server(&mut self, profile: &SavedServerProfile, cx: &mut Context<Self>) {
        if self.server_connection_pending {
            return;
        }
        let Some(sender) = &self.instance_sender else {
            self.server_connection_error = Some("Desktop connection manager is unavailable".into());
            cx.notify();
            return;
        };
        let command = InstanceCommand::Connect {
            profile_id: Some(profile.id.clone()),
            name: profile.name.clone(),
            base_url: profile.base_url.clone(),
            bearer_token: None,
            remember_token: profile.has_saved_token,
        };
        if sender.send(command).is_err() {
            self.server_connection_error = Some("Desktop connection manager stopped".into());
        } else {
            self.server_connection_pending = true;
            self.server_connection_error = None;
        }
        cx.notify();
    }

    fn select_server_profile(
        &mut self,
        profile: &SavedServerProfile,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.selected_server_profile = Some(profile.id.clone());
        self.server_name_input
            .update(cx, |input, cx| input.set_text(profile.name.clone(), cx));
        self.server_url_input
            .update(cx, |input, cx| input.set_text(profile.base_url.clone(), cx));
        self.server_token_input
            .update(cx, |input, cx| input.set_text("", cx));
        self.server_url_input.focus_handle(cx).focus(window, cx);
        self.server_connection_error = None;
        cx.notify();
    }

    fn new_server_profile(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.selected_server_profile = None;
        self.server_name_input
            .update(cx, |input, cx| input.set_text("", cx));
        self.server_url_input
            .update(cx, |input, cx| input.set_text("", cx));
        self.server_token_input
            .update(cx, |input, cx| input.set_text("", cx));
        self.server_connection_error = None;
        self.server_name_input.focus_handle(cx).focus(window, cx);
        cx.notify();
    }

    fn submit_server_connection(&mut self, cx: &mut Context<Self>) {
        if self.server_connection_pending {
            return;
        }
        let Some(sender) = &self.instance_sender else {
            self.server_connection_error = Some("Desktop connection manager is unavailable".into());
            cx.notify();
            return;
        };
        let name = self.server_name_input.read(cx).text().trim().to_owned();
        let base_url = self.server_url_input.read(cx).text().trim().to_owned();
        let token = self.server_token_input.read(cx).text().to_owned();
        if name.is_empty() || base_url.is_empty() {
            self.server_connection_error = Some("Display name and server URL are required".into());
            cx.notify();
            return;
        }
        let command = InstanceCommand::Connect {
            profile_id: self.selected_server_profile.clone(),
            name,
            base_url,
            bearer_token: (!token.is_empty()).then_some(token),
            remember_token: self.remember_server_token,
        };
        if sender.send(command).is_err() {
            self.server_connection_error = Some("Desktop connection manager stopped".into());
        } else {
            self.server_connection_pending = true;
            self.server_connection_error = None;
        }
        cx.notify();
    }

    fn use_local_server(&mut self, cx: &mut Context<Self>) {
        if self
            .lifecycle
            .selected_instance
            .as_ref()
            .is_some_and(|instance| instance.id == "local")
        {
            return;
        }
        let Some(sender) = &self.instance_sender else {
            self.server_connection_error = Some("Desktop connection manager is unavailable".into());
            cx.notify();
            return;
        };
        if sender.send(InstanceCommand::UseLocal).is_err() {
            self.server_connection_error = Some("Desktop connection manager stopped".into());
        } else {
            self.server_connection_pending = true;
            self.server_connection_error = None;
        }
        cx.notify();
    }

    fn forget_selected_server(&mut self, cx: &mut Context<Self>) {
        let Some(profile_id) = self.selected_server_profile.take() else {
            return;
        };
        if let Some(sender) = &self.instance_sender {
            let _ = sender.send(InstanceCommand::Forget { profile_id });
        }
        self.server_name_input
            .update(cx, |input, cx| input.set_text("", cx));
        self.server_url_input
            .update(cx, |input, cx| input.set_text("", cx));
        self.server_token_input
            .update(cx, |input, cx| input.set_text("", cx));
        cx.notify();
    }

    /// Commands matching the palette search text (case-insensitive substring).
    fn filtered_commands(&self, cx: &App) -> Vec<CommandSpec> {
        let query = self.query_input.read(cx).text().to_lowercase();
        self.command_specs(cx)
            .into_iter()
            .filter(|command| query.is_empty() || command.label.to_lowercase().contains(&query))
            .collect()
    }

    fn palette_up(&mut self, _: &PaletteUp, _: &mut Window, cx: &mut Context<Self>) {
        if self.modal != Some(Modal::CommandPalette) {
            return;
        }
        self.palette_selected = self.palette_selected.saturating_sub(1);
        self.palette_scroll_handle
            .scroll_to_item(self.palette_selected, ScrollStrategy::Nearest);
        cx.notify();
    }

    fn palette_down(&mut self, _: &PaletteDown, _: &mut Window, cx: &mut Context<Self>) {
        if self.modal != Some(Modal::CommandPalette) {
            return;
        }
        let last = self.filtered_commands(cx).len().saturating_sub(1);
        self.palette_selected = (self.palette_selected + 1).min(last);
        self.palette_scroll_handle
            .scroll_to_item(self.palette_selected, ScrollStrategy::Nearest);
        cx.notify();
    }

    fn palette_confirm(&mut self, _: &PaletteConfirm, window: &mut Window, cx: &mut Context<Self>) {
        if self.modal != Some(Modal::CommandPalette) {
            return;
        }
        let commands = self.filtered_commands(cx);
        if let Some(command) = commands.get(self.palette_selected) {
            if command.enabled() {
                self.run_command(command.id, window, cx);
            }
        }
    }

    fn dismiss_modal(&mut self, _: &DismissModal, window: &mut Window, cx: &mut Context<Self>) {
        if self.modal == Some(Modal::ServerConnection) {
            self.server_token_input
                .update(cx, |input, cx| input.set_text("", cx));
        }
        if self.modal == Some(Modal::DatabaseConnection) {
            self.database_password_input
                .update(cx, |input, cx| input.set_text("", cx));
        }
        self.modal = None;
        // Return focus to the workspace so keybindings keep routing.
        self.focus_active_pane(window, cx);
        cx.notify();
    }

    /// Run a command palette entry by its stable id: dismiss the palette, then
    /// dispatch the matching workspace action. Ids come from `command_specs`.
    fn run_command(&mut self, id: CommandId, window: &mut Window, cx: &mut Context<Self>) {
        self.dismiss_modal(&DismissModal, window, cx);
        match id {
            CommandId::ConnectServer => {
                self.open_server_connection(&OpenServerConnection, window, cx)
            }
            CommandId::ExecuteStatement => {
                self.dispatch_active_editor_action(&crate::editor::ExecuteStatement, window, cx)
            }
            CommandId::ExecuteDocument => {
                self.dispatch_active_editor_action(&crate::editor::ExecuteDocument, window, cx)
            }
            CommandId::UndoQuery => {
                self.dispatch_active_editor_action(&crate::editor::Undo, window, cx)
            }
            CommandId::RedoQuery => {
                self.dispatch_active_editor_action(&crate::editor::Redo, window, cx)
            }
            CommandId::SplitPane => self.split_pane(&SplitPane, window, cx),
            CommandId::FocusNextPane => self.focus_next_pane(&FocusNextPane, window, cx),
            CommandId::ClosePane => self.close_active_pane(&CloseActivePane, window, cx),
            CommandId::SaveItem => self.save_active_item(&SaveActiveItem, window, cx),
            CommandId::CloseItem => self.close_active_item(&CloseActiveItem, window, cx),
            CommandId::ToggleLeftDock => self.toggle_left_dock(&ToggleLeftDock, window, cx),
            CommandId::ToggleInspectorDock => self.toggle_right_dock(&ToggleRightDock, window, cx),
            CommandId::ToggleBottomDock => self.toggle_bottom_dock(&ToggleBottomDock, window, cx),
            CommandId::OpenSettings => {
                self.modal = Some(Modal::Settings);
                cx.notify();
            }
            CommandId::ToggleTheme => self.toggle_theme(cx),
            CommandId::OpenCommandPalette => {
                self.open_command_palette(&OpenCommandPalette, window, cx)
            }
            CommandId::Quit => window.remove_window(),
        }
    }

    fn dispatch_active_editor_action(
        &self,
        action: &dyn gpui::Action,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(pane) = self.panes.get(self.active_pane) {
            pane.read(cx)
                .active_focus_handle(cx)
                .dispatch_action(action, window, cx);
        }
    }

    fn toggle_left_dock(
        &mut self,
        _: &ToggleLeftDock,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.left_dock.presentation.open = !self.left_dock.presentation.open;
        self.fit_side_docks_to_width(window.window_bounds().get_bounds().size.width.into());
        self.persist(cx);
        cx.notify();
    }

    fn toggle_right_dock(
        &mut self,
        _: &ToggleRightDock,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.right_dock.presentation.open = !self.right_dock.presentation.open;
        self.fit_side_docks_to_width(window.window_bounds().get_bounds().size.width.into());
        self.persist(cx);
        cx.notify();
    }

    fn toggle_bottom_dock(
        &mut self,
        _: &ToggleBottomDock,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.bottom_dock.presentation.open = !self.bottom_dock.presentation.open;
        let viewport = window.window_bounds().get_bounds().size;
        let vertical_chrome: f32 =
            (cx.theme().metrics.toolbar_height + cx.theme().metrics.status_height).into();
        self.bottom_dock.presentation.size = dock_layout::fit_bottom_dock(
            f32::from(viewport.height) - vertical_chrome,
            self.bottom_dock.presentation.size,
        );
        self.persist(cx);
        cx.notify();
    }

    fn toggle_app_bar_menu(&mut self, menu: AppBarMenu, cx: &mut Context<Self>) {
        self.close_app_bar_modal(cx);
        self.app_bar_menu = (self.app_bar_menu != Some(menu)).then_some(menu);
        cx.notify();
    }

    fn hover_app_bar_menu(&mut self, menu: AppBarMenu, hovered: bool, cx: &mut Context<Self>) {
        if !hovered || !self.app_bar_expanded || self.app_bar_menu == Some(menu) {
            return;
        }
        self.close_app_bar_modal(cx);
        self.app_bar_menu = Some(menu);
        cx.notify();
    }

    fn toggle_app_bar_navigation(&mut self, cx: &mut Context<Self>) {
        self.close_app_bar_modal(cx);
        self.app_bar_expanded = !self.app_bar_expanded;
        self.app_bar_menu = self.app_bar_expanded.then_some(AppBarMenu::Main);
        cx.notify();
    }

    fn app_bar_modal_is_open(&self) -> bool {
        matches!(
            self.modal,
            Some(Modal::ServerPicker | Modal::ServerConnection | Modal::Account)
        )
    }

    fn close_app_bar_modal(&mut self, cx: &mut Context<Self>) {
        if !self.app_bar_modal_is_open() {
            return;
        }
        if self.modal == Some(Modal::ServerConnection) {
            self.server_token_input
                .update(cx, |input, cx| input.set_text("", cx));
        }
        if self.modal == Some(Modal::Account) {
            self.account_password_input
                .update(cx, |input, cx| input.set_text("", cx));
        }
        self.modal = None;
    }

    fn open_app_bar_modal(&mut self, modal: Modal, cx: &mut Context<Self>) {
        debug_assert!(matches!(
            &modal,
            Modal::ServerPicker | Modal::ServerConnection | Modal::Account
        ));
        self.close_app_bar_modal(cx);
        self.app_bar_menu = None;
        self.modal = Some(modal);
        cx.notify();
    }

    fn app_bar_navigation_expanded(&self) -> bool {
        self.app_bar_expanded
    }

    fn collapse_app_bar_navigation(&mut self, cx: &mut Context<Self>) {
        let changed = self.app_bar_expanded || self.app_bar_menu.is_some();
        self.app_bar_expanded = false;
        self.app_bar_menu = None;
        if changed {
            cx.notify();
        }
    }

    fn dismiss_app_bar_overlays(&mut self, cx: &mut Context<Self>) {
        let changed =
            self.app_bar_expanded || self.app_bar_menu.is_some() || self.app_bar_modal_is_open();
        self.close_app_bar_modal(cx);
        self.app_bar_expanded = false;
        self.app_bar_menu = None;
        if changed {
            cx.notify();
        }
    }

    fn activate_app_bar_item(
        &mut self,
        command: CommandId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.app_bar_menu = None;
        self.app_bar_expanded = false;
        self.run_command(command, window, cx);
    }

    fn render_app_bar_dropdown(
        &self,
        menu: AppBarMenu,
        align_right: bool,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let theme = cx.theme();
        let colors = theme.colors;
        let row_height = theme.metrics.row_height;
        let rows = app_bar::menu_items(menu)
            .into_iter()
            .enumerate()
            .map(|(index, item)| {
                let disabled_reason = item.command.map_or(Some("Not implemented"), |command| {
                    CommandRegistry::spec(command, self.command_context(cx)).disabled_reason
                });
                let available = disabled_reason.is_none();
                let row = div()
                    .id(("app-bar-menu-item", index))
                    .role(Role::MenuItem)
                    .aria_label(if available {
                        item.label.to_string()
                    } else {
                        format!("{} (not implemented)", item.label)
                    })
                    .h(row_height)
                    .px_2()
                    .flex()
                    .items_center()
                    .gap_3()
                    .rounded_sm()
                    .text_sm()
                    .text_color(if available {
                        colors.text
                    } else {
                        colors.disabled_text
                    })
                    .when(available, |row| {
                        row.hover(|row| row.bg(colors.hovered_surface))
                    })
                    .child(div().flex_1().min_w_0().truncate().child(item.label))
                    .when(!item.shortcut.is_empty(), |row| {
                        row.child(KeyBinding::new(item.shortcut))
                    })
                    .when_some(disabled_reason, |row, reason| {
                        row.child(
                            div()
                                .flex_none()
                                .px_1()
                                .rounded_sm()
                                .bg(colors.hovered_surface)
                                .text_xs()
                                .child(reason),
                        )
                    });
                match item.command {
                    Some(command) if available => row
                        .on_click(cx.listener(move |shell, _, window, cx| {
                            shell.activate_app_bar_item(command, window, cx)
                        }))
                        .into_any_element(),
                    Some(_) | None => row.into_any_element(),
                }
            })
            .collect::<Vec<_>>();

        div()
            .id("app-bar-dropdown")
            .absolute()
            .top(theme.metrics.toolbar_height - px(4.))
            .when(align_right, |menu| menu.right_0())
            .when(!align_right, |menu| menu.left_0())
            .w(px(280.))
            .p_1()
            .flex()
            .flex_col()
            .rounded(theme.metrics.radius_large)
            .border_1()
            .border_color(colors.strong_border)
            .bg(colors.elevated_surface)
            .shadow_lg()
            .occlude()
            .children(rows)
            .into_any_element()
    }

    fn render_app_bar_menu_button(
        &self,
        menu: AppBarMenu,
        label: &'static str,
        align_right: bool,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let colors = cx.theme().colors;
        let open = self.app_bar_menu == Some(menu);
        div()
            .relative()
            .flex_none()
            .child(
                div()
                    .id(("app-bar-menu-button", menu as usize))
                    .role(Role::Button)
                    .aria_label(format!("Open {label} menu"))
                    .h(px(26.))
                    .px_1()
                    .flex()
                    .items_center()
                    .rounded_sm()
                    .text_sm()
                    .text_color(if open { colors.text } else { colors.muted_text })
                    .when(open, |button| button.bg(colors.active_surface))
                    .hover(|button| button.bg(colors.hovered_surface).text_color(colors.text))
                    .on_hover(cx.listener(move |shell, hovered: &bool, _, cx| {
                        shell.hover_app_bar_menu(menu, *hovered, cx)
                    }))
                    .on_click(
                        cx.listener(move |shell, _, _, cx| shell.toggle_app_bar_menu(menu, cx)),
                    )
                    .child(label),
            )
            .when(open, |button| {
                button.child(deferred(self.render_app_bar_dropdown(
                    menu,
                    align_right,
                    cx,
                )))
            })
            .into_any_element()
    }

    /// Global application context: commands, Sift instance, workspace, updates,
    /// and identity. Database profiles deliberately stay in the workspace dock.
    fn render_toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = cx.theme().colors;
        let workspace_label = self.workspace_context_label();
        let server_name = self.active_server_name();
        let status_label = self.lifecycle.status_label();
        let show_status = !matches!(self.lifecycle.phase, crate::ConnectionPhase::Ready);
        let account_label = self
            .lifecycle
            .identity
            .as_ref()
            .map(|identity| identity.principal.display_name.clone())
            .unwrap_or_else(|| "Sign in".into());
        let main_menu_open = self.app_bar_menu == Some(AppBarMenu::Main);
        let server_picker_active = matches!(
            self.modal,
            Some(Modal::ServerPicker | Modal::ServerConnection)
        );
        let account_active = self.modal == Some(Modal::Account);
        let navigation_expanded = self.app_bar_navigation_expanded();
        let launcher_content = if navigation_expanded {
            div()
                .px_1()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .child("Sift")
                .into_any_element()
        } else {
            icon(IconName::Menu, colors.muted_text, 15.)
        };
        let file_menu = navigation_expanded
            .then(|| self.render_app_bar_menu_button(AppBarMenu::File, "File", false, cx));
        let edit_menu = navigation_expanded
            .then(|| self.render_app_bar_menu_button(AppBarMenu::Edit, "Edit", false, cx));
        let selection_menu = navigation_expanded.then(|| {
            self.render_app_bar_menu_button(AppBarMenu::Selection, "Selection", false, cx)
        });
        let view_menu = navigation_expanded
            .then(|| self.render_app_bar_menu_button(AppBarMenu::View, "View", false, cx));
        let go_menu = navigation_expanded
            .then(|| self.render_app_bar_menu_button(AppBarMenu::Go, "Go", false, cx));
        let run_menu = navigation_expanded
            .then(|| self.render_app_bar_menu_button(AppBarMenu::Run, "Run", false, cx));
        let window_menu = navigation_expanded
            .then(|| self.render_app_bar_menu_button(AppBarMenu::Window, "Window", false, cx));
        let help_menu = navigation_expanded
            .then(|| self.render_app_bar_menu_button(AppBarMenu::Help, "Help", false, cx));

        div()
            .id("integrated-titlebar")
            .debug_selector(|| "integrated-titlebar".into())
            .key_context("SiftWindow")
            .h(cx.theme().metrics.toolbar_height)
            .relative()
            .flex()
            .items_center()
            .justify_between()
            .pl_2()
            .pr_2()
            .gap_2()
            .bg(colors.toolbar)
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(
                div()
                    .absolute()
                    .inset_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        div()
                            .id("toolbar-title-drag-region")
                            .h_full()
                            .max_w(px(260.))
                            .min_w_0()
                            .px_3()
                            .flex()
                            .items_center()
                            .window_control_area(WindowControlArea::Drag)
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|shell, _, window, cx| {
                                    cx.stop_propagation();
                                    shell.dismiss_app_bar_overlays(cx);
                                    window.start_window_move();
                                }),
                            )
                            .truncate()
                            .text_center()
                            .text_sm()
                            .text_color(colors.muted_text)
                            .child(workspace_label),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_1()
                    .items_center()
                    .gap_1()
                    .min_w_0()
                    .child(
                        div()
                            .relative()
                            .flex_none()
                            .child(
                                div()
                                    .id("toolbar-menu")
                                    .role(Role::Button)
                                    .aria_label(if navigation_expanded {
                                        "Open Sift application menu"
                                    } else {
                                        "Expand Sift application menu"
                                    })
                                    .min_w(px(28.))
                                    .h(px(28.))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .rounded_sm()
                                    .text_sm()
                                    .text_color(if main_menu_open {
                                        colors.text
                                    } else {
                                        colors.muted_text
                                    })
                                    .when(main_menu_open, |slot| slot.bg(colors.active_surface))
                                    .hover(|slot| {
                                        slot.bg(colors.hovered_surface).text_color(colors.text)
                                    })
                                    .on_hover(cx.listener(|shell, hovered: &bool, _, cx| {
                                        shell.hover_app_bar_menu(AppBarMenu::Main, *hovered, cx)
                                    }))
                                    .on_click(cx.listener(|shell, _, _, cx| {
                                        shell.toggle_app_bar_navigation(cx)
                                    }))
                                    .child(launcher_content),
                            )
                            .when(main_menu_open, |button| {
                                button.child(deferred(self.render_app_bar_dropdown(
                                    AppBarMenu::Main,
                                    false,
                                    cx,
                                )))
                            }),
                    )
                    .children((!navigation_expanded).then(|| {
                        div()
                            .id("toolbar-server-picker")
                            .debug_selector(|| "toolbar-server-picker".into())
                            .role(Role::Button)
                            .aria_label(format!(
                                "Current Sift server: {server_name}, {status_label}"
                            ))
                            .h(px(28.))
                            .max_w(px(260.))
                            .px_2()
                            .flex()
                            .items_center()
                            .gap_2()
                            .rounded_sm()
                            .border_1()
                            .border_color(colors.subtle_border)
                            .bg(colors.surface)
                            .when(server_picker_active, |slot| {
                                slot.bg(colors.active_surface).border_color(colors.accent)
                            })
                            .hover(|slot| {
                                slot.bg(colors.hovered_surface).border_color(colors.border)
                            })
                            .on_click(cx.listener(|shell, _, _, cx| {
                                shell.toggle_app_bar_modal(Modal::ServerPicker, cx)
                            }))
                            .min_w_0()
                            .text_sm()
                            .child(div().min_w_0().truncate().child(server_name))
                            .when(show_status, |picker| {
                                picker.child(
                                    div()
                                        .text_xs()
                                        .text_color(colors.muted_text)
                                        .child(status_label),
                                )
                            })
                            .child(icon(IconName::ChevronDown, colors.muted_text, 12.))
                    }))
                    .children(file_menu)
                    .children(edit_menu)
                    .children(selection_menu)
                    .children(view_menu)
                    .children(go_menu)
                    .children(run_menu)
                    .children(window_menu)
                    .children(help_menu)
                    .child(
                        div()
                            .id("toolbar-empty-drag-region")
                            .h_full()
                            .flex_1()
                            .window_control_area(WindowControlArea::Drag)
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|shell, _, window, cx| {
                                    cx.stop_propagation();
                                    shell.dismiss_app_bar_overlays(cx);
                                    window.start_window_move();
                                }),
                            ),
                    ),
            )
            .child(
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_end()
                    .gap_1()
                    .child(
                        div()
                            .id("toolbar-account")
                            .debug_selector(|| "toolbar-account".into())
                            .role(Role::Button)
                            .aria_label(format!("Account: {account_label}"))
                            .size(px(26.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded_sm()
                            .when(account_active, |button| button.bg(colors.active_surface))
                            .hover(|button| button.bg(colors.hovered_surface))
                            .on_click(cx.listener(|shell, _, _, cx| {
                                shell.toggle_app_bar_modal(Modal::Account, cx)
                            }))
                            .child(self.render_account_icon(18., cx)),
                    )
                    .child(
                        div()
                            .relative()
                            .flex_none()
                            .child(
                                div()
                                    .id("toolbar-profile-menu")
                                    .role(Role::Button)
                                    .aria_label("Open settings and profile menu")
                                    .size(px(26.))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .rounded_sm()
                                    .when(
                                        self.app_bar_menu == Some(AppBarMenu::Profile),
                                        |button| button.bg(colors.active_surface),
                                    )
                                    .hover(|button| button.bg(colors.hovered_surface))
                                    .on_click(cx.listener(|shell, _, _, cx| {
                                        shell.toggle_app_bar_menu(AppBarMenu::Profile, cx)
                                    }))
                                    .child(icon(IconName::ChevronDown, colors.muted_text, 12.)),
                            )
                            .when(self.app_bar_menu == Some(AppBarMenu::Profile), |button| {
                                button.child(deferred(self.render_app_bar_dropdown(
                                    AppBarMenu::Profile,
                                    true,
                                    cx,
                                )))
                            }),
                    )
                    .child(
                        div()
                            .ml_1()
                            .pl_1()
                            .flex()
                            .items_center()
                            .border_l_1()
                            .border_color(colors.subtle_border)
                            .child(
                                IconButton::new(
                                    "window-minimize",
                                    IconName::Minimize,
                                    "Minimize window",
                                )
                                .square(px(26.))
                                .icon_size(16.)
                                .on_click(|_, window, _| window.minimize_window()),
                            )
                            .child(
                                IconButton::new(
                                    "window-size-toggle",
                                    IconName::Maximize,
                                    "Maximize or restore window",
                                )
                                .square(px(26.))
                                .icon_size(16.)
                                .on_click(|_, window, _| window.zoom_window()),
                            )
                            .child(
                                IconButton::new("window-close", IconName::Close, "Close window")
                                    .square(px(26.))
                                    .icon_size(16.)
                                    .on_click(|_, window, _| window.remove_window()),
                            ),
                    ),
            )
    }

    fn connection_schema_rows(
        &self,
        connection: &ConnectionNavEntry,
        colors: sift_ui::ThemeColors,
        cx: &mut Context<Self>,
    ) -> Vec<gpui::AnyElement> {
        let profile_id = connection.id;
        match &self.connection_schema {
            ConnectionSchemaState::Loading {
                profile_id: loading,
            } if *loading == profile_id => vec![div()
                .ml_6()
                .h(cx.theme().metrics.row_height)
                .flex()
                .items_center()
                .text_xs()
                .text_color(colors.muted_text)
                .child("Loading schema…")
                .into_any_element()],
            ConnectionSchemaState::Failed {
                profile_id: failed,
                message,
            } if *failed == profile_id => vec![div()
                .ml_6()
                .mr_2()
                .py_1()
                .flex()
                .flex_col()
                .gap_1()
                .text_xs()
                .text_color(colors.danger)
                .child("Schema unavailable")
                .child(
                    div()
                        .truncate()
                        .text_color(colors.muted_text)
                        .child(message.clone()),
                )
                .into_any_element()],
            ConnectionSchemaState::Ready {
                profile_id: ready,
                snapshot,
            } if *ready == profile_id => {
                let mut rows = Vec::new();
                for (catalog_index, catalog) in snapshot.trees.iter().enumerate() {
                    let catalog_key = (profile_id, catalog.name.clone());
                    let catalog_open = self.expanded_catalogs.contains(&catalog_key);
                    let catalog_name = catalog.name.clone();
                    rows.push(
                        div()
                            .id(("schema-catalog", catalog_index + profile_id as usize * 1000))
                            .mx_2()
                            .h(cx.theme().metrics.row_height)
                            .pl_4()
                            .pr_2()
                            .flex()
                            .items_center()
                            .gap_1()
                            .rounded_sm()
                            .text_color(colors.muted_text)
                            .hover(|row| row.bg(colors.hovered_surface).text_color(colors.text))
                            .on_click(cx.listener(move |shell, _, _, cx| {
                                shell.toggle_catalog_schema(profile_id, catalog_name.clone(), cx)
                            }))
                            .child(icon(
                                if catalog_open {
                                    IconName::ChevronDown
                                } else {
                                    IconName::ChevronRight
                                },
                                colors.muted_text,
                                11.,
                            ))
                            .child(icon(IconName::Database, colors.muted_text, 12.))
                            .child(div().min_w_0().truncate().child(catalog.name.clone()))
                            .into_any_element(),
                    );
                    if !catalog_open {
                        continue;
                    }
                    for (schema_index, schema) in catalog.schemas.iter().enumerate() {
                        let schema_key = (profile_id, catalog.name.clone(), schema.name.clone());
                        let schema_open = self.expanded_schemas.contains(&schema_key);
                        let catalog_name = catalog.name.clone();
                        let schema_name = schema.name.clone();
                        rows.push(
                            div()
                                .id((
                                    "schema-namespace",
                                    schema_index
                                        + catalog_index * 100
                                        + profile_id as usize * 100_000,
                                ))
                                .mx_2()
                                .h(cx.theme().metrics.row_height)
                                .pl_6()
                                .pr_2()
                                .flex()
                                .items_center()
                                .gap_1()
                                .rounded_sm()
                                .text_color(colors.muted_text)
                                .hover(|row| row.bg(colors.hovered_surface).text_color(colors.text))
                                .on_click(cx.listener(move |shell, _, _, cx| {
                                    shell.toggle_database_schema(
                                        profile_id,
                                        catalog_name.clone(),
                                        schema_name.clone(),
                                        cx,
                                    )
                                }))
                                .child(icon(
                                    if schema_open {
                                        IconName::ChevronDown
                                    } else {
                                        IconName::ChevronRight
                                    },
                                    colors.muted_text,
                                    11.,
                                ))
                                .child(div().min_w_0().truncate().child(schema.name.clone()))
                                .child(
                                    div()
                                        .ml_auto()
                                        .text_xs()
                                        .text_color(colors.disabled_text)
                                        .child(schema.objects.len().to_string()),
                                )
                                .into_any_element(),
                        );
                        if !schema_open {
                            continue;
                        }
                        for (object_index, object) in schema.objects.iter().enumerate() {
                            let can_preview = matches!(
                                object.kind,
                                sift_protocol::ObjectKind::Table
                                    | sift_protocol::ObjectKind::View
                                    | sift_protocol::ObjectKind::MaterializedView
                                    | sift_protocol::ObjectKind::ForeignTable
                                    | sift_protocol::ObjectKind::PartitionedTable
                            );
                            let connection_for_preview = connection.clone();
                            let catalog_name = catalog.name.clone();
                            let schema_name = schema.name.clone();
                            let object_name = object.name.clone();
                            let object_kind = object.kind;
                            let row = div()
                                .id((
                                    "schema-object",
                                    object_index
                                        + schema_index * 1000
                                        + catalog_index * 100_000
                                        + profile_id as usize * 10_000_000,
                                ))
                                .mx_2()
                                .h(cx.theme().metrics.row_height)
                                .pl_8()
                                .pr_2()
                                .flex()
                                .items_center()
                                .gap_2()
                                .rounded_sm()
                                .text_color(if can_preview {
                                    colors.text
                                } else {
                                    colors.muted_text
                                })
                                .when(can_preview, |row| {
                                    row.hover(|row| row.bg(colors.hovered_surface)).on_click(
                                        cx.listener(move |shell, _, window, cx| {
                                            shell.open_table_preview(
                                                DatabaseObjectTarget {
                                                    connection: connection_for_preview.clone(),
                                                    catalog: catalog_name.clone(),
                                                    schema: schema_name.clone(),
                                                    object: object_name.clone(),
                                                    object_kind,
                                                },
                                                window,
                                                cx,
                                            )
                                        }),
                                    )
                                })
                                .child(
                                    div()
                                        .min_w_0()
                                        .flex_1()
                                        .truncate()
                                        .child(object.name.clone()),
                                )
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(colors.disabled_text)
                                        .child(schema_object_kind_label(object.kind)),
                                );
                            rows.push(row.into_any_element());
                        }
                    }
                }
                if rows.is_empty() {
                    rows.push(
                        div()
                            .ml_6()
                            .h(cx.theme().metrics.row_height)
                            .flex()
                            .items_center()
                            .text_xs()
                            .text_color(colors.muted_text)
                            .child("No schema objects found")
                            .into_any_element(),
                    );
                }
                rows
            }
            _ => Vec::new(),
        }
    }

    fn render_dock(&self, dock: &Dock, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let colors = theme.colors;
        let definition = dock.definition();
        let title = match dock.id {
            DockId::Left => self.active_left_panel.label(),
            DockId::Inspector | DockId::Bottom => definition.title,
        };
        let debug_selector = match dock.id {
            DockId::Left => "left-dock",
            DockId::Inspector => "right-dock",
            DockId::Bottom => "bottom-dock",
        };
        div()
            .id(title)
            .debug_selector(move || debug_selector.to_owned())
            .key_context("SiftDock")
            .relative()
            .w(px(dock.presentation.size))
            .min_h_0()
            .flex()
            .flex_col()
            .overflow_hidden()
            .py_2()
            .border_color(colors.subtle_border)
            .border_t_1()
            .bg(colors.panel)
            .text_sm()
            .child(div().pl_3().child(SectionLabel::new(title.to_uppercase())))
            .when(
                dock.id == DockId::Left && self.active_left_panel == LeftPanel::Connections,
                |dock_view| {
                dock_view.child(
                    div()
                        .mx_2()
                        .h(cx.theme().metrics.row_height)
                        .flex()
                        .items_center()
                        .justify_between()
                        .child(
                            Button::new("add-database-connection", "Add connection…")
                                .tone(ButtonTone::Ghost)
                                .start_icon(IconName::Add)
                                .on_click(cx.listener(|shell, _, window, cx| {
                                    shell.open_database_connection(window, cx)
                                })),
                        )
                        .when(
                            matches!(self.connection_status, ConnectionStatus::Connected { .. }),
                            |toolbar| {
                                toolbar.child(
                                    Button::new("refresh-connection-schema", "Refresh")
                                        .tone(ButtonTone::Ghost)
                                        .on_click(cx.listener(|shell, _, _, cx| {
                                            shell.refresh_connection_schema(cx)
                                        })),
                                )
                            },
                        ),
                )
                },
            )
            .when(
                dock.id == DockId::Left && self.active_left_panel == LeftPanel::Connections,
                |dock_view| {
                let selected = self.selected_workspace_id;
                let mut rows: Vec<gpui::AnyElement> = Vec::new();
                for tenant in &self.lifecycle.tenants {
                    let tenant_id = tenant.id.0;
                    let tenant_open = self.expanded_tenants.contains(&tenant_id);
                    rows.push(
                        div()
                            .id(("connection-tenant", tenant_id as usize))
                            .mt_2()
                            .h(px(24.))
                            .px_3()
                            .flex()
                            .items_center()
                            .gap_1()
                            .rounded_sm()
                            .text_xs()
                            .text_color(colors.muted_text)
                            .hover(|row| row.bg(colors.hovered_surface).text_color(colors.text))
                            .on_click(cx.listener(move |shell, _, _, cx| {
                                shell.toggle_tenant(tenant_id, cx)
                            }))
                            .child(icon(
                                if tenant_open {
                                    IconName::ChevronDown
                                } else {
                                    IconName::ChevronRight
                                },
                                colors.muted_text,
                                11.,
                            ))
                            .child(div().min_w_0().truncate().child(tenant.name.clone()))
                            .into_any_element(),
                    );
                    if !tenant_open {
                        continue;
                    }
                    if !tenant.connections.is_empty() {
                        rows.push(
                            div()
                                .h(px(20.))
                                .px_4()
                                .flex()
                                .items_end()
                                .text_xs()
                                .text_color(colors.disabled_text)
                                .child("DATABASES")
                                .into_any_element(),
                        );
                    }
                    for conn in &tenant.connections {
                        let connection_id = conn.id;
                        let entry_for_delete = conn.clone();
                        let (connection_color, connected) = match &self.connection_status {
                            ConnectionStatus::Connected { profile_id, .. }
                                if *profile_id == conn.id =>
                            {
                                (colors.success, true)
                            }
                            ConnectionStatus::Connecting { profile_id }
                                if *profile_id == conn.id =>
                            {
                                (colors.warning, false)
                            }
                            ConnectionStatus::Failed { profile_id, .. }
                                if *profile_id == conn.id =>
                            {
                                (colors.danger, false)
                            }
                            _ => (colors.muted_text, false),
                        };
                        let connection_open = self.expanded_connections.contains(&connection_id);
                        let leading = if connected {
                            div()
                                .id(("toggle-connection", connection_id as usize))
                                .role(Role::Button)
                                .flex_none()
                                .size(px(16.))
                                .flex()
                                .items_center()
                                .justify_center()
                                .on_mouse_down(MouseButton::Left, |_, _, cx| {
                                    cx.stop_propagation()
                                })
                                .on_click(cx.listener(move |shell, _, _, cx| {
                                    cx.stop_propagation();
                                    shell.toggle_connection(connection_id, cx)
                                }))
                                .child(icon(
                                    if connection_open {
                                        IconName::ChevronDown
                                    } else {
                                        IconName::ChevronRight
                                    },
                                    colors.muted_text,
                                    11.,
                                ))
                                .into_any_element()
                        } else {
                            div()
                                .flex_none()
                                .size(px(16.))
                                .flex()
                                .items_center()
                                .justify_center()
                                .child(icon(IconName::Database, connection_color, 13.))
                                .into_any_element()
                        };
                        let mut row = div()
                            .id(("conn", conn.id as usize))
                            .flex()
                            .items_center()
                            .gap_2()
                            .mx_2()
                            .h(cx.theme().metrics.row_height)
                            .px_2()
                            .rounded_sm()
                            .when(connected, |row| row.bg(colors.active_surface))
                            .hover(|row| row.bg(colors.hovered_surface))
                            .child(leading)
                            .child(div().flex_1().min_w_0().truncate().child(conn.name.clone()));
                        if connected {
                            row = row.child(
                                Button::new(("disconnect", conn.id as usize), "Disconnect")
                                    .tone(ButtonTone::DangerGhost)
                                    .on_click(
                                        cx.listener(|shell, _, _, cx| shell.disconnect(cx)),
                                    ),
                            );
                        } else {
                            let entry = conn.clone();
                            row = row.on_click(
                                cx.listener(move |shell, _, _, cx| shell.connect(&entry, cx)),
                            );
                        }
                        row = row.child(
                            div()
                                .id(("delete-connection", conn.id as usize))
                                .flex_none()
                                .role(Role::Button)
                                .aria_label(format!("Delete connection {}", conn.name))
                                .p_1()
                                .rounded_sm()
                                .text_color(colors.muted_text)
                                .hover(|button| {
                                    button
                                        .bg(colors.danger_muted)
                                        .text_color(colors.danger)
                                })
                                .on_mouse_down(MouseButton::Left, |_, _, cx| {
                                    cx.stop_propagation()
                                })
                                .on_click(cx.listener(move |shell, _, _, cx| {
                                    cx.stop_propagation();
                                    shell.request_delete_connection(&entry_for_delete, cx)
                                }))
                                .child(icon(IconName::Close, colors.danger, 11.)),
                        );
                        rows.push(row.into_any_element());
                        if connected && connection_open {
                            rows.extend(self.connection_schema_rows(conn, colors, cx));
                        }
                    }
                    if tenant
                        .rooms
                        .iter()
                        .any(|room| !room.workspaces.is_empty())
                    {
                        rows.push(
                            div()
                                .h(px(20.))
                                .px_4()
                                .flex()
                                .items_end()
                                .text_xs()
                                .text_color(colors.disabled_text)
                                .child("WORKSPACES")
                                .into_any_element(),
                        );
                    }
                    for room in &tenant.rooms {
                        let room_id = room.id.0;
                        let room_open = self.expanded_rooms.contains(&room_id);
                        rows.push(
                            div()
                                .id(("connection-room", room_id as usize))
                                .mx_2()
                                .h(cx.theme().metrics.row_height)
                                .pl_4()
                                .pr_2()
                                .flex()
                                .items_center()
                                .gap_1()
                                .rounded_sm()
                                .text_color(colors.muted_text)
                                .hover(|row| {
                                    row.bg(colors.hovered_surface).text_color(colors.text)
                                })
                                .on_click(cx.listener(move |shell, _, _, cx| {
                                    shell.toggle_room(room_id, cx)
                                }))
                                .child(icon(
                                    if room_open {
                                        IconName::ChevronDown
                                    } else {
                                        IconName::ChevronRight
                                    },
                                    colors.muted_text,
                                    11.,
                                ))
                                .child(div().min_w_0().truncate().child(room.name.clone()))
                                .into_any_element(),
                        );
                        if !room_open {
                            continue;
                        }
                        for workspace in &room.workspaces {
                            let features =
                                match (workspace.git_enabled, workspace.scheduling_enabled) {
                                    (true, true) => " · Git · Runs",
                                    (true, false) => " · Git",
                                    (false, true) => " · Runs",
                                    (false, false) => "",
                                };
                            let is_open = selected == Some(workspace.id);
                            let entry = workspace.clone();
                            rows.push(
                                div()
                                    .id(("workspace", workspace.id as usize))
                                    .mx_2()
                                    .h(cx.theme().metrics.row_height)
                                    .pl_8()
                                    .pr_2()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .rounded_sm()
                                    .when(is_open, |row| {
                                        row.bg(colors.active_surface).text_color(colors.text)
                                    })
                                    .hover(|row| row.bg(colors.hovered_surface))
                                    .on_click(cx.listener(move |shell, _, _, cx| {
                                        shell.open_workspace(&entry, cx)
                                    }))
                                    .child(
                                        div()
                                            .min_w_0()
                                            .truncate()
                                            .child(format!("{}{features}", workspace.name)),
                                    )
                                    .into_any_element(),
                            );
                        }
                    }
                }
                dock_view.child(
                    div()
                        .id("connections-scroll")
                        .flex_1()
                        .min_h_0()
                        .overflow_y_scroll()
                        .children(rows),
                )
                },
            )
            .when(
                dock.id == DockId::Left
                    && self.active_left_panel == LeftPanel::Connections
                    && self.lifecycle.tenants.is_empty(),
                |dock_view| {
                    dock_view.child(
                        div()
                            .px_3()
                            .py_2()
                            .text_color(colors.muted_text)
                            .child(self.lifecycle.status_label()),
                    )
                },
            )
            .when(
                dock.id == DockId::Left && self.active_left_panel == LeftPanel::Git,
                |dock_view| {
                    let content = self.selected_workspace().map_or_else(
                        || "Select a Git-enabled workspace to inspect changes.".into(),
                        |workspace| {
                            if workspace.git_enabled {
                                format!(
                                    "Git projection enabled for {}. Change-list rendering arrives with desktop VCS integration.",
                                    workspace.name
                                )
                            } else {
                                format!("Git is disabled for {}.", workspace.name)
                            }
                        },
                    );
                    dock_view.child(
                        div()
                            .p_3()
                            .whitespace_normal()
                            .text_color(colors.muted_text)
                            .child(content),
                    )
                },
            )
            .when(
                dock.id == DockId::Left
                    && self.active_left_panel == LeftPanel::Collaboration,
                |dock_view| {
                    let rows = self.presence.participants.iter().map(|participant| {
                        let followed = self.presence.followed_attachment
                            == Some(participant.attachment_id);
                        div()
                            .id(("collaborator", participant.attachment_id as usize))
                            .mx_2()
                            .h(cx.theme().metrics.row_height)
                            .px_2()
                            .flex()
                            .items_center()
                            .gap_2()
                            .rounded_sm()
                            .when(followed, |row| row.bg(colors.active_surface))
                            .child(div().size(px(7.)).rounded_full().bg(colors.success))
                            .child(format!("Participant {}", participant.principal_id))
                    });
                    dock_view
                        .child(
                            div()
                                .px_3()
                                .pb_2()
                                .text_color(colors.muted_text)
                                .child(format!(
                                    "{} participant(s) in this room",
                                    self.presence.participants.len()
                                )),
                        )
                        .child(
                            div()
                                .id("collaboration-scroll")
                                .flex_1()
                                .min_h_0()
                                .overflow_y_scroll()
                                .children(rows),
                        )
                },
            )
            .when(
                dock.id == DockId::Left && self.active_left_panel == LeftPanel::QueryOutline,
                |dock_view| {
                    let active = self
                        .panes
                        .get(self.active_pane)
                        .and_then(|pane| pane.read(cx).active_item())
                        .cloned();
                    dock_view.child(
                        div()
                            .p_3()
                            .flex()
                            .flex_col()
                            .gap_2()
                            .whitespace_normal()
                            .children(active.map(|item| {
                                div()
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .child(item.title)
                            }))
                            .child(
                                div()
                                    .text_color(colors.muted_text)
                                    .child("Statements, CTEs, parameters, and referenced objects appear here when desktop semantic diagnostics land."),
                            ),
                    )
                },
            )
            .when(dock.id == DockId::Inspector, |dock_view| {
                dock_view.child(
                    div()
                        .p_3()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .min_w_0()
                        .text_sm()
                        .child(format!("{} participants", self.presence.participants.len()))
                        .child(
                            div()
                                .min_w_0()
                                .whitespace_normal()
                                .text_color(colors.muted_text)
                                .child(match self.presence.followed_attachment {
                                    Some(attachment) => {
                                        format!("Following attachment {attachment}")
                                    }
                                    None => "Follow mode off".into(),
                                }),
                        ),
                )
            })
    }

    fn render_modal(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let colors = cx.theme().colors;
        self.modal.as_ref().map(|modal| {
            let server_picker = matches!(modal, Modal::ServerPicker);
            let account = matches!(modal, Modal::Account);
            let settings = matches!(modal, Modal::Settings);
            let app_bar_modal = matches!(
                modal,
                Modal::ServerPicker | Modal::ServerConnection | Modal::Account
            );
            let database_connection = matches!(modal, Modal::DatabaseConnection);
            let command_palette = matches!(modal, Modal::CommandPalette);
            let instance_setup = matches!(modal, Modal::InstanceSetup);
            let card_width = if settings {
                720.0
            } else if server_picker || account {
                360.0
            } else if instance_setup {
                720.0
            } else if database_connection {
                match self.database_wizard_step {
                    DatabaseWizardStep::Provider => 760.0,
                    DatabaseWizardStep::Details => 900.0,
                    DatabaseWizardStep::Review => 720.0,
                }
            } else {
                520.0
            };
            let content = match modal {
                Modal::CommandPalette => {
                    let commands = self.filtered_commands(cx);
                    let command_count = commands.len();
                    let palette_height =
                        command_count.min(PALETTE_VISIBLE_ROWS) as f32 * PALETTE_ROW_HEIGHT;
                    div()
                        .flex()
                        .flex_col()
                        .child(
                            div()
                                .h(px(38.))
                                .px_2()
                                .flex()
                                .items_center()
                                .gap_2()
                                .border_b_1()
                                .border_color(colors.subtle_border)
                                .bg(colors.toolbar)
                                .child(icon(IconName::Search, colors.muted_text, 15.))
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .overflow_hidden()
                                        .child(self.query_input.clone()),
                                ),
                        )
                        .when(commands.is_empty(), |palette| {
                            palette.child(
                                div()
                                    .px_2()
                                    .py_4()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .text_color(colors.muted_text)
                                    .child("No matching commands"),
                            )
                        })
                        .when(command_count > 0, |palette| {
                            palette.child(
                                uniform_list(
                                    "command-list",
                                    command_count,
                                    cx.processor(move |shell, range: Range<usize>, _, cx| {
                                        let query =
                                            shell.query_input.read(cx).text().to_lowercase();
                                        let commands = shell.filtered_commands(cx);
                                        let selected_idx = shell
                                            .palette_selected
                                            .min(commands.len().saturating_sub(1));
                                        range
                                            .filter_map(|idx| {
                                                commands
                                                    .get(idx)
                                                    .cloned()
                                                    .map(|command| (idx, command))
                                            })
                                            .map(|(idx, command)| {
                                                let enabled = command.enabled();
                                                let id = command.id;
                                                let selected = idx == selected_idx;
                                                let right = command
                                                    .disabled_reason
                                                    .unwrap_or(command.shortcut);
                                                let mut row = div()
                                                    .id(id.as_str())
                                                    .flex()
                                                    .items_center()
                                                    .justify_between()
                                                    .gap_2()
                                                    .h(px(PALETTE_ROW_HEIGHT))
                                                    .px_2()
                                                    .rounded_sm()
                                                    .when(selected && enabled, |row| {
                                                        row.bg(colors.active_surface)
                                                    })
                                                    .when(!enabled, |row| {
                                                        row.text_color(colors.muted_text)
                                                    })
                                                    .child(highlight_match(
                                                        command.label,
                                                        &query,
                                                        colors.accent,
                                                    ))
                                                    .when_some(
                                                        (!right.is_empty()).then_some(right),
                                                        |row, right| {
                                                            if enabled {
                                                                row.child(KeyBinding::new(right))
                                                            } else {
                                                                row.child(
                                                                    div()
                                                                        .flex_none()
                                                                        .max_w(px(220.))
                                                                        .truncate()
                                                                        .text_xs()
                                                                        .text_color(
                                                                            colors.muted_text,
                                                                        )
                                                                        .child(right),
                                                                )
                                                            }
                                                        },
                                                    );
                                                if enabled {
                                                    row = row
                                                        .hover(|row| {
                                                            row.bg(colors.hovered_surface)
                                                        })
                                                        .on_click(cx.listener(
                                                            move |shell, _, window, cx| {
                                                                shell.run_command(id, window, cx)
                                                            },
                                                        ));
                                                }
                                                row
                                            })
                                            .collect()
                                    }),
                                )
                                .h(px(palette_height))
                                .track_scroll(&self.palette_scroll_handle),
                            )
                        })
                        .into_any_element()
                }
                Modal::ServerPicker => {
                    let current_id = self
                        .lifecycle
                        .selected_instance
                        .as_ref()
                        .map(|instance| instance.id.clone());
                    let pending = self.server_connection_pending;
                    let local_active = current_id.as_deref() == Some("local");
                    let status_label = self.lifecycle.status_label();
                    // One row shape for every switchable target: leading icon
                    // chip, title + subtitle, and a current-state slot.
                    let picker_row = |row: gpui::Stateful<gpui::Div>,
                                      name: SharedString,
                                      subtitle: SharedString,
                                      current: bool| {
                        row.flex()
                            .items_center()
                            .gap_2()
                            .px_2()
                            .py_1()
                            .min_h(px(38.))
                            .rounded_sm()
                            .child(
                                div()
                                    .flex_none()
                                    .size(px(22.))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .rounded_sm()
                                    .bg(colors.active_surface)
                                    .child(icon(
                                        if current {
                                            IconName::Check
                                        } else {
                                            IconName::Server
                                        },
                                        if current {
                                            colors.accent
                                        } else {
                                            colors.muted_text
                                        },
                                        12.,
                                    )),
                            )
                            .child(
                                div()
                                    .flex()
                                    .flex_1()
                                    .flex_col()
                                    .min_w_0()
                                    .child(
                                        div()
                                            .min_w_0()
                                            .truncate()
                                            .text_sm()
                                            .child(name),
                                    )
                                    .child(
                                        div()
                                            .min_w_0()
                                            .truncate()
                                            .text_xs()
                                            .text_color(colors.muted_text)
                                            .child(subtitle),
                                    ),
                            )
                    };
                    let mut rows = Vec::new();
                    rows.push(
                        picker_row(
                            div()
                                .id("picker-local-sift")
                                .role(Role::Button)
                                .aria_label("Bundled Local Sift")
                                .when(local_active, |row| row.bg(colors.active_surface))
                                .when(!pending && !local_active, |row| {
                                    row.hover(|row| row.bg(colors.hovered_surface)).on_click(
                                        cx.listener(|shell, _, _, cx| {
                                            shell.use_local_server(cx)
                                        }),
                                    )
                                }),
                            "Bundled Local Sift".into(),
                            "Built into this app · no TOML".into(),
                            local_active,
                        )
                        .child(
                            Badge::new(if local_active { "Current" } else { "Built-in" })
                                .tone(if local_active {
                                    Tone::Success
                                } else {
                                    Tone::Neutral
                                }),
                        )
                        .into_any_element(),
                    );
                    for (index, profile) in self.saved_servers.iter().cloned().enumerate() {
                        let active = current_id.as_deref()
                            == Some(format!("hosted:{}", profile.id).as_str());
                        let profile_for_click = profile.clone();
                        rows.push(
                            picker_row(
                                div()
                                    .id(("picker-saved-server", index))
                                    .role(Role::Button)
                                    .aria_label(profile.name.clone())
                                    .when(active, |row| row.bg(colors.active_surface))
                                    .when(!pending && !active, |row| {
                                        row.hover(|row| row.bg(colors.hovered_surface)).on_click(
                                            cx.listener(move |shell, _, _, cx| {
                                                shell.connect_saved_server(
                                                    &profile_for_click,
                                                    cx,
                                                )
                                            }),
                                        )
                                    }),
                                profile.name.clone().into(),
                                profile.base_url.clone().into(),
                                active,
                            )
                            .child(if profile.has_saved_token {
                                Badge::new("Token saved").tone(Tone::Neutral)
                            } else {
                                Badge::new("Saved").tone(Tone::Neutral)
                            })
                            .into_any_element(),
                        );
                    }
                    for (index, instance) in self.instance_roots.iter().cloned().enumerate() {
                        let root = instance.root.clone();
                        let root_for_manage = instance.root.clone();
                        let root_for_remove = instance.root.clone();
                        let active = current_id.as_deref()
                            == Some(format!("config:{}", instance.manifest_id).as_str());
                        rows.push(
                            picker_row(
                                div()
                                    .id(("picker-instance-root", index))
                                    .role(Role::Button)
                                    .aria_label(instance.name.clone())
                                    .when(active, |row| row.bg(colors.active_surface))
                                    .when(!pending, |row| {
                                        row.hover(|row| row.bg(colors.hovered_surface)).on_click(
                                            cx.listener(move |shell, _, _, cx| {
                                                shell.connect_instance_root(root.clone(), cx)
                                            }),
                                        )
                                    }),
                                instance.name.clone().into(),
                                instance.root.display().to_string().into(),
                                active,
                            )
                            .child(
                                Badge::new(if active { "Current" } else { "sift.toml" })
                                    .tone(if active {
                                        Tone::Success
                                    } else {
                                        Tone::Neutral
                                    }),
                            )
                            .child(
                                div()
                                    .id(("manage-instance-root", index))
                                    .role(Role::Button)
                                    .aria_label(format!("Manage {}", instance.name))
                                    .flex_none()
                                    .p_1()
                                    .rounded_sm()
                                    .text_color(colors.muted_text)
                                    .hover(|button| {
                                        button.bg(colors.hovered_surface).text_color(colors.text)
                                    })
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        |_, _, cx| cx.stop_propagation(),
                                    )
                                    .on_click(cx.listener(move |shell, _, _, cx| {
                                        cx.stop_propagation();
                                        shell.inspect_instance_root(root_for_manage.clone(), cx)
                                    }))
                                    .child(icon(IconName::Fallback, colors.muted_text, 12.)),
                            )
                            .child(
                                div()
                                    .id(("forget-instance-root", index))
                                    .role(Role::Button)
                                    .aria_label(format!(
                                        "Remove {} from Sift; keep files",
                                        instance.name
                                    ))
                                    .flex_none()
                                    .p_1()
                                    .rounded_sm()
                                    .text_color(colors.muted_text)
                                    .hover(|button| {
                                        button
                                            .bg(colors.danger_muted)
                                            .text_color(colors.danger)
                                    })
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        |_, _, cx| cx.stop_propagation(),
                                    )
                                    .on_click(cx.listener(move |shell, _, _, cx| {
                                        cx.stop_propagation();
                                        shell.forget_instance_root(
                                            root_for_remove.clone(),
                                            cx,
                                        )
                                    }))
                                    .child(icon(IconName::Close, colors.danger, 12.)),
                            )
                            .into_any_element(),
                        );
                    }
                    div()
                        .id("server-picker-menu")
                        .flex()
                        .flex_col()
                        .min_w_0()
                        .p_1()
                        .gap_0p5()
                        .child(
                            div()
                                .px_2()
                                .py_1()
                                .flex()
                                .items_center()
                                .gap_2()
                                .child(icon(IconName::Server, colors.muted_text, 14.))
                                .child(
                                    div()
                                        .flex()
                                        .flex_1()
                                        .flex_col()
                                        .min_w_0()
                                        .child(
                                            div()
                                                .truncate()
                                                .text_sm()
                                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                                .child("Sift Server"),
                                        )
                                        .child(
                                            div()
                                                .truncate()
                                                .text_xs()
                                                .text_color(colors.muted_text)
                                                .child(format!(
                                                    "{} · {}",
                                                    self.active_server_name(),
                                                    status_label
                                                )),
                                        ),
                                ),
                        )
                        .child(div().flex().flex_col().gap_0p5().py_1().children(rows))
                        .children(self.server_connection_error.as_ref().map(|message| {
                            ErrorBanner::new(message.clone())
                        }))
                        .when(pending, |picker| {
                            picker.child(
                                div()
                                    .px_2()
                                    .py_1()
                                    .text_xs()
                                    .text_color(colors.muted_text)
                                    .child("Testing connection…"),
                            )
                        })
                        .child(
                            div()
                                .mt_1()
                                .pt_1()
                                .border_t_1()
                                .border_color(colors.subtle_border)
                                .flex()
                                .flex_col()
                                .gap_0p5()
                                .children(
                                    current_id
                                        .as_deref()
                                        .is_some_and(|id| id.starts_with("config:"))
                                        .then(|| {
                                            Button::new(
                                                "picker-edit-current-instance",
                                                "Edit current sift.toml…",
                                            )
                                            .tone(ButtonTone::Ghost)
                                            .start_icon(IconName::Fallback)
                                            .on_click(cx.listener(|shell, _, _, cx| {
                                                shell.open_current_configuration(cx)
                                            }))
                                        }),
                                )
                                .child(
                                    Button::new("picker-new-instance", "Create Sift Instance…")
                                        .tone(ButtonTone::Ghost)
                                        .start_icon(IconName::Add)
                                        .on_click(cx.listener(|shell, _, _, cx| {
                                            shell.prompt_for_new_instance_root(cx)
                                        })),
                                )
                                .child(
                                    Button::new(
                                        "picker-import-instance",
                                        "Open Existing Sift Instance…",
                                    )
                                    .tone(ButtonTone::Ghost)
                                    .start_icon(IconName::Workspace)
                                    .on_click(cx.listener(|shell, _, _, cx| {
                                        shell.prompt_for_instance_root(cx)
                                    })),
                                )
                                .child(
                                    Button::new(
                                        "picker-manage-servers",
                                        "Connect to or manage servers…",
                                    )
                                    .tone(ButtonTone::Ghost)
                                    .start_icon(IconName::Server)
                                    .on_click(cx.listener(|shell, _, window, cx| {
                                        shell.open_server_connection(
                                            &OpenServerConnection,
                                            window,
                                            cx,
                                        )
                                    })),
                                ),
                        )
                        .into_any_element()
                }
                Modal::InstanceSetup => {
                    let Some(plan) = self.instance_plan.clone() else {
                        unreachable!("instance setup opens only after a plan is loaded")
                    };
                    let pending = self.instance_operation_pending;
                    let selected_slot = self.selected_instance_credential.clone();
                    let missing_credentials = plan
                        .credentials
                        .iter()
                        .filter(|credential| credential.readiness != "ready")
                        .count();
                    let ready_to_start = plan.current_generation.is_some()
                        && !plan.drifted
                        && missing_credentials == 0;
                    let allow_destroy = plan.destroy_confirmation_required;
                    let manifest_path = plan.root.join("sift.toml");
                    let edit_manifest_root = plan.root.clone();
                    let lock_path = plan.root.join("sift.lock");
                    let refresh_root = plan.root.clone();
                    let configuration_digest = plan.configuration_digest.chars().take(12).collect::<String>();
                    let lock_digest = plan.lock_digest.chars().take(12).collect::<String>();
                    let credential_rows = plan
                        .credentials
                        .iter()
                        .cloned()
                        .enumerate()
                        .map(|(credential_index, credential)| {
                        let slot = credential.slot.clone();
                        let selected = selected_slot.as_deref() == Some(slot.as_str());
                        div()
                            .id(("instance-credential", credential_index))
                            .role(Role::Button)
                            .px_2()
                            .py_1()
                            .rounded_sm()
                            .when(selected, |row| row.bg(colors.active_surface))
                            .hover(|row| row.bg(colors.hovered_surface))
                            .on_click(cx.listener(move |shell, _, _, cx| {
                                shell.selected_instance_credential = Some(slot.clone());
                                cx.notify();
                            }))
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .justify_between()
                                    .gap_3()
                                    .child(
                                        div()
                                            .flex()
                                            .flex_col()
                                            .min_w_0()
                                            .child(credential.kind.label())
                                            .child(
                                                div()
                                                    .text_xs()
                                                    .text_color(colors.muted_text)
                                                    .truncate()
                                                    .child(format!(
                                                        "{} · {}",
                                                        credential.slot, credential.consumer
                                                    )),
                                            ),
                                    )
                                    .child({
                                        let readiness = credential.readiness.clone();
                                        Badge::new(readiness).tone(
                                            if credential.readiness == "ready" {
                                                Tone::Success
                                            } else {
                                                Tone::Warning
                                            },
                                        )
                                    }),
                            )
                    });

                    div()
                        .id("instance-setup-scroll")
                        .flex()
                        .flex_col()
                        .min_w_0()
                        .max_h(px(680.))
                        .overflow_y_scroll()
                        .gap_3()
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .justify_between()
                                .gap_3()
                                .child(
                                    div()
                                        .flex()
                                        .flex_col()
                                        .min_w_0()
                                        .child(
                                            div()
                                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                                .child(plan.name.clone()),
                                        )
                                        .child(
                                            div()
                                                .text_xs()
                                                .text_color(colors.muted_text)
                                                .truncate()
                                                .child(plan.root.display().to_string()),
                                        )
                                        .child(
                                            div()
                                                .text_xs()
                                                .text_color(colors.muted_text)
                                                .child(format!(
                                                    "Config {configuration_digest} · Lock {lock_digest}"
                                                )),
                                        ),
                                )
                                .child(
                                    Badge::new(if plan.drifted {
                                        "Unapplied drift"
                                    } else {
                                        "Applied"
                                    })
                                    .tone(if plan.drifted {
                                        Tone::Warning
                                    } else {
                                        Tone::Success
                                    }),
                                ),
                        )
                        .child(
                            div()
                                .rounded_sm()
                                .border_1()
                                .border_color(colors.subtle_border)
                                .p_2()
                                .grid()
                                .grid_cols(2)
                                .gap_2()
                                .text_sm()
                                .child(format!("Deployment: {}", plan.deployment))
                                .child(format!("Bind: {}", plan.bind))
                                .child(format!(
                                    "Generation: {}",
                                    plan.current_generation.map_or_else(
                                        || "not applied".into(),
                                        |generation| generation.to_string()
                                    )
                                ))
                                .child(format!("Generations: {}", plan.generation_count))
                                .child(format!("Principals: {}", plan.principals))
                                .child(format!("Tenants: {}", plan.tenants))
                                .child(format!("Memberships: {}", plan.memberships))
                                .child(format!("Connections: {}", plan.connections))
                                .child(format!("Extensions: {}", plan.extensions)),
                        )
                        .child(
                            div()
                                .flex()
                                .gap_2()
                                .child(
                                    Button::new(
                                        "edit-instance-manifest",
                                        "Edit sift.toml",
                                    )
                                    .tone(ButtonTone::Accent)
                                    .on_click(cx.listener(move |shell, _, _, cx| {
                                        shell.open_root_configuration(
                                            edit_manifest_root.clone(),
                                            cx,
                                        )
                                    })),
                                )
                                .child(
                                    Button::new(
                                        "open-instance-manifest",
                                        "Open externally",
                                    )
                                    .tone(ButtonTone::Neutral)
                                    .on_click(move |_, _, cx| {
                                        cx.open_with_system(&manifest_path)
                                    }),
                                )
                                .child(
                                    Button::new(
                                        "open-instance-lock",
                                        "Open sift.lock",
                                    )
                                    .tone(ButtonTone::Neutral)
                                    .on_click(move |_, _, cx| {
                                        cx.open_with_system(&lock_path)
                                    }),
                                )
                                .child(
                                    Button::new(
                                        "refresh-instance-plan",
                                        "Refresh plan",
                                    )
                                    .tone(ButtonTone::Neutral)
                                    .on_click(cx.listener(move |shell, _, _, cx| {
                                        shell.inspect_instance_root(refresh_root.clone(), cx)
                                    })),
                                ),
                        )
                        .children((!plan.warnings.is_empty()).then(|| {
                            div()
                                .p_2()
                                .rounded_sm()
                                .bg(colors.warning_muted)
                                .text_color(colors.warning)
                                .children(plan.warnings.iter().cloned())
                        }))
                        .children(plan.last_apply.clone().map(|summary| {
                            div()
                                .p_2()
                                .rounded_sm()
                                .bg(colors.active_surface)
                                .text_sm()
                                .child(summary)
                        }))
                        .when(!plan.credentials.is_empty(), |view| {
                            view.child(
                                div()
                                    .text_xs()
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .child("CREDENTIAL SLOTS"),
                            )
                            .child(div().flex().flex_col().gap_1().children(credential_rows))
                        })
                        .children(selected_slot.map(|_| {
                            div()
                                .flex()
                                .gap_2()
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .rounded_sm()
                                        .border_1()
                                        .border_color(colors.subtle_border)
                                        .child(self.instance_secret_input.clone()),
                                )
                                .child(
                                    Button::new("import-instance-credential", "Import")
                                        .tone(ButtonTone::Accent)
                                        .loading(pending)
                                        .on_click(cx.listener(|shell, _, _, cx| {
                                            shell.import_instance_credential(cx)
                                        })),
                                )
                        }))
                        .children(self.instance_operation_error.as_ref().map(|error| {
                            ErrorBanner::new(error.clone())
                        }))
                        .when(pending, |view| {
                            view.child(
                                div()
                                    .text_xs()
                                    .text_color(colors.muted_text)
                                    .child("Working…"),
                            )
                        })
                        .child(
                            div()
                                .flex()
                                .justify_end()
                                .gap_2()
                                .child(
                                    Button::new(
                                        "apply-instance-plan",
                                        if plan.destroy_confirmation_required {
                                            "Apply destructive changes"
                                        } else {
                                            "Apply"
                                        },
                                    )
                                    .tone(if plan.destroy_confirmation_required {
                                        ButtonTone::Danger
                                    } else {
                                        ButtonTone::Accent
                                    })
                                    .loading(pending)
                                    .on_click(cx.listener(move |shell, _, _, cx| {
                                        shell.apply_instance_root(allow_destroy, cx)
                                    })),
                                )
                                .child(
                                    Button::new("start-instance-root", "Start & Connect")
                                        .tone(ButtonTone::Success)
                                        .disabled(!ready_to_start)
                                        .loading(pending)
                                        .on_click(cx.listener(|shell, _, _, cx| {
                                            shell.start_instance_root(cx)
                                        })),
                                ),
                        )
                        .into_any_element()
                }
                Modal::ServerConnection => {
                    let profiles = self.saved_servers.clone();
                    let selected = self.selected_server_profile.clone();
                    let pending = self.server_connection_pending;
                    let remember = self.remember_server_token;
                    let mut saved_rows = Vec::new();
                    for (profile_index, profile) in profiles.into_iter().enumerate() {
                        let active = selected.as_deref() == Some(profile.id.as_str());
                        let profile_for_click = profile.clone();
                        saved_rows.push(
                            div()
                                .id(("saved-server", profile_index))
                                .flex()
                                .items_center()
                                .justify_between()
                                .px_2()
                                .py_1()
                                .rounded_sm()
                                .when(active, |row| row.bg(colors.active_surface))
                                .hover(|row| row.bg(colors.hovered_surface))
                                .on_click(cx.listener(move |shell, _, window, cx| {
                                    shell.select_server_profile(&profile_for_click, window, cx)
                                }))
                                .child(
                                    div()
                                        .flex()
                                        .flex_1()
                                        .flex_col()
                                        .min_w_0()
                                        .child(
                                            div()
                                                .min_w_0()
                                                .truncate()
                                                .child(profile.name.clone()),
                                        )
                                        .child(
                                            div()
                                                .text_xs()
                                                .text_color(colors.muted_text)
                                                .truncate()
                                                .child(profile.base_url.clone()),
                                        ),
                                )
                                .when(profile.has_saved_token, |row| {
                                    row.child(Badge::new("Token saved").tone(Tone::Success))
                                })
                                .into_any_element(),
                        );
                    }
                    div()
                        .id("server-connection-scroll")
                        .flex()
                        .flex_col()
                        .min_w_0()
                        .max_h(px(620.))
                        .overflow_y_scroll()
                        .gap_3()
                        .child(
                            div()
                                .flex()
                                .min_w_0()
                                .items_center()
                                .justify_between()
                                .gap_2()
                                .child(
                                    div()
                                        .min_w_0()
                                        .truncate()
                                        .font_weight(gpui::FontWeight::SEMIBOLD)
                                        .child("Connect to Sift Server"),
                                )
                                .child(
                                    Button::new("new-server-profile", "New Server")
                                        .tone(ButtonTone::Ghost)
                                        .start_icon(IconName::Add)
                                        .on_click(cx.listener(|shell, _, window, cx| {
                                            shell.new_server_profile(window, cx)
                                        })),
                                ),
                        )
                        .child(
                            div()
                                .id("use-local-sift")
                                .role(Role::Button)
                                .h(cx.theme().metrics.row_height)
                                .flex()
                                .items_center()
                                .justify_between()
                                .px_2()
                                .rounded_sm()
                                .hover(|row| row.bg(colors.hovered_surface))
                                .on_click(cx.listener(|shell, _, _, cx| shell.use_local_server(cx)))
                                .child(
                                    div()
                                        .flex()
                                        .flex_1()
                                        .min_w_0()
                                        .items_center()
                                        .gap_2()
                                        .child(
                                            div().min_w_0().truncate().child("Local Sift"),
                                        ),
                                )
                                .child(
                                    div()
                                        .flex_none()
                                        .text_xs()
                                        .text_color(colors.muted_text)
                                        .child("Bundled server"),
                                ),
                        )
                        .when(!saved_rows.is_empty(), |form| {
                            form.child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap_1()
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(colors.muted_text)
                                            .child("SAVED SERVERS"),
                                    )
                                    .children(saved_rows),
                            )
                        })
                        .child(
                            Field::new(
                                "NAME",
                                Some(self.server_name_input.focus_handle(cx)),
                                self.server_name_input.clone(),
                            ),
                        )
                        .child(
                            Field::new(
                                "SERVER URL",
                                Some(self.server_url_input.focus_handle(cx)),
                                self.server_url_input.clone(),
                            ),
                        )
                        .child(
                            Field::new(
                                "BEARER TOKEN",
                                Some(self.server_token_input.focus_handle(cx)),
                                self.server_token_input.clone(),
                            ),
                        )
                        .child(
                            div()
                                .id("remember-server-token")
                                .role(Role::CheckBox)
                                .aria_label(if remember {
                                    "Remember token in the OS keychain, checked"
                                } else {
                                    "Remember token in the OS keychain, unchecked"
                                })
                                .flex()
                                .min_w_0()
                                .items_center()
                                .gap_2()
                                .cursor_pointer()
                                .on_click(cx.listener(|shell, _, _, cx| {
                                    shell.remember_server_token = !shell.remember_server_token;
                                    cx.notify();
                                }))
                                .child(
                                    div()
                                        .flex_none()
                                        .size(px(16.))
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .rounded_sm()
                                        .border_1()
                                        .border_color(if remember {
                                            colors.accent
                                        } else {
                                            colors.strong_border
                                        })
                                        .when(remember, |box_view| {
                                            box_view
                                                .bg(colors.accent)
                                                .child(icon(
                                                    IconName::Check,
                                                    colors.on_accent,
                                                    11.,
                                                ))
                                        }),
                                )
                                .child(
                                    div()
                                        .min_w_0()
                                        .whitespace_normal()
                                        .child("Remember token in the OS keychain"),
                                ),
                        )
                        .children(self.server_connection_error.as_ref().map(|message| {
                            ErrorBanner::new(message.clone())
                        }))
                        .child(
                            div()
                                .flex()
                                .min_w_0()
                                .justify_between()
                                .items_center()
                                .gap_2()
                                .children(selected.is_some().then(|| {
                                    Button::new("forget-server", "Forget")
                                        .tone(ButtonTone::DangerGhost)
                                        .on_click(cx.listener(|shell, _, _, cx| {
                                            shell.forget_selected_server(cx)
                                        }))
                                }))
                                .child(
                                    Button::new(
                                        "connect-server",
                                        if pending {
                                            "Testing connection…"
                                        } else {
                                            "Test & Connect"
                                        },
                                    )
                                    .tone(ButtonTone::Accent)
                                    .wide(true)
                                    .loading(pending)
                                    .on_click(cx.listener(|shell, _, _, cx| {
                                        shell.submit_server_connection(cx)
                                    })),
                                ),
                        )
                        .into_any_element()
                }
                Modal::Settings => {
                    let vim_mode_default = self.vim_mode_default();
                    let dark_theme = self.dark_theme;
                    let toggle_row = |id: &'static str,
                                      title: &'static str,
                                      description: &'static str,
                                      on: bool,
                                      on_click: sift_ui::ClickHandler| {
                        div()
                            .id(id)
                            .debug_selector(move || id.to_owned())
                            .role(Role::Button)
                            .aria_label(format!("{title}: {}", if on { "on" } else { "off" }))
                            .min_h(px(54.))
                            .px_2()
                            .flex()
                            .items_center()
                            .gap_3()
                            .rounded_sm()
                            .hover(|row| row.bg(colors.hovered_surface))
                            .on_click(on_click)
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .flex()
                                    .flex_col()
                                    .gap_1()
                                    .child(title)
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(colors.muted_text)
                                            .child(description),
                                    ),
                            )
                            .child(
                                div()
                                    .w(px(32.))
                                    .h(px(18.))
                                    .p(px(2.))
                                    .flex_none()
                                    .flex()
                                    .items_center()
                                    .rounded_full()
                                    .bg(if on {
                                        colors.accent
                                    } else {
                                        colors.strong_border
                                    })
                                    .when(on, |toggle| toggle.justify_end())
                                    .child(
                                        div()
                                            .size(px(14.))
                                            .rounded_full()
                                            .bg(colors.text),
                                    ),
                            )
                    };
                    div()
                        .flex()
                        .flex_col()
                        .gap_3()
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap_1()
                                .child(
                                    div()
                                        .text_lg()
                                        .font_weight(gpui::FontWeight::SEMIBOLD)
                                        .child("Settings"),
                                )
                                .child(
                                    div()
                                        .text_sm()
                                        .text_color(colors.muted_text)
                                        .child("Preferences are stored on this device."),
                                ),
                        )
                        .child(toggle_row(
                            "settings-vim-default",
                            "Vim mode by default",
                            "New SQL and TOML editors start in Vim normal mode.",
                            vim_mode_default,
                            Box::new(cx.listener(
                                |shell: &mut WorkspaceShell, _, _, cx| {
                                    shell.toggle_vim_mode_default(cx)
                                },
                            )) as sift_ui::ClickHandler,
                        ))
                        .child(toggle_row(
                            "settings-theme",
                            "Dark theme",
                            "Switch between the dark and light appearance.",
                            dark_theme,
                            Box::new(cx.listener(
                                |shell: &mut WorkspaceShell, _, _, cx| shell.toggle_theme(cx),
                            )) as sift_ui::ClickHandler,
                        ))
                        .child(
                            div()
                                .pt_3()
                                .border_t_1()
                                .border_color(colors.subtle_border)
                                .flex()
                                .items_center()
                                .justify_between()
                                .gap_3()
                                .child(
                                    div()
                                        .min_w_0()
                                        .flex()
                                        .flex_col()
                                        .gap_1()
                                        .child("Advanced settings")
                                        .child(
                                            div()
                                                .truncate()
                                                .text_xs()
                                                .text_color(colors.muted_text)
                                                .child(
                                                    self.settings_store
                                                        .as_ref()
                                                        .map(|store| {
                                                            store.path().display().to_string()
                                                        })
                                                        .unwrap_or_else(|| {
                                                            "settings.toml is unavailable".into()
                                                        }),
                                                ),
                                        ),
                                )
                                .child(
                                    Button::new("open-settings-file", "Open settings.toml")
                                        .tone(ButtonTone::Neutral)
                                        .debug_selector("open-settings-file")
                                        .on_click(cx.listener(|shell, _, window, cx| {
                                            shell.open_user_settings(window, cx)
                                        })),
                                ),
                        )
                        .into_any_element()
                }
                Modal::Account => {
                    let server_name = self.active_server_name();
                    let identity = self.lifecycle.identity.as_ref();
                    let is_local = self
                        .lifecycle
                        .selected_instance
                        .as_ref()
                        .is_some_and(|instance| instance.kind == crate::InstanceKind::Local);
                    let interactive = identity
                        .is_some_and(|identity| identity.auth_session_id.is_some());
                    let pending = self.account_pending;
                    let username = identity.map(|identity| {
                        identity
                            .github_login
                            .as_ref()
                            .map(|login| format!("@{login}"))
                            .unwrap_or_else(|| identity.principal.display_name.clone())
                    });
                    let github_link = identity
                        .and_then(|identity| identity.github_login.as_ref())
                        .map(|login| {
                            let github_url = format!("https://github.com/{login}");
                            div()
                                .id("account-github-profile")
                                .debug_selector(|| "account-github-profile".into())
                                .role(Role::Link)
                                .aria_label(format!("Open @{login} on GitHub"))
                                .size(px(24.))
                                .flex()
                                .items_center()
                                .justify_center()
                                .rounded_sm()
                                .text_color(colors.muted_text)
                                .cursor(CursorStyle::PointingHand)
                                .hover(|link| {
                                    link.bg(colors.hovered_surface).text_color(colors.text)
                                })
                                .on_click(move |_, _, cx| cx.open_url(&github_url))
                                .child(icon(IconName::Github, colors.muted_text, 14.))
                        });
                    let field = |label: &'static str, input: Entity<TextInput>| {
                        Field::new(label, Some(input.focus_handle(cx)), input.clone())
                    };

                    div()
                        .flex()
                        .flex_col()
                        .min_w_0()
                        .when_some(username, |account, username| {
                            account.child(
                                div()
                                    .min_h(px(48.))
                                    .px_3()
                                    .py_2()
                                    .flex()
                                    .items_center()
                                    .gap_1()
                                    .child(
                                        div()
                                            .min_w_0()
                                            .truncate()
                                            .font_weight(gpui::FontWeight::SEMIBOLD)
                                            .child(username),
                                    )
                                    .children(github_link),
                            )
                        })
                        .when(identity.is_none(), |account| {
                            account.child(
                                div()
                                    .px_3()
                                    .py_3()
                                    .flex()
                                    .flex_col()
                                    .gap_1()
                                    .child(
                                        div()
                                            .font_weight(gpui::FontWeight::SEMIBOLD)
                                            .child("Sign in to Sift"),
                                    )
                                    .child(
                                        div()
                                            .truncate()
                                            .text_sm()
                                            .text_color(colors.muted_text)
                                            .child(server_name),
                                    ),
                            )
                        })
                        .when(is_local, |account| {
                            account.child(
                                div()
                                    .border_t_1()
                                    .border_color(colors.subtle_border)
                                    .px_3()
                                    .py_3()
                                    .text_sm()
                                    .text_color(colors.muted_text)
                                    .whitespace_normal()
                                    .child("This local instance manages its built-in identity."),
                            )
                        })
                        .when(!is_local && identity.is_none(), |account| {
                            account.child(
                                div()
                                    .border_t_1()
                                    .border_color(colors.subtle_border)
                                    .px_3()
                                    .py_3()
                                    .flex()
                                    .flex_col()
                                    .gap_3()
                                    .child(
                                        Button::new(
                                            "account-github-sign-in",
                                            if pending {
                                                "Waiting for sign in…"
                                            } else {
                                                "Continue with GitHub"
                                            },
                                        )
                                        .tone(ButtonTone::Accent)
                                        .wide(true)
                                        .start_icon(IconName::Github)
                                        .loading(pending)
                                        .on_click(cx.listener(|shell, _, _, cx| {
                                            shell.sign_in_with_github(cx)
                                        })),
                                    )
                                    .child(
                                        div()
                                            .flex()
                                            .items_center()
                                            .gap_2()
                                            .child(
                                                div()
                                                    .flex_1()
                                                    .border_t_1()
                                                    .border_color(colors.subtle_border),
                                            )
                                            .child(
                                                div()
                                                    .text_xs()
                                                    .text_color(colors.muted_text)
                                                    .child("OR"),
                                            )
                                            .child(
                                                div()
                                                    .flex_1()
                                                    .border_t_1()
                                                    .border_color(colors.subtle_border),
                                            ),
                                    )
                                    .child(field("Username", self.account_username_input.clone()))
                                    .child(field("Password", self.account_password_input.clone()))
                                    .child(
                                        Button::new(
                                            "account-password-sign-in",
                                            "Sign in with password",
                                        )
                                        .tone(ButtonTone::Neutral)
                                        .wide(true)
                                        .loading(pending)
                                        .on_click(cx.listener(|shell, _, _, cx| {
                                            shell.sign_in_with_password(cx)
                                        })),
                                    ),
                            )
                        })
                        .when(!is_local && identity.is_some() && !interactive, |account| {
                            account.child(
                                div()
                                    .border_t_1()
                                    .border_color(colors.subtle_border)
                                    .px_3()
                                    .py_3()
                                    .text_sm()
                                    .text_color(colors.muted_text)
                                    .whitespace_normal()
                                    .child("This identity is managed by the current instance."),
                            )
                        })
                        .when(!is_local && interactive, |account| {
                            account.child(
                                div()
                                    .border_t_1()
                                    .border_color(colors.subtle_border)
                                    .px_3()
                                    .py_2()
                                    .flex()
                                    .items_center()
                                    .justify_between()
                                    .gap_2()
                                    .child(
                                        Button::new(
                                            "account-sign-out-all",
                                            "Sign out everywhere",
                                        )
                                        .tone(ButtonTone::DangerGhost)
                                        .loading(pending)
                                        .on_click(cx.listener(|shell, _, _, cx| {
                                            shell.sign_out(true, cx)
                                        })),
                                    )
                                    .child(
                                        Button::new(
                                            "account-sign-out",
                                            if pending {
                                                "Signing out…"
                                            } else {
                                                "Sign out"
                                            },
                                        )
                                        .tone(ButtonTone::Neutral)
                                        .loading(pending)
                                        .on_click(cx.listener(|shell, _, _, cx| {
                                            shell.sign_out(false, cx)
                                        })),
                                    ),
                            )
                        })
                        .children(self.account_error.as_ref().map(|message| {
                            ErrorBanner::new(message.clone())
                        }))
                        .into_any_element()
                }
                Modal::DatabaseConnection => {
                    let step = self.database_wizard_step;
                    let selected_tenant = self.selected_database_tenant;
                    let selected_provider = self.selected_database_provider.clone();
                    let selected_ssl_mode = self.selected_database_ssl_mode.clone();
                    let pending = self.database_connection_pending;
                    let tenant_rows = self.lifecycle.tenants.iter().map(|tenant| {
                        let tenant_id = tenant.id.0;
                        let selected = selected_tenant == Some(tenant_id);
                        div()
                            .id(("database-tenant", tenant_id as usize))
                            .role(Role::Button)
                            .px_2()
                            .py_1()
                            .rounded_sm()
                            .border_1()
                            .border_color(if selected {
                                colors.accent
                            } else {
                                colors.subtle_border
                            })
                            .when(selected, |row| row.bg(colors.accent_muted))
                            .when(!selected, |row| {
                                row.hover(|row| row.bg(colors.hovered_surface))
                            })
                            .on_click(cx.listener(move |shell, _, _, cx| {
                                shell.selected_database_tenant = Some(tenant_id);
                                cx.notify();
                            }))
                            .child(tenant.name.clone())
                    });
                    let provider_rows = [
                        (
                            "sift/postgres",
                            "PostgreSQL",
                            "databases/postgres.svg",
                        ),
                        (
                            "sift/sql-server",
                            "Microsoft SQL Server",
                            "databases/sql-server.svg",
                        ),
                    ]
                    .into_iter()
                    .enumerate()
                    .map(|(index, (provider_id, display_name, asset))| {
                            let available = self
                                .lifecycle
                                .providers
                                .iter()
                                .find(|provider| {
                                    provider.provider.provider_id.as_str() == provider_id
                                })
                                .is_some_and(|provider| provider.available);
                            let selected =
                                selected_provider.as_deref() == Some(provider_id);
                            let logo_size = if provider_id == "sift/postgres" {
                                82.0
                            } else {
                                76.0
                            };
                            let provider_id = provider_id.to_owned();
                            div()
                                .id(("database-provider", index))
                                .role(Role::Button)
                                .aria_label(format!("Select {display_name}"))
                                .relative()
                                .flex_1()
                                .min_w(px(280.))
                                .min_h(px(190.))
                                .p_4()
                                .rounded_lg()
                                .border_2()
                                .border_color(if selected {
                                    colors.accent
                                } else {
                                    colors.subtle_border
                                })
                                .when(selected, |row| row.bg(colors.accent_muted))
                                .when(!available, |row| row.opacity(0.45))
                                .when(!selected && available, |row| {
                                    row.hover(|row| row.bg(colors.hovered_surface))
                                })
                                .when(available, |row| {
                                    row.on_click(cx.listener(move |shell, _, _, cx| {
                                        shell.select_database_provider(provider_id.clone(), cx);
                                    }))
                                })
                                .child(
                                    div()
                                        .h(px(112.))
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .child(
                                            div()
                                                .size(px(96.))
                                                .flex()
                                                .items_center()
                                                .justify_center()
                                                .rounded_lg()
                                                .bg(gpui::white())
                                                .border_1()
                                                .border_color(colors.subtle_border)
                                                .child(
                                                    img(database_logo(asset))
                                                        .size(px(logo_size))
                                                        .object_fit(gpui::ObjectFit::Contain),
                                                ),
                                        ),
                                )
                                .child(
                                    div()
                                        .text_center()
                                        .font_weight(gpui::FontWeight::SEMIBOLD)
                                        .child(display_name),
                                )
                                .when(!available, |card| card.child(
                                    div()
                                        .text_xs()
                                        .text_center()
                                        .when(!selected, |copy| copy.text_color(colors.muted_text))
                                        .child("Unavailable on this server"),
                                ))
                                .when(selected, |card| {
                                    card.child(
                                        div()
                                            .absolute()
                                            .top_2()
                                            .right_2()
                                            .size(px(22.))
                                            .flex()
                                            .items_center()
                                            .justify_center()
                                             .rounded_full()
                                             .bg(colors.accent)
                                             .child(icon(IconName::Check, colors.on_accent, 13.)),
                                    )
                                })
                        });
                    let (security_label, security_options): (&str, &[(&str, &str)]) =
                        if selected_provider.as_deref() == Some("sift/sql-server") {
                            (
                                "ENCRYPTION",
                                &[
                                    ("disable", "Disabled"),
                                    ("require", "Required"),
                                    ("trust_server_certificate", "Trust Server Certificate"),
                                ],
                            )
                        } else {
                            (
                                "SSL MODE",
                                &[
                                    ("disable", "Disabled"),
                                    ("prefer", "Prefer"),
                                    ("require", "Require"),
                                    ("verify_ca", "Verify CA"),
                                    ("verify_full", "Verify Full"),
                                ],
                            )
                        };
                    let ssl_rows = security_options.iter().copied().enumerate().map(
                        |(index, (value, label))| {
                            let selected = selected_ssl_mode.as_deref() == Some(value);
                            div()
                                .id(("database-ssl-mode", index))
                                .role(Role::Button)
                                .px_2()
                                .py_1()
                                .rounded_sm()
                                .border_1()
                                .border_color(if selected {
                                    colors.accent
                                } else {
                                    colors.subtle_border
                                })
                                .when(selected, |row| row.bg(colors.accent_muted))
                                .when(!selected, |row| {
                                    row.hover(|row| row.bg(colors.hovered_surface))
                                })
                                .on_click(cx.listener(move |shell, _, _, cx| {
                                    shell.selected_database_ssl_mode = Some(value.to_owned());
                                    cx.notify();
                                }))
                                .child(label)
                        },
                    );
                    let field = |label: &'static str, input: Entity<TextInput>| {
                        Field::new(
                            label,
                            Some(input.focus_handle(cx)),
                            input.clone(),
                        )
                    };
                    let step_number = match step {
                        DatabaseWizardStep::Provider => 1,
                        DatabaseWizardStep::Details => 2,
                        DatabaseWizardStep::Review => 3,
                    };
                    let step_rows = ["Database", "Connection", "Review"]
                        .into_iter()
                        .enumerate()
                        .map(|(index, label)| {
                            let number = index + 1;
                            let active = number == step_number;
                            let complete = number < step_number;
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .child(
                                    div()
                                        .size(px(22.))
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .rounded_full()
                                        .border_1()
                                        .border_color(if active || complete {
                                            colors.accent
                                        } else {
                                            colors.strong_border
                                        })
                                        .when(active || complete, |circle| {
                                            circle.bg(colors.accent).text_color(colors.on_accent)
                                        })
                                        .text_xs()
                                        .font_weight(gpui::FontWeight::SEMIBOLD)
                                        .when(complete, |circle| {
                                            circle
                                                .child(icon(IconName::Check, colors.on_accent, 12.))
                                        })
                                        .when(!complete, |circle| {
                                            circle.child(number.to_string())
                                        }),
                                )
                                .child(
                                    div()
                                        .text_xs()
                                        .font_weight(if active {
                                            gpui::FontWeight::SEMIBOLD
                                        } else {
                                            gpui::FontWeight::NORMAL
                                        })
                                        .text_color(if active {
                                            colors.text
                                        } else {
                                            colors.muted_text
                                        })
                                        .child(label),
                                )
                        });
                    let provider_name = selected_provider
                        .as_deref()
                        .map(|provider| match provider {
                            "sift/postgres" => "PostgreSQL",
                            "sift/sql-server" => "Microsoft SQL Server",
                            _ => provider,
                        })
                        .unwrap_or("Not selected");
                    let tenant_name = selected_tenant
                        .and_then(|id| {
                            self.lifecycle
                                .tenants
                                .iter()
                                .find(|tenant| tenant.id.0 == id)
                        })
                        .map(|tenant| tenant.name.clone())
                        .unwrap_or_else(|| "Not selected".into());
                    let review_row = |label: &'static str, value: String| {
                        div()
                            .flex()
                            .min_w_0()
                            .items_start()
                            .justify_between()
                            .gap_3()
                            .py_2()
                            .border_b_1()
                            .border_color(colors.subtle_border)
                            .child(
                                div()
                                    .flex_none()
                                    .text_color(colors.muted_text)
                                    .child(label),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .whitespace_normal()
                                    .text_right()
                                    .line_clamp(2)
                                    .text_ellipsis()
                                    .child(value),
                            )
                    };
                    div()
                        .flex()
                        .flex_col()
                        .min_h_0()
                        .max_h(gpui::relative(1.))
                        .overflow_hidden()
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .items_stretch()
                                .gap_2()
                                .px_3()
                                .pt_2()
                                .pb_2()
                                .border_b_1()
                                .border_color(colors.subtle_border)
                                .bg(colors.toolbar)
                                .child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap_2()
                                        .font_weight(gpui::FontWeight::SEMIBOLD)
                                        .child(
                                            div()
                                                .min_w_0()
                                                .truncate()
                                                .child("Add Database Connection"),
                                        ),
                                )
                                .child(
                                    div()
                                        .flex()
                                        .min_w_0()
                                        .overflow_x_hidden()
                                        .items_center()
                                        .gap_4()
                                        .children(step_rows),
                                ),
                        )
                        .child(
                            div()
                                .id("database-connection-form")
                                .tab_group()
                                .flex()
                                .flex_1()
                                .flex_col()
                                .min_h_0()
                                .gap_3()
                                .max_h(px(540.))
                                .overflow_y_scroll()
                                .p_3()
                                .when(step == DatabaseWizardStep::Provider, |form| {
                                    form.child(
                                        div()
                                            .flex()
                                            .flex_col()
                                            .gap_3()
                                            .child(
                                                div()
                                                    .flex()
                                                    .flex_wrap()
                                                    .gap_3()
                                                    .children(provider_rows),
                                            ),
                                    )
                                })
                                .when(step == DatabaseWizardStep::Details, |form| {
                                    form
                                        .child(
                                            div()
                                                .flex_col()
                                                .flex()
                                                .gap_2()
                                                .child(
                                                    div()
                                                        .text_xs()
                                                        .text_color(colors.muted_text)
                                                        .child("WORKSPACE"),
                                                )
                                                .child(div().flex().flex_1().flex_wrap().gap_1().children(tenant_rows)),
                                        )
                                        .child(
                                            div()
                                                .flex()
                                                .flex_wrap()
                                                .gap_3()
                                                .child(div().flex_1().min_w(px(220.)).child(field("CONNECTION NAME", self.database_name_input.clone())))
                                                .child(div().flex_1().min_w(px(220.)).child(field("DATABASE", self.database_catalog_input.clone()))),
                                        )
                                        .child(
                                            div()
                                                .flex()
                                                .min_w_0()
                                                .gap_3()
                                                .child(div().flex_1().min_w_0().child(field("HOST", self.database_host_input.clone())))
                                                .child(div().flex_none().w(px(112.)).child(field("PORT", self.database_port_input.clone()))),
                                        )
                                        .child(
                                            div()
                                                .flex()
                                                .flex_wrap()
                                                .gap_3()
                                                .child(div().flex_1().min_w(px(220.)).child(field("USERNAME", self.database_user_input.clone())))
                                                .child(div().flex_1().min_w(px(220.)).child(field("PASSWORD", self.database_password_input.clone()))),
                                        )
                                        .child(
                                            div()
                                                .flex()
                                                .flex_col()
                                                .gap_1()
                                                .child(div().text_xs().text_color(colors.muted_text).child(security_label))
                                                .child(div().flex().flex_wrap().gap_1().children(ssl_rows)),
                                        )
                                        .child(
                                            div()
                                                .pt_3()
                                                .border_t_1()
                                                .border_color(colors.subtle_border)
                                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                                .child("Advanced"),
                                        )
                                        .when(selected_provider.as_deref() == Some("sift/postgres"), |form| {
                                            form.child(
                                                div()
                                                    .flex()
                                                    .flex_wrap()
                                                    .gap_3()
                                                    .child(div().flex_1().min_w(px(220.)).child(field("SEARCH PATH", self.database_search_path_input.clone())))
                                                    .child(div().flex_1().min_w(px(220.)).child(field("APPLICATION NAME", self.database_application_name_input.clone()))),
                                            )
                                        })
                                        .child(
                                            div()
                                                .flex()
                                                .flex_wrap()
                                                .gap_3()
                                                .child(div().flex_1().min_w(px(180.)).child(field("CONNECT TIMEOUT (SECONDS)", self.database_timeout_input.clone())))
                                                .child(div().flex_1().min_w(px(180.)).child(field("MINIMUM POOL SIZE", self.database_pool_min_input.clone())))
                                                .when(selected_provider.as_deref() == Some("sift/postgres"), |row| {
                                                    row.child(div().flex_1().min_w(px(180.)).child(field("MAXIMUM POOL SIZE", self.database_pool_max_input.clone())))
                                                }),
                                        )
                                })
                                .when(step == DatabaseWizardStep::Review, |form| {
                                    form
                                        .child(review_row("Database type", provider_name.to_owned()))
                                        .child(review_row("Workspace", tenant_name))
                                        .child(review_row("Connection name", self.database_name_input.read(cx).text().to_owned()))
                                        .child(review_row(
                                            "Server",
                                            format!("{}:{}", self.database_host_input.read(cx).text(), self.database_port_input.read(cx).text()),
                                        ))
                                        .child(review_row(
                                            "Database",
                                            if self.database_catalog_input.read(cx).text().is_empty() {
                                                "Provider default".into()
                                            } else {
                                                self.database_catalog_input.read(cx).text().to_owned()
                                            },
                                        ))
                                        .child(review_row("Username", self.database_user_input.read(cx).text().to_owned()))
                                        .child(review_row(
                                            "Password",
                                            if self.database_password_input.read(cx).text().is_empty() {
                                                "Not provided".into()
                                            } else {
                                                "Stored securely".into()
                                            },
                                        ))
                                        .child(review_row(
                                            "Transport security",
                                            selected_ssl_mode.clone().unwrap_or_else(|| "Provider default".into()).replace('_', " "),
                                        ))
                                })
                                .children(self.database_connection_error.as_ref().map(|message| {
                                    ErrorBanner::new(message.clone())
                                })),
                        )
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .justify_end()
                                .px_3()
                                .py_2()
                                .border_t_1()
                                .border_color(colors.subtle_border)
                                .bg(colors.toolbar)
                                .child(
                                    div()
                                        .flex()
                                        .gap_2()
                                        .child(
                                            Button::new(
                                                "database-wizard-secondary",
                                                if step == DatabaseWizardStep::Provider {
                                                    "Cancel"
                                                } else {
                                                    "Back"
                                                },
                                            )
                                            .tone(ButtonTone::Neutral)
                                            .wide(true)
                                            .loading(pending)
                                            .on_click(cx.listener(
                                                move |shell, _, window, cx| {
                                                    if step == DatabaseWizardStep::Provider {
                                                        shell.dismiss_modal(
                                                            &DismissModal,
                                                            window,
                                                            cx,
                                                        )
                                                    } else {
                                                        shell.database_wizard_back(window, cx)
                                                    }
                                                },
                                            )),
                                        )
                                        .child(
                                            Button::new(
                                                "database-wizard-primary",
                                                if pending {
                                                    "Saving & Testing…"
                                                } else if step == DatabaseWizardStep::Review {
                                                    "Save & Connect"
                                                } else {
                                                    "Continue"
                                                },
                                            )
                                            .tone(ButtonTone::Accent)
                                            .wide(true)
                                            .loading(
                                                pending
                                                    || (step == DatabaseWizardStep::Provider
                                                        && selected_provider.is_none()),
                                            )
                                            .on_click(cx.listener(
                                                move |shell, _, window, cx| {
                                                    if step == DatabaseWizardStep::Review {
                                                        shell.submit_database_connection(cx)
                                                    } else {
                                                        shell.database_wizard_next(window, cx)
                                                    }
                                                },
                                            )),
                                        ),
                                ),
                        )
                        .into_any_element()
                }
                Modal::ConfirmDeleteConnection(entry) => {
                    let entry = entry.clone();
                    let entry_for_delete = entry.clone();
                    div()
                        .flex()
                        .flex_col()
                        .gap_4()
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .child(icon(IconName::Warning, colors.danger, 16.))
                                .child(format!("Delete {}?", entry.name)),
                        )
                        .child(
                            div()
                                .text_sm()
                                .text_color(colors.muted_text)
                                .whitespace_normal()
                                .child("This removes the connection and its stored credentials. Connections managed by sift.toml must be removed from the manifest instead."),
                        )
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .justify_end()
                                .gap_2()
                                .child(
                                    Button::new("cancel-delete-connection", "Cancel")
                                        .tone(ButtonTone::Neutral)
                                        .wide(true)
                                        .on_click(cx.listener(|shell, _, window, cx| {
                                            shell.dismiss_modal(&DismissModal, window, cx)
                                        })),
                                )
                                .child(
                                    Button::new("confirm-delete-connection", "Delete")
                                        .tone(ButtonTone::DangerMuted)
                                        .wide(true)
                                        .on_click(cx.listener(move |shell, _, _, cx| {
                                            shell.confirm_delete_connection(
                                                &entry_for_delete,
                                                cx,
                                            )
                                        })),
                                ),
                        )
                        .into_any_element()
                }
            };
            let toolbar_height = cx.theme().metrics.toolbar_height;
            // Scrim-clicking dismisses transient surfaces. Long-form dialogs
            // with typed-but-unsaved input keep their explicit cancel control.
            let dismiss_on_scrim = matches!(
                modal,
                Modal::ServerPicker
                    | Modal::Settings
                    | Modal::Account
                    | Modal::CommandPalette
                    | Modal::DatabaseConnection
                    | Modal::ConfirmDeleteConnection(_)
            );
            div()
                .id("modal-layer")
                .key_context("SiftModal")
                .absolute()
                .top(if app_bar_modal {
                    toolbar_height
                } else {
                    px(0.)
                })
                .right_0()
                .bottom_0()
                .left_0()
                .occlude()
                .when(dismiss_on_scrim, |layer| {
                    layer.on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|shell, _, window, cx| {
                            shell.dismiss_modal(&DismissModal, window, cx)
                        }),
                    )
                })
                .when(!dismiss_on_scrim, |layer| {
                    layer.on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                })
                .flex()
                .items_start()
                .when(server_picker, |layer| {
                    layer.justify_start().pt_1().pl(px(38.))
                })
                .when(account, |layer| layer.justify_end().pt_1().pr_2())
                .when(settings || database_connection, |layer| {
                    layer
                        .items_center()
                        .justify_center()
                        .px_4()
                        .py_4()
                        .bg(colors.scrim)
                })
                .when(
                    !server_picker && !settings && !account && !database_connection,
                    |layer| {
                        layer
                            .justify_center()
                            .pt(if app_bar_modal {
                                px(100.) - toolbar_height
                            } else {
                                px(100.)
                            })
                            .bg(colors.scrim)
                    },
                )
                .child(
                    div()
                        .id("modal-card")
                        .occlude()
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .w_full()
                        .max_w(px(card_width))
                        .max_h(gpui::relative(0.92))
                        .flex()
                        .flex_col()
                        .when(
                            !database_connection && !command_palette && !account && !server_picker,
                            |card| card.p_3(),
                        )
                        .overflow_hidden()
                        .rounded(cx.theme().metrics.radius_large)
                        .border_1()
                        .border_color(colors.strong_border)
                        .bg(colors.panel)
                        .shadow_lg()
                        .child(content),
                )
        })
    }
}

impl Focusable for WorkspaceShell {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

fn window_resize_handle(
    id: &'static str,
    edge: ResizeEdge,
    cursor: CursorStyle,
) -> gpui::AnyElement {
    const EDGE_SIZE: f32 = 5.;
    const CORNER_SIZE: f32 = 10.;

    let handle = div().id(id).absolute().cursor(cursor).on_mouse_down(
        MouseButton::Left,
        move |_, window, cx| {
            cx.stop_propagation();
            window.start_window_resize(edge);
        },
    );

    match edge {
        ResizeEdge::Top => handle.top_0().left_0().w_full().h(px(EDGE_SIZE)),
        ResizeEdge::Right => handle.top_0().right_0().h_full().w(px(EDGE_SIZE)),
        ResizeEdge::Bottom => handle.bottom_0().left_0().w_full().h(px(EDGE_SIZE)),
        ResizeEdge::Left => handle.top_0().left_0().h_full().w(px(EDGE_SIZE)),
        ResizeEdge::TopLeft => handle.top_0().left_0().size(px(CORNER_SIZE)),
        ResizeEdge::TopRight => handle.top_0().right_0().size(px(CORNER_SIZE)),
        ResizeEdge::BottomRight => handle.bottom_0().right_0().size(px(CORNER_SIZE)),
        ResizeEdge::BottomLeft => handle.bottom_0().left_0().size(px(CORNER_SIZE)),
    }
    .into_any_element()
}

fn window_resize_handles() -> Vec<gpui::AnyElement> {
    vec![
        window_resize_handle("resize-top", ResizeEdge::Top, CursorStyle::ResizeUpDown),
        window_resize_handle(
            "resize-right",
            ResizeEdge::Right,
            CursorStyle::ResizeLeftRight,
        ),
        window_resize_handle(
            "resize-bottom",
            ResizeEdge::Bottom,
            CursorStyle::ResizeUpDown,
        ),
        window_resize_handle(
            "resize-left",
            ResizeEdge::Left,
            CursorStyle::ResizeLeftRight,
        ),
        window_resize_handle(
            "resize-top-left",
            ResizeEdge::TopLeft,
            CursorStyle::ResizeUpLeftDownRight,
        ),
        window_resize_handle(
            "resize-top-right",
            ResizeEdge::TopRight,
            CursorStyle::ResizeUpRightDownLeft,
        ),
        window_resize_handle(
            "resize-bottom-right",
            ResizeEdge::BottomRight,
            CursorStyle::ResizeUpLeftDownRight,
        ),
        window_resize_handle(
            "resize-bottom-left",
            ResizeEdge::BottomLeft,
            CursorStyle::ResizeUpRightDownLeft,
        ),
    ]
}

fn edge_resize_enabled(is_maximized: bool, is_fullscreen: bool) -> bool {
    !is_maximized && !is_fullscreen
}

fn dock_resize_separator(dock: DockId, border: gpui::Hsla) -> gpui::AnyElement {
    let (id, cursor) = match dock {
        DockId::Left => ("resize-left-dock", CursorStyle::ResizeLeftRight),
        DockId::Inspector => ("resize-right-dock", CursorStyle::ResizeLeftRight),
        DockId::Bottom => ("resize-bottom-dock", CursorStyle::ResizeUpDown),
    };
    let vertical = dock != DockId::Bottom;
    div()
        .relative()
        .flex_none()
        .bg(border)
        .when(vertical, |separator| separator.w(px(1.0)).h_full())
        .when(!vertical, |separator| separator.w_full().h(px(1.0)))
        .child(
            div()
                .id(id)
                .debug_selector(move || id.to_owned())
                .absolute()
                .cursor(cursor)
                .block_mouse_except_scroll()
                .when(vertical, |handle| {
                    handle
                        .left(px(-(DOCK_RESIZE_HANDLE_SIZE - 1.0) / 2.0))
                        .top_0()
                        .h_full()
                        .w(px(DOCK_RESIZE_HANDLE_SIZE))
                })
                .when(!vertical, |handle| {
                    handle
                        .top(px(-(DOCK_RESIZE_HANDLE_SIZE - 1.0) / 2.0))
                        .left_0()
                        .w_full()
                        .h(px(DOCK_RESIZE_HANDLE_SIZE))
                })
                .on_drag(DockResizeDrag { dock }, |_, _, _, cx| {
                    cx.new(|_| gpui::Empty)
                }),
        )
        .into_any_element()
}

fn pane_resize_handle(boundary: usize, border: gpui::Hsla) -> gpui::AnyElement {
    div()
        .id(("pane-separator", boundary))
        .debug_selector(move || format!("pane-separator-{boundary}"))
        .relative()
        .flex_none()
        .w(px(1.0))
        .h_full()
        .bg(border)
        .child(
            div()
                .id(("resize-pane", boundary))
                .debug_selector(move || format!("resize-pane-{boundary}"))
                .absolute()
                .left(px(-(PANE_RESIZE_HANDLE_SIZE - 1.0) / 2.0))
                .top_0()
                .w(px(PANE_RESIZE_HANDLE_SIZE))
                .h_full()
                .cursor(CursorStyle::ResizeLeftRight)
                .block_mouse_except_scroll()
                .on_drag(PaneResizeDrag { boundary }, |_, _, _, cx| {
                    cx.new(|_| gpui::Empty)
                }),
        )
        .into_any_element()
}

impl gpui::Render for WorkspaceShell {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = cx.theme().colors;
        // Docks are built before the element chain so each borrows `cx`
        // sequentially rather than two `when` closures capturing it at once.
        let left_dock = self
            .left_dock
            .presentation
            .open
            .then(|| self.render_dock(&self.left_dock, cx));
        let left_dock_separator = self
            .left_dock
            .presentation
            .open
            .then(|| dock_resize_separator(DockId::Left, colors.subtle_border));
        let right_dock = self
            .right_dock
            .presentation
            .open
            .then(|| self.render_dock(&self.right_dock, cx));
        let right_dock_separator = self
            .right_dock
            .presentation
            .open
            .then(|| dock_resize_separator(DockId::Inspector, colors.subtle_border));
        let bottom_dock = self
            .bottom_dock
            .presentation
            .open
            .then(|| bottom_tools::render_bottom_panel(self, cx));
        let bottom_dock_separator = self
            .bottom_dock
            .presentation
            .open
            .then(|| dock_resize_separator(DockId::Bottom, colors.subtle_border));
        let total_flex = self.pane_flexes.iter().sum::<f32>().max(f32::EPSILON);
        let mut pane_elements = Vec::with_capacity(self.panes.len().saturating_mul(2));
        for (index, pane) in self.panes.iter().enumerate() {
            let fraction = self.pane_flexes.get(index).copied().unwrap_or(1.0) / total_flex;
            pane_elements.push(
                div()
                    .id(("pane-slot", index))
                    .debug_selector(move || format!("pane-slot-{index}"))
                    .flex()
                    .h_full()
                    .min_w_0()
                    .flex_shrink_1()
                    .flex_basis(DefiniteLength::Fraction(fraction))
                    .overflow_hidden()
                    .child(pane.clone())
                    .into_any_element(),
            );
            if index + 1 < self.panes.len() {
                pane_elements.push(pane_resize_handle(index, colors.subtle_border));
            }
        }
        div()
            .id("sift-shell")
            .key_context("SiftWorkspace")
            .role(Role::Application)
            .aria_label("Sift database workspace")
            .track_focus(&self.focus_handle)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|shell, _, _, cx| shell.collapse_app_bar_navigation(cx)),
            )
            .on_action(cx.listener(Self::open_command_palette))
            .on_action(cx.listener(Self::open_server_connection))
            .on_action(cx.listener(Self::dismiss_modal))
            .on_action(cx.listener(Self::palette_up))
            .on_action(cx.listener(Self::palette_down))
            .on_action(cx.listener(Self::palette_confirm))
            .on_action(cx.listener(Self::split_pane))
            .on_action(cx.listener(Self::focus_next_pane))
            .on_action(cx.listener(Self::close_active_pane))
            .on_action(cx.listener(Self::close_active_item))
            .on_action(cx.listener(Self::save_active_item))
            .on_action(cx.listener(Self::toggle_left_dock))
            .on_action(cx.listener(Self::toggle_right_dock))
            .on_action(cx.listener(Self::toggle_bottom_dock))
            .relative()
            .flex()
            .flex_col()
            .size_full()
            .bg(colors.background)
            .text_color(colors.text)
            .child(self.render_toolbar(cx))
            .child(
                div()
                    .id("workspace-content")
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .on_drag_move::<DockResizeDrag>(cx.listener(Self::resize_dock))
                    .on_drop::<DockResizeDrag>(cx.listener(Self::finish_dock_resize))
                    .children(left_dock)
                    .children(left_dock_separator)
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .min_h_0()
                            .child(
                                div()
                                    .flex()
                                    .flex_1()
                                    .min_w_0()
                                    .min_h_0()
                                    .on_drag_move::<PaneResizeDrag>(cx.listener(Self::resize_panes))
                                    .on_drop::<PaneResizeDrag>(
                                        cx.listener(Self::finish_pane_resize),
                                    )
                                    .children(pane_elements),
                            )
                            .children(bottom_dock_separator)
                            .children(bottom_dock),
                    )
                    .children(right_dock_separator)
                    .children(right_dock),
            )
            .child(status_bar::render_status_bar(self, cx))
            .children((!self.toasts.is_empty()).then(|| {
                div()
                    .id("toast-stack")
                    .absolute()
                    .right_3()
                    .bottom(px(38.))
                    .flex()
                    .flex_col()
                    .items_end()
                    .gap_2()
                    .children(self.toasts.iter().map(|toast| {
                        let (icon_name, tone_color, chip_bg) = match toast.tone {
                            ToastTone::Info => {
                                (IconName::Info, colors.accent_hover, colors.accent_muted)
                            }
                            ToastTone::Success => {
                                (IconName::Check, colors.success, colors.success_muted)
                            }
                            ToastTone::Error => {
                                (IconName::Warning, colors.danger, colors.danger_muted)
                            }
                        };
                        let mut toast_bg = colors.elevated_surface;
                        toast_bg.a = 1.0;
                        div()
                            .id(("toast", toast.id as usize))
                            .role(Role::Button)
                            .aria_label(format!("Dismiss notification: {}", toast.message))
                            .w(px(340.))
                            .max_w(gpui::relative(0.8))
                            .px_3()
                            .py_2()
                            .flex()
                            .items_center()
                            .gap_2()
                            .rounded(cx.theme().metrics.radius_large)
                            .border_1()
                            .border_color(tone_color)
                            .bg(toast_bg)
                            .shadow_lg()
                            .hover(|toast| toast.bg(colors.panel))
                            .on_click(cx.listener(|shell, _, _, cx| shell.dismiss_toast(cx)))
                            .child(
                                div()
                                    .flex_none()
                                    .size(px(24.))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .rounded(cx.theme().metrics.radius)
                                    .bg(chip_bg)
                                    .child(icon(icon_name, tone_color, 14.)),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .whitespace_normal()
                                    .text_sm()
                                    .child(toast.message.clone()),
                            )
                            .child(icon(IconName::Close, colors.muted_text, 12.))
                    }))
            }))
            .children(self.render_modal(cx))
            .children(
                edge_resize_enabled(window.is_maximized(), window.is_fullscreen())
                    .then(window_resize_handles)
                    .into_iter()
                    .flatten(),
            )
    }
}

/// Render `label` with the case-insensitive `query` substring emphasized in the
/// accent color, like a fuzzy-finder match highlight.
fn highlight_match(label: &'static str, query: &str, accent: gpui::Hsla) -> impl IntoElement {
    let base = div()
        .flex()
        .flex_1()
        .min_w_0()
        .overflow_hidden()
        .whitespace_nowrap();
    if query.is_empty() {
        return base.child(label);
    }
    match label.to_lowercase().find(query) {
        Some(start) => {
            let end = start + query.len();
            base.child(&label[..start])
                .child(
                    div()
                        .text_color(accent)
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .child(&label[start..end]),
                )
                .child(&label[end..])
        }
        None => base.child(label),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{point, EntityInputHandler, Modifiers, TestAppContext, VisualTestContext};

    fn shell(cx: &mut TestAppContext) -> gpui::WindowHandle<WorkspaceShell> {
        cx.update(|cx| {
            cx.open_window(Default::default(), |window, cx| {
                cx.new(|cx| {
                    WorkspaceShell::new(
                        Default::default(),
                        Default::default(),
                        None,
                        None,
                        window,
                        cx,
                    )
                })
            })
            .unwrap()
        })
    }

    fn shell_with_state(
        state: PresentationState,
        cx: &mut TestAppContext,
    ) -> gpui::WindowHandle<WorkspaceShell> {
        cx.update(|cx| {
            cx.open_window(Default::default(), |window, cx| {
                cx.new(|cx| WorkspaceShell::new(state, Default::default(), None, None, window, cx))
            })
            .unwrap()
        })
    }

    #[test]
    fn edge_resize_handles_are_disabled_when_window_fills_the_screen() {
        assert!(edge_resize_enabled(false, false));
        assert!(!edge_resize_enabled(true, false));
        assert!(!edge_resize_enabled(false, true));
        assert!(!edge_resize_enabled(true, true));
    }

    #[test]
    fn pane_resize_changes_only_the_adjacent_pair_and_preserves_total_flex() {
        let mut flexes = vec![1.0, 1.0, 1.0];
        resize_pane_flexes(&mut flexes, 1, 800.0, 1_000.0);

        assert_eq!(flexes[0], 1.0);
        assert!((flexes.iter().sum::<f32>() - 3.0).abs() < 0.001);
        assert!(flexes[1] > flexes[2]);
    }

    #[test]
    fn invalid_restored_pane_flexes_fall_back_to_equal_sizes() {
        assert_eq!(valid_pane_flexes(vec![1.0], 2), vec![1.0, 1.0]);
        assert_eq!(valid_pane_flexes(vec![1.0, -1.0], 2), vec![1.0, 1.0]);
    }

    #[gpui::test]
    fn server_switch_swaps_and_restores_full_workspace_session(cx: &mut TestAppContext) {
        let mut state = PresentationState::default();
        state.workspace.workspace_id = Some(11);
        let mut remote = PresentationState::default().workspace;
        remote.instance_id = Some("hosted:team".into());
        remote.workspace_id = Some(22);
        remote.panes[0].items[0].kind = ItemKind::Welcome;
        remote.panes[0].items[0].title = "Team home".into();
        state
            .instance_workspaces
            .insert("hosted:team".into(), remote);

        let window = shell_with_state(state, cx);
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        let workspace = window.root(&mut cx).unwrap();
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        workspace.update_in(&mut cx, |shell, window, cx| {
            shell.executor_sender = Some(sender);
            let editor = shell.panes[0].read(cx).editor(1).unwrap().clone();
            editor.update(cx, |editor, cx| {
                editor.replace_text_in_range(None, "select 'local'", window, cx)
            });
            shell.switch_instance_workspace("hosted:team", window, cx);
        });
        assert!(matches!(
            receiver.try_recv(),
            Ok(ExecutorCommand::Disconnect)
        ));
        workspace.read_with(&cx, |shell, cx| {
            assert_eq!(shell.selected_workspace_id, Some(22));
            assert_eq!(shell.panes[0].read(cx).items[0].title, "Team home");
        });

        workspace.update_in(&mut cx, |shell, window, cx| {
            shell.switch_instance_workspace("local", window, cx)
        });
        assert!(matches!(
            receiver.try_recv(),
            Ok(ExecutorCommand::Disconnect)
        ));
        workspace.read_with(&cx, |shell, cx| {
            assert_eq!(shell.selected_workspace_id, Some(11));
            assert_eq!(
                shell.panes[0]
                    .read(cx)
                    .editor(1)
                    .unwrap()
                    .read(cx)
                    .document()
                    .text(),
                "select 'local'"
            );
            let snapshot = shell.snapshot(cx);
            assert_eq!(
                snapshot.instance_workspaces["hosted:team"].workspace_id,
                Some(22)
            );
        });
    }

    #[test]
    fn focused_pane_border_uses_the_primary_accent() {
        let theme = Theme::dark();
        assert_eq!(pane_border_color(&theme, true), theme.colors.accent);
        assert_eq!(pane_border_color(&theme, false), theme.colors.subtle_border);
    }

    #[test]
    fn table_previews_are_bounded_and_quote_identifiers() {
        let postgres = sift_protocol::ProviderId::new("sift/postgres").unwrap();
        assert_eq!(
            table_preview_sql(&postgres, "lab", "odd\"table"),
            "SELECT * FROM \"lab\".\"odd\"\"table\" LIMIT 100;"
        );
        let sql_server = sift_protocol::ProviderId::new("sift/sql-server").unwrap();
        assert_eq!(
            table_preview_sql(&sql_server, "dbo", "people"),
            "SELECT TOP (100) * FROM \"dbo\".\"people\";"
        );
    }

    #[gpui::test]
    fn loaded_schema_expands_the_demo_catalog_and_lab_schema(cx: &mut TestAppContext) {
        let window = shell(cx);
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        let workspace = window.root(&mut cx).unwrap();
        let mut snapshot =
            sift_protocol::SchemaSnapshot::empty(sift_protocol::SchemaScope::shallow());
        snapshot.trees.push(sift_protocol::CatalogTree {
            name: "sifttest".into(),
            schemas: vec![sift_protocol::SchemaTree {
                name: "lab".into(),
                objects: vec![sift_protocol::ObjectInfo::new(
                    "people",
                    sift_protocol::ObjectKind::Table,
                )],
            }],
        });

        workspace.update(&mut cx, |shell, cx| {
            shell.on_executor_event(
                ExecutorEvent::SchemaLoaded {
                    profile_id: 7,
                    snapshot: Box::new(snapshot),
                },
                cx,
            )
        });

        workspace.read_with(&cx, |shell, _| {
            assert!(shell.expanded_catalogs.contains(&(7, "sifttest".into())));
            assert!(shell
                .expanded_schemas
                .contains(&(7, "sifttest".into(), "lab".into())));
        });
    }

    #[gpui::test]
    fn every_connections_tree_container_can_collapse_and_expand(cx: &mut TestAppContext) {
        let window = shell(cx);
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        let workspace = window.root(&mut cx).unwrap();

        workspace.update(&mut cx, |shell, cx| {
            shell.toggle_tenant(1, cx);
            shell.toggle_connection(2, cx);
            shell.toggle_room(3, cx);
            shell.toggle_catalog_schema(2, "sifttest".into(), cx);
            shell.toggle_database_schema(2, "sifttest".into(), "lab".into(), cx);
        });
        workspace.read_with(&cx, |shell, _| {
            assert!(shell.expanded_tenants.contains(&1));
            assert!(shell.expanded_connections.contains(&2));
            assert!(shell.expanded_rooms.contains(&3));
            assert!(shell.expanded_catalogs.contains(&(2, "sifttest".into())));
            assert!(shell
                .expanded_schemas
                .contains(&(2, "sifttest".into(), "lab".into())));
        });

        workspace.update(&mut cx, |shell, cx| {
            shell.toggle_tenant(1, cx);
            shell.toggle_connection(2, cx);
            shell.toggle_room(3, cx);
            shell.toggle_catalog_schema(2, "sifttest".into(), cx);
            shell.toggle_database_schema(2, "sifttest".into(), "lab".into(), cx);
        });
        workspace.read_with(&cx, |shell, _| {
            assert!(shell.expanded_tenants.is_empty());
            assert!(shell.expanded_connections.is_empty());
            assert!(shell.expanded_rooms.is_empty());
            assert!(shell.expanded_catalogs.is_empty());
            assert!(shell.expanded_schemas.is_empty());
        });
    }

    #[gpui::test]
    fn schema_object_opens_and_runs_a_preview_query(cx: &mut TestAppContext) {
        let window = shell(cx);
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        let workspace = window.root(&mut cx).unwrap();
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();

        workspace.update_in(&mut cx, |shell, window, cx| {
            shell.executor_sender = Some(sender);
            shell.connection_status = ConnectionStatus::Connected {
                profile_id: 2,
                name: "Demo".into(),
            };
            shell.open_table_preview(
                DatabaseObjectTarget {
                    connection: ConnectionNavEntry {
                        id: 2,
                        tenant_id: 1,
                        name: "Demo".into(),
                        provider_id: sift_protocol::ProviderId::new("sift/postgres").unwrap(),
                    },
                    catalog: "sifttest".into(),
                    schema: "lab".into(),
                    object: "people".into(),
                    object_kind: sift_protocol::ObjectKind::Table,
                },
                window,
                cx,
            );
        });

        let (item_id, sql) = match receiver.try_recv().unwrap() {
            ExecutorCommand::Execute { item_id, sql } => (item_id, sql),
            _ => panic!("expected preview execution"),
        };
        assert_eq!(sql, "SELECT * FROM \"lab\".\"people\" LIMIT 100;");
        workspace.read_with(&cx, |shell, cx| {
            let pane = shell.panes[shell.active_pane].read(cx);
            assert_eq!(pane.active_item().map(|item| item.id), Some(item_id));
            assert_eq!(
                pane.active_item().map(|item| item.title.as_str()),
                Some("lab.people")
            );
            assert_eq!(
                pane.editor(item_id).unwrap().read(cx).document().text(),
                sql
            );
        });

        workspace.update_in(&mut cx, |shell, window, cx| {
            shell.open_table_preview(
                DatabaseObjectTarget {
                    connection: ConnectionNavEntry {
                        id: 2,
                        tenant_id: 1,
                        name: "Demo".into(),
                        provider_id: sift_protocol::ProviderId::new("sift/postgres").unwrap(),
                    },
                    catalog: "sifttest".into(),
                    schema: "lab".into(),
                    object: "people".into(),
                    object_kind: sift_protocol::ObjectKind::Table,
                },
                window,
                cx,
            );
        });
        assert!(
            receiver.try_recv().is_err(),
            "focused previews must not rerun"
        );
        workspace.read_with(&cx, |shell, cx| {
            let matching_tabs = shell
                .panes
                .iter()
                .map(|pane| {
                    pane.read(cx)
                        .items
                        .iter()
                        .filter(|item| item.kind == ItemKind::Query && item.title == "lab.people")
                        .count()
                })
                .sum::<usize>();
            assert_eq!(matching_tabs, 1);
            assert_eq!(
                shell.panes[shell.active_pane]
                    .read(cx)
                    .active_item()
                    .map(|item| item.id),
                Some(item_id)
            );
        });
    }

    #[gpui::test]
    fn database_snapshot_reconnects_lazily_and_becomes_live_after_refresh(cx: &mut TestAppContext) {
        let window = shell(cx);
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        let workspace = window.root(&mut cx).unwrap();
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let connection = ConnectionNavEntry {
            id: 9,
            tenant_id: 4,
            name: "Warehouse".into(),
            provider_id: sift_protocol::ProviderId::new("sift/postgres").unwrap(),
        };

        workspace.update_in(&mut cx, |shell, window, cx| {
            shell.executor_sender = Some(sender);
            shell.open_table_preview(
                DatabaseObjectTarget {
                    connection,
                    catalog: "analytics".into(),
                    schema: "public".into(),
                    object: "events".into(),
                    object_kind: sift_protocol::ObjectKind::View,
                },
                window,
                cx,
            );
        });
        let item_id = workspace.read_with(&cx, |shell, cx| {
            let pane = shell.panes[shell.active_pane].read(cx);
            let item = pane.active_item().unwrap();
            assert!(matches!(
                pane.database_item_states.get(&item.id),
                Some(DatabaseItemState::Reconnecting)
            ));
            item.id
        });
        assert!(matches!(
            receiver.try_recv(),
            Ok(ExecutorCommand::Connect {
                tenant_id: 4,
                profile_id: 9,
                ..
            })
        ));
        cx.run_until_parked();
        assert!(cx.debug_bounds("database-snapshot-overlay").is_some());

        workspace.update(&mut cx, |shell, cx| {
            shell.on_executor_event(
                ExecutorEvent::Connection(ConnectionStatus::Connected {
                    profile_id: 9,
                    name: "Warehouse".into(),
                }),
                cx,
            )
        });
        assert!(matches!(
            receiver.try_recv(),
            Ok(ExecutorCommand::Execute { item_id: executed, .. }) if executed == item_id
        ));
        workspace.update(&mut cx, |shell, cx| {
            shell.route_result(item_id, ResultState::Ready(Default::default()), cx);
        });
        workspace.read_with(&cx, |shell, cx| {
            let pane = shell.panes[shell.active_pane].read(cx);
            assert!(matches!(
                pane.database_item_states.get(&item_id),
                Some(DatabaseItemState::Live)
            ));
            assert!(pane
                .database_source(item_id)
                .unwrap()
                .last_refreshed_at_ms
                .is_some());
        });

        workspace.update(&mut cx, |shell, cx| shell.disconnect(cx));
        workspace.read_with(&cx, |shell, cx| {
            assert!(matches!(
                shell.panes[shell.active_pane]
                    .read(cx)
                    .database_item_states
                    .get(&item_id),
                Some(DatabaseItemState::Offline)
            ));
        });
    }

    #[gpui::test]
    fn pane_navigation_history_tracks_tabs_and_skips_closed_items(cx: &mut TestAppContext) {
        let mut state = PresentationState::default();
        state.workspace.panes[0].items.extend([
            ItemPresentation {
                id: 2,
                kind: ItemKind::Query,
                title: "two.sql".into(),
                dirty: false,
                source: None,
            },
            ItemPresentation {
                id: 3,
                kind: ItemKind::Query,
                title: "three.sql".into(),
                dirty: false,
                source: None,
            },
        ]);
        let window = shell_with_state(state, cx);
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        let workspace = window.root(&mut cx).unwrap();
        let pane = workspace.read_with(&cx, |workspace, _| workspace.panes[0].clone());

        pane.update(&mut cx, |pane, _| {
            pane.activate_item(1, true);
            pane.activate_item(2, true);
            assert!(pane.can_navigate_backward());
            pane.navigate_backward();
            assert_eq!(pane.active_item().map(|item| item.id), Some(2));
            pane.navigate_forward();
            assert_eq!(pane.active_item().map(|item| item.id), Some(3));
            pane.forget_item(2);
            assert!(!pane.backward_items.contains(&2));
            assert!(!pane.forward_items.contains(&2));
        });
    }

    #[gpui::test]
    fn results_layout_is_owned_by_each_query_tab(cx: &mut TestAppContext) {
        let mut state = PresentationState::default();
        state.workspace.panes[0].items.push(ItemPresentation {
            id: 2,
            kind: ItemKind::Query,
            title: "second.sql".into(),
            dirty: false,
            source: None,
        });
        let window = shell_with_state(state, cx);
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        let workspace = window.root(&mut cx).unwrap();
        let pane = workspace.read_with(&cx, |shell, _| shell.panes[0].clone());
        let result = pane.read_with(&cx, |pane, _| pane.results.get(&1).unwrap().clone());

        result.update(&mut cx, ResultsView::toggle_placement);
        pane.read_with(&cx, |pane, cx| {
            assert_eq!(
                pane.results.get(&1).unwrap().read(cx).placement(),
                ResultPlacement::Right
            );
            assert_eq!(
                pane.results.get(&2).unwrap().read(cx).placement(),
                ResultPlacement::Bottom
            );
        });
    }

    #[gpui::test]
    fn split_and_focus_actions_route_to_the_workspace(cx: &mut TestAppContext) {
        let window = shell(cx);
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        let workspace = window.root(&mut cx).unwrap();
        let focus = workspace.read_with(&cx, |shell, cx| shell.focus_handle(cx));
        cx.update(|window, cx| focus.dispatch_action(&SplitPane, window, cx));
        assert_eq!(
            workspace.read_with(&cx, |workspace, _| workspace.pane_count()),
            2
        );
        assert_eq!(
            workspace.read_with(&cx, |workspace, _| workspace.active_pane()),
            1
        );
        let focus = workspace.read_with(&cx, |shell, cx| shell.focus_handle(cx));
        cx.update(|window, cx| focus.dispatch_action(&FocusNextPane, window, cx));
        assert_eq!(
            workspace.read_with(&cx, |workspace, _| workspace.active_pane()),
            0
        );
    }

    #[gpui::test]
    fn splitting_an_empty_pane_does_not_create_an_asymmetric_tab(cx: &mut TestAppContext) {
        let window = shell(cx);
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        let workspace = window.root(&mut cx).unwrap();
        let focus = workspace.read_with(&cx, |shell, cx| shell.focus_handle(cx));

        cx.update(|window, cx| focus.dispatch_action(&CloseActiveItem, window, cx));
        cx.update(|window, cx| focus.dispatch_action(&SplitPane, window, cx));

        let item_counts = workspace.read_with(&cx, |workspace, cx| {
            workspace
                .panes
                .iter()
                .map(|pane| pane.read(cx).items.len())
                .collect::<Vec<_>>()
        });
        assert_eq!(item_counts, vec![0, 0]);
    }

    #[gpui::test]
    fn opening_a_tab_replaces_the_new_pane_placeholder(cx: &mut TestAppContext) {
        let pane = cx.update(|cx| {
            cx.new(|cx| {
                Pane::from_presentation(
                    PanePresentation {
                        id: 1,
                        items: vec![ItemPresentation {
                            id: 1,
                            kind: ItemKind::Welcome,
                            title: "New pane".into(),
                            dirty: false,
                            source: None,
                        }],
                        active_item: 0,
                    },
                    false,
                    cx,
                )
            })
        });
        let (editor, results) = cx.update(|cx| {
            (
                cx.new(|cx| QueryEditor::new(QueryDocument::with_random_peer("select 1"), cx)),
                cx.new(ResultsView::new),
            )
        });

        pane.update(cx, |pane, cx| {
            pane.open_query(
                ItemPresentation {
                    id: 2,
                    kind: ItemKind::Query,
                    title: "Query 1".into(),
                    dirty: false,
                    source: None,
                },
                editor,
                results,
                cx,
            );
        });

        pane.read_with(cx, |pane, _| {
            assert_eq!(pane.items.len(), 1);
            assert_eq!(pane.items[0].title, "Query 1");
            assert_eq!(pane.active_item, 0);
            assert!(pane.backward_items.is_empty());
        });
    }

    #[gpui::test]
    fn closing_the_last_item_collapses_an_extra_pane(cx: &mut TestAppContext) {
        let window = shell(cx);
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        let workspace = window.root(&mut cx).unwrap();
        let focus = workspace.read_with(&cx, |shell, cx| shell.focus_handle(cx));
        cx.update(|window, cx| focus.dispatch_action(&SplitPane, window, cx));
        assert_eq!(
            workspace.read_with(&cx, |workspace, _| workspace.pane_count()),
            2
        );
        // The new pane is active with a single clean item; closing it must
        // remove the whole pane rather than leave an un-closeable ghost.
        cx.update(|window, cx| focus.dispatch_action(&CloseActiveItem, window, cx));
        assert_eq!(
            workspace.read_with(&cx, |workspace, _| workspace.pane_count()),
            1
        );
        assert_eq!(
            workspace.read_with(&cx, |workspace, _| workspace.active_pane()),
            0
        );
    }

    #[gpui::test]
    fn tabs_own_close_controls_and_empty_pane_hides_its_tab_bar(cx: &mut TestAppContext) {
        let window = shell(cx);
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        let workspace = window.root(&mut cx).unwrap();
        cx.run_until_parked();
        assert!(cx.debug_bounds("pane-close").is_none());
        assert!(cx.debug_bounds("pane-tab-bar").is_some());

        let focus = workspace.read_with(&cx, |shell, cx| shell.focus_handle(cx));
        cx.update(|window, cx| focus.dispatch_action(&CloseActiveItem, window, cx));
        cx.run_until_parked();

        assert_eq!(
            workspace.read_with(&cx, |workspace, cx| workspace.active_item_count(cx)),
            0
        );
        assert!(cx.debug_bounds("pane-close").is_none());
        assert!(cx.debug_bounds("pane-tab-bar").is_none());
    }

    #[gpui::test]
    fn close_active_pane_never_removes_the_final_pane(cx: &mut TestAppContext) {
        let window = shell(cx);
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        let workspace = window.root(&mut cx).unwrap();
        let focus = workspace.read_with(&cx, |shell, cx| shell.focus_handle(cx));
        cx.update(|window, cx| focus.dispatch_action(&SplitPane, window, cx));
        cx.update(|window, cx| focus.dispatch_action(&SplitPane, window, cx));
        assert_eq!(
            workspace.read_with(&cx, |workspace, _| workspace.pane_count()),
            3
        );
        for _ in 0..5 {
            cx.update(|window, cx| focus.dispatch_action(&CloseActivePane, window, cx));
        }
        assert_eq!(
            workspace.read_with(&cx, |workspace, _| workspace.pane_count()),
            1
        );
        assert_eq!(
            workspace.read_with(&cx, |workspace, _| workspace.active_pane()),
            0
        );
    }

    #[gpui::test]
    fn dirty_item_close_is_inline_and_clean_item_closes_immediately(cx: &mut TestAppContext) {
        let window = shell(cx);
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        let workspace = window.root(&mut cx).unwrap();
        workspace.update(&mut cx, |workspace, cx| {
            workspace.mark_active_item_dirty(true, cx)
        });
        let focus = workspace.read_with(&cx, |shell, cx| shell.focus_handle(cx));
        cx.update(|window, cx| focus.dispatch_action(&CloseActiveItem, window, cx));
        assert!(workspace.read_with(&cx, |workspace, _| workspace.modal().is_none()));
        assert_eq!(
            workspace.read_with(&cx, |workspace, cx| workspace.panes[workspace.active_pane]
                .read(cx)
                .pending_close_item),
            Some(1)
        );
        workspace.update(&mut cx, |workspace, cx| {
            workspace.mark_active_item_dirty(false, cx)
        });
        cx.update(|window, cx| focus.dispatch_action(&CloseActiveItem, window, cx));
        assert_eq!(
            workspace.read_with(&cx, |workspace, cx| workspace.active_item_count(cx)),
            0
        );
        assert!(workspace.read_with(&cx, |workspace, _| workspace.modal().is_none()));
    }

    #[gpui::test]
    fn command_palette_uses_typed_action_routing(cx: &mut TestAppContext) {
        let window = shell(cx);
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        let workspace = window.root(&mut cx).unwrap();
        let focus = workspace.read_with(&cx, |shell, cx| shell.focus_handle(cx));
        cx.update(|window, cx| focus.dispatch_action(&OpenCommandPalette, window, cx));
        assert_eq!(
            workspace.read_with(&cx, |workspace, _| workspace.modal().cloned()),
            Some(Modal::CommandPalette)
        );
    }

    #[gpui::test]
    fn vim_colon_opens_workspace_command_palette(cx: &mut TestAppContext) {
        let window = shell(cx);
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        let workspace = window.root(&mut cx).unwrap();
        let editor = workspace.read_with(&cx, |workspace, cx| {
            let pane = workspace.panes[workspace.active_pane].read(cx);
            let item_id = pane.active_item().expect("active query item").id;
            pane.editor(item_id).expect("active query editor")
        });

        editor.update_in(&mut cx, |editor, window, cx| {
            editor.toggle_keymap(cx);
            editor.replace_text_in_range(None, ":", window, cx);
        });

        assert_eq!(
            workspace.read_with(&cx, |workspace, _| workspace.modal().cloned()),
            Some(Modal::CommandPalette)
        );
        editor.read_with(&cx, |editor, _| {
            assert_eq!(editor.vim_mode(), VimMode::Normal);
            assert_eq!(editor.document().text(), "");
        });
    }

    #[gpui::test]
    fn settings_controls_the_default_keymap_for_new_editors(cx: &mut TestAppContext) {
        let mut state = PresentationState::default();
        state.workspace.panes[0].items[0].kind = ItemKind::Configuration;
        let settings = UserSettings {
            editor: crate::settings::EditorSettings {
                default_mode: EditorMode::Vim,
            },
            ..UserSettings::default()
        };
        let window = cx.update(|cx| {
            cx.open_window(Default::default(), |window, cx| {
                cx.new(|cx| WorkspaceShell::new(state, settings, None, None, window, cx))
            })
            .unwrap()
        });
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        let workspace = window.root(&mut cx).unwrap();

        let editor = workspace.read_with(&cx, |workspace, cx| {
            let pane = workspace.panes[workspace.active_pane].read(cx);
            pane.editor(pane.active_item().unwrap().id).unwrap()
        });
        editor.read_with(&cx, |editor, _| {
            assert_eq!(editor.keymap(), EditorKeymap::Vim);
            assert_eq!(editor.vim_mode(), VimMode::Normal);
        });

        workspace.update_in(&mut cx, |workspace, window, cx| {
            workspace.run_command(CommandId::OpenSettings, window, cx);
        });
        cx.run_until_parked();
        assert!(
            cx.debug_bounds("open-settings-file").is_some(),
            "centered settings modal must expose manual settings.toml editing"
        );
        workspace.update(&mut cx, |workspace, cx| {
            workspace.toggle_vim_mode_default(cx)
        });
        workspace.read_with(&cx, |workspace, cx| {
            assert_eq!(workspace.modal(), Some(&Modal::Settings));
            assert!(!workspace.vim_mode_default());
            assert!(!workspace.snapshot(cx).legacy_vim_mode_default);
        });
    }

    #[gpui::test]
    fn settings_toml_opens_once_and_validates_before_save(cx: &mut TestAppContext) {
        let directory = tempfile::tempdir().unwrap();
        let settings_store = Arc::new(SettingsStore::new(directory.path().join("settings.toml")));
        settings_store.save(&UserSettings::default()).unwrap();
        let store_for_window = settings_store.clone();
        let window = cx.update(|cx| {
            cx.open_window(Default::default(), |window, cx| {
                cx.new(|cx| {
                    WorkspaceShell::new(
                        PresentationState::default(),
                        UserSettings::default(),
                        None,
                        Some(store_for_window),
                        window,
                        cx,
                    )
                })
            })
            .unwrap()
        });
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        let workspace = window.root(&mut cx).unwrap();

        workspace.update_in(&mut cx, |workspace, window, cx| {
            workspace.open_user_settings(window, cx);
            workspace.open_user_settings(window, cx);
        });
        let (item_id, editor) = workspace.read_with(&cx, |workspace, cx| {
            let pane = workspace.panes[workspace.active_pane].read(cx);
            assert_eq!(
                pane.items
                    .iter()
                    .filter(|item| item.title == "settings.toml")
                    .count(),
                1
            );
            let item = pane.active_item().unwrap();
            (item.id, pane.editor(item.id).unwrap())
        });
        let updated = "version = 1\n\n[editor]\ndefault_mode = \"vim\"\n";
        editor.update_in(&mut cx, |editor, window, cx| {
            let end = editor.document().text().encode_utf16().count();
            editor.replace_text_in_range(Some(0..end), updated, window, cx);
        });
        workspace.update_in(&mut cx, |workspace, window, cx| {
            workspace.save_active_item(&SaveActiveItem, window, cx)
        });

        assert_eq!(
            settings_store.load().unwrap().editor.default_mode,
            EditorMode::Vim
        );
        workspace.read_with(&cx, |workspace, cx| {
            assert!(workspace.vim_mode_default());
            let pane = workspace.panes[workspace.active_pane].read(cx);
            assert!(
                !pane
                    .items
                    .iter()
                    .find(|item| item.id == item_id)
                    .unwrap()
                    .dirty
            );
        });
    }

    #[gpui::test]
    fn app_bar_menus_toggle_without_opening_product_modals(cx: &mut TestAppContext) {
        let window = shell(cx);
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        let workspace = window.root(&mut cx).unwrap();
        assert!(!workspace.read_with(&cx, |shell, _| shell.app_bar_navigation_expanded()));

        workspace.update(&mut cx, |shell, cx| shell.toggle_app_bar_navigation(cx));
        workspace.update(&mut cx, |shell, cx| {
            shell.toggle_app_bar_menu(AppBarMenu::File, cx)
        });
        assert_eq!(
            workspace.read_with(&cx, |shell, _| shell.app_bar_menu),
            Some(AppBarMenu::File)
        );
        assert!(workspace.read_with(&cx, |shell, _| shell.app_bar_navigation_expanded()));
        assert!(workspace.read_with(&cx, |shell, _| shell.modal().is_none()));

        workspace.update(&mut cx, |shell, cx| {
            shell.toggle_app_bar_menu(AppBarMenu::File, cx)
        });
        assert_eq!(
            workspace.read_with(&cx, |shell, _| shell.app_bar_menu),
            None
        );
        assert!(workspace.read_with(&cx, |shell, _| shell.app_bar_navigation_expanded()));

        workspace.update(&mut cx, |shell, cx| {
            shell.toggle_app_bar_menu(AppBarMenu::Help, cx)
        });
        cx.simulate_mouse_down(
            point(px(10.), px(500.)),
            MouseButton::Left,
            Modifiers::default(),
        );
        assert_eq!(
            workspace.read_with(&cx, |shell, _| shell.app_bar_menu),
            None
        );
        assert!(!workspace.read_with(&cx, |shell, _| shell.app_bar_navigation_expanded()));
    }

    #[gpui::test]
    fn app_bar_launcher_is_a_strict_two_state_switch(cx: &mut TestAppContext) {
        let window = shell(cx);
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        let workspace = window.root(&mut cx).unwrap();

        workspace.update(&mut cx, |shell, cx| shell.toggle_app_bar_navigation(cx));
        assert_eq!(
            workspace.read_with(&cx, |shell, _| shell.app_bar_menu),
            Some(AppBarMenu::Main)
        );

        workspace.update(&mut cx, |shell, cx| {
            shell.toggle_app_bar_menu(AppBarMenu::File, cx)
        });
        workspace.update(&mut cx, |shell, cx| {
            shell.toggle_app_bar_menu(AppBarMenu::File, cx)
        });
        assert!(workspace.read_with(&cx, |shell, _| shell.app_bar_navigation_expanded()));
        assert_eq!(
            workspace.read_with(&cx, |shell, _| shell.app_bar_menu),
            None
        );

        workspace.update(&mut cx, |shell, cx| shell.toggle_app_bar_navigation(cx));
        assert_eq!(
            workspace.read_with(&cx, |shell, _| shell.app_bar_menu),
            None
        );

        workspace.update(&mut cx, |shell, cx| shell.toggle_app_bar_navigation(cx));
        assert_eq!(
            workspace.read_with(&cx, |shell, _| shell.app_bar_menu),
            Some(AppBarMenu::Main)
        );
        workspace.update(&mut cx, |shell, cx| shell.toggle_app_bar_navigation(cx));
        assert_eq!(
            workspace.read_with(&cx, |shell, _| shell.app_bar_menu),
            None
        );
    }

    #[gpui::test]
    fn expanded_app_bar_switches_menus_on_hover(cx: &mut TestAppContext) {
        let window = shell(cx);
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        let workspace = window.root(&mut cx).unwrap();

        workspace.update(&mut cx, |shell, cx| {
            shell.hover_app_bar_menu(AppBarMenu::File, true, cx)
        });
        assert_eq!(
            workspace.read_with(&cx, |shell, _| shell.app_bar_menu),
            None
        );

        workspace.update(&mut cx, |shell, cx| shell.toggle_app_bar_navigation(cx));
        workspace.update(&mut cx, |shell, cx| {
            shell.hover_app_bar_menu(AppBarMenu::File, true, cx)
        });
        assert_eq!(
            workspace.read_with(&cx, |shell, _| shell.app_bar_menu),
            Some(AppBarMenu::File)
        );

        workspace.update(&mut cx, |shell, cx| {
            shell.hover_app_bar_menu(AppBarMenu::File, false, cx);
            shell.hover_app_bar_menu(AppBarMenu::Edit, true, cx);
        });
        assert_eq!(
            workspace.read_with(&cx, |shell, _| shell.app_bar_menu),
            Some(AppBarMenu::Edit)
        );

        workspace.update(&mut cx, |shell, cx| {
            shell.open_app_bar_modal(Modal::Account, cx);
            shell.hover_app_bar_menu(AppBarMenu::Help, true, cx);
        });
        assert!(workspace.read_with(&cx, |shell, _| shell.modal().is_none()));
        assert_eq!(
            workspace.read_with(&cx, |shell, _| shell.app_bar_menu),
            Some(AppBarMenu::Help)
        );

        workspace.update(&mut cx, |shell, cx| shell.dismiss_app_bar_overlays(cx));
        assert!(!workspace.read_with(&cx, |shell, _| shell.app_bar_navigation_expanded()));
        assert_eq!(
            workspace.read_with(&cx, |shell, _| shell.app_bar_menu),
            None
        );
    }

    #[gpui::test]
    fn app_bar_overlays_are_mutually_exclusive(cx: &mut TestAppContext) {
        let window = shell(cx);
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        let workspace = window.root(&mut cx).unwrap();

        workspace.update(&mut cx, |shell, cx| {
            shell.open_app_bar_modal(Modal::Account, cx)
        });
        assert_eq!(
            workspace.read_with(&cx, |shell, _| shell.modal().cloned()),
            Some(Modal::Account)
        );

        workspace.update(&mut cx, |shell, cx| {
            shell.toggle_app_bar_menu(AppBarMenu::Profile, cx)
        });
        assert!(workspace.read_with(&cx, |shell, _| shell.modal().is_none()));
        assert_eq!(
            workspace.read_with(&cx, |shell, _| shell.app_bar_menu),
            Some(AppBarMenu::Profile)
        );

        workspace.update(&mut cx, |shell, cx| {
            shell.open_app_bar_modal(Modal::ServerPicker, cx)
        });
        assert_eq!(
            workspace.read_with(&cx, |shell, _| shell.modal().cloned()),
            Some(Modal::ServerPicker)
        );
        assert_eq!(
            workspace.read_with(&cx, |shell, _| shell.app_bar_menu),
            None
        );

        workspace.update(&mut cx, |shell, cx| shell.toggle_app_bar_navigation(cx));
        assert!(workspace.read_with(&cx, |shell, _| shell.modal().is_none()));
        assert_eq!(
            workspace.read_with(&cx, |shell, _| shell.app_bar_menu),
            Some(AppBarMenu::Main)
        );
    }

    #[test]
    fn app_bar_scaffold_marks_unimplemented_entries() {
        let main = app_bar::menu_items(AppBarMenu::Main);
        assert_eq!(
            main.iter().map(|item| item.label).collect::<Vec<_>>(),
            vec!["About Sift", "Check for Updates…", "Quit Sift"]
        );
        assert!(main[..2].iter().all(|item| item.command.is_none()));
        assert_eq!(main[2].command, Some(CommandId::Quit));

        let profile = app_bar::menu_items(AppBarMenu::Profile);
        assert_eq!(
            profile.iter().map(|item| item.label).collect::<Vec<_>>(),
            vec![
                "Settings",
                "Keymaps",
                "Toggle Light/Dark Theme",
                "Server Configuration"
            ]
        );
        assert_eq!(profile[0].command, Some(CommandId::OpenSettings));
        assert_eq!(profile[2].command, Some(CommandId::ToggleTheme));
        assert!(profile[1].command.is_none());
        assert!(profile[3].command.is_none());

        for menu in [
            AppBarMenu::Main,
            AppBarMenu::File,
            AppBarMenu::Edit,
            AppBarMenu::Selection,
            AppBarMenu::View,
            AppBarMenu::Go,
            AppBarMenu::Run,
            AppBarMenu::Window,
            AppBarMenu::Help,
        ] {
            assert!(!app_bar::menu_items(menu).is_empty());
        }
    }

    #[gpui::test]
    fn dismissing_the_palette_restores_editor_focus(cx: &mut TestAppContext) {
        let window = shell(cx);
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        let workspace = window.root(&mut cx).unwrap();
        let focus = workspace.read_with(&cx, |shell, cx| shell.focus_handle(cx));
        // Opening the palette moves focus to its search field…
        cx.update(|window, cx| focus.dispatch_action(&OpenCommandPalette, window, cx));
        // …and dismissing it must return focus to the query editor, or all
        // keybindings silently stop routing.
        cx.update(|window, cx| focus.dispatch_action(&DismissModal, window, cx));
        let focused = cx.update(|window, cx| workspace.read(cx).active_editor_focused(window, cx));
        assert!(focused);
    }

    #[gpui::test]
    fn clicking_results_releases_editor_focus_and_routes_grid_navigation(cx: &mut TestAppContext) {
        let window = shell(cx);
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        let workspace = window.root(&mut cx).unwrap();
        workspace.update(&mut cx, |shell, cx| {
            shell.route_result(
                1,
                ResultState::Ready(crate::results::ResultData {
                    columns: vec![
                        crate::results::ResultColumn {
                            name: "id".into(),
                            type_label: "int64".into(),
                            nullable: false,
                        },
                        crate::results::ResultColumn {
                            name: "name".into(),
                            type_label: "text".into(),
                            nullable: false,
                        },
                    ],
                    rows: vec![sift_protocol::Row::new(vec![
                        sift_protocol::Value::Int64(1),
                        sift_protocol::Value::Text("Ada".into()),
                    ])],
                    ..Default::default()
                }),
                cx,
            );
        });
        cx.run_until_parked();

        let row = cx.debug_bounds("result-row-0").expect("visible result row");
        cx.simulate_mouse_down(
            point(
                row.left() + px(crate::results::ROW_NUMBER_WIDTH + 12.0),
                row.top() + px(crate::results::ROW_HEIGHT / 2.0),
            ),
            MouseButton::Left,
            Modifiers::default(),
        );

        assert!(cx.update(|window, cx| workspace.read(cx).active_results_focused(window, cx)));
        assert!(!cx.update(|window, cx| workspace.read(cx).active_editor_focused(window, cx)));
        let results = workspace.read_with(&cx, |shell, cx| {
            let pane = shell.panes[shell.active_pane].read(cx);
            pane.results.get(&1).unwrap().clone()
        });
        let result_focus = results.read_with(&cx, |results, cx| results.focus_handle(cx));
        cx.update(|window, cx| {
            result_focus.dispatch_action(&crate::results::MoveCellRight, window, cx)
        });
        assert_eq!(
            results.read_with(&cx, |results, _| results.selected_cell()),
            Some((0, 1))
        );
    }

    #[gpui::test]
    fn pane_tabs_drag_reorder_within_a_pane(cx: &mut TestAppContext) {
        let mut state = PresentationState::default();
        state.workspace.panes[0].items.extend([
            ItemPresentation {
                id: 2,
                kind: ItemKind::Query,
                title: "two.sql".into(),
                dirty: false,
                source: None,
            },
            ItemPresentation {
                id: 3,
                kind: ItemKind::Query,
                title: "three.sql".into(),
                dirty: false,
                source: None,
            },
        ]);
        let window = shell_with_state(state, cx);
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        let workspace = window.root(&mut cx).unwrap();
        cx.run_until_parked();

        let source = cx.debug_bounds("tab-1").expect("first tab");
        let target = cx.debug_bounds("tab-3").expect("last tab");
        let start = source.center();
        cx.simulate_mouse_down(start, MouseButton::Left, Modifiers::default());
        // Move past the drag threshold, then over the last tab's right half.
        cx.simulate_mouse_move(
            point(start.x + px(6.), start.y),
            MouseButton::Left,
            Modifiers::default(),
        );
        let drop = point(target.right() - px(6.), target.center().y);
        cx.simulate_mouse_move(drop, MouseButton::Left, Modifiers::default());
        cx.simulate_mouse_up(drop, MouseButton::Left, Modifiers::default());
        cx.run_until_parked();

        workspace.read_with(&cx, |shell, cx| {
            let pane = shell.panes[0].read(cx);
            let order = pane.items.iter().map(|item| item.id).collect::<Vec<_>>();
            assert_eq!(order, vec![2, 3, 1], "tab must drop after the last tab");
            assert_eq!(pane.active_item, 2, "the dragged tab becomes active");
        });
    }

    #[gpui::test]
    fn pane_tabs_drag_moves_items_across_panes(cx: &mut TestAppContext) {
        let mut state = PresentationState::default();
        state.workspace.panes[0].items.push(ItemPresentation {
            id: 2,
            kind: ItemKind::Query,
            title: "two.sql".into(),
            dirty: false,
            source: None,
        });
        let window = shell_with_state(state, cx);
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        let workspace = window.root(&mut cx).unwrap();
        let focus = workspace.read_with(&cx, |shell, cx| shell.focus_handle(cx));
        cx.update(|window, cx| focus.dispatch_action(&SplitPane, window, cx));
        cx.run_until_parked();

        let source = cx.debug_bounds("tab-1").expect("first tab in pane 0");
        let target = cx.debug_bounds("pane-slot-1").expect("second pane slot");
        let start = source.center();
        cx.simulate_mouse_down(start, MouseButton::Left, Modifiers::default());
        let drop = target.center();
        cx.simulate_mouse_move(
            point(start.x + px(6.), start.y + px(20.)),
            MouseButton::Left,
            Modifiers::default(),
        );
        cx.simulate_mouse_move(drop, MouseButton::Left, Modifiers::default());
        cx.simulate_mouse_up(drop, MouseButton::Left, Modifiers::default());
        cx.run_until_parked();

        workspace.read_with(&cx, |shell, cx| {
            assert!(!shell.panes[0].read(cx).contains_item(1));
            let target_pane = shell.panes[1].read(cx);
            assert!(target_pane.contains_item(1));
            assert_eq!(
                target_pane.active_item().map(|item| item.id),
                Some(1),
                "the moved tab activates in its new pane"
            );
            assert!(target_pane.editor(1).is_some());
            assert!(target_pane.results.contains_key(&1));
            assert_eq!(shell.active_pane, 1);
            // The moved editor left its originating pane entirely.
            assert!(shell.panes[0].read(cx).editor(1).is_none());
        });
    }

    #[gpui::test]
    fn palette_keyboard_nav_selects_and_runs(cx: &mut TestAppContext) {
        let window = shell(cx);
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        let workspace = window.root(&mut cx).unwrap();
        let focus = workspace.read_with(&cx, |shell, cx| shell.focus_handle(cx));
        cx.update(|window, cx| focus.dispatch_action(&OpenCommandPalette, window, cx));
        assert!(workspace.read_with(&cx, |shell, _| shell.left_dock.presentation.open));
        // Arrow down to "Toggle Connections Dock" (index 10) and run it.
        // This crosses the virtual list's ten-row viewport, exercising the
        // scroll-to-selection path as well as command dispatch.
        for _ in 0..10 {
            cx.update(|window, cx| focus.dispatch_action(&PaletteDown, window, cx));
        }
        cx.update(|window, cx| focus.dispatch_action(&PaletteConfirm, window, cx));
        assert!(!workspace.read_with(&cx, |shell, _| shell.left_dock.presentation.open));
        assert!(workspace.read_with(&cx, |shell, _| shell.modal().is_none()));
    }

    #[gpui::test]
    fn command_palette_registry_exceeds_the_lazy_viewport(cx: &mut TestAppContext) {
        let window = shell(cx);
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        let workspace = window.root(&mut cx).unwrap();
        let commands = workspace.read_with(&cx, |shell, cx| shell.command_specs(cx));
        assert!(commands.len() > PALETTE_VISIBLE_ROWS);
        assert!(commands
            .iter()
            .any(|command| command.id == CommandId::ExecuteStatement));
        assert!(commands
            .iter()
            .any(|command| command.id == CommandId::ExecuteDocument));
    }

    #[gpui::test]
    fn executor_events_update_connection_status_and_database(cx: &mut TestAppContext) {
        let window = shell(cx);
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        let workspace = window.root(&mut cx).unwrap();
        workspace.update(&mut cx, |shell, cx| {
            shell.on_executor_event(
                ExecutorEvent::Connection(ConnectionStatus::Connected {
                    profile_id: 5,
                    name: "prod".into(),
                }),
                cx,
            );
        });
        assert_eq!(
            workspace.read_with(&cx, |shell, _| shell.connection_status().clone()),
            ConnectionStatus::Connected {
                profile_id: 5,
                name: "prod".into()
            }
        );
        assert_eq!(
            workspace.read_with(&cx, |shell, _| shell.status.database.clone()),
            "prod"
        );
        workspace.update(&mut cx, |shell, cx| {
            shell.on_executor_event(
                ExecutorEvent::Connection(ConnectionStatus::Disconnected),
                cx,
            );
        });
        assert_eq!(
            workspace.read_with(&cx, |shell, _| shell.status.database.clone()),
            "No database"
        );
    }

    #[gpui::test]
    fn account_actions_route_typed_authentication_commands(cx: &mut TestAppContext) {
        let window = shell(cx);
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        let workspace = window.root(&mut cx).unwrap();
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let (_event_sender, event_receiver) = tokio::sync::mpsc::unbounded_channel();
        workspace.update(&mut cx, |shell, cx| {
            shell.attach_instance_manager(sender, event_receiver, Vec::new(), cx);
            shell
                .account_username_input
                .update(cx, |input, cx| input.set_text("octocat", cx));
            shell
                .account_password_input
                .update(cx, |input, cx| input.set_text("correct horse", cx));
            shell.sign_in_with_password(cx);
        });
        match receiver.try_recv().unwrap() {
            InstanceCommand::SignInWithPassword { username, password } => {
                assert_eq!(username, "octocat");
                assert_eq!(password, "correct horse");
            }
            _ => panic!("expected password sign-in command"),
        }

        workspace.update(&mut cx, |shell, cx| {
            shell.account_pending = false;
            shell.sign_in_with_github(cx);
        });
        assert!(matches!(
            receiver.try_recv().unwrap(),
            InstanceCommand::SignInWithGithub
        ));

        workspace.update(&mut cx, |shell, cx| {
            shell.account_pending = false;
            shell.sign_out(true, cx);
        });
        assert!(matches!(
            receiver.try_recv().unwrap(),
            InstanceCommand::SignOut { everywhere: true }
        ));
    }

    #[gpui::test]
    fn account_rejects_empty_password_login_before_dispatch(cx: &mut TestAppContext) {
        let window = shell(cx);
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        let workspace = window.root(&mut cx).unwrap();
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let (_event_sender, event_receiver) = tokio::sync::mpsc::unbounded_channel();
        workspace.update(&mut cx, |shell, cx| {
            shell.attach_instance_manager(sender, event_receiver, Vec::new(), cx);
            shell.sign_in_with_password(cx);
        });
        assert!(receiver.try_recv().is_err());
        assert_eq!(
            workspace.read_with(&cx, |shell, _| shell.account_error.clone()),
            Some("Username and password are required".into())
        );
    }

    #[gpui::test]
    fn add_database_form_sends_profile_without_exposing_password(cx: &mut TestAppContext) {
        let window = shell(cx);
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        let workspace = window.root(&mut cx).unwrap();
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let (_event_sender, event_receiver) = tokio::sync::mpsc::unbounded_channel();
        workspace.update(&mut cx, |shell, cx| {
            shell.attach_executor(sender, event_receiver, cx);
            shell.selected_database_tenant = Some(7);
            shell.selected_database_provider = Some("sift/postgres".into());
            shell
                .database_name_input
                .update(cx, |input, cx| input.set_text("Reporting", cx));
            shell
                .database_host_input
                .update(cx, |input, cx| input.set_text("db.internal", cx));
            shell
                .database_port_input
                .update(cx, |input, cx| input.set_text("5432", cx));
            shell
                .database_catalog_input
                .update(cx, |input, cx| input.set_text("analytics", cx));
            shell
                .database_user_input
                .update(cx, |input, cx| input.set_text("sift", cx));
            shell
                .database_password_input
                .update(cx, |input, cx| input.set_text("top-secret", cx));
            shell
                .database_search_path_input
                .update(cx, |input, cx| input.set_text("public, reporting", cx));
            shell
                .database_application_name_input
                .update(cx, |input, cx| input.set_text("sift-desktop", cx));
            shell
                .database_timeout_input
                .update(cx, |input, cx| input.set_text("15", cx));
            shell
                .database_pool_min_input
                .update(cx, |input, cx| input.set_text("2", cx));
            shell
                .database_pool_max_input
                .update(cx, |input, cx| input.set_text("12", cx));
            shell.submit_database_connection(cx);
        });
        match receiver.try_recv().unwrap() {
            ExecutorCommand::CreateConnectionProfile {
                tenant_id,
                name,
                provider_id,
                configuration,
                credentials,
            } => {
                assert_eq!(tenant_id, 7);
                assert_eq!(name, "Reporting");
                assert_eq!(provider_id.as_str(), "sift/postgres");
                assert_eq!(configuration["port"], 5432);
                assert_eq!(configuration["ssl_mode"], "prefer");
                assert_eq!(
                    configuration["engine_specific"]["search_path"],
                    serde_json::json!(["public", "reporting"])
                );
                assert_eq!(
                    configuration["engine_specific"]["application_name"],
                    "sift-desktop"
                );
                assert_eq!(configuration["engine_specific"]["connect_timeout_secs"], 15);
                assert_eq!(configuration["engine_specific"]["pool_min_size"], 2);
                assert_eq!(configuration["engine_specific"]["pool_max_size"], 12);
                serde_json::from_value::<sift_protocol::ConnectionSpec>(configuration).unwrap();
                assert_eq!(credentials.unwrap()["password"], "top-secret");
            }
            _ => panic!("expected profile creation command"),
        }
    }

    #[gpui::test]
    fn database_wizard_stages_selection_details_and_review(cx: &mut TestAppContext) {
        let window = shell(cx);
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        let workspace = window.root(&mut cx).unwrap();

        workspace.update_in(&mut cx, |shell, window, cx| {
            shell.open_database_connection(window, cx);
            assert_eq!(shell.database_wizard_step, DatabaseWizardStep::Provider);
            assert!(shell.selected_database_provider.is_none());
            shell.select_database_provider("sift/postgres".into(), cx);
            assert_eq!(shell.database_port_input.read(cx).text(), "5432");
            assert_eq!(shell.selected_database_ssl_mode.as_deref(), Some("prefer"));
            shell.database_wizard_next(window, cx);
            assert_eq!(shell.database_wizard_step, DatabaseWizardStep::Details);
            shell.selected_database_tenant = Some(7);
            shell
                .database_name_input
                .update(cx, |input, cx| input.set_text("Reporting", cx));
            shell
                .database_user_input
                .update(cx, |input, cx| input.set_text("sift", cx));
            shell.database_wizard_next(window, cx);
            assert_eq!(shell.database_wizard_step, DatabaseWizardStep::Review);
        });
    }

    #[gpui::test]
    fn database_details_support_tab_and_shift_tab_focus_traversal(cx: &mut TestAppContext) {
        let window = shell(cx);
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        let workspace = window.root(&mut cx).unwrap();

        workspace.update_in(&mut cx, |shell, _, cx| {
            shell.modal = Some(Modal::DatabaseConnection);
            shell.database_wizard_step = DatabaseWizardStep::Details;
            shell.select_database_provider("sift/postgres".into(), cx);
            cx.notify();
        });
        cx.run_until_parked();

        let name_bounds = cx
            .debug_bounds("CONNECTION NAME")
            .expect("connection name field should be rendered");
        cx.simulate_click(name_bounds.center(), gpui::Modifiers::default());
        cx.simulate_input("Reporting");

        let name_focus =
            workspace.read_with(&cx, |shell, cx| shell.database_name_input.focus_handle(cx));
        workspace.update_in(&mut cx, |_, window, _| {
            assert!(
                name_focus.is_focused(window),
                "clicking the modal field should retain input focus"
            );
        });
        cx.update(|window, cx| name_focus.dispatch_action(&sift_ui::Tab, window, cx));
        workspace.update_in(&mut cx, |shell, window, cx| {
            assert!(
                shell
                    .database_catalog_input
                    .focus_handle(cx)
                    .is_focused(window),
                "Tab should move focus from connection name to database"
            );
        });
        cx.simulate_input("analytics");
        assert_eq!(
            workspace.read_with(&cx, |shell, cx| shell
                .database_catalog_input
                .read(cx)
                .text()
                .to_owned()),
            "analytics"
        );

        let catalog_focus = workspace.read_with(&cx, |shell, cx| {
            shell.database_catalog_input.focus_handle(cx)
        });
        cx.update(|window, cx| catalog_focus.dispatch_action(&sift_ui::Backtab, window, cx));
        assert_eq!(
            workspace.read_with(&cx, |shell, cx| shell
                .database_name_input
                .read(cx)
                .text()
                .to_owned()),
            "Reporting"
        );
    }

    #[gpui::test]
    fn sql_server_selection_sets_defaults_and_engine_security(cx: &mut TestAppContext) {
        let window = shell(cx);
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        let workspace = window.root(&mut cx).unwrap();
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let (_event_sender, event_receiver) = tokio::sync::mpsc::unbounded_channel();
        workspace.update(&mut cx, |shell, cx| {
            shell.attach_executor(sender, event_receiver, cx);
            shell.selected_database_tenant = Some(7);
            shell.select_database_provider("sift/sql-server".into(), cx);
            shell
                .database_name_input
                .update(cx, |input, cx| input.set_text("Warehouse", cx));
            shell
                .database_host_input
                .update(cx, |input, cx| input.set_text("sql.internal", cx));
            shell
                .database_user_input
                .update(cx, |input, cx| input.set_text("sift", cx));
            shell.selected_database_ssl_mode = Some("trust_server_certificate".into());
            shell.submit_database_connection(cx);
        });

        match receiver.try_recv().unwrap() {
            ExecutorCommand::CreateConnectionProfile {
                provider_id,
                configuration,
                ..
            } => {
                assert_eq!(provider_id.as_str(), "sift/sql-server");
                assert_eq!(configuration["port"], 1433);
                assert!(configuration.get("ssl_mode").is_none());
                assert_eq!(configuration["engine_specific"]["encrypt"], true);
                assert_eq!(configuration["engine_specific"]["engine"], "sql_server");
                assert_eq!(
                    configuration["engine_specific"]["trust_server_certificate"],
                    true
                );
                serde_json::from_value::<sift_protocol::ConnectionSpec>(configuration).unwrap();
            }
            _ => panic!("expected profile creation command"),
        }
    }

    #[gpui::test]
    fn bundled_local_is_a_no_op_but_configured_local_can_switch(cx: &mut TestAppContext) {
        let window = shell(cx);
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        let workspace = window.root(&mut cx).unwrap();
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let (_event_sender, event_receiver) = tokio::sync::mpsc::unbounded_channel();
        workspace.update(&mut cx, |shell, cx| {
            shell.attach_instance_manager(sender, event_receiver, Vec::new(), cx);
            shell
                .lifecycle
                .apply(LifecycleEvent::Selected(crate::InstanceSpec {
                    id: "local".into(),
                    name: "Local Sift".into(),
                    base_url: "http://127.0.0.1:7474".into(),
                    kind: crate::InstanceKind::Local,
                }));
            shell.use_local_server(cx);
            shell.show_toast("Connected to Local Sift".into(), cx);
        });
        assert!(receiver.try_recv().is_err());
        workspace.update(&mut cx, |shell, cx| {
            shell
                .lifecycle
                .apply(LifecycleEvent::Selected(crate::InstanceSpec {
                    id: "config:demo".into(),
                    name: "Demo".into(),
                    base_url: "auto-loopback".into(),
                    kind: crate::InstanceKind::Local,
                }));
            shell.use_local_server(cx);
        });
        assert!(matches!(receiver.try_recv(), Ok(InstanceCommand::UseLocal)));
        assert!(workspace.read_with(&cx, |shell, _| !shell.toasts.is_empty()));
        cx.background_executor
            .advance_clock(std::time::Duration::from_secs(5));
        cx.run_until_parked();
        assert!(workspace.read_with(&cx, |shell, _| shell.toasts.is_empty()));
    }

    #[gpui::test]
    fn instance_root_selection_dispatches_a_typed_inspection(cx: &mut TestAppContext) {
        let window = shell(cx);
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        let workspace = window.root(&mut cx).unwrap();
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let (_event_sender, event_receiver) = tokio::sync::mpsc::unbounded_channel();
        let root = std::path::PathBuf::from("/tmp/sift-instance");

        workspace.update(&mut cx, |shell, cx| {
            shell.attach_instance_manager(sender, event_receiver, Vec::new(), cx);
            shell.inspect_instance_root(root.clone(), cx);
        });

        match receiver.try_recv().unwrap() {
            InstanceCommand::InspectRoot { root: dispatched } => assert_eq!(dispatched, root),
            _ => panic!("expected instance-root inspection command"),
        }
    }

    #[gpui::test]
    fn configured_instance_picker_dispatches_direct_connection(cx: &mut TestAppContext) {
        let window = shell(cx);
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        let workspace = window.root(&mut cx).unwrap();
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let (_event_sender, event_receiver) = tokio::sync::mpsc::unbounded_channel();
        let root = std::path::PathBuf::from("/tmp/sift-instance");

        workspace.update(&mut cx, |shell, cx| {
            shell.attach_instance_manager(sender, event_receiver, Vec::new(), cx);
            shell.connect_instance_root(root.clone(), cx);
        });

        match receiver.try_recv().unwrap() {
            InstanceCommand::StartRoot { root: dispatched } => assert_eq!(dispatched, root),
            _ => panic!("expected direct instance connection command"),
        }
    }

    #[gpui::test]
    fn stacked_toasts_each_expire_three_seconds_after_creation(cx: &mut TestAppContext) {
        let window = shell(cx);
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        let workspace = window.root(&mut cx).unwrap();

        workspace.update(&mut cx, |shell, cx| shell.show_toast("First".into(), cx));
        cx.background_executor
            .advance_clock(std::time::Duration::from_secs(1));
        cx.run_until_parked();
        workspace.update(&mut cx, |shell, cx| shell.show_toast("Second".into(), cx));
        cx.background_executor
            .advance_clock(std::time::Duration::from_secs(2));
        cx.run_until_parked();
        workspace.read_with(&cx, |shell, _| {
            assert_eq!(
                shell
                    .toasts
                    .iter()
                    .map(|toast| toast.message.as_str())
                    .collect::<Vec<_>>(),
                vec!["Second"]
            );
        });
        cx.background_executor
            .advance_clock(std::time::Duration::from_secs(1));
        cx.run_until_parked();
        assert!(workspace.read_with(&cx, |shell, _| shell.toasts.is_empty()));
    }

    #[gpui::test]
    fn current_instance_configuration_opens_and_saves_through_typed_commands(
        cx: &mut TestAppContext,
    ) {
        let window = shell(cx);
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        let workspace = window.root(&mut cx).unwrap();
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let (_event_sender, event_receiver) = tokio::sync::mpsc::unbounded_channel();
        workspace.update(&mut cx, |shell, cx| {
            shell.attach_instance_manager(sender, event_receiver, Vec::new(), cx);
            shell.app_bar_expanded = true;
            shell.app_bar_menu = Some(AppBarMenu::Profile);
            shell.open_current_configuration(cx);
        });
        workspace.read_with(&cx, |shell, _| {
            assert!(!shell.app_bar_expanded);
            assert!(shell.app_bar_menu.is_none());
        });
        assert!(matches!(
            receiver.try_recv().unwrap(),
            InstanceCommand::OpenCurrentConfiguration
        ));

        workspace.update(&mut cx, |shell, cx| {
            shell.on_instance_manager_event(
                InstanceManagerEvent::InstanceConfiguration(Box::new(
                    InstanceConfigurationPresentation {
                        root: None,
                        manifest: "name = \"current\"\n".into(),
                        source_revision: Some("sha256:one".into()),
                        name: "current".into(),
                        is_new: false,
                    },
                )),
                cx,
            );
        });
        workspace.read_with(&cx, |shell, cx| {
            assert!(shell.modal().is_none());
            let item_id = shell.instance_configuration_item.unwrap();
            assert!(shell
                .panes
                .iter()
                .any(|pane| pane.read(cx).contains_item(item_id)));
        });
        let configuration_editor = workspace.read_with(&cx, |shell, cx| {
            let item_id = shell.instance_configuration_item.unwrap();
            shell.panes[shell.active_pane]
                .read(cx)
                .editor(item_id)
                .unwrap()
        });
        configuration_editor.update_in(&mut cx, |editor, window, cx| {
            editor.replace_text_in_range(None, "# changed\n", window, cx)
        });
        assert_eq!(
            workspace.read_with(&cx, |workspace, cx| workspace.active_item_dirty(cx)),
            Some(true)
        );
        let editor_focus =
            configuration_editor.read_with(&cx, |editor, cx| editor.focus_handle(cx));
        cx.update(|window, cx| editor_focus.dispatch_action(&crate::editor::Undo, window, cx));
        assert_eq!(
            workspace.read_with(&cx, |workspace, cx| workspace.active_item_dirty(cx)),
            Some(false),
            "undoing to the clean text must not prompt on close"
        );
        let focus = workspace.read_with(&cx, |shell, cx| shell.focus_handle(cx));
        cx.update(|window, cx| focus.dispatch_action(&SaveActiveItem, window, cx));
        match receiver.try_recv().unwrap() {
            InstanceCommand::SaveConfiguration {
                root,
                manifest,
                expected_source_revision,
                is_new,
            } => {
                assert!(root.is_none());
                assert_eq!(manifest, "name = \"current\"\n");
                assert_eq!(expected_source_revision.as_deref(), Some("sha256:one"));
                assert!(!is_new);
            }
            _ => panic!("expected current instance configuration save"),
        }
    }

    #[gpui::test]
    fn palette_command_dispatches_action_and_closes(cx: &mut TestAppContext) {
        let window = shell(cx);
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        let workspace = window.root(&mut cx).unwrap();
        assert!(workspace.read_with(&cx, |shell, _| shell.left_dock.presentation.open));
        workspace.update_in(&mut cx, |shell, window, cx| {
            shell.modal = Some(Modal::CommandPalette);
            shell.run_command(CommandId::ToggleLeftDock, window, cx);
        });
        assert!(!workspace.read_with(&cx, |shell, _| shell.left_dock.presentation.open));
        assert!(workspace.read_with(&cx, |shell, _| shell.modal().is_none()));
    }

    #[gpui::test]
    fn connect_dialog_sends_a_typed_secret_bearing_request(cx: &mut TestAppContext) {
        let window = shell(cx);
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        let workspace = window.root(&mut cx).unwrap();
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let (_event_sender, event_receiver) = tokio::sync::mpsc::unbounded_channel();
        workspace.update(&mut cx, |shell, cx| {
            shell.attach_instance_manager(sender, event_receiver, Vec::new(), cx);
            shell
                .server_name_input
                .update(cx, |input, cx| input.set_text("LAN", cx));
            shell.server_url_input.update(cx, |input, cx| {
                input.set_text("http://192.168.1.20:7474", cx)
            });
            shell
                .server_token_input
                .update(cx, |input, cx| input.set_text("secret", cx));
            shell.remember_server_token = false;
            shell.submit_server_connection(cx);
        });
        let command = receiver.try_recv().unwrap();
        match command {
            InstanceCommand::Connect {
                name,
                base_url,
                bearer_token,
                remember_token,
                ..
            } => {
                assert_eq!(name, "LAN");
                assert_eq!(base_url, "http://192.168.1.20:7474");
                assert_eq!(bearer_token.as_deref(), Some("secret"));
                assert!(!remember_token);
            }
            _ => panic!("expected connect command"),
        }
    }

    #[gpui::test]
    fn app_bar_server_picker_switches_using_the_saved_profile(cx: &mut TestAppContext) {
        let window = shell(cx);
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        let workspace = window.root(&mut cx).unwrap();
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let (_event_sender, event_receiver) = tokio::sync::mpsc::unbounded_channel();
        let profile = SavedServerProfile {
            id: "team".into(),
            name: "Team Sift".into(),
            base_url: "https://sift.example.test".into(),
            has_saved_token: true,
        };
        workspace.update(&mut cx, |shell, cx| {
            shell.attach_instance_manager(sender, event_receiver, vec![profile.clone()], cx);
            shell.open_app_bar_modal(Modal::ServerPicker, cx);
            shell.connect_saved_server(&profile, cx);
        });
        assert_eq!(
            workspace.read_with(&cx, |shell, _| shell.modal().cloned()),
            Some(Modal::ServerPicker)
        );
        match receiver.try_recv().unwrap() {
            InstanceCommand::Connect {
                profile_id,
                name,
                base_url,
                bearer_token,
                remember_token,
            } => {
                assert_eq!(profile_id.as_deref(), Some("team"));
                assert_eq!(name, "Team Sift");
                assert_eq!(base_url, "https://sift.example.test");
                assert!(bearer_token.is_none());
                assert!(remember_token);
            }
            _ => panic!("expected connect command"),
        }
    }

    #[gpui::test]
    fn app_bar_popovers_dismiss_on_mouse_down_outside(cx: &mut TestAppContext) {
        let window = shell(cx);
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        let workspace = window.root(&mut cx).unwrap();

        workspace.update(&mut cx, |shell, cx| {
            shell.open_app_bar_modal(Modal::ServerPicker, cx)
        });
        cx.simulate_mouse_down(
            point(px(10.), px(500.)),
            MouseButton::Left,
            Modifiers::default(),
        );
        assert!(workspace.read_with(&cx, |shell, _| shell.modal().is_none()));

        workspace.update(&mut cx, |shell, cx| {
            shell.open_app_bar_modal(Modal::Account, cx)
        });
        cx.simulate_mouse_down(
            point(px(10.), px(500.)),
            MouseButton::Left,
            Modifiers::default(),
        );
        assert!(workspace.read_with(&cx, |shell, _| shell.modal().is_none()));
    }

    #[gpui::test]
    fn app_bar_buttons_replace_an_open_popover_with_one_click(cx: &mut TestAppContext) {
        let window = shell(cx);
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        let workspace = window.root(&mut cx).unwrap();

        workspace.update(&mut cx, |shell, cx| {
            shell.open_app_bar_modal(Modal::ServerPicker, cx)
        });
        cx.run_until_parked();
        let account_bounds = cx
            .debug_bounds("toolbar-account")
            .expect("account button should be rendered");
        cx.simulate_click(account_bounds.center(), Modifiers::default());
        assert_eq!(
            workspace.read_with(&cx, |shell, _| shell.modal().cloned()),
            Some(Modal::Account)
        );

        cx.simulate_click(account_bounds.center(), Modifiers::default());
        assert!(workspace.read_with(&cx, |shell, _| shell.modal().is_none()));

        cx.simulate_click(account_bounds.center(), Modifiers::default());
        assert_eq!(
            workspace.read_with(&cx, |shell, _| shell.modal().cloned()),
            Some(Modal::Account)
        );

        let server_bounds = cx
            .debug_bounds("toolbar-server-picker")
            .expect("server picker button should be rendered");
        cx.simulate_click(server_bounds.center(), Modifiers::default());
        assert_eq!(
            workspace.read_with(&cx, |shell, _| shell.modal().cloned()),
            Some(Modal::ServerPicker)
        );
    }

    #[gpui::test]
    fn app_bar_uses_authenticated_identity_and_room_workspace_context(cx: &mut TestAppContext) {
        let window = shell(cx);
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        let workspace = window.root(&mut cx).unwrap();
        workspace.update(&mut cx, |shell, _| {
            shell.selected_workspace_id = Some(12);
            shell
                .lifecycle
                .apply(LifecycleEvent::Selected(crate::InstanceSpec {
                    id: "hosted:team".into(),
                    name: "Team Sift".into(),
                    base_url: "https://sift.example.test".into(),
                    kind: crate::InstanceKind::Hosted,
                }));
            shell.lifecycle.apply(LifecycleEvent::Authenticated(
                sift_protocol::WhoAmIResponse {
                    principal: sift_protocol::AuthPrincipal {
                        id: 7,
                        display_name: "Ada Lovelace".into(),
                        email: Some("ada@example.test".into()),
                        avatar_url: None,
                        is_instance_admin: false,
                    },
                    memberships: vec![sift_protocol::AuthTenantMembership {
                        tenant_id: 1,
                        tenant_name: "Analytical Engine".into(),
                        role: "owner".into(),
                    }],
                    github_login: Some("ada-lovelace".into()),
                    auth_session_id: Some("session".into()),
                },
            ));
            shell
                .lifecycle
                .apply(LifecycleEvent::TenantLoaded(crate::TenantNavEntry {
                    id: sift_api_types::TenantId(1),
                    name: "Analytical Engine".into(),
                    connections: Vec::new(),
                    rooms: vec![crate::RoomNavEntry {
                        id: RoomId(4),
                        tenant_id: sift_api_types::TenantId(1),
                        name: "Research".into(),
                        workspaces: vec![WorkspaceNavEntry {
                            id: 12,
                            room_id: 4,
                            name: "Reporting".into(),
                            git_enabled: false,
                            scheduling_enabled: false,
                        }],
                    }],
                }));
        });
        assert_eq!(
            workspace.read_with(&cx, |shell, _| shell.active_server_name()),
            "Team Sift"
        );
        assert_eq!(
            workspace.read_with(&cx, |shell, _| shell.workspace_context_label()),
            "Research / Reporting"
        );
        assert_eq!(
            workspace.read_with(&cx, |shell, _| shell.account_initials()),
            "AL"
        );
        workspace.update(&mut cx, |shell, cx| {
            shell.open_app_bar_modal(Modal::Account, cx)
        });
        cx.run_until_parked();
        assert!(cx.debug_bounds("account-github-profile").is_some());
    }

    #[gpui::test]
    fn stale_restored_workspace_is_cleared_after_authoritative_load(cx: &mut TestAppContext) {
        let mut state = PresentationState::default();
        state.workspace.workspace_id = Some(404);
        let window = shell_with_state(state, cx);
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        let workspace = window.root(&mut cx).unwrap();
        workspace.update(&mut cx, |workspace, cx| {
            workspace
                .lifecycle
                .apply(LifecycleEvent::Phase(crate::ConnectionPhase::Ready));
            workspace.reconcile_restored_workspace(cx);
        });
        assert_eq!(
            workspace.read_with(&cx, |workspace, cx| workspace
                .snapshot(cx)
                .workspace
                .workspace_id),
            None
        );
        assert_eq!(
            workspace.read_with(&cx, |workspace, _| {
                workspace.toasts.last().map(|toast| toast.message.clone())
            }),
            Some("Restored workspace is no longer available".into())
        );
    }

    #[gpui::test]
    fn opening_workspace_persists_reference_and_follow_validates_presence(cx: &mut TestAppContext) {
        let window = shell(cx);
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        let workspace = window.root(&mut cx).unwrap();
        workspace.update(&mut cx, |shell, cx| {
            shell.open_workspace(
                &WorkspaceNavEntry {
                    id: 12,
                    room_id: 7,
                    name: "Reporting".into(),
                    git_enabled: true,
                    scheduling_enabled: false,
                },
                cx,
            );
            shell.presence.apply(PresenceEvent::Joined {
                room_id: RoomId(7),
                attachment_id: 40,
                presence: vec![sift_protocol::RoomPresence {
                    attachment_id: 41,
                    principal_id: 3,
                    client_id: "peer".into(),
                    active_document_id: None,
                    selection: None,
                }],
            });
            assert!(shell.follow_participant(41, cx));
            assert!(!shell.follow_participant(999, cx));
        });
        assert_eq!(
            workspace.read_with(&cx, |workspace, cx| workspace
                .snapshot(cx)
                .workspace
                .workspace_id),
            Some(12)
        );
        assert_eq!(
            workspace.read_with(&cx, |workspace, _| workspace.presence.followed_attachment),
            Some(41)
        );
    }

    #[gpui::test]
    fn footer_panel_switches_are_exclusive_and_persisted(cx: &mut TestAppContext) {
        let window = shell(cx);
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        let workspace = window.root(&mut cx).unwrap();

        workspace.update(&mut cx, |shell, cx| {
            shell.select_left_panel(LeftPanel::Collaboration, cx);
            assert!(shell.left_dock.presentation.open);
            assert_eq!(shell.active_left_panel, LeftPanel::Collaboration);

            shell.select_left_panel(LeftPanel::QueryOutline, cx);
            assert!(shell.left_dock.presentation.open);
            assert_eq!(shell.active_left_panel, LeftPanel::QueryOutline);

            shell.select_left_panel(LeftPanel::QueryOutline, cx);
            assert!(!shell.left_dock.presentation.open);

            shell.select_bottom_tool(BottomTool::Monitor, cx);
            assert!(shell.bottom_dock.presentation.open);
            assert_eq!(shell.active_bottom_tool, BottomTool::Monitor);

            shell.select_bottom_tool(BottomTool::Automations, cx);
            assert!(shell.bottom_dock.presentation.open);
            assert_eq!(shell.active_bottom_tool, BottomTool::Automations);

            shell.select_bottom_tool(BottomTool::Automations, cx);
            assert!(!shell.bottom_dock.presentation.open);

            assert!(shell.right_dock.presentation.open);
            shell.close_inspector(cx);
            assert!(!shell.right_dock.presentation.open);

            let snapshot = shell.snapshot(cx);
            assert_eq!(snapshot.workspace.left_panel, LeftPanel::QueryOutline);
            assert_eq!(snapshot.workspace.bottom_tool, BottomTool::Automations);
            assert!(!snapshot.workspace.left_dock.open);
            assert!(!snapshot.workspace.bottom_dock.open);
            assert!(!snapshot.workspace.right_dock.open);
        });
    }

    #[gpui::test]
    fn bottom_dock_is_laid_out_between_the_side_docks(cx: &mut TestAppContext) {
        let window = shell(cx);
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        let workspace = window.root(&mut cx).unwrap();
        workspace.update(&mut cx, |shell, cx| {
            shell.bottom_dock.presentation.open = true;
            cx.notify();
        });
        cx.run_until_parked();

        let left = cx.debug_bounds("left-dock").expect("left dock");
        let right = cx.debug_bounds("right-dock").expect("right dock");
        let bottom = cx.debug_bounds("bottom-dock").expect("bottom dock");

        assert!(bottom.left() >= left.right());
        assert!(bottom.right() <= right.left());
        assert!(cx.debug_bounds("resize-left-dock").is_some());
        assert!(cx.debug_bounds("resize-right-dock").is_some());
        assert!(cx.debug_bounds("resize-bottom-dock").is_some());
    }

    #[gpui::test]
    fn shared_pane_edges_render_as_single_pixel_separators(cx: &mut TestAppContext) {
        let window = shell(cx);
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        let workspace = window.root(&mut cx).unwrap();
        let focus = workspace.read_with(&cx, |shell, cx| shell.focus_handle(cx));
        cx.update(|window, cx| focus.dispatch_action(&SplitPane, window, cx));
        workspace.update(&mut cx, |shell, cx| {
            shell.bottom_dock.presentation.open = true;
            cx.notify();
        });
        cx.run_until_parked();

        let pane_line = cx.debug_bounds("pane-separator-0").expect("pane separator");
        let pane_hitbox = cx
            .debug_bounds("resize-pane-0")
            .expect("pane resize hitbox");
        assert_eq!(pane_line.size.width, px(1.0));
        assert_eq!(pane_hitbox.size.width, px(PANE_RESIZE_HANDLE_SIZE));
        assert_eq!(
            pane_line.left() + pane_line.size.width / 2.0,
            pane_hitbox.left() + pane_hitbox.size.width / 2.0
        );

        let first_slot = cx.debug_bounds("pane-slot-0").expect("first pane slot");
        let second_slot = cx.debug_bounds("pane-slot-1").expect("second pane slot");
        assert!(first_slot.size.width > px(PANE_MIN_WIDTH));
        assert!(second_slot.size.width > px(PANE_MIN_WIDTH));
        assert_eq!(first_slot.right(), pane_line.left());
        assert_eq!(pane_line.right(), second_slot.left());

        let result_line = cx
            .debug_bounds("query-results-separator-1")
            .expect("result separator");
        let result_hitbox = cx
            .debug_bounds("resize-query-results-1")
            .expect("result resize hitbox");
        assert_eq!(result_line.size.height, px(1.0));
        assert_eq!(result_hitbox.size.height, px(RESULT_RESIZE_HANDLE_SIZE));
        assert_eq!(
            result_line.top() + result_line.size.height / 2.0,
            result_hitbox.top() + result_hitbox.size.height / 2.0
        );

        let titlebar = cx.debug_bounds("integrated-titlebar").expect("titlebar");
        let left_dock = cx.debug_bounds("left-dock").expect("left dock");
        let right_dock = cx.debug_bounds("right-dock").expect("right dock");
        let bottom_dock = cx.debug_bounds("bottom-dock").expect("bottom dock");
        let left_dock_hitbox = cx.debug_bounds("resize-left-dock").unwrap();
        let right_dock_hitbox = cx.debug_bounds("resize-right-dock").unwrap();
        let bottom_dock_hitbox = cx.debug_bounds("resize-bottom-dock").unwrap();
        let first_pane = cx.debug_bounds("pane-slot-0").expect("first pane");
        assert_eq!(titlebar.bottom(), left_dock.top());
        assert_eq!(left_dock.top(), first_pane.top());
        assert_eq!(
            left_dock.right() + px(0.5),
            left_dock_hitbox.left() + left_dock_hitbox.size.width / 2.0
        );
        assert_eq!(
            right_dock.left() - px(0.5),
            right_dock_hitbox.left() + right_dock_hitbox.size.width / 2.0
        );
        assert_eq!(
            bottom_dock.top() - px(0.5),
            bottom_dock_hitbox.top() + bottom_dock_hitbox.size.height / 2.0
        );
    }

    #[gpui::test]
    fn pane_resize_hitbox_drags_without_being_covered_by_a_sibling(cx: &mut TestAppContext) {
        let window = shell(cx);
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        let workspace = window.root(&mut cx).unwrap();
        let focus = workspace.read_with(&cx, |shell, cx| shell.focus_handle(cx));
        cx.update(|window, cx| focus.dispatch_action(&SplitPane, window, cx));
        cx.run_until_parked();

        let before = cx.debug_bounds("pane-slot-0").unwrap().size.width;
        let hitbox = cx.debug_bounds("resize-pane-0").unwrap();
        let start = point(
            hitbox.left() + px(2.0),
            hitbox.top() + hitbox.size.height / 2.0,
        );
        cx.simulate_mouse_down(start, MouseButton::Left, Modifiers::default());
        cx.simulate_mouse_move(
            point(start.x + px(8.0), start.y),
            MouseButton::Left,
            Modifiers::default(),
        );
        cx.simulate_mouse_move(
            point(start.x + px(80.0), start.y),
            MouseButton::Left,
            Modifiers::default(),
        );
        cx.simulate_mouse_up(
            point(start.x + px(80.0), start.y),
            MouseButton::Left,
            Modifiers::default(),
        );
        cx.run_until_parked();

        let after = cx.debug_bounds("pane-slot-0").unwrap().size.width;
        assert!(after > before + px(40.0));
    }

    #[gpui::test]
    fn result_resize_coalesces_pointer_updates_per_frame(cx: &mut TestAppContext) {
        let window = shell(cx);
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        let workspace = window.root(&mut cx).unwrap();
        let pane = workspace.read_with(&cx, |shell, _| shell.panes[0].clone());

        pane.update_in(&mut cx, |pane, window, cx| {
            // Existing bottom extent is 240, so sub-pixel movement is a no-op.
            pane.queue_result_resize(1, 240.4, window, cx);
            assert!(!pane.result_resize_frame_pending);

            pane.queue_result_resize(1, 280.1, window, cx);
            pane.queue_result_resize(1, 300.2, window, cx);
            assert!(pane.result_resize_frame_pending);
            assert_eq!(pane.live_result_extents.get(&1), Some(&300.0));
        });

        assert_eq!(cx.update(|window, cx| window.simulate_next_frame(cx)), 1);
        assert!(!pane.read_with(&cx, |pane, _| pane.result_resize_frame_pending));

        pane.update_in(&mut cx, |pane, window, cx| {
            pane.queue_result_resize(1, 300.4, window, cx);
            assert!(!pane.result_resize_frame_pending);
        });
        assert_eq!(cx.update(|window, cx| window.simulate_next_frame(cx)), 0);
    }

    #[gpui::test]
    fn workspace_resize_coalesces_pointer_updates_per_frame(cx: &mut TestAppContext) {
        let window = shell(cx);
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        let workspace = window.root(&mut cx).unwrap();

        workspace.update_in(&mut cx, |shell, window, cx| {
            shell.queue_workspace_resize(window, cx);
            shell.queue_workspace_resize(window, cx);
            assert!(shell.workspace_resize_frame_pending);
        });

        assert_eq!(cx.update(|window, cx| window.simulate_next_frame(cx)), 1);
        assert!(!workspace.read_with(&cx, |shell, _| shell.workspace_resize_frame_pending));
    }

    #[gpui::test]
    fn footer_copies_the_current_query_error(cx: &mut TestAppContext) {
        let window = shell(cx);
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        let workspace = window.root(&mut cx).unwrap();

        workspace.update(&mut cx, |shell, cx| {
            shell.route_result(1, ResultState::Failed("syntax error near FROM".into()), cx);
            assert_eq!(shell.status.diagnostic_count, 1);
            assert_eq!(
                shell.status.current_error.as_deref(),
                Some("syntax error near FROM")
            );
            shell.copy_current_error(cx);
            let copied = cx.read_from_clipboard().and_then(|item| item.text());
            assert_eq!(copied.as_deref(), Some("syntax error near FROM"));
            shell.route_result(
                1,
                ResultState::Ready(crate::results::ResultData::default()),
                cx,
            );
            assert_eq!(shell.status.execution, "Ready");
        });
    }

    #[gpui::test]
    fn problems_footer_opens_panel_and_copies_entries(cx: &mut TestAppContext) {
        let mut state = PresentationState::default();
        state.workspace.panes[0].items.push(ItemPresentation {
            id: 2,
            kind: ItemKind::Query,
            title: "report.sql".into(),
            dirty: false,
            source: None,
        });
        let window = shell_with_state(state, cx);
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        let workspace = window.root(&mut cx).unwrap();

        workspace.update(&mut cx, |shell, cx| {
            shell.route_result(1, ResultState::Failed("syntax error near FROM".into()), cx);
            shell.route_result(2, ResultState::Failed("column total does not exist".into()), cx);
            assert_eq!(shell.status.diagnostic_count, 2);
            assert_eq!(shell.sql_problems.len(), 2);

            shell.show_diagnostics(cx);
            assert!(shell.bottom_dock.presentation.open);
            assert_eq!(shell.active_bottom_tool, BottomTool::Problems);

            shell.copy_problem(0, cx);
            let copied = cx.read_from_clipboard().and_then(|item| item.text());
            assert_eq!(copied.as_deref(), Some("syntax error near FROM"));

            shell.copy_all_problems(cx);
            let copied = cx.read_from_clipboard().and_then(|item| item.text());
            assert_eq!(
                copied.as_deref(),
                Some(
                    "[Error] query.sql: syntax error near FROM\n[Error] report.sql: column total does not exist"
                )
            );
        });
        cx.run_until_parked();

        assert!(cx.debug_bounds("problem-row-0").is_some());
        assert!(cx.debug_bounds("problem-row-1").is_some());
        assert!(cx.debug_bounds("copy-all-problems").is_some());

        workspace.update(&mut cx, |shell, cx| {
            shell.route_result(
                1,
                ResultState::Ready(crate::results::ResultData::default()),
                cx,
            );
            assert_eq!(shell.status.diagnostic_count, 1);
            assert_eq!(shell.sql_problems[0].item_id, Some(2));
        });
    }
}
