use std::collections::HashMap;
use std::sync::Arc;

use gpui::{
    actions, div, prelude::*, px, App, Context, Entity, EventEmitter, FocusHandle, Focusable,
    IntoElement, MouseButton, Role, Subscription, Task, Window, WindowBounds,
};
use sift_api_types::RoomId;
use sift_ui::{TextInput, Theme};

use crate::editor::{EditorEvent, QueryDocument, QueryEditor};
use crate::results::{ResultState, ResultsView};

use crate::presentation::{
    DockPresentation, ItemKind, ItemPresentation, PanePresentation, PresentationState,
    PresentationStore, WindowPresentation, WorkspacePresentation,
};
use crate::{
    ConnectionNavEntry, LifecycleEvent, LifecycleProjection, PresenceEvent, RoomPresenceProjection,
    WorkspaceNavEntry,
};

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
        ConfirmCloseWithoutSaving,
        ToggleLeftDock,
        ToggleRightDock,
        ToggleBottomDock
    ]
);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandSpec {
    pub id: &'static str,
    pub label: &'static str,
    pub shortcut: &'static str,
    pub disabled_reason: Option<&'static str>,
}

impl CommandSpec {
    pub fn enabled(&self) -> bool {
        self.disabled_reason.is_none()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Dock {
    pub title: &'static str,
    pub presentation: DockPresentation,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Modal {
    CommandPalette,
    ServerConnection,
    ConfirmClose { title: String },
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SavedServerProfile {
    pub id: String,
    pub name: String,
    pub base_url: String,
    #[serde(default, skip_serializing)]
    pub has_saved_token: bool,
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
}

#[derive(Debug, Clone)]
pub enum InstanceManagerEvent {
    Profiles(Vec<SavedServerProfile>),
    Testing,
    Connected { name: String },
    Failed { message: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Toast {
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tooltip {
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusBar {
    pub connection: String,
    pub database: String,
    pub transaction: String,
    pub room: String,
    pub execution: String,
}

impl Default for StatusBar {
    fn default() -> Self {
        Self {
            connection: "Offline".into(),
            database: "No database".into(),
            transaction: "No transaction".into(),
            room: "Local workspace".into(),
            execution: "Ready".into(),
        }
    }
}

/// Events a pane emits upward to the workspace. A pane never mutates sibling
/// panes or the workspace's pane list directly; it asks its owner instead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PaneEvent {
    /// The pane was interacted with and should become the active pane.
    FocusRequested,
    /// The pane should be removed (its close control was used, or it emptied).
    CloseRequested,
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

/// Shell → executor. The executor owns the SDK client, session, and
/// connection; the shell only reports intent (connect / disconnect / run).
#[derive(Debug, Clone)]
pub enum ExecutorCommand {
    Connect {
        tenant_id: i64,
        profile_id: i64,
        name: String,
    },
    Disconnect,
    Execute {
        item_id: u64,
        sql: String,
    },
}

/// Executor → shell. Connection-state changes and query outcomes share one
/// channel so ordering (connect before its run's result) is preserved.
#[derive(Debug, Clone)]
pub enum ExecutorEvent {
    Connection(ConnectionStatus),
    Execution { item_id: u64, state: ResultState },
}

/// A pane owns its ordered items and focus handle. The workspace owns panes;
/// items never reach sideways into sibling panes.
pub struct Pane {
    id: u64,
    items: Vec<ItemPresentation>,
    active_item: usize,
    focus_handle: FocusHandle,
    theme: Theme,
    /// Live editor per query item. Not persisted — the document is server/Loro
    /// backed and rehydrated by reference, so the client stores no query text.
    editors: HashMap<u64, Entity<QueryEditor>>,
    /// The Data/Messages/Explain/History surface owned by each query item.
    results: HashMap<u64, Entity<ResultsView>>,
}

impl Pane {
    fn from_presentation(pane: PanePresentation, theme: Theme, cx: &mut Context<Self>) -> Self {
        let mut editors = HashMap::new();
        let mut results = HashMap::new();
        for item in pane
            .items
            .iter()
            .filter(|item| item.kind == ItemKind::Query)
        {
            let id = item.id;
            let document = QueryDocument::with_random_peer("");
            let editor = cx.new(|cx| QueryEditor::new(document, theme, cx));
            let result = cx.new(|cx| ResultsView::new(theme, cx));
            cx.subscribe(&editor, move |pane, _, event, cx| {
                pane.on_editor_event(id, event, cx);
            })
            .detach();
            editors.insert(id, editor);
            results.insert(id, result);
        }
        Self {
            id: pane.id,
            items: pane.items,
            active_item: pane.active_item,
            focus_handle: cx.focus_handle(),
            theme,
            editors,
            results,
        }
    }

    fn on_editor_event(&mut self, item_id: u64, event: &EditorEvent, cx: &mut Context<Self>) {
        match event {
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
        PanePresentation {
            id: self.id,
            items: self.items.clone(),
            active_item: self.active_item.min(self.items.len().saturating_sub(1)),
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
            .filter(|item| item.kind == ItemKind::Query)
            .and_then(|item| self.editors.get(&item.id))
            .map(|editor| editor.focus_handle(cx))
            .unwrap_or_else(|| self.focus_handle.clone())
    }
}

impl Focusable for Pane {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl EventEmitter<PaneEvent> for Pane {}

impl Pane {
    fn close_item(&mut self, index: usize, cx: &mut Context<Self>) {
        if index >= self.items.len() {
            return;
        }
        let removed = self.items.remove(index);
        self.editors.remove(&removed.id);
        self.results.remove(&removed.id);
        if self.active_item >= self.items.len() {
            self.active_item = self.items.len().saturating_sub(1);
        }
        if self.items.is_empty() {
            cx.emit(PaneEvent::CloseRequested);
        }
        cx.notify();
    }
}

impl gpui::Render for Pane {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = self.theme.colors;
        let active = self.active_item().cloned();
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
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .child(
                div()
                    .h(px(32.))
                    .flex()
                    .items_stretch()
                    .border_b_1()
                    .border_color(colors.border)
                    .bg(colors.surface)
                    .child(
                        div()
                            .flex()
                            .flex_1()
                            .min_w_0()
                            .items_stretch()
                            .overflow_hidden()
                            .children(self.items.iter().enumerate().map(|(index, item)| {
                                let dirty = if item.dirty { " ●" } else { "" };
                                let selected = index == self.active_item;
                                div()
                                    .id(("tab", item.id as usize))
                                    .flex()
                                    .items_center()
                                    .h_full()
                                    .border_r_1()
                                    .border_color(colors.border)
                                    .when(selected, |tab| tab.bg(colors.selected_surface))
                                    .child(
                                        div()
                                            .id(("tab-label", item.id as usize))
                                            .flex()
                                            .items_center()
                                            .h_full()
                                            .px_2()
                                            .when(!selected, |label| {
                                                label.text_color(colors.muted_text)
                                            })
                                            .hover(|label| label.text_color(colors.text))
                                            .on_click(cx.listener(move |pane, _, _, cx| {
                                                pane.active_item = index;
                                                cx.notify();
                                            }))
                                            .child(format!("{}{dirty}", item.title)),
                                    )
                                    .child(
                                        div()
                                            .id(("tab-close", item.id as usize))
                                            .flex()
                                            .items_center()
                                            .justify_center()
                                            .h_full()
                                            .px_1()
                                            .text_color(colors.muted_text)
                                            .hover(|close| {
                                                close.bg(colors.border).text_color(colors.text)
                                            })
                                            .on_click(cx.listener(move |pane, _, _, cx| {
                                                pane.close_item(index, cx);
                                            }))
                                            .child("×"),
                                    )
                            })),
                    )
                    .child(
                        div()
                            .id(("pane-close", self.id as usize))
                            .flex()
                            .items_center()
                            .justify_center()
                            .w(px(28.))
                            .h_full()
                            .border_l_1()
                            .border_color(colors.border)
                            .text_color(colors.muted_text)
                            .hover(|close| {
                                close.bg(colors.selected_surface).text_color(colors.text)
                            })
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(PaneEvent::CloseRequested)))
                            .child("⨯"),
                    ),
            )
            .child({
                let body = div().flex_1().min_h_0().flex().flex_col();
                match active {
                    Some(item) if item.kind == ItemKind::Query => {
                        match (self.editors.get(&item.id), self.results.get(&item.id)) {
                            (Some(editor), Some(result)) => body
                                .child(div().flex_1().min_h_0().child(editor.clone()))
                                .child(
                                    div()
                                        .h(px(240.))
                                        .min_h_0()
                                        .border_t_1()
                                        .border_color(colors.border)
                                        .child(result.clone()),
                                ),
                            _ => body
                                .child(div().p_4().child(format!("Query editor · {}", item.title))),
                        }
                    }
                    Some(item) => body.child(div().p_4().text_color(colors.muted_text).child(
                        match item.kind {
                            ItemKind::Schema => format!("Schema view · {}", item.title),
                            _ => "Welcome to Sift".into(),
                        },
                    )),
                    None => body.child(
                        div()
                            .p_4()
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
    palette_selected: usize,
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
    modal: Option<Modal>,
    toasts: Vec<Toast>,
    tooltip: Option<Tooltip>,
    status: StatusBar,
    lifecycle: LifecycleProjection,
    presence: RoomPresenceProjection,
    _lifecycle_task: Option<Task<()>>,
    _presence_task: Option<Task<()>>,
    _executor_task: Option<Task<()>>,
    _instance_task: Option<Task<()>>,
    executor_sender: Option<tokio::sync::mpsc::UnboundedSender<ExecutorCommand>>,
    instance_sender: Option<tokio::sync::mpsc::UnboundedSender<InstanceCommand>>,
    saved_servers: Vec<SavedServerProfile>,
    selected_server_profile: Option<String>,
    remember_server_token: bool,
    server_connection_pending: bool,
    server_connection_error: Option<String>,
    connection_status: ConnectionStatus,
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
        // Re-render the palette as the search text changes so its list filters.
        cx.observe(&query_input, |shell, _, cx| {
            shell.palette_selected = 0;
            cx.notify();
        })
        .detach();
        panes[active_pane]
            .read(cx)
            .active_focus_handle(cx)
            .focus(window, cx);
        let bounds_subscription = cx.observe_window_bounds(window, |shell, window, cx| {
            shell.capture_window_bounds(window.window_bounds());
            shell.persist(cx);
        });
        Self {
            focus_handle: cx.focus_handle(),
            query_input,
            server_name_input,
            server_url_input,
            server_token_input,
            palette_selected: 0,
            theme,
            dark_theme: state.dark_theme,
            window_presentation,
            panes,
            active_pane,
            selected_workspace_id,
            selected_instance_id,
            left_dock: Dock {
                title: "Connections",
                presentation: workspace.left_dock,
            },
            right_dock: Dock {
                title: "Inspector",
                presentation: workspace.right_dock,
            },
            bottom_dock: Dock {
                title: "Results",
                presentation: workspace.bottom_dock,
            },
            modal: None,
            toasts: Vec::new(),
            tooltip: None,
            status: StatusBar::default(),
            lifecycle: LifecycleProjection::default(),
            presence: RoomPresenceProjection::default(),
            _lifecycle_task: None,
            _presence_task: None,
            _executor_task: None,
            _instance_task: None,
            executor_sender: None,
            instance_sender: None,
            saved_servers: Vec::new(),
            selected_server_profile: None,
            remember_server_token: true,
            server_connection_pending: false,
            server_connection_error: None,
            connection_status: ConnectionStatus::Disconnected,
            store,
            _bounds_subscription: bounds_subscription,
            next_id,
        }
    }

    pub fn command_specs(&self, cx: &App) -> Vec<CommandSpec> {
        let has_item = self
            .panes
            .get(self.active_pane)
            .is_some_and(|pane| pane.read(cx).active_item().is_some());
        let has_extra_pane = self.panes.len() > 1;
        let no_item = (!has_item).then_some("No active item");
        vec![
            CommandSpec {
                id: "instance.connect-server",
                label: "Connect to Server…",
                shortcut: "",
                disabled_reason: None,
            },
            CommandSpec {
                id: "workspace.split-pane",
                label: "Split Pane",
                shortcut: "Ctrl+\\",
                disabled_reason: None,
            },
            CommandSpec {
                id: "workspace.focus-next-pane",
                label: "Focus Next Pane",
                shortcut: "Ctrl+K Ctrl+→",
                disabled_reason: (!has_extra_pane).then_some("Only one pane"),
            },
            CommandSpec {
                id: "workspace.close-pane",
                label: "Close Pane",
                shortcut: "Ctrl+Shift+W",
                disabled_reason: (!has_extra_pane).then_some("Only one pane"),
            },
            CommandSpec {
                id: "workspace.save-item",
                label: "Save Active Item",
                shortcut: "Ctrl+S",
                disabled_reason: no_item,
            },
            CommandSpec {
                id: "workspace.close-item",
                label: "Close Active Item",
                shortcut: "Ctrl+W",
                disabled_reason: no_item,
            },
            CommandSpec {
                id: "workspace.toggle-left-dock",
                label: "Toggle Connections Dock",
                shortcut: "Ctrl+Shift+B",
                disabled_reason: None,
            },
            CommandSpec {
                id: "workspace.toggle-right-dock",
                label: "Toggle Inspector Dock",
                shortcut: "Ctrl+Shift+I",
                disabled_reason: None,
            },
            CommandSpec {
                id: "workspace.toggle-bottom-dock",
                label: "Toggle Results Dock",
                shortcut: "Ctrl+J",
                disabled_reason: None,
            },
        ]
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
                        if let LifecycleEvent::Selected(instance) = &event {
                            instance_changed =
                                shell.selected_instance_id.as_deref() != Some(instance.id.as_str());
                            shell.selected_instance_id = Some(instance.id.clone());
                            if instance_changed {
                                shell.selected_workspace_id = None;
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
                self.toasts.push(Toast {
                    message: format!("Connected to {name}"),
                });
            }
            InstanceManagerEvent::Failed { message } => {
                self.server_connection_pending = false;
                self.server_connection_error = Some(message);
            }
        }
        cx.notify();
    }

    fn on_executor_event(&mut self, event: ExecutorEvent, cx: &mut Context<Self>) {
        match event {
            ExecutorEvent::Connection(status) => {
                self.status.database = match &status {
                    ConnectionStatus::Connected { name, .. } => name.clone(),
                    ConnectionStatus::Connecting { .. } => "Connecting…".into(),
                    ConnectionStatus::Failed { .. } => "Connection failed".into(),
                    ConnectionStatus::Disconnected => "No database".into(),
                };
                self.connection_status = status;
                cx.notify();
            }
            ExecutorEvent::Execution { item_id, state } => self.route_result(item_id, state, cx),
        }
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
            cx.notify();
        }
    }

    fn disconnect(&mut self, cx: &mut Context<Self>) {
        if let Some(sender) = &self.executor_sender {
            let _ = sender.send(ExecutorCommand::Disconnect);
        }
        self.connection_status = ConnectionStatus::Disconnected;
        self.status.database = "No database".into();
        cx.notify();
    }

    /// Deliver an execution outcome to whichever pane owns the query item.
    fn route_result(&mut self, item_id: u64, state: ResultState, cx: &mut Context<Self>) {
        for pane in &self.panes {
            if pane.update(cx, |pane, cx| pane.set_result(item_id, state.clone(), cx)) {
                break;
            }
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
            self.toasts.push(Toast {
                message: "Restored workspace is no longer available".into(),
            });
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
        let pane = cx.new(|cx| {
            Pane::from_presentation(
                PanePresentation {
                    id,
                    items: vec![ItemPresentation {
                        id,
                        kind: ItemKind::Welcome,
                        title: "New pane".into(),
                        dirty: false,
                    }],
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
            PaneEvent::ExecuteRequested { item_id, sql } => match &self.executor_sender {
                Some(sender) => {
                    let _ = sender.send(ExecutorCommand::Execute {
                        item_id: *item_id,
                        sql: sql.clone(),
                    });
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
                self.modal = Some(Modal::ConfirmClose {
                    title: item.title.clone(),
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
                    pane.items.remove(pane.active_item);
                    pane.active_item = pane.active_item.min(pane.items.len().saturating_sub(1));
                }
            });
        }
        self.modal = None;
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
        let close_after_save = matches!(self.modal, Some(Modal::ConfirmClose { .. }));
        self.mark_active_item_dirty(false, cx);
        self.toasts.push(Toast {
            message: "Presentation saved".into(),
        });
        if close_after_save {
            self.remove_active_item(window, cx);
        } else {
            self.modal = None;
            self.persist(cx);
            cx.notify();
        }
    }

    fn confirm_close_without_saving(
        &mut self,
        _: &ConfirmCloseWithoutSaving,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.remove_active_item(window, cx);
    }

    fn open_command_palette(
        &mut self,
        _: &OpenCommandPalette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.modal = Some(Modal::CommandPalette);
        self.palette_selected = 0;
        self.query_input.focus_handle(cx).focus(window, cx);
        cx.notify();
    }

    fn open_server_connection(
        &mut self,
        _: &OpenServerConnection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.modal = Some(Modal::ServerConnection);
        self.server_connection_error = None;
        self.server_connection_pending = false;
        self.server_name_input.focus_handle(cx).focus(window, cx);
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
        cx.notify();
    }

    fn palette_down(&mut self, _: &PaletteDown, _: &mut Window, cx: &mut Context<Self>) {
        if self.modal != Some(Modal::CommandPalette) {
            return;
        }
        let last = self.filtered_commands(cx).len().saturating_sub(1);
        self.palette_selected = (self.palette_selected + 1).min(last);
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
        self.modal = None;
        // Return focus to the workspace so keybindings keep routing.
        self.focus_active_pane(window, cx);
        cx.notify();
    }

    /// Run a command palette entry by its stable id: dismiss the palette, then
    /// dispatch the matching workspace action. Ids come from `command_specs`.
    fn run_command(&mut self, id: &'static str, window: &mut Window, cx: &mut Context<Self>) {
        self.dismiss_modal(&DismissModal, window, cx);
        match id {
            "instance.connect-server" => {
                self.open_server_connection(&OpenServerConnection, window, cx)
            }
            "workspace.split-pane" => self.split_pane(&SplitPane, window, cx),
            "workspace.focus-next-pane" => self.focus_next_pane(&FocusNextPane, window, cx),
            "workspace.close-pane" => self.close_active_pane(&CloseActivePane, window, cx),
            "workspace.save-item" => self.save_active_item(&SaveActiveItem, window, cx),
            "workspace.close-item" => self.close_active_item(&CloseActiveItem, window, cx),
            "workspace.toggle-left-dock" => self.toggle_left_dock(&ToggleLeftDock, window, cx),
            "workspace.toggle-right-dock" => self.toggle_right_dock(&ToggleRightDock, window, cx),
            "workspace.toggle-bottom-dock" => {
                self.toggle_bottom_dock(&ToggleBottomDock, window, cx)
            }
            _ => {}
        }
    }

    fn toggle_left_dock(&mut self, _: &ToggleLeftDock, _: &mut Window, cx: &mut Context<Self>) {
        self.left_dock.presentation.open = !self.left_dock.presentation.open;
        self.persist(cx);
        cx.notify();
    }

    fn toggle_right_dock(&mut self, _: &ToggleRightDock, _: &mut Window, cx: &mut Context<Self>) {
        self.right_dock.presentation.open = !self.right_dock.presentation.open;
        self.persist(cx);
        cx.notify();
    }

    fn toggle_bottom_dock(&mut self, _: &ToggleBottomDock, _: &mut Window, cx: &mut Context<Self>) {
        self.bottom_dock.presentation.open = !self.bottom_dock.presentation.open;
        self.persist(cx);
        cx.notify();
    }

    /// Compact, blocky top toolbar in the Zed idiom. The only interactive
    /// control is the hamburger, which opens the command palette; the brand,
    /// workspace label, and connection indicator are read-only. Controls whose
    /// behavior is not built yet are omitted rather than shown as dead buttons.
    fn render_toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = self.theme.colors;
        let workspace_label = self.workspace_label();

        // Read-only connection/sync indicator driven by the lifecycle phase.
        let (update_glyph, update_label, update_color) = match &self.lifecycle.phase {
            crate::ConnectionPhase::Ready => ("●", "Connected".to_string(), colors.success),
            crate::ConnectionPhase::Degraded(_) => {
                ("!", self.lifecycle.status_label(), colors.warning)
            }
            crate::ConnectionPhase::Offline => ("○", "Offline".to_string(), colors.muted_text),
            crate::ConnectionPhase::Reconnecting { .. } => {
                ("⟳", self.lifecycle.status_label(), colors.warning)
            }
            _ => ("⟳", self.lifecycle.status_label(), colors.accent),
        };

        div()
            .id("integrated-titlebar")
            .key_context("SiftWindow")
            .h(px(34.))
            .flex()
            .items_center()
            .justify_between()
            .pl_1()
            .pr_2()
            .gap_2()
            .border_b_1()
            .border_color(colors.border)
            .bg(colors.surface)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .min_w_0()
                    // The only interactive control: opens the command palette.
                    .child(
                        div()
                            .id("toolbar-menu")
                            .role(Role::Button)
                            .aria_label("Open command palette")
                            .w(px(28.))
                            .h(px(28.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded_sm()
                            .text_color(colors.muted_text)
                            .hover(|slot| slot.bg(colors.selected_surface).text_color(colors.text))
                            .on_click(cx.listener(|shell, _, window, cx| {
                                shell.open_command_palette(&OpenCommandPalette, window, cx)
                            }))
                            .child("☰"),
                    )
                    .child(div().font_weight(gpui::FontWeight::SEMIBOLD).child("sift"))
                    // Current workspace — a static label, not a switcher yet.
                    .child(
                        div()
                            .min_w_0()
                            .text_sm()
                            .text_color(colors.muted_text)
                            .truncate()
                            .child(workspace_label),
                    ),
            )
            .child(
                div()
                    .id("toolbar-status")
                    .aria_label(update_label.clone())
                    .flex()
                    .items_center()
                    .gap_1()
                    .h(px(24.))
                    .px_2()
                    .text_xs()
                    .text_color(colors.muted_text)
                    .child(div().text_color(update_color).child(update_glyph))
                    .child(update_label),
            )
    }

    fn render_dock(&self, dock: &Dock, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = self.theme.colors;
        div()
            .id(dock.title)
            .key_context("SiftDock")
            .w(px(dock.presentation.size))
            .flex()
            .flex_col()
            .p_3()
            .gap_2()
            .border_r_1()
            .border_color(colors.border)
            .bg(colors.surface)
            .text_sm()
            .child(
                div()
                    .text_xs()
                    .text_color(colors.muted_text)
                    .child(dock.title.to_uppercase()),
            )
            .when(dock.title == "Connections", |dock_view| {
                let selected = self.selected_workspace_id;
                let mut rows: Vec<gpui::AnyElement> = Vec::new();
                for tenant in &self.lifecycle.tenants {
                    rows.push(
                        div()
                            .mt_2()
                            .text_color(colors.muted_text)
                            .child(tenant.name.clone())
                            .into_any_element(),
                    );
                    for conn in &tenant.connections {
                        let (dot, connected) = match &self.connection_status {
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
                        let mut row = div()
                            .id(("conn", conn.id as usize))
                            .flex()
                            .items_center()
                            .gap_1()
                            .pl_2()
                            .py_1()
                            .rounded_sm()
                            .when(connected, |row| row.bg(colors.selected_surface))
                            .hover(|row| row.bg(colors.selected_surface))
                            .child(div().text_color(dot).child("●"))
                            .child(div().flex_1().min_w_0().truncate().child(conn.name.clone()));
                        if connected {
                            row = row.child(
                                div()
                                    .id(("disconnect", conn.id as usize))
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
                        rows.push(row.into_any_element());
                    }
                    for room in &tenant.rooms {
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
                                    .pl_2()
                                    .py_1()
                                    .rounded_sm()
                                    .when(is_open, |row| {
                                        row.bg(colors.selected_surface).text_color(colors.text)
                                    })
                                    .hover(|row| row.bg(colors.selected_surface))
                                    .on_click(cx.listener(move |shell, _, _, cx| {
                                        shell.open_workspace(&entry, cx)
                                    }))
                                    .child(format!("{} / {}{features}", room.name, workspace.name))
                                    .into_any_element(),
                            );
                        }
                    }
                }
                dock_view.children(rows)
            })
            .when(
                dock.title == "Connections" && self.lifecycle.tenants.is_empty(),
                |dock_view| {
                    dock_view.child(
                        div()
                            .text_color(colors.muted_text)
                            .child(self.lifecycle.status_label()),
                    )
                },
            )
            .when(dock.title == "Inspector", |dock_view| {
                dock_view
                    .child(format!("{} participants", self.presence.participants.len()))
                    .child(match self.presence.followed_attachment {
                        Some(attachment) => format!("Following attachment {attachment}"),
                        None => "Follow mode off".into(),
                    })
            })
    }

    fn render_modal(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let colors = self.theme.colors;
        self.modal.as_ref().map(|modal| {
            let content = match modal {
                Modal::CommandPalette => {
                    let query = self.query_input.read(cx).text().to_lowercase();
                    let commands = self.filtered_commands(cx);
                    let selected_idx = self.palette_selected.min(commands.len().saturating_sub(1));
                    div()
                        .flex()
                        .flex_col()
                        .child(
                            // Search field with a divider under it, Zed-style.
                            div()
                                .pb_2()
                                .mb_1()
                                .border_b_1()
                                .border_color(colors.border)
                                .child(self.query_input.clone()),
                        )
                        .when(commands.is_empty(), |palette| {
                            palette.child(
                                div()
                                    .px_2()
                                    .py_1()
                                    .text_color(colors.muted_text)
                                    .child("No matching commands"),
                            )
                        })
                        .child(
                            div()
                                .id("command-list")
                                .flex()
                                .flex_col()
                                .gap_px()
                                .max_h(px(360.))
                                .overflow_y_scroll()
                                .children(commands.into_iter().enumerate().map(
                                    |(idx, command)| {
                                        let enabled = command.enabled();
                                        let id = command.id;
                                        let selected = idx == selected_idx;
                                        let right =
                                            command.disabled_reason.unwrap_or(command.shortcut);
                                        let mut row = div()
                                            .id(id)
                                            .flex()
                                            .items_center()
                                            .justify_between()
                                            .gap_2()
                                            .h(px(30.))
                                            .px_2()
                                            .rounded_sm()
                                            .when(selected && enabled, |row| {
                                                row.bg(colors.selected_surface)
                                            })
                                            .when(!enabled, |row| row.text_color(colors.muted_text))
                                            .child(highlight_match(
                                                command.label,
                                                &query,
                                                colors.accent,
                                            ))
                                            .child(
                                                div()
                                                    .flex_none()
                                                    .text_xs()
                                                    .text_color(colors.muted_text)
                                                    .child(right),
                                            );
                                        if enabled {
                                            row = row
                                                .hover(|row| row.bg(colors.selected_surface))
                                                .on_click(cx.listener(
                                                    move |shell, _, window, cx| {
                                                        shell.run_command(id, window, cx)
                                                    },
                                                ));
                                        }
                                        row
                                    },
                                )),
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
                                .when(active, |row| row.bg(colors.selected_surface))
                                .hover(|row| row.bg(colors.selected_surface))
                                .on_click(cx.listener(move |shell, _, window, cx| {
                                    shell.select_server_profile(&profile_for_click, window, cx)
                                }))
                                .child(
                                    div()
                                        .flex()
                                        .flex_col()
                                        .min_w_0()
                                        .child(profile.name.clone())
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
                        .flex()
                        .flex_col()
                        .gap_3()
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .justify_between()
                                .child(
                                    div()
                                        .font_weight(gpui::FontWeight::SEMIBOLD)
                                        .child("Connect to Sift Server"),
                                )
                                .child(
                                    div()
                                        .id("new-server-profile")
                                        .px_2()
                                        .py_1()
                                        .rounded_sm()
                                        .text_color(colors.muted_text)
                                        .hover(|button| {
                                            button
                                                .bg(colors.selected_surface)
                                                .text_color(colors.text)
                                        })
                                        .on_click(cx.listener(|shell, _, window, cx| {
                                            shell.new_server_profile(window, cx)
                                        }))
                                        .child("New Server"),
                                ),
                        )
                        .child(
                            div()
                                .id("use-local-sift")
                                .flex()
                                .items_center()
                                .justify_between()
                                .px_2()
                                .py_1()
                                .rounded_sm()
                                .hover(|row| row.bg(colors.selected_surface))
                                .on_click(cx.listener(|shell, _, _, cx| shell.use_local_server(cx)))
                                .child("Local Sift")
                                .child(
                                    div()
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
                                .gap_1()
                                .child(div().text_xs().text_color(colors.muted_text).child("NAME"))
                                .child(
                                    div()
                                        .border_1()
                                        .border_color(colors.border)
                                        .rounded_sm()
                                        .child(self.server_name_input.clone()),
                                ),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap_1()
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(colors.muted_text)
                                        .child("SERVER URL"),
                                )
                                .child(
                                    div()
                                        .border_1()
                                        .border_color(colors.border)
                                        .rounded_sm()
                                        .child(self.server_url_input.clone()),
                                ),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap_1()
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(colors.muted_text)
                                        .child("BEARER TOKEN"),
                                )
                                .child(
                                    div()
                                        .border_1()
                                        .border_color(colors.border)
                                        .rounded_sm()
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
                                .items_center()
                                .gap_2()
                                .cursor_pointer()
                                .on_click(cx.listener(|shell, _, _, cx| {
                                    shell.remember_server_token = !shell.remember_server_token;
                                    cx.notify();
                                }))
                                .child(if remember { "☑" } else { "☐" })
                                .child("Remember token in the OS keychain"),
                        )
                        .children(self.server_connection_error.as_ref().map(|message| {
                            div()
                                .p_2()
                                .rounded_sm()
                                .bg(colors.surface)
                                .text_color(colors.danger)
                                .child(message.clone())
                        }))
                        .child(
                            div()
                                .flex()
                                .justify_between()
                                .items_center()
                                .child(
                                    div()
                                        .id("forget-server")
                                        .px_2()
                                        .py_1()
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
                                        .px_3()
                                        .py_1()
                                        .rounded_sm()
                                        .bg(if pending {
                                            colors.surface
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
                Modal::ConfirmClose { title } => div()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child(format!("Save changes to {title}?"))
                    .child("Use Save, Close Without Saving, or Escape.")
                    .into_any_element(),
            };
            div()
                .id("modal-layer")
                .key_context("SiftModal")
                .absolute()
                .inset_0()
                .flex()
                .items_start()
                .justify_center()
                .pt(px(100.))
                .bg(colors.scrim)
                .child(
                    div()
                        .w(px(520.))
                        .p_4()
                        .rounded_lg()
                        .border_1()
                        .border_color(colors.border)
                        .bg(colors.elevated_surface)
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

impl gpui::Render for WorkspaceShell {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
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
        div()
            .id("sift-shell")
            .key_context("SiftWorkspace")
            .role(Role::Application)
            .aria_label("Sift database workspace")
            .track_focus(&self.focus_handle)
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
            .on_action(cx.listener(Self::confirm_close_without_saving))
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
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .children(left_dock)
                    .child(
                        div()
                            .flex()
                            .flex_1()
                            .min_w_0()
                            .children(self.panes.iter().cloned()),
                    )
                    .children(right_dock),
            )
            .when(self.bottom_dock.presentation.open, |shell| {
                shell.child(
                    div()
                        .h(px(self.bottom_dock.presentation.size.min(160.0)))
                        .px_3()
                        .py_2()
                        .border_t_1()
                        .border_color(colors.border)
                        .bg(colors.surface)
                        .text_sm()
                        .text_color(colors.muted_text)
                        .child("Query output opens with each query, beside its editor."),
                )
            })
            .child(
                div()
                    .id("status-bar")
                    .h(px(26.))
                    .flex()
                    .items_center()
                    .justify_between()
                    .px_3()
                    .border_t_1()
                    .border_color(colors.border)
                    .bg(colors.surface)
                    .text_xs()
                    .text_color(colors.muted_text)
                    .child(format!(
                        "{} · {} · {}",
                        self.status.connection, self.status.database, self.status.transaction
                    ))
                    .child(format!("{} · {}", self.status.room, self.status.execution)),
            )
            .children(self.toasts.last().map(|toast| {
                div()
                    .id("toast")
                    .absolute()
                    .right_3()
                    .bottom(px(38.))
                    .p_3()
                    .rounded_md()
                    .border_1()
                    .border_color(colors.border)
                    .bg(colors.elevated_surface)
                    .child(toast.message.clone())
            }))
            .children(self.tooltip.as_ref().map(|tooltip| {
                div()
                    .id("tooltip")
                    .absolute()
                    .right_3()
                    .top(px(44.))
                    .px_2()
                    .py_1()
                    .rounded_sm()
                    .bg(colors.elevated_surface)
                    .child(tooltip.message.clone())
            }))
            .children(self.render_modal(cx))
    }
}

/// Render `label` with the case-insensitive `query` substring emphasized in the
/// accent color, like a fuzzy-finder match highlight.
fn highlight_match(label: &'static str, query: &str, accent: gpui::Hsla) -> impl IntoElement {
    let base = div().flex().min_w_0();
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
    use gpui::{TestAppContext, VisualTestContext};

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
    fn dirty_item_close_and_save_require_explicit_choice(cx: &mut TestAppContext) {
        let window = shell(cx);
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        let workspace = window.root(&mut cx).unwrap();
        workspace.update(&mut cx, |workspace, cx| {
            workspace.mark_active_item_dirty(true, cx)
        });
        let focus = workspace.read_with(&cx, |shell, cx| shell.focus_handle(cx));
        cx.update(|window, cx| focus.dispatch_action(&CloseActiveItem, window, cx));
        assert!(matches!(
            workspace.read_with(&cx, |workspace, _| workspace.modal().cloned()),
            Some(Modal::ConfirmClose { .. })
        ));
        cx.update(|window, cx| focus.dispatch_action(&SaveActiveItem, window, cx));
        assert_eq!(
            workspace.read_with(&cx, |workspace, cx| workspace.active_item_dirty(cx)),
            None
        );
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
        // Arrow down to "Toggle Connections Dock" (index 6) and run it.
        for _ in 0..6 {
            cx.update(|window, cx| focus.dispatch_action(&PaletteDown, window, cx));
        }
        cx.update(|window, cx| focus.dispatch_action(&PaletteConfirm, window, cx));
        assert!(!workspace.read_with(&cx, |shell, _| shell.left_dock.presentation.open));
        assert!(workspace.read_with(&cx, |shell, _| shell.modal().is_none()));
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
    fn palette_command_dispatches_action_and_closes(cx: &mut TestAppContext) {
        let window = shell(cx);
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        let workspace = window.root(&mut cx).unwrap();
        assert!(workspace.read_with(&cx, |shell, _| shell.left_dock.presentation.open));
        workspace.update_in(&mut cx, |shell, window, cx| {
            shell.modal = Some(Modal::CommandPalette);
            shell.run_command("workspace.toggle-left-dock", window, cx);
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
            InstanceCommand::UseLocal | InstanceCommand::Forget { .. } => {
                panic!("expected connect command")
            }
        }
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
}
