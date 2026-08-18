use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::sync::Arc;

use gpui::{
    actions, deferred, div, img, prelude::*, px, uniform_list, App, Context, CursorStyle, Entity,
    EventEmitter, FocusHandle, Focusable, IntoElement, MouseButton, PathPromptOptions, ResizeEdge,
    Role, ScrollStrategy, Subscription, Task, UniformListScrollHandle, Window, WindowBounds,
    WindowControlArea,
};
use sift_api_types::RoomId;
use sift_ui::{database_logo, icon, IconName, TextInput, Theme};

use crate::editor::{
    EditorEvent, EditorKeymap, EditorLanguage, QueryDocument, QueryEditor, VimMode,
    EDITOR_GUTTER_WIDTH,
};
use crate::results::{ResultPlacement, ResultState, ResultsView};

use crate::presentation::{
    BottomTool, ItemKind, ItemPresentation, LeftPanel, PanePresentation, PresentationState,
    PresentationStore, WindowPresentation, WorkspacePresentation,
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
const RESULT_RESIZE_HANDLE_SIZE: f32 = 5.0;
const RESULT_MIN_EXTENT: f32 = 140.0;
const EDITOR_MIN_EXTENT: f32 = 160.0;

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
struct ResultResizeDrag {
    item_id: u64,
    placement: ResultPlacement,
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Toast {
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tooltip {
    pub message: String,
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
    /// Active editor state changed. Cursor-only changes do not dirty the tab.
    EditorStateChanged { item_id: u64, dirty: Option<bool> },
    /// An editor requested the workspace-level command palette.
    OpenCommandPaletteRequested,
    /// A query item asked to run SQL; the workspace dispatches it to execution.
    ExecuteRequested { item_id: u64, sql: String },
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
    theme: Theme,
    /// Live editor per SQL or configuration item. Editor contents are not
    /// persisted in the local layout; their owning service rehydrates them.
    editors: HashMap<u64, Entity<QueryEditor>>,
    /// Text at the last clean point for each editor. Comparing content rather
    /// than latching a boolean means undoing back to the clean text is clean.
    clean_documents: HashMap<u64, String>,
    /// The Data/Messages/Explain/History surface owned by each query item.
    results: HashMap<u64, Entity<ResultsView>>,
    /// Dirty-close confirmation belongs to its tab and is rendered inline.
    pending_close_item: Option<u64>,
}

impl Pane {
    fn from_presentation(pane: PanePresentation, theme: Theme, cx: &mut Context<Self>) -> Self {
        let PanePresentation {
            id,
            mut items,
            active_item,
        } = pane;
        let mut editors = HashMap::new();
        let mut clean_documents = HashMap::new();
        let mut results = HashMap::new();
        for item in items
            .iter_mut()
            .filter(|item| ItemRegistry::definition(&item.kind).runtime.is_editor())
        {
            // Editor text is rehydrated independently of presentation state.
            // A persisted dirty bit without its document would create a false
            // close warning for an unchanged, empty editor.
            item.dirty = false;
            let id = item.id;
            let document = QueryDocument::with_random_peer("");
            let language = if item.kind == ItemKind::Configuration {
                EditorLanguage::Toml
            } else {
                EditorLanguage::Sql
            };
            let editor = cx.new(|cx| QueryEditor::new(document, theme, cx).with_language(language));
            cx.subscribe(&editor, move |pane, _, event, cx| {
                pane.on_editor_event(id, event, cx);
            })
            .detach();
            editors.insert(id, editor);
            clean_documents.insert(id, String::new());
            if item.kind == ItemKind::Query {
                results.insert(id, cx.new(|cx| ResultsView::new(theme, cx)));
            }
        }
        Self {
            id,
            items,
            active_item,
            backward_items: Vec::new(),
            forward_items: Vec::new(),
            focus_handle: cx.focus_handle(),
            theme,
            editors,
            clean_documents,
            results,
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
        if self.pending_close_item == Some(item_id) {
            self.pending_close_item = None;
        }
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
            if let Some(current) = self.active_item().map(|item| item.id) {
                self.backward_items.push(current);
                self.forward_items.clear();
            }
            self.items.push(item);
            self.active_item = self.items.len() - 1;
        }
        cx.subscribe(&editor, move |pane, _, event, cx| {
            pane.on_editor_event(item_id, event, cx);
        })
        .detach();
        self.clean_documents
            .insert(item_id, editor.read(cx).document().text().to_owned());
        self.editors.insert(item_id, editor);
        self.results.remove(&item_id);
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
        if let Some(current) = self.active_item().map(|item| item.id) {
            self.backward_items.push(current);
            self.forward_items.clear();
        }
        self.items.push(item);
        self.active_item = self.items.len() - 1;
        cx.subscribe(&editor, move |pane, _, event, cx| {
            pane.on_editor_event(item_id, event, cx);
        })
        .detach();
        self.clean_documents
            .insert(item_id, editor.read(cx).document().text().to_owned());
        self.editors.insert(item_id, editor);
        self.results.insert(item_id, results);
        cx.notify();
    }

    fn resize_results(
        &mut self,
        event: &gpui::DragMoveEvent<ResultResizeDrag>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let drag = *event.drag(cx);
        let Some(results) = self.results.get(&drag.item_id) else {
            return;
        };
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
        results.update(cx, |results, cx| results.set_extent(extent, cx));
    }

    fn toggle_results_placement(&mut self, item_id: u64, cx: &mut Context<Self>) {
        if let Some(results) = self.results.get(&item_id) {
            results.update(cx, ResultsView::toggle_placement);
        }
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

struct PaneTooltip {
    label: &'static str,
    theme: Theme,
}

impl gpui::Render for PaneTooltip {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .px_2()
            .py_1()
            .rounded_sm()
            .border_1()
            .border_color(self.theme.colors.strong_border)
            .bg(self.theme.colors.elevated_surface)
            .text_xs()
            .text_color(self.theme.colors.text)
            .child(self.label)
    }
}

impl gpui::Render for Pane {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = self.theme.colors;
        let is_focused = self.active_focus_handle(cx).is_focused(window)
            || self.focus_handle.contains_focused(window, cx);
        let active = self.active_item().cloned();
        let pending_close = active
            .as_ref()
            .filter(|item| self.pending_close_item == Some(item.id))
            .cloned();
        let has_items = !self.items.is_empty();
        let can_go_back = self.can_navigate_backward();
        let can_go_forward = self.can_navigate_forward();
        let pane_id = self.id;
        let theme = self.theme;
        div()
            .id(("pane", self.id as usize))
            .key_context("SiftPane")
            .track_focus(&self.focus_handle)
            // Clicking anywhere in the pane makes it the active pane. The
            // workspace owns the pane list, so we ask rather than reach across.
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|_, _, _, cx| cx.emit(PaneEvent::FocusRequested)),
            )
            .on_drag_move::<ResultResizeDrag>(cx.listener(Self::resize_results))
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .border_t_1()
            .border_color(pane_border_color(&self.theme, is_focused))
            .bg(colors.background)
            .children(has_items.then(|| {
                div()
                    .debug_selector(|| "pane-tab-bar".into())
                    .h(self.theme.metrics.tab_height)
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
                                                cx.new(|_| PaneTooltip { label, theme }).into()
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
                                            div()
                                                .id(("tab", item.id as usize))
                                                .relative()
                                                .flex_none()
                                                .flex()
                                                .items_center()
                                                .h_full()
                                                .min_w(px(110.))
                                                .max_w(px(240.))
                                                .border_r_1()
                                                .border_color(colors.subtle_border)
                                                .when(selected, |tab| {
                                                    tab.bg(colors.background)
                                                        .text_color(colors.text)
                                                        .child(
                                                            div()
                                                                .absolute()
                                                                .left_0()
                                                                .right_0()
                                                                .bottom_0()
                                                                .h(px(1.))
                                                                .bg(colors.accent),
                                                        )
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
                                                    div()
                                                        .id(("tab-close", item.id as usize))
                                                        .flex_none()
                                                        .flex()
                                                        .items_center()
                                                        .justify_center()
                                                        .h(px(22.))
                                                        .w(px(22.))
                                                        .mr_1()
                                                        .rounded_sm()
                                                        .text_color(colors.muted_text)
                                                        .hover(|close| {
                                                            close
                                                                .bg(colors.hovered_surface)
                                                                .text_color(colors.text)
                                                        })
                                                        .on_click(cx.listener(
                                                            move |_, _, _, cx| {
                                                                cx.emit(
                                                                    PaneEvent::CloseItemRequested {
                                                                        item_id,
                                                                    },
                                                                );
                                                            },
                                                        ))
                                                        .child(icon(
                                                            IconName::Close,
                                                            colors.muted_text,
                                                            12.,
                                                        )),
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
                    .h(px(34.))
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
                        div()
                            .id(("keep-editing", item_id as usize))
                            .role(Role::Button)
                            .h(px(24.))
                            .px_2()
                            .flex()
                            .items_center()
                            .rounded_sm()
                            .hover(|button| button.bg(colors.hovered_surface))
                            .on_click(cx.listener(move |pane, _, window, cx| {
                                pane.pending_close_item = None;
                                pane.active_focus_handle(cx).focus(window, cx);
                                cx.notify();
                            }))
                            .child("Keep editing"),
                    )
                    .children(is_configuration.then(|| {
                        div()
                            .id(("save-dirty-item", item_id as usize))
                            .role(Role::Button)
                            .h(px(24.))
                            .px_2()
                            .flex()
                            .items_center()
                            .rounded_sm()
                            .bg(colors.accent)
                            .hover(|button| button.bg(colors.accent_hover))
                            .on_click(cx.listener(move |_, _, _, cx| {
                                cx.emit(PaneEvent::SaveItemRequested { item_id });
                            }))
                            .child("Save")
                    }))
                    .child(
                        div()
                            .id(("discard-dirty-item", item_id as usize))
                            .role(Role::Button)
                            .h(px(24.))
                            .px_2()
                            .flex()
                            .items_center()
                            .rounded_sm()
                            .text_color(colors.danger)
                            .hover(|button| button.bg(colors.danger_muted))
                            .on_click(cx.listener(move |_, _, _, cx| {
                                cx.emit(PaneEvent::DiscardItemRequested { item_id });
                            }))
                            .child("Discard"),
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
                                let extent = result.read(cx).extent();
                                let item_id = item.id;
                                let handle = div()
                                    .id(("resize-query-results", item_id as usize))
                                    .flex_none()
                                    .cursor(match placement {
                                        ResultPlacement::Bottom => CursorStyle::ResizeUpDown,
                                        ResultPlacement::Right => CursorStyle::ResizeLeftRight,
                                    })
                                    .block_mouse_except_scroll()
                                    .border_color(colors.subtle_border)
                                    .when(placement == ResultPlacement::Bottom, |handle| {
                                        handle
                                            .w_full()
                                            .h(px(RESULT_RESIZE_HANDLE_SIZE))
                                            .border_t_1()
                                    })
                                    .when(placement == ResultPlacement::Right, |handle| {
                                        handle
                                            .h_full()
                                            .w(px(RESULT_RESIZE_HANDLE_SIZE))
                                            .border_l_1()
                                    })
                                    .on_drag(
                                        ResultResizeDrag { item_id, placement },
                                        |_, _, _, cx| cx.new(|_| gpui::Empty),
                                    )
                                    .on_click(cx.listener(move |pane, _, _, cx| {
                                        pane.toggle_results_placement(item_id, cx)
                                    }))
                                    .tooltip(move |_, cx| {
                                        cx.new(|_| PaneTooltip {
                                            label: match placement {
                                                ResultPlacement::Bottom => {
                                                    "Drag to resize; click to move results right"
                                                }
                                                ResultPlacement::Right => {
                                                    "Drag to resize; click to move results below"
                                                }
                                            },
                                            theme,
                                        })
                                        .into()
                                    });
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
    theme: Theme,
    dark_theme: bool,
    window_presentation: WindowPresentation,
    panes: Vec<Entity<Pane>>,
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
    tooltip: Option<Tooltip>,
    status: StatusBar,
    lifecycle: LifecycleProjection,
    presence: RoomPresenceProjection,
    _lifecycle_task: Option<Task<()>>,
    _presence_task: Option<Task<()>>,
    _executor_task: Option<Task<()>>,
    _instance_task: Option<Task<()>>,
    _toast_task: Option<Task<()>>,
    executor_sender: Option<tokio::sync::mpsc::UnboundedSender<ExecutorCommand>>,
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

impl WorkspaceShell {
    pub fn new(
        state: PresentationState,
        store: Option<Arc<PresentationStore>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let window_presentation = state.window.clone();
        let theme = if state.dark_theme {
            Theme::dark()
        } else {
            Theme::light()
        };
        let workspace = if state.workspace.panes.is_empty() {
            PresentationState::default().workspace
        } else {
            state.workspace
        };
        let selected_workspace_id = workspace.workspace_id;
        let selected_instance_id = workspace.instance_id.clone();
        let panes = workspace
            .panes
            .into_iter()
            .map(|pane| cx.new(|cx| Pane::from_presentation(pane, theme, cx)))
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
            QueryEditor::new(QueryDocument::with_random_peer(""), theme, cx)
                .with_language(EditorLanguage::Toml)
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
            theme,
            dark_theme: state.dark_theme,
            window_presentation,
            panes,
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
            tooltip: None,
            status: StatusBar::default(),
            lifecycle: LifecycleProjection::default(),
            presence: RoomPresenceProjection::default(),
            _lifecycle_task: None,
            _presence_task: None,
            _executor_task: None,
            _instance_task: None,
            _toast_task: None,
            executor_sender: None,
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

    fn render_account_icon(&self, size: f32) -> gpui::AnyElement {
        let colors = self.theme.colors;
        match &self.lifecycle.identity {
            Some(identity) => div()
                .relative()
                .size(px(size))
                .flex()
                .items_center()
                .justify_center()
                .overflow_hidden()
                .rounded(px(4.))
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
        cx: &mut Context<Self>,
    ) {
        self._lifecycle_task = Some(cx.spawn(async move |shell, cx| {
            while let Some(event) = receiver.recv().await {
                if shell
                    .update(cx, |shell, cx| {
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
                            shell.selected_instance_id = Some(instance.id.clone());
                            if instance_changed {
                                shell.selected_workspace_id = None;
                                shell.expanded_tenants.clear();
                                shell.expanded_connections.clear();
                                shell.expanded_rooms.clear();
                                shell.expanded_catalogs.clear();
                                shell.expanded_schemas.clear();
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
                    QueryEditor::new(
                        QueryDocument::with_random_peer(&configuration.manifest),
                        self.theme,
                        cx,
                    )
                    .with_language(EditorLanguage::Toml)
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
                self.show_toast(format!("Connected to {name}"), cx);
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
                self.show_toast(format!("Signed in as {display_name}"), cx);
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
                self.connection_status = status;
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
                self.show_toast(
                    connection_error.map_or_else(
                        || format!("Added and connected to {}", entry.name),
                        |error| format!("Added {}, but connection failed: {error}", entry.name),
                    ),
                    cx,
                );
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
                self.show_toast(message, cx);
            }
        }
    }

    fn show_toast(&mut self, message: String, cx: &mut Context<Self>) {
        self.toasts.clear();
        self.toasts.push(Toast {
            message: message.clone(),
        });
        self._toast_task = Some(cx.spawn(async move |shell, cx| {
            cx.background_executor()
                .timer(std::time::Duration::from_secs(4))
                .await;
            let _ = shell.update(cx, |shell, cx| {
                if shell
                    .toasts
                    .last()
                    .is_some_and(|toast| toast.message == message)
                {
                    shell.toasts.clear();
                    cx.notify();
                }
            });
        }));
        cx.notify();
    }

    fn dismiss_toast(&mut self, cx: &mut Context<Self>) {
        self.toasts.clear();
        self._toast_task = None;
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
            cx.notify();
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
        provider_id: sift_protocol::ProviderId,
        schema: String,
        object: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let item_id = self.next_id;
        self.next_id += 1;
        let sql = table_preview_sql(&provider_id, &schema, &object);
        let editor =
            cx.new(|cx| QueryEditor::new(QueryDocument::with_random_peer(&sql), self.theme, cx));
        let results = cx.new(|cx| ResultsView::new(self.theme, cx));
        results.update(cx, |results, cx| results.set_pending(cx));
        if let Some(pane) = self.panes.get(self.active_pane) {
            pane.update(cx, |pane, cx| {
                pane.open_query(
                    ItemPresentation {
                        id: item_id,
                        kind: ItemKind::Query,
                        title: format!("{schema}.{object}"),
                        dirty: false,
                    },
                    editor,
                    results,
                    cx,
                )
            });
        }
        match &self.executor_sender {
            Some(sender) => {
                let _ = sender.send(ExecutorCommand::Execute { item_id, sql });
                self.status.execution = "Running…".into();
            }
            None => self.route_result(
                item_id,
                ResultState::Unavailable("Not connected to a database.".into()),
                cx,
            ),
        }
        self.focus_active_pane(window, cx);
        self.persist(cx);
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
        self.status.execution = match &state {
            ResultState::Ready(_) | ResultState::Idle => "Ready".into(),
            _ => state.status_label(),
        };
        self.status.current_error = match &state {
            ResultState::Unavailable(reason) | ResultState::Failed(reason) => Some(reason.clone()),
            ResultState::TimedOut => Some("Query timed out".into()),
            ResultState::OutcomeUnknown => Some("Query outcome is unknown".into()),
            ResultState::Idle
            | ResultState::Pending
            | ResultState::Ready(_)
            | ResultState::Cancelled => None,
        };
        self.status.diagnostic_count = match &state {
            ResultState::Ready(data) => data.warnings.len(),
            _ => usize::from(self.status.current_error.is_some()),
        };
        for pane in &self.panes {
            if pane.update(cx, |pane, cx| pane.set_result(item_id, state.clone(), cx)) {
                break;
            }
        }
        cx.notify();
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
        let message = self.status.current_error.as_ref().map_or_else(
            || "No current SQL diagnostics".into(),
            |error| format!("Current diagnostic: {error}"),
        );
        self.show_toast(message, cx);
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
                active_pane: self.active_pane,
                workspace_id: self.selected_workspace_id,
                instance_id: self.selected_instance_id.clone(),
            },
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
            (self.theme.metrics.toolbar_height + self.theme.metrics.status_height).into();
        self.bottom_dock.presentation.size = dock_layout::fit_bottom_dock(
            (height - vertical_chrome).max(0.0),
            self.bottom_dock.presentation.size,
        );
    }

    fn resize_dock(
        &mut self,
        event: &gpui::DragMoveEvent<DockResizeDrag>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let dock = event.drag(cx).dock;
        let width: f32 = event.bounds.size.width.into();
        let height: f32 = event.bounds.size.height.into();
        let pointer_x: f32 = (event.event.position.x - event.bounds.left()).into();
        let pointer_y: f32 = (event.event.position.y - event.bounds.top()).into();

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
                self.left_dock.presentation.size = sizes.left;
                self.right_dock.presentation.size = sizes.right;
            }
            DockId::Bottom => {
                self.bottom_dock.presentation.size =
                    dock_layout::fit_bottom_dock(height, height - pointer_y);
            }
        }
        cx.notify();
    }

    fn finish_dock_resize(&mut self, _: &DockResizeDrag, _: &mut Window, cx: &mut Context<Self>) {
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
                        })
                        .into_iter()
                        .collect(),
                    active_item: 0,
                },
                self.theme,
                cx,
            )
        });
        cx.subscribe_in(&pane, window, Self::on_pane_event).detach();
        self.panes.push(pane);
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
                // Always (re)focus the clicked pane's editor — not just when the
                // active pane changes — so a click after a modal restores focus.
                self.active_pane = index;
                self.focus_active_pane(window, cx);
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
            PaneEvent::ExecuteRequested { item_id, sql } => match &self.executor_sender {
                Some(sender) => {
                    self.status.execution = "Running…".into();
                    self.status.current_error = None;
                    self.status.diagnostic_count = 0;
                    let _ = sender.send(ExecutorCommand::Execute {
                        item_id: *item_id,
                        sql: sql.clone(),
                    });
                    cx.notify();
                }
                None => {
                    self.route_result(
                        *item_id,
                        ResultState::Unavailable("Not connected to a database.".into()),
                        cx,
                    );
                }
            },
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
        self.panes.remove(index);
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
        if let Some(pane) = self.panes.get(self.active_pane) {
            pane.update(cx, |pane, _| {
                if !pane.items.is_empty() {
                    let removed = pane.items.remove(pane.active_item);
                    pane.forget_item(removed.id);
                    pane.active_item = pane.active_item.min(pane.items.len().saturating_sub(1));
                }
            });
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
                .and_then(|item| pane.editor(item.id))
        });
        if let Some(editor) = active_configuration {
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
            (self.theme.metrics.toolbar_height + self.theme.metrics.status_height).into();
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
        let colors = self.theme.colors;
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
                    .h(px(28.))
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
                        row.child(
                            div()
                                .flex_none()
                                .text_xs()
                                .text_color(colors.muted_text)
                                .child(item.shortcut),
                        )
                    })
                    .when_some(disabled_reason, |row, reason| {
                        row.child(
                            div()
                                .flex_none()
                                .px_1()
                                .rounded(px(3.))
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
            .top(px(30.))
            .when(align_right, |menu| menu.right_0())
            .when(!align_right, |menu| menu.left_0())
            .w(px(280.))
            .p_1()
            .flex()
            .flex_col()
            .rounded(self.theme.metrics.radius_large)
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
        let colors = self.theme.colors;
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
        let colors = self.theme.colors;
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
            .key_context("SiftWindow")
            .h(self.theme.metrics.toolbar_height)
            .relative()
            .flex()
            .items_center()
            .justify_between()
            .pl_2()
            .pr_2()
            .gap_2()
            .border_b_1()
            .border_color(colors.subtle_border)
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
                            .child(self.render_account_icon(18.)),
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
                                div()
                                    .id("window-minimize")
                                    .role(Role::Button)
                                    .aria_label("Minimize window")
                                    .size(px(26.))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .rounded_sm()
                                    .hover(|button| button.bg(colors.hovered_surface))
                                    .on_click(|_, window, _| window.minimize_window())
                                    .child(icon(IconName::Minimize, colors.muted_text, 16.)),
                            )
                            .child(
                                div()
                                    .id("window-size-toggle")
                                    .role(Role::Button)
                                    .aria_label("Maximize or restore window")
                                    .size(px(26.))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .rounded_sm()
                                    .hover(|button| button.bg(colors.hovered_surface))
                                    .on_click(|_, window, _| window.zoom_window())
                                    .child(icon(IconName::Maximize, colors.muted_text, 16.)),
                            )
                            .child(
                                div()
                                    .id("window-close")
                                    .role(Role::Button)
                                    .aria_label("Close window")
                                    .size(px(26.))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .rounded_sm()
                                    .hover(|button| button.bg(colors.danger_muted))
                                    .on_click(|_, window, _| window.remove_window())
                                    .child(icon(IconName::Close, colors.muted_text, 16.)),
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
                .h(self.theme.metrics.row_height)
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
                            .h(self.theme.metrics.row_height)
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
                                .h(self.theme.metrics.row_height)
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
                            let provider_id = connection.provider_id.clone();
                            let schema_name = schema.name.clone();
                            let object_name = object.name.clone();
                            let row = div()
                                .id((
                                    "schema-object",
                                    object_index
                                        + schema_index * 1000
                                        + catalog_index * 100_000
                                        + profile_id as usize * 10_000_000,
                                ))
                                .mx_2()
                                .h(self.theme.metrics.row_height)
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
                                                provider_id.clone(),
                                                schema_name.clone(),
                                                object_name.clone(),
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
                            .h(self.theme.metrics.row_height)
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
        let colors = self.theme.colors;
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
            .when(definition.placement == DockPlacement::Left, |dock| {
                dock.border_r_1()
            })
            .when(definition.placement == DockPlacement::Right, |dock| {
                dock.border_l_1()
            })
            .bg(colors.panel)
            .text_sm()
            .child(
                div()
                    .h(px(26.))
                    .px_3()
                    .flex()
                    .items_center()
                    .gap_2()
                    .text_xs()
                    .text_color(colors.muted_text)
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child(title.to_uppercase()),
            )
            .when(
                dock.id == DockId::Left && self.active_left_panel == LeftPanel::Connections,
                |dock_view| {
                dock_view.child(
                    div()
                        .mx_2()
                        .h(self.theme.metrics.row_height)
                        .flex()
                        .items_center()
                        .justify_between()
                        .child(
                            div()
                                .id("add-database-connection")
                                .role(Role::Button)
                                .h_full()
                                .px_2()
                                .flex()
                                .items_center()
                                .gap_2()
                                .rounded_sm()
                                .text_color(colors.muted_text)
                                .hover(|button| {
                                    button.bg(colors.hovered_surface).text_color(colors.text)
                                })
                                .on_click(cx.listener(|shell, _, window, cx| {
                                    shell.open_database_connection(window, cx)
                                }))
                                .child(icon(IconName::Add, colors.muted_text, 14.))
                                .child(div().min_w_0().truncate().child("Add connection…")),
                        )
                        .when(
                            matches!(self.connection_status, ConnectionStatus::Connected { .. }),
                            |toolbar| {
                                toolbar.child(
                                    div()
                                        .id("refresh-connection-schema")
                                        .role(Role::Button)
                                        .aria_label("Refresh database schema")
                                        .h_full()
                                        .px_2()
                                        .flex()
                                        .items_center()
                                        .rounded_sm()
                                        .text_xs()
                                        .text_color(colors.muted_text)
                                        .hover(|button| {
                                            button
                                                .bg(colors.hovered_surface)
                                                .text_color(colors.text)
                                        })
                                        .on_click(cx.listener(|shell, _, _, cx| {
                                            shell.refresh_connection_schema(cx)
                                        }))
                                        .child("Refresh"),
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
                            .h(self.theme.metrics.row_height)
                            .px_2()
                            .rounded_sm()
                            .when(connected, |row| row.bg(colors.active_surface))
                            .hover(|row| row.bg(colors.hovered_surface))
                            .child(leading)
                            .child(div().flex_1().min_w_0().truncate().child(conn.name.clone()));
                        if connected {
                            row = row.child(
                                div()
                                    .id(("disconnect", conn.id as usize))
                                    .flex_none()
                                    .text_xs()
                                    .text_color(colors.muted_text)
                                    .hover(|d| d.text_color(colors.danger))
                                    .on_click(cx.listener(|shell, _, _, cx| shell.disconnect(cx)))
                                    .child("Disconnect"),
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
                                .h(self.theme.metrics.row_height)
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
                                    .h(self.theme.metrics.row_height)
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
                            .h(self.theme.metrics.row_height)
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
            .child(dock_resize_handle(dock.id))
    }

    fn render_modal(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let colors = self.theme.colors;
        self.modal.as_ref().map(|modal| {
            let server_picker = matches!(modal, Modal::ServerPicker);
            let account = matches!(modal, Modal::Account);
            let app_bar_modal = matches!(
                modal,
                Modal::ServerPicker | Modal::ServerConnection | Modal::Account
            );
            let database_connection = matches!(modal, Modal::DatabaseConnection);
            let command_palette = matches!(modal, Modal::CommandPalette);
            let instance_setup = matches!(modal, Modal::InstanceSetup);
            let card_width = if server_picker || account {
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
                                                    .child(
                                                        div()
                                                            .flex_none()
                                                            .max_w(px(220.))
                                                            .truncate()
                                                            .text_xs()
                                                            .text_color(colors.muted_text)
                                                            .child(right),
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
                    let mut rows = Vec::new();
                    let local_active = current_id.as_deref() == Some("local");
                    rows.push(
                        div()
                            .id("picker-local-sift")
                            .role(Role::Button)
                            .flex()
                            .items_center()
                            .justify_between()
                            .px_2()
                            .py_2()
                            .gap_2()
                            .rounded_sm()
                            .when(local_active, |row| row.bg(colors.active_surface))
                            .when(!pending && !local_active, |row| {
                                row.hover(|row| row.bg(colors.hovered_surface)).on_click(
                                    cx.listener(|shell, _, _, cx| shell.use_local_server(cx)),
                                )
                            })
                            .child(
                                div()
                                    .flex()
                                    .flex_1()
                                    .min_w_0()
                                    .items_center()
                                    .gap_2()
                                    .child(
                                        div()
                                            .min_w_0()
                                            .truncate()
                                            .child("Bundled Local Sift"),
                                    ),
                            )
                            .child(
                                div()
                                    .flex_none()
                                    .text_xs()
                                    .text_color(colors.muted_text)
                                    .px_1()
                                    .rounded(px(3.))
                                    .bg(colors.hovered_surface)
                                    .child(if local_active {
                                        "Current · no TOML"
                                    } else {
                                        "No TOML"
                                    }),
                            )
                            .into_any_element(),
                    );
                    for (index, profile) in self.saved_servers.iter().cloned().enumerate() {
                        let active = current_id.as_deref()
                            == Some(format!("hosted:{}", profile.id).as_str());
                        let profile_for_click = profile.clone();
                        rows.push(
                            div()
                                .id(("picker-saved-server", index))
                                .role(Role::Button)
                                .flex()
                                .items_center()
                                .justify_between()
                                .gap_3()
                                .px_2()
                                .py_2()
                                .rounded_sm()
                                .when(active, |row| row.bg(colors.active_surface))
                                .when(!pending && !active, |row| {
                                    row.hover(|row| row.bg(colors.hovered_surface)).on_click(
                                        cx.listener(move |shell, _, _, cx| {
                                            shell.connect_saved_server(&profile_for_click, cx)
                                        }),
                                    )
                                })
                                .child(
                                    div()
                                        .flex()
                                        .flex_1()
                                        .items_center()
                                        .gap_2()
                                        .min_w_0()
                                        .child(
                                        div()
                                            .flex()
                                            .flex_col()
                                            .min_w_0()
                                            .child(profile.name)
                                            .child(
                                                div()
                                                    .text_xs()
                                                    .text_color(colors.muted_text)
                                                    .truncate()
                                                    .child(profile.base_url),
                                            ),
                                        ),
                                )
                                .when(active, |row| {
                                    row.child(
                                        div()
                                            .flex_none()
                                            .text_xs()
                                            .text_color(colors.muted_text)
                                            .child("Current"),
                                    )
                                })
                                .into_any_element(),
                        );
                    }
                    for (index, instance) in self.instance_roots.iter().cloned().enumerate() {
                        let root = instance.root.clone();
                        let root_for_remove = instance.root.clone();
                        let active = current_id.as_deref()
                            == Some(format!("config:{}", instance.manifest_id).as_str());
                        rows.push(
                            div()
                                .id(("picker-instance-root", index))
                                .role(Role::Button)
                                .flex()
                                .items_center()
                                .justify_between()
                                .gap_3()
                                .px_2()
                                .py_2()
                                .rounded_sm()
                                .when(active, |row| row.bg(colors.active_surface))
                                .when(!pending, |row| {
                                    row.hover(|row| row.bg(colors.hovered_surface)).on_click(
                                        cx.listener(move |shell, _, _, cx| {
                                            shell.inspect_instance_root(root.clone(), cx)
                                        }),
                                    )
                                })
                                .child(
                                    div()
                                        .flex()
                                        .flex_1()
                                        .flex_col()
                                        .min_w_0()
                                        .child(instance.name.clone())
                                        .child(
                                            div()
                                                .text_xs()
                                                .text_color(colors.muted_text)
                                                .truncate()
                                                .child(instance.root.display().to_string()),
                                        ),
                                )
                                .child(
                                    div()
                                        .flex()
                                        .flex_none()
                                        .items_center()
                                        .gap_2()
                                        .child(
                                            div()
                                                .text_xs()
                                                .text_color(colors.muted_text)
                                                .child(if active {
                                                    "Current"
                                                } else {
                                                    "Config root"
                                                }),
                                        )
                                        .child(
                                            div()
                                                .id(("forget-instance-root", index))
                                                .role(Role::Button)
                                                .aria_label(format!(
                                                    "Remove {} from Sift; keep files",
                                                    instance.name
                                                ))
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
                                                .on_click(cx.listener(
                                                    move |shell, _, _, cx| {
                                                        cx.stop_propagation();
                                                        shell.forget_instance_root(
                                                            root_for_remove.clone(),
                                                            cx,
                                                        )
                                                    },
                                                ))
                                                .child(icon(
                                                    IconName::Close,
                                                    colors.danger,
                                                    12.,
                                                )),
                                        ),
                                )
                                .into_any_element(),
                        );
                    }
                    div()
                        .flex()
                        .flex_col()
                        .min_w_0()
                        .gap_2()
                        .child(
                            div()
                                .h(px(28.))
                                .flex()
                                .items_center()
                                .gap_2()
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .child("Sift Server"),
                        )
                        .child(div().flex().flex_col().gap_1().children(rows))
                        .children(self.server_connection_error.as_ref().map(|message| {
                            div()
                                .min_w_0()
                                .whitespace_normal()
                                .text_sm()
                                .text_color(colors.danger)
                                .child(message.clone())
                        }))
                        .when(pending, |picker| {
                            picker.child(
                                div()
                                    .px_2()
                                    .text_xs()
                                    .text_color(colors.muted_text)
                                    .child("Testing connection…"),
                            )
                        })
                        .when(
                            current_id
                                .as_deref()
                                .is_some_and(|id| id.starts_with("config:")),
                            |picker| {
                                picker.child(
                                    div()
                                        .id("picker-edit-current-instance")
                                        .role(Role::Button)
                                        .px_2()
                                        .py_2()
                                        .rounded_sm()
                                        .text_color(colors.muted_text)
                                        .hover(|button| {
                                            button
                                                .bg(colors.hovered_surface)
                                                .text_color(colors.text)
                                        })
                                        .on_click(cx.listener(|shell, _, _, cx| {
                                            shell.open_current_configuration(cx)
                                        }))
                                        .child(
                                            div()
                                                .flex()
                                                .items_center()
                                                .gap_2()
                                                .child(icon(
                                                    IconName::Fallback,
                                                    colors.danger,
                                                    13.,
                                                ))
                                                .child("Edit current sift.toml…"),
                                        ),
                                )
                            },
                        )
                        .child(
                            div()
                                .id("picker-new-instance")
                                .role(Role::Button)
                                .px_2()
                                .py_2()
                                .rounded_sm()
                                .text_color(colors.muted_text)
                                .hover(|button| {
                                    button.bg(colors.hovered_surface).text_color(colors.text)
                                })
                                .on_click(cx.listener(|shell, _, _, cx| {
                                    shell.prompt_for_new_instance_root(cx)
                                }))
                                .child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap_2()
                                        .child(icon(IconName::Add, colors.muted_text, 13.))
                                        .child("Create Sift Instance…"),
                                ),
                        )
                        .child(
                            div()
                                .id("picker-import-instance")
                                .role(Role::Button)
                                .px_2()
                                .py_2()
                                .rounded_sm()
                                .text_color(colors.muted_text)
                                .hover(|button| {
                                    button.bg(colors.hovered_surface).text_color(colors.text)
                                })
                                .on_click(cx.listener(|shell, _, _, cx| {
                                    shell.prompt_for_instance_root(cx)
                                }))
                                .child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap_2()
                                        .child(icon(IconName::Workspace, colors.muted_text, 13.))
                                        .child("Open Existing Sift Instance…"),
                                ),
                        )
                        .child(
                            div()
                                .id("picker-manage-servers")
                                .role(Role::Button)
                                .mt_1()
                                .pt_2()
                                .px_2()
                                .border_t_1()
                                .border_color(colors.subtle_border)
                                .text_color(colors.muted_text)
                                .hover(|button| {
                                    button
                                        .bg(colors.hovered_surface)
                                        .text_color(colors.text)
                                })
                                .on_click(cx.listener(|shell, _, window, cx| {
                                    shell.open_server_connection(&OpenServerConnection, window, cx)
                                }))
                                .child(
                                    div()
                                        .flex()
                                        .min_w_0()
                                        .items_center()
                                        .gap_2()
                                        .child(icon(IconName::Add, colors.muted_text, 13.))
                                        .child(
                                            div()
                                                .min_w_0()
                                                .truncate()
                                                .child("Connect to or manage servers…"),
                                        ),
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
                                    .child(
                                        div()
                                            .flex_none()
                                            .text_xs()
                                            .text_color(if credential.readiness == "ready" {
                                                colors.success
                                            } else {
                                                colors.warning
                                            })
                                            .child(credential.readiness),
                                    ),
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
                                    div()
                                        .flex_none()
                                        .text_xs()
                                        .text_color(if plan.drifted {
                                            colors.warning
                                        } else {
                                            colors.success
                                        })
                                        .child(if plan.drifted {
                                            "Unapplied drift"
                                        } else {
                                            "Applied"
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
                                    div()
                                        .id("edit-instance-manifest")
                                        .role(Role::Button)
                                        .px_2()
                                        .py_1()
                                        .rounded_sm()
                                        .bg(colors.accent)
                                        .text_color(colors.background)
                                        .on_click(cx.listener(move |shell, _, _, cx| {
                                            shell.open_root_configuration(
                                                edit_manifest_root.clone(),
                                                cx,
                                            )
                                        }))
                                        .child("Edit sift.toml"),
                                )
                                .child(
                                    div()
                                        .id("open-instance-manifest")
                                        .role(Role::Button)
                                        .px_2()
                                        .py_1()
                                        .rounded_sm()
                                        .bg(colors.hovered_surface)
                                        .on_click(move |_, _, cx| {
                                            cx.open_with_system(&manifest_path)
                                        })
                                        .child("Open externally"),
                                )
                                .child(
                                    div()
                                        .id("open-instance-lock")
                                        .role(Role::Button)
                                        .px_2()
                                        .py_1()
                                        .rounded_sm()
                                        .bg(colors.hovered_surface)
                                        .on_click(move |_, _, cx| cx.open_with_system(&lock_path))
                                        .child("Open sift.lock"),
                                )
                                .child(
                                    div()
                                        .id("refresh-instance-plan")
                                        .role(Role::Button)
                                        .px_2()
                                        .py_1()
                                        .rounded_sm()
                                        .bg(colors.hovered_surface)
                                        .on_click(cx.listener(move |shell, _, _, cx| {
                                            shell.inspect_instance_root(refresh_root.clone(), cx)
                                        }))
                                        .child("Refresh plan"),
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
                                    div()
                                        .id("import-instance-credential")
                                        .role(Role::Button)
                                        .px_2()
                                        .flex()
                                        .items_center()
                                        .rounded_sm()
                                        .bg(colors.accent)
                                        .text_color(colors.background)
                                        .when(!pending, |button| {
                                            button.on_click(cx.listener(|shell, _, _, cx| {
                                                shell.import_instance_credential(cx)
                                            }))
                                        })
                                        .child("Import"),
                                )
                        }))
                        .children(self.instance_operation_error.as_ref().map(|error| {
                            div()
                                .p_2()
                                .rounded_sm()
                                .bg(colors.danger_muted)
                                .text_color(colors.danger)
                                .whitespace_normal()
                                .child(error.clone())
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
                                    div()
                                        .id("apply-instance-plan")
                                        .role(Role::Button)
                                        .px_3()
                                        .py_1()
                                        .rounded_sm()
                                        .bg(if plan.destroy_confirmation_required {
                                            colors.danger
                                        } else {
                                            colors.accent
                                        })
                                        .text_color(colors.background)
                                        .when(!pending, |button| {
                                            let allow_destroy = plan.destroy_confirmation_required;
                                            button.on_click(cx.listener(move |shell, _, _, cx| {
                                                shell.apply_instance_root(allow_destroy, cx)
                                            }))
                                        })
                                        .child(if plan.destroy_confirmation_required {
                                            "Apply destructive changes"
                                        } else {
                                            "Apply"
                                        }),
                                )
                                .child(
                                    div()
                                        .id("start-instance-root")
                                        .role(Role::Button)
                                        .px_3()
                                        .py_1()
                                        .rounded_sm()
                                        .border_1()
                                        .border_color(colors.subtle_border)
                                        .when(ready_to_start && !pending, |button| {
                                            button
                                                .bg(colors.success)
                                                .text_color(colors.background)
                                                .on_click(cx.listener(|shell, _, _, cx| {
                                                    shell.start_instance_root(cx)
                                                }))
                                        })
                                        .when(!ready_to_start || pending, |button| {
                                            button.text_color(colors.muted_text)
                                        })
                                        .child("Start & Connect"),
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
                                    row.child(
                                        div()
                                            .text_xs()
                                            .text_color(colors.success)
                                            .child("Token saved"),
                                    )
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
                                    div()
                                        .id("new-server-profile")
                                        .role(Role::Button)
                                        .flex_none()
                                        .whitespace_nowrap()
                                        .h(self.theme.metrics.control_height)
                                        .px_2()
                                        .flex()
                                        .items_center()
                                        .gap_1()
                                        .rounded_sm()
                                        .text_color(colors.muted_text)
                                        .hover(|button| {
                                            button
                                                .bg(colors.hovered_surface)
                                                .text_color(colors.text)
                                        })
                                        .on_click(cx.listener(|shell, _, window, cx| {
                                            shell.new_server_profile(window, cx)
                                        }))
                                        .child(icon(IconName::Add, colors.muted_text, 13.))
                                        .child("New Server"),
                                ),
                        )
                        .child(
                            div()
                                .id("use-local-sift")
                                .role(Role::Button)
                                .h(self.theme.metrics.row_height)
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
                            div()
                                .flex()
                                .flex_col()
                                .min_w_0()
                                .gap_1()
                                .child(div().text_xs().text_color(colors.muted_text).child("NAME"))
                                .child(
                                    div()
                                        .min_w_0()
                                        .overflow_hidden()
                                        .border_1()
                                        .border_color(colors.subtle_border)
                                        .rounded_sm()
                                        .bg(colors.background)
                                        .child(self.server_name_input.clone()),
                                ),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .min_w_0()
                                .gap_1()
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(colors.muted_text)
                                        .child("SERVER URL"),
                                )
                                .child(
                                    div()
                                        .min_w_0()
                                        .overflow_hidden()
                                        .border_1()
                                        .border_color(colors.subtle_border)
                                        .rounded_sm()
                                        .bg(colors.background)
                                        .child(self.server_url_input.clone()),
                                ),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .min_w_0()
                                .gap_1()
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(colors.muted_text)
                                        .child("BEARER TOKEN"),
                                )
                                .child(
                                    div()
                                        .min_w_0()
                                        .overflow_hidden()
                                        .border_1()
                                        .border_color(colors.subtle_border)
                                        .rounded_sm()
                                        .bg(colors.background)
                                        .child(self.server_token_input.clone()),
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
                                        .rounded(px(4.))
                                        .border_1()
                                        .border_color(if remember {
                                            colors.accent
                                        } else {
                                            colors.strong_border
                                        })
                                        .when(remember, |box_view| {
                                            box_view
                                                .bg(colors.accent)
                                                .child(icon(IconName::Check, gpui::white(), 11.))
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
                            div()
                                .p_3()
                                .flex()
                                .items_start()
                                .gap_2()
                                .rounded_sm()
                                .border_1()
                                .border_color(colors.danger)
                                .bg(colors.danger_muted)
                                .text_color(colors.danger)
                                .child(icon(IconName::Warning, colors.danger, 14.))
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .whitespace_normal()
                                        .child(message.clone()),
                                )
                        }))
                        .child(
                            div()
                                .flex()
                                .min_w_0()
                                .justify_between()
                                .items_center()
                                .gap_2()
                                .child(
                                    div()
                                        .id("forget-server")
                                        .role(Role::Button)
                                        .h(self.theme.metrics.control_height)
                                        .px_2()
                                        .flex()
                                        .items_center()
                                        .rounded_sm()
                                        .text_color(colors.muted_text)
                                        .when(selected.is_some(), |button| {
                                            button
                                                .hover(|button| button.text_color(colors.danger))
                                                .on_click(cx.listener(|shell, _, _, cx| {
                                                    shell.forget_selected_server(cx)
                                                }))
                                        })
                                        .child(if selected.is_some() { "Forget" } else { "" }),
                                )
                                .child(
                                    div()
                                        .id("connect-server")
                                        .role(Role::Button)
                                        .flex_none()
                                        .whitespace_nowrap()
                                        .h(self.theme.metrics.control_height)
                                        .px_3()
                                        .flex()
                                        .items_center()
                                        .gap_1()
                                        .rounded_sm()
                                        .bg(if pending {
                                            colors.hovered_surface
                                        } else {
                                            colors.accent
                                        })
                                        .text_color(colors.text)
                                        .when(!pending, |button| {
                                            button
                                                .hover(|button| button.bg(colors.accent_hover))
                                                .on_click(cx.listener(|shell, _, _, cx| {
                                                    shell.submit_server_connection(cx)
                                                }))
                                        })
                                        .child(if pending {
                                            "Testing connection…"
                                        } else {
                                            "Test & Connect"
                                        }),
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
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(colors.muted_text)
                                    .child(label),
                            )
                            .child(input)
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
                                        div()
                                            .id("account-github-sign-in")
                                            .role(Role::Button)
                                            .h(self.theme.metrics.control_height)
                                            .px_3()
                                            .flex()
                                            .items_center()
                                            .justify_center()
                                            .gap_2()
                                            .rounded_sm()
                                            .bg(if pending {
                                                colors.hovered_surface
                                            } else {
                                                colors.accent
                                            })
                                            .when(!pending, |button| {
                                                button
                                                    .hover(|button| button.bg(colors.accent_hover))
                                                    .on_click(cx.listener(|shell, _, _, cx| {
                                                        shell.sign_in_with_github(cx)
                                                    }))
                                            })
                                            .child(icon(IconName::Github, colors.text, 14.))
                                            .child(if pending {
                                                "Waiting for sign in…"
                                            } else {
                                                "Continue with GitHub"
                                            }),
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
                                        div()
                                            .id("account-password-sign-in")
                                            .role(Role::Button)
                                            .h(self.theme.metrics.control_height)
                                            .px_3()
                                            .flex()
                                            .items_center()
                                            .justify_center()
                                            .rounded_sm()
                                            .border_1()
                                            .border_color(colors.subtle_border)
                                            .bg(colors.surface)
                                            .when(!pending, |button| {
                                                button
                                                    .hover(|button| {
                                                        button.bg(colors.hovered_surface)
                                                    })
                                                    .on_click(cx.listener(|shell, _, _, cx| {
                                                        shell.sign_in_with_password(cx)
                                                    }))
                                            })
                                            .child("Sign in with password"),
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
                                        div()
                                            .id("account-sign-out-all")
                                            .role(Role::Button)
                                            .h(self.theme.metrics.control_height)
                                            .px_2()
                                            .flex()
                                            .items_center()
                                            .rounded_sm()
                                            .text_color(colors.muted_text)
                                            .when(!pending, |button| {
                                                button
                                                    .hover(|button| {
                                                        button.text_color(colors.danger)
                                                    })
                                                    .on_click(cx.listener(|shell, _, _, cx| {
                                                        shell.sign_out(true, cx)
                                                    }))
                                            })
                                            .child("Sign out everywhere"),
                                    )
                                    .child(
                                        div()
                                            .id("account-sign-out")
                                            .role(Role::Button)
                                            .h(self.theme.metrics.control_height)
                                            .px_3()
                                            .flex()
                                            .items_center()
                                            .rounded_sm()
                                            .border_1()
                                            .border_color(colors.subtle_border)
                                            .bg(colors.surface)
                                            .when(!pending, |button| {
                                                button
                                                    .hover(|button| {
                                                        button.bg(colors.hovered_surface)
                                                    })
                                                    .on_click(cx.listener(|shell, _, _, cx| {
                                                        shell.sign_out(false, cx)
                                                    }))
                                            })
                                            .child(if pending {
                                                "Signing out…"
                                            } else {
                                                "Sign out"
                                            }),
                                    ),
                            )
                        })
                        .children(self.account_error.as_ref().map(|message| {
                            div()
                                .border_t_1()
                                .border_color(colors.danger)
                                .px_3()
                                .py_2()
                                .bg(colors.danger_muted)
                                .text_sm()
                                .text_color(colors.danger)
                                .whitespace_normal()
                                .child(message.clone())
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
                                            .child(icon(IconName::Check, gpui::white(), 13.)),
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
                        let focus_handle = input.focus_handle(cx);
                        div()
                            .debug_selector(move || label.to_owned())
                            .flex()
                            .flex_col()
                            .min_w_0()
                            .gap_1()
                            .child(
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .text_xs()
                                    .text_color(colors.muted_text)
                                    .child(label),
                            )
                            .child(
                                div()
                                    .min_w_0()
                                    .overflow_hidden()
                                    .border_1()
                                    .border_color(colors.subtle_border)
                                    .rounded_sm()
                                    .bg(colors.background)
                                    .cursor(gpui::CursorStyle::IBeam)
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        move |_, window, cx| {
                                            cx.stop_propagation();
                                            focus_handle.focus(window, cx);
                                        },
                                    )
                                    .child(input),
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
                                            circle.bg(colors.accent).text_color(gpui::white())
                                        })
                                        .text_xs()
                                        .font_weight(gpui::FontWeight::SEMIBOLD)
                                        .when(complete, |circle| {
                                            circle.child(icon(IconName::Check, gpui::white(), 12.))
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
                                    div()
                                        .p_3()
                                        .flex()
                                        .items_start()
                                        .gap_2()
                                        .rounded_sm()
                                        .border_1()
                                        .border_color(colors.danger)
                                        .bg(colors.danger_muted)
                                        .text_color(colors.danger)
                                        .child(icon(IconName::Warning, colors.danger, 14.))
                                        .child(
                                            div()
                                                .flex_1()
                                                .min_w_0()
                                                .whitespace_normal()
                                                .child(message.clone()),
                                        )
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
                                            div()
                                                .id("database-wizard-secondary")
                                                .role(Role::Button)
                                                .h(self.theme.metrics.control_height)
                                                .px_3()
                                                .flex()
                                                .items_center()
                                                .rounded_sm()
                                                .border_1()
                                                .border_color(colors.subtle_border)
                                                .bg(colors.surface)
                                                .hover(|button| button.bg(colors.hovered_surface))
                                                .when(!pending, |button| {
                                                    button.on_click(cx.listener(
                                                        move |shell, _, window, cx| {
                                                            if step
                                                                == DatabaseWizardStep::Provider
                                                            {
                                                                shell.dismiss_modal(
                                                                    &DismissModal,
                                                                    window,
                                                                    cx,
                                                                )
                                                            } else {
                                                                shell.database_wizard_back(
                                                                    window, cx,
                                                                )
                                                            }
                                                        },
                                                    ))
                                                })
                                                .child(
                                                    if step == DatabaseWizardStep::Provider {
                                                        "Cancel"
                                                    } else {
                                                        "Back"
                                                    },
                                                ),
                                        )
                                        .child(
                                            div()
                                                .id("database-wizard-primary")
                                                .role(Role::Button)
                                                .h(self.theme.metrics.control_height)
                                                .px_3()
                                                .flex()
                                                .items_center()
                                                .gap_1()
                                                .rounded_sm()
                                                .bg(if pending
                                                    || (step == DatabaseWizardStep::Provider
                                                        && selected_provider.is_none())
                                                {
                                                    colors.hovered_surface
                                                } else {
                                                    colors.accent
                                                })
                                                .when(
                                                    !pending
                                                        && (step != DatabaseWizardStep::Provider
                                                            || selected_provider.is_some()),
                                                    |button| {
                                                        button
                                                            .hover(|button| {
                                                                button.bg(colors.accent_hover)
                                                            })
                                                            .on_click(cx.listener(
                                                                move |shell, _, window, cx| {
                                                                    if step
                                                                        == DatabaseWizardStep::Review
                                                                    {
                                                                        shell.submit_database_connection(cx)
                                                                    } else {
                                                                        shell.database_wizard_next(window, cx)
                                                                    }
                                                                },
                                                            ))
                                                    },
                                                )
                                                .child(if pending {
                                                    "Saving & Testing…"
                                                } else if step == DatabaseWizardStep::Review {
                                                    "Save & Connect"
                                                } else {
                                                    "Continue"
                                                }),
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
                                    div()
                                        .id("cancel-delete-connection")
                                        .role(Role::Button)
                                        .h(self.theme.metrics.control_height)
                                        .px_3()
                                        .flex()
                                        .items_center()
                                        .rounded_sm()
                                        .border_1()
                                        .border_color(colors.subtle_border)
                                        .hover(|button| button.bg(colors.hovered_surface))
                                        .on_click(cx.listener(|shell, _, window, cx| {
                                            shell.dismiss_modal(&DismissModal, window, cx)
                                        }))
                                        .child("Cancel"),
                                )
                                .child(
                                    div()
                                        .id("confirm-delete-connection")
                                        .role(Role::Button)
                                        .h(self.theme.metrics.control_height)
                                        .px_3()
                                        .flex()
                                        .items_center()
                                        .rounded_sm()
                                        .bg(colors.danger_muted)
                                        .text_color(colors.danger)
                                        .hover(|button| {
                                            button.bg(colors.danger).text_color(gpui::white())
                                        })
                                        .on_click(cx.listener(move |shell, _, _, cx| {
                                            shell.confirm_delete_connection(
                                                &entry_for_delete,
                                                cx,
                                            )
                                        }))
                                        .child("Delete"),
                                ),
                        )
                        .into_any_element()
                }
            };
            div()
                .id("modal-layer")
                .key_context("SiftModal")
                .absolute()
                .top(if app_bar_modal {
                    self.theme.metrics.toolbar_height
                } else {
                    px(0.)
                })
                .right_0()
                .bottom_0()
                .left_0()
                .occlude()
                .when(server_picker || account, |layer| {
                    layer.on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|shell, _, window, cx| {
                            shell.dismiss_modal(&DismissModal, window, cx)
                        }),
                    )
                })
                .when(!server_picker && !account, |layer| {
                    layer.on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                })
                .flex()
                .items_start()
                .when(server_picker, |layer| {
                    layer.justify_start().pt_1().pl(px(38.))
                })
                .when(account, |layer| layer.justify_end().pt_1().pr_2())
                .when(database_connection, |layer| {
                    layer
                        .items_center()
                        .justify_center()
                        .px_4()
                        .py_4()
                        .bg(colors.scrim)
                })
                .when(
                    !server_picker && !account && !database_connection,
                    |layer| {
                        layer
                            .justify_center()
                            .pt(if app_bar_modal {
                                px(100.) - self.theme.metrics.toolbar_height
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
                        .when(!database_connection && !command_palette && !account, |card| {
                            card.p_3()
                        })
                        .overflow_hidden()
                        .rounded(self.theme.metrics.radius_large)
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

fn dock_resize_handle(dock: DockId) -> gpui::AnyElement {
    let (id, cursor) = match dock {
        DockId::Left => ("resize-left-dock", CursorStyle::ResizeLeftRight),
        DockId::Inspector => ("resize-right-dock", CursorStyle::ResizeLeftRight),
        DockId::Bottom => ("resize-bottom-dock", CursorStyle::ResizeUpDown),
    };
    let handle = div()
        .id(id)
        .debug_selector(move || id.to_owned())
        .absolute()
        .cursor(cursor)
        .block_mouse_except_scroll()
        .on_drag(DockResizeDrag { dock }, |_, _, _, cx| {
            cx.new(|_| gpui::Empty)
        });

    match dock {
        DockId::Left => handle
            .right_0()
            .top_0()
            .h_full()
            .w(px(DOCK_RESIZE_HANDLE_SIZE)),
        DockId::Inspector => handle
            .left_0()
            .top_0()
            .h_full()
            .w(px(DOCK_RESIZE_HANDLE_SIZE)),
        DockId::Bottom => handle
            .top_0()
            .left_0()
            .w_full()
            .h(px(DOCK_RESIZE_HANDLE_SIZE)),
    }
    .into_any_element()
}

impl gpui::Render for WorkspaceShell {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = self.theme.colors;
        // Docks are built before the element chain so each borrows `cx`
        // sequentially rather than two `when` closures capturing it at once.
        let left_dock = self
            .left_dock
            .presentation
            .open
            .then(|| self.render_dock(&self.left_dock, cx));
        let right_dock = self
            .right_dock
            .presentation
            .open
            .then(|| self.render_dock(&self.right_dock, cx));
        let bottom_dock = self
            .bottom_dock
            .presentation
            .open
            .then(|| bottom_tools::render_bottom_panel(self));
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
                                    .children(self.panes.iter().cloned()),
                            )
                            .children(bottom_dock),
                    )
                    .children(right_dock),
            )
            .child(status_bar::render_status_bar(self, cx))
            .children(self.toasts.last().map(|toast| {
                div()
                    .id("toast")
                    .role(Role::Button)
                    .aria_label("Dismiss notification")
                    .absolute()
                    .right_3()
                    .bottom(px(38.))
                    .w(px(360.))
                    .max_w(gpui::relative(0.8))
                    .p_2()
                    .flex()
                    .items_center()
                    .gap_2()
                    .rounded(self.theme.metrics.radius_large)
                    .border_1()
                    .border_color(colors.strong_border)
                    .bg(colors.elevated_surface)
                    .shadow_lg()
                    .hover(|toast| toast.bg(colors.hovered_surface))
                    .on_click(cx.listener(|shell, _, _, cx| shell.dismiss_toast(cx)))
                    .child(
                        div()
                            .flex_none()
                            .size(px(24.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(6.))
                            .bg(colors.accent_muted)
                            .child(icon(IconName::Info, colors.accent_hover, 14.)),
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
            .children(self.tooltip.as_ref().map(|tooltip| {
                div()
                    .id("tooltip")
                    .absolute()
                    .right_3()
                    .top(px(44.))
                    .max_w(px(320.))
                    .px_2()
                    .py_1()
                    .rounded_sm()
                    .border_1()
                    .border_color(colors.strong_border)
                    .bg(colors.elevated_surface)
                    .shadow_md()
                    .text_xs()
                    .whitespace_normal()
                    .child(tooltip.message.clone())
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
                cx.new(|cx| WorkspaceShell::new(Default::default(), None, window, cx))
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
                cx.new(|cx| WorkspaceShell::new(state, None, window, cx))
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
            shell.open_table_preview(
                sift_protocol::ProviderId::new("sift/postgres").unwrap(),
                "lab".into(),
                "people".into(),
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
            },
            ItemPresentation {
                id: 3,
                kind: ItemKind::Query,
                title: "three.sql".into(),
                dirty: false,
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
        });
        let window = shell_with_state(state, cx);
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        let workspace = window.root(&mut cx).unwrap();
        let pane = workspace.read_with(&cx, |shell, _| shell.panes[0].clone());

        pane.update(&mut cx, |pane, cx| pane.toggle_results_placement(1, cx));
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
            vec!["Settings", "Keymaps", "Themes", "Server Configuration"]
        );
        assert!(profile.iter().all(|item| item.command.is_none()));

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
            workspace.read_with(&cx, |workspace, _| workspace.toasts.last().cloned()),
            Some(Toast {
                message: "Restored workspace is no longer available".into()
            })
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
}
