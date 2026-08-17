use std::collections::HashMap;
use std::ops::Range;
use std::sync::Arc;

use gpui::{
    actions, deferred, div, img, prelude::*, px, uniform_list, App, Context, CursorStyle, Entity,
    EventEmitter, FocusHandle, Focusable, IntoElement, MouseButton, ResizeEdge, Role,
    ScrollStrategy, Subscription, Task, UniformListScrollHandle, Window, WindowBounds,
    WindowControlArea,
};
use sift_api_types::RoomId;
use sift_ui::{database_logo, icon, IconName, TextInput, Theme};

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

const PALETTE_VISIBLE_ROWS: usize = 10;
const PALETTE_ROW_HEIGHT: f32 = 30.0;

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
    ServerPicker,
    ServerConnection,
    Account,
    DatabaseConnection,
    ConfirmClose { title: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AppBarMenu {
    Main,
    File,
    Edit,
    Selection,
    View,
    Go,
    Run,
    Window,
    Help,
    Profile,
}

#[derive(Debug, Clone, Copy)]
struct AppBarMenuItem {
    label: &'static str,
    shortcut: &'static str,
    command: Option<&'static str>,
}

impl AppBarMenuItem {
    const fn available(label: &'static str, shortcut: &'static str, command: &'static str) -> Self {
        Self {
            label,
            shortcut,
            command: Some(command),
        }
    }

    const fn unimplemented(label: &'static str) -> Self {
        Self {
            label,
            shortcut: "",
            command: None,
        }
    }
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
#[derive(Clone)]
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
    CreateConnectionProfile {
        tenant_id: i64,
        name: String,
        provider_id: sift_protocol::ProviderId,
        configuration: serde_json::Value,
        credentials: Option<serde_json::Value>,
    },
}

/// Executor → shell. Connection-state changes and query outcomes share one
/// channel so ordering (connect before its run's result) is preserved.
#[derive(Debug, Clone)]
pub enum ExecutorEvent {
    Connection(ConnectionStatus),
    Execution {
        item_id: u64,
        state: ResultState,
    },
    ProfileCreated {
        entry: ConnectionNavEntry,
        connection_error: Option<String>,
    },
    ProfileCreationFailed(String),
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
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = self.theme.colors;
        let is_focused = self.active_focus_handle(cx).is_focused(window)
            || self.focus_handle.contains_focused(window, cx);
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
            .border_t_1()
            .border_color(if is_focused {
                colors.strong_border
            } else {
                colors.subtle_border
            })
            .bg(colors.background)
            .child(
                div()
                    .h(self.theme.metrics.tab_height)
                    .flex_none()
                    .flex()
                    .items_stretch()
                    .relative()
                    .bg(colors.toolbar)
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
                                            div()
                                                .id(("tab", item.id as usize))
                                                .flex_none()
                                                .flex()
                                                .items_center()
                                                .h_full()
                                                .min_w(px(96.))
                                                .max_w(px(220.))
                                                .when(selected, |tab| {
                                                    tab.bg(colors.background)
                                                        .border_l_1()
                                                        .border_r_1()
                                                        .border_color(colors.subtle_border)
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
                                                            move |pane, _, _, cx| {
                                                                pane.active_item = index;
                                                                cx.notify();
                                                            },
                                                        ))
                                                        .child(
                                                            div()
                                                                .flex_1()
                                                                .min_w_0()
                                                                .truncate()
                                                                .child(item.title.clone()),
                                                        )
                                                        .when(item.dirty, |label| {
                                                            label.child(
                                                                div()
                                                                    .flex_none()
                                                                    .ml_1()
                                                                    .size(px(5.))
                                                                    .rounded_full()
                                                                    .bg(colors.accent),
                                                            )
                                                        }),
                                                )
                                                .child(
                                                    div()
                                                        .id(("tab-close", item.id as usize))
                                                        .flex_none()
                                                        .flex()
                                                        .items_center()
                                                        .justify_center()
                                                        .h_full()
                                                        .w(px(22.))
                                                        .text_color(colors.muted_text)
                                                        .hover(|close| {
                                                            close
                                                                .bg(colors.hovered_surface)
                                                                .text_color(colors.text)
                                                        })
                                                        .on_click(cx.listener(
                                                            move |pane, _, _, cx| {
                                                                pane.close_item(index, cx);
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
                    .child(
                        div()
                            .id(("pane-close", self.id as usize))
                            .flex()
                            .items_center()
                            .justify_center()
                            .w(px(28.))
                            .h_full()
                            .border_l_1()
                            .border_color(colors.subtle_border)
                            .text_color(colors.muted_text)
                            .hover(|close| close.bg(colors.hovered_surface).text_color(colors.text))
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(PaneEvent::CloseRequested)))
                            .child(icon(IconName::Close, colors.muted_text, 14.)),
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
                                        .border_color(colors.subtle_border)
                                        .child(result.clone()),
                                ),
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
                    Some(item) => body.child(
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
                                    .child(match item.kind {
                                        ItemKind::Schema => {
                                            format!("Schema view · {}", item.title)
                                        }
                                        _ => "Open a connection or create a query to begin.".into(),
                                    }),
                            ),
                    ),
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
    selected_server_profile: Option<String>,
    remember_server_token: bool,
    server_connection_pending: bool,
    server_connection_error: Option<String>,
    account_pending: bool,
    account_error: Option<String>,
    connection_status: ConnectionStatus,
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
            shell.capture_window_bounds(window.window_bounds());
            shell.persist(cx);
        });
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
            palette_selected: 0,
            palette_scroll_handle: UniformListScrollHandle::new(),
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
            selected_server_profile: None,
            remember_server_token: true,
            server_connection_pending: false,
            server_connection_error: None,
            account_pending: false,
            account_error: None,
            connection_status: ConnectionStatus::Disconnected,
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
                id: "query.execute-statement",
                label: "Run Query Statement",
                shortcut: "Ctrl+Enter",
                disabled_reason: no_item,
            },
            CommandSpec {
                id: "query.execute-document",
                label: "Run Query Document",
                shortcut: "Ctrl+Shift+Enter",
                disabled_reason: no_item,
            },
            CommandSpec {
                id: "query.undo",
                label: "Undo Query Edit",
                shortcut: "Ctrl+Z",
                disabled_reason: no_item,
            },
            CommandSpec {
                id: "query.redo",
                label: "Redo Query Edit",
                shortcut: "Ctrl+Shift+Z",
                disabled_reason: no_item,
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

    fn render_account_avatar(&self, size: f32) -> gpui::AnyElement {
        let colors = self.theme.colors;
        let avatar = div()
            .relative()
            .size(px(size))
            .flex()
            .items_center()
            .justify_center()
            .overflow_hidden()
            .rounded(px(4.))
            .border_1()
            .border_color(colors.strong_border)
            .bg(colors.accent_muted)
            .text_xs()
            .font_weight(gpui::FontWeight::SEMIBOLD);
        match &self.lifecycle.identity {
            Some(identity) => avatar
                .child(self.account_initials())
                .children(identity.principal.avatar_url.clone().map(|url| {
                    img(url)
                        .absolute()
                        .inset_0()
                        .size_full()
                        .object_fit(gpui::ObjectFit::Cover)
                }))
                .into_any_element(),
            None => avatar
                .child(icon(IconName::User, colors.muted_text, size * 0.55))
                .into_any_element(),
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
                self.show_toast(format!("Connected to {name}"), cx);
            }
            InstanceManagerEvent::Failed { message } => {
                self.server_connection_pending = false;
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
                self.database_connection_error = Some(message);
                cx.notify();
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
                let mut engine = serde_json::Map::new();
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
                if !engine.is_empty() {
                    configuration.insert("engine_specific".into(), engine.into());
                }
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
        self.show_toast("Presentation saved".into(), cx);
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

    fn open_server_picker(&mut self, cx: &mut Context<Self>) {
        self.open_app_bar_modal(Modal::ServerPicker, cx);
        self.server_connection_error = None;
    }

    fn open_account(&mut self, cx: &mut Context<Self>) {
        self.open_app_bar_modal(Modal::Account, cx);
        self.account_error = None;
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
            .is_some_and(|instance| instance.kind == crate::InstanceKind::Local)
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
    fn run_command(&mut self, id: &'static str, window: &mut Window, cx: &mut Context<Self>) {
        self.dismiss_modal(&DismissModal, window, cx);
        match id {
            "instance.connect-server" => {
                self.open_server_connection(&OpenServerConnection, window, cx)
            }
            "query.execute-statement" => {
                self.dispatch_active_editor_action(&crate::editor::ExecuteStatement, window, cx)
            }
            "query.execute-document" => {
                self.dispatch_active_editor_action(&crate::editor::ExecuteDocument, window, cx)
            }
            "query.undo" => self.dispatch_active_editor_action(&crate::editor::Undo, window, cx),
            "query.redo" => self.dispatch_active_editor_action(&crate::editor::Redo, window, cx),
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

    fn toggle_app_bar_menu(&mut self, menu: AppBarMenu, cx: &mut Context<Self>) {
        self.close_app_bar_modal(cx);
        self.app_bar_menu = (self.app_bar_menu != Some(menu)).then_some(menu);
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

    fn app_bar_menu_items(menu: AppBarMenu) -> Vec<AppBarMenuItem> {
        use AppBarMenuItem as Item;
        match menu {
            AppBarMenu::Main => vec![
                Item::unimplemented("About Sift"),
                Item::unimplemented("Check for Updates…"),
                Item::available("Quit Sift", "", "window.quit"),
            ],
            AppBarMenu::File => vec![
                Item::unimplemented("New Query"),
                Item::unimplemented("Open…"),
                Item::available("Save Active Item", "Ctrl+S", "workspace.save-item"),
                Item::available("Close Active Item", "Ctrl+W", "workspace.close-item"),
            ],
            AppBarMenu::Edit => vec![
                Item::available("Undo", "Ctrl+Z", "query.undo"),
                Item::available("Redo", "Ctrl+Shift+Z", "query.redo"),
                Item::unimplemented("Cut"),
                Item::unimplemented("Copy"),
                Item::unimplemented("Paste"),
            ],
            AppBarMenu::Selection => vec![
                Item::unimplemented("Select All"),
                Item::unimplemented("Expand Selection"),
                Item::unimplemented("Shrink Selection"),
                Item::unimplemented("Add Cursor Above"),
                Item::unimplemented("Add Cursor Below"),
            ],
            AppBarMenu::View => vec![
                Item::available(
                    "Connections Dock",
                    "Ctrl+Shift+B",
                    "workspace.toggle-left-dock",
                ),
                Item::available(
                    "Inspector Dock",
                    "Ctrl+Shift+I",
                    "workspace.toggle-right-dock",
                ),
                Item::available("Results Dock", "Ctrl+J", "workspace.toggle-bottom-dock"),
                Item::unimplemented("Appearance"),
                Item::unimplemented("Full Screen"),
            ],
            AppBarMenu::Go => vec![
                Item::available(
                    "Focus Next Pane",
                    "Ctrl+K Ctrl+→",
                    "workspace.focus-next-pane",
                ),
                Item::unimplemented("Go to Query"),
                Item::unimplemented("Go to Symbol"),
                Item::unimplemented("Back"),
                Item::unimplemented("Forward"),
            ],
            AppBarMenu::Run => vec![
                Item::available(
                    "Run Query Statement",
                    "Ctrl+Enter",
                    "query.execute-statement",
                ),
                Item::available(
                    "Run Query Document",
                    "Ctrl+Shift+Enter",
                    "query.execute-document",
                ),
                Item::unimplemented("Run Configuration…"),
                Item::unimplemented("Stop"),
            ],
            AppBarMenu::Window => vec![
                Item::available("Split Pane", "Ctrl+\\", "workspace.split-pane"),
                Item::available("Close Pane", "Ctrl+Shift+W", "workspace.close-pane"),
                Item::unimplemented("New Window"),
                Item::unimplemented("Previous Window"),
                Item::unimplemented("Next Window"),
            ],
            AppBarMenu::Help => vec![
                Item::available("Command Palette…", "Ctrl+Shift+P", "ui.command-palette"),
                Item::unimplemented("Sift Documentation"),
                Item::unimplemented("Keyboard Shortcuts"),
                Item::unimplemented("Report Issue"),
                Item::unimplemented("About Sift"),
            ],
            AppBarMenu::Profile => vec![
                Item::available("Account", "", "account.open"),
                Item::unimplemented("Settings"),
                Item::unimplemented("Keymaps"),
                Item::unimplemented("Themes"),
                Item::unimplemented("Server Configuration"),
            ],
        }
    }

    fn activate_app_bar_item(
        &mut self,
        command: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.app_bar_menu = None;
        self.app_bar_expanded = false;
        match command {
            "ui.command-palette" => self.open_command_palette(&OpenCommandPalette, window, cx),
            "account.open" => self.open_account(cx),
            "window.quit" => window.remove_window(),
            command => self.run_command(command, window, cx),
        }
    }

    fn render_app_bar_dropdown(
        &self,
        menu: AppBarMenu,
        align_right: bool,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let colors = self.theme.colors;
        let rows = Self::app_bar_menu_items(menu)
            .into_iter()
            .enumerate()
            .map(|(index, item)| {
                let available = item.command.is_some();
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
                    .when(!available, |row| {
                        row.child(
                            div()
                                .flex_none()
                                .px_1()
                                .rounded(px(3.))
                                .bg(colors.hovered_surface)
                                .text_xs()
                                .child("Not implemented"),
                        )
                    });
                match item.command {
                    Some(command) => row
                        .on_click(cx.listener(move |shell, _, window, cx| {
                            shell.activate_app_bar_item(command, window, cx)
                        }))
                        .into_any_element(),
                    None => row.into_any_element(),
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
                            .on_mouse_down(MouseButton::Left, |_, window, cx| {
                                cx.stop_propagation();
                                window.start_window_move();
                            })
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
                            .on_click(cx.listener(|shell, _, _, cx| shell.open_server_picker(cx)))
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
                            .on_mouse_down(MouseButton::Left, |_, window, cx| {
                                cx.stop_propagation();
                                window.start_window_move();
                            }),
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
                            .h(px(26.))
                            .max_w(px(140.))
                            .px_2()
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(5.))
                            .text_sm()
                            .text_color(colors.muted_text)
                            .when(account_active, |button| {
                                button.bg(colors.active_surface).text_color(colors.text)
                            })
                            .hover(|button| {
                                button.bg(colors.hovered_surface).text_color(colors.text)
                            })
                            .on_click(cx.listener(|shell, _, _, cx| shell.open_account(cx)))
                            .child(div().truncate().child(account_label)),
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
                                    .text_sm()
                                    .text_color(colors.muted_text)
                                    .hover(|button| button.bg(colors.hovered_surface))
                                    .on_click(|_, window, _| window.minimize_window())
                                    .child("—"),
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
                                    .text_xs()
                                    .text_color(colors.muted_text)
                                    .hover(|button| button.bg(colors.hovered_surface))
                                    .on_click(|_, window, _| window.zoom_window())
                                    .child("□"),
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
                                    .text_sm()
                                    .text_color(colors.muted_text)
                                    .hover(|button| {
                                        button.bg(colors.danger_muted).text_color(colors.danger)
                                    })
                                    .on_click(|_, window, _| window.remove_window())
                                    .child("×"),
                            ),
                    ),
            )
    }

    fn render_dock(&self, dock: &Dock, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = self.theme.colors;
        div()
            .id(dock.title)
            .key_context("SiftDock")
            .w(px(dock.presentation.size))
            .min_h_0()
            .flex()
            .flex_col()
            .overflow_hidden()
            .py_2()
            .border_color(colors.subtle_border)
            .when(dock.title == "Connections", |dock| dock.border_r_1())
            .when(dock.title == "Inspector", |dock| dock.border_l_1())
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
                    .child(dock.title.to_uppercase()),
            )
            .when(dock.title == "Connections", |dock_view| {
                dock_view.child(
                    div()
                        .id("add-database-connection")
                        .role(Role::Button)
                        .mx_2()
                        .h(self.theme.metrics.row_height)
                        .px_2()
                        .flex()
                        .items_center()
                        .gap_2()
                        .rounded_sm()
                        .text_color(colors.muted_text)
                        .hover(|button| button.bg(colors.hovered_surface).text_color(colors.text))
                        .on_click(cx.listener(|shell, _, window, cx| {
                            shell.open_database_connection(window, cx)
                        }))
                        .child(icon(IconName::Add, colors.muted_text, 14.))
                        .child(div().min_w_0().truncate().child("Add database connection…")),
                )
            })
            .when(dock.title == "Connections", |dock_view| {
                let selected = self.selected_workspace_id;
                let mut rows: Vec<gpui::AnyElement> = Vec::new();
                for tenant in &self.lifecycle.tenants {
                    rows.push(
                        div()
                            .mt_2()
                            .h(px(24.))
                            .px_3()
                            .flex()
                            .items_center()
                            .gap_1()
                            .text_xs()
                            .text_color(colors.muted_text)
                            .child(icon(IconName::ChevronDown, colors.muted_text, 11.))
                            .child(div().min_w_0().truncate().child(tenant.name.clone()))
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
                            .gap_2()
                            .mx_2()
                            .h(self.theme.metrics.row_height)
                            .px_2()
                            .rounded_sm()
                            .when(connected, |row| row.bg(colors.active_surface))
                            .hover(|row| row.bg(colors.hovered_surface))
                            .child(
                                div()
                                    .size(px(7.))
                                    .rounded_full()
                                    .bg(dot)
                                    .border_1()
                                    .border_color(colors.panel),
                            )
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
                                    .mx_2()
                                    .h(self.theme.metrics.row_height)
                                    .px_2()
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
                                    .child(div().min_w_0().truncate().child(format!(
                                        "{} / {}{features}",
                                        room.name, workspace.name
                                    )))
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
            })
            .when(
                dock.title == "Connections" && self.lifecycle.tenants.is_empty(),
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
            .when(dock.title == "Inspector", |dock_view| {
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
            let card_width = if server_picker {
                360.0
            } else if account {
                320.0
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
                                                    .id(id)
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
                                    .child(div().min_w_0().truncate().child("Local Sift")),
                            )
                            .child(
                                div()
                                    .flex_none()
                                    .text_xs()
                                    .text_color(colors.muted_text)
                                    .px_1()
                                    .rounded(px(3.))
                                    .bg(colors.hovered_surface)
                                    .child(if local_active { "Current" } else { "Bundled" }),
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
                    let is_local = self
                        .lifecycle
                        .selected_instance
                        .as_ref()
                        .is_some_and(|instance| instance.kind == crate::InstanceKind::Local);
                    let interactive = self
                        .lifecycle
                        .identity
                        .as_ref()
                        .is_some_and(|identity| identity.auth_session_id.is_some());
                    let pending = self.account_pending;
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
                        .gap_3()
                        .when_some(self.lifecycle.identity.as_ref(), |account, identity| {
                            account
                                .child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .min_w_0()
                                        .gap_3()
                                        .child(self.render_account_avatar(40.))
                                        .child(
                                            div()
                                                .flex()
                                                .flex_col()
                                                .min_w_0()
                                                .child(
                                                    div()
                                                        .truncate()
                                                        .child(identity.principal.display_name.clone()),
                                                )
                                                .children(identity.principal.email.clone().map(
                                                    |email| {
                                                        div()
                                                            .truncate()
                                                            .text_sm()
                                                            .text_color(colors.muted_text)
                                                            .child(email)
                                                    },
                                                )),
                                        ),
                                )
                                .child(
                                    div()
                                        .text_sm()
                                        .text_color(colors.muted_text)
                                        .child(format!("Signed in on {server_name}")),
                                )
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(colors.muted_text)
                                        .whitespace_normal()
                                        .child(format!(
                                            "{} tenant membership(s){}",
                                            identity.memberships.len(),
                                            if identity.principal.is_instance_admin {
                                                " · Instance administrator"
                                            } else {
                                                ""
                                            }
                                        )),
                                )
                        })
                        .when(self.lifecycle.identity.is_none(), |account| {
                            account
                                .child(
                                    div()
                                        .font_weight(gpui::FontWeight::SEMIBOLD)
                                        .child(format!("Sign in to {server_name}")),
                                )
                                .child(
                                    div()
                                        .text_sm()
                                        .text_color(colors.muted_text)
                                        .whitespace_normal()
                                        .child("Use the account your Sift administrator has enabled."),
                                )
                        })
                        .when(is_local, |account| {
                            account.child(
                                div()
                                    .pt_2()
                                    .border_t_1()
                                    .border_color(colors.subtle_border)
                                    .text_sm()
                                    .text_color(colors.muted_text)
                                    .child("Local Sift uses its built-in local identity."),
                            )
                        })
                        .when(!is_local && !interactive, |account| {
                            account.child(
                                div()
                                    .pt_3()
                                    .border_t_1()
                                    .border_color(colors.subtle_border)
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
                                    .child(field("USERNAME", self.account_username_input.clone()))
                                    .child(field("PASSWORD", self.account_password_input.clone()))
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
                                                    .hover(|button| button.bg(colors.hovered_surface))
                                                    .on_click(cx.listener(|shell, _, _, cx| {
                                                        shell.sign_in_with_password(cx)
                                                    }))
                                            })
                                            .child("Sign in with password"),
                                    ),
                            )
                        })
                        .when(!is_local && interactive, |account| {
                            account.child(
                                div()
                                    .pt_3()
                                    .border_t_1()
                                    .border_color(colors.subtle_border)
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
                                                    .hover(|button| button.text_color(colors.danger))
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
                                                    .hover(|button| button.bg(colors.hovered_surface))
                                                    .on_click(cx.listener(|shell, _, _, cx| {
                                                        shell.sign_out(false, cx)
                                                    }))
                                            })
                                            .child(if pending { "Signing out…" } else { "Sign out" }),
                                    ),
                            )
                        })
                        .children(self.account_error.as_ref().map(|message| {
                            div()
                                .p_2()
                                .rounded_sm()
                                .border_1()
                                .border_color(colors.danger)
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
                Modal::ConfirmClose { title } => div()
                    .flex()
                    .flex_col()
                    .gap_4()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child(icon(IconName::Warning, colors.warning, 16.))
                            .child(format!("Save changes to {title}?")),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(colors.muted_text)
                            .child("Your edits have not been saved."),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_end()
                            .gap_2()
                            .child(
                                div()
                                    .id("cancel-close-item")
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
                                    .id("close-item-without-saving")
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
                                    .on_click(cx.listener(|shell, _, window, cx| {
                                        shell.confirm_close_without_saving(
                                            &ConfirmCloseWithoutSaving,
                                            window,
                                            cx,
                                        )
                                    }))
                                    .child("Discard"),
                            )
                            .child(
                                div()
                                    .id("save-and-close-item")
                                    .role(Role::Button)
                                    .h(self.theme.metrics.control_height)
                                    .px_3()
                                    .flex()
                                    .items_center()
                                    .rounded_sm()
                                    .bg(colors.accent)
                                    .hover(|button| button.bg(colors.accent_hover))
                                    .on_click(cx.listener(|shell, _, window, cx| {
                                        shell.save_active_item(&SaveActiveItem, window, cx)
                                    }))
                                    .child("Save"),
                            ),
                    )
                    .into_any_element(),
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
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
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
                        .when(server_picker || account, |card| {
                            card.on_mouse_down_out(cx.listener(|shell, _, window, cx| {
                                shell.dismiss_modal(&DismissModal, window, cx)
                            }))
                        })
                        .when(!database_connection && !command_palette, |card| card.p_3())
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
                        .flex()
                        .flex_col()
                        .border_t_1()
                        .border_color(colors.subtle_border)
                        .bg(colors.panel)
                        .text_sm()
                        .text_color(colors.muted_text)
                        .child(
                            div()
                                .h(px(28.))
                                .flex_none()
                                .flex()
                                .items_center()
                                .px_3()
                                .border_b_1()
                                .border_color(colors.subtle_border)
                                .text_xs()
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .child("OUTPUT"),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_1()
                                .min_h_0()
                                .items_center()
                                .justify_center()
                                .px_4()
                                .child(
                                    div()
                                        .max_w(px(420.))
                                        .min_w_0()
                                        .text_center()
                                        .whitespace_normal()
                                        .child("Query results stay with their editor."),
                                ),
                        ),
                )
            })
            .child(
                div()
                    .id("status-bar")
                    .role(Role::Toolbar)
                    .aria_label("Workspace status")
                    .tab_group()
                    .h(self.theme.metrics.status_height)
                    .w_full()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .px_2()
                    .border_t_1()
                    .border_color(colors.subtle_border)
                    .bg(colors.toolbar)
                    .text_xs()
                    .text_color(colors.muted_text)
                    .child(
                        div()
                            .flex()
                            .flex_1()
                            .min_w_0()
                            .overflow_x_hidden()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .flex_none()
                                    .flex()
                                    .items_center()
                                    .gap_1()
                                    .child(div().size(px(6.)).rounded_full().bg(
                                        match &self.connection_status {
                                            ConnectionStatus::Connected { .. } => colors.success,
                                            ConnectionStatus::Connecting { .. } => colors.warning,
                                            ConnectionStatus::Failed { .. } => colors.danger,
                                            ConnectionStatus::Disconnected => colors.muted_text,
                                        },
                                    ))
                                    .child(self.status.connection.clone()),
                            )
                            .child(
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .child(self.status.database.clone()),
                            )
                            .child(div().flex_none().child(self.status.transaction.clone())),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_shrink_0()
                            .overflow_x_hidden()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .max_w(px(220.))
                                    .min_w_0()
                                    .truncate()
                                    .child(self.status.room.clone()),
                            )
                            .child(
                                div()
                                    .flex_none()
                                    .px_1()
                                    .rounded(px(3.))
                                    .bg(colors.hovered_surface)
                                    .child(self.status.execution.clone()),
                            ),
                    ),
            )
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
            .children(window_resize_handles())
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
    use gpui::{point, Modifiers, TestAppContext, VisualTestContext};

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
    fn app_bar_overlays_are_mutually_exclusive(cx: &mut TestAppContext) {
        let window = shell(cx);
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        let workspace = window.root(&mut cx).unwrap();

        workspace.update(&mut cx, |shell, cx| shell.open_account(cx));
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

        workspace.update(&mut cx, |shell, cx| shell.open_server_picker(cx));
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
        let main = WorkspaceShell::app_bar_menu_items(AppBarMenu::Main);
        assert_eq!(
            main.iter().map(|item| item.label).collect::<Vec<_>>(),
            vec!["About Sift", "Check for Updates…", "Quit Sift"]
        );
        assert!(main[..2].iter().all(|item| item.command.is_none()));
        assert_eq!(main[2].command, Some("window.quit"));

        let profile = WorkspaceShell::app_bar_menu_items(AppBarMenu::Profile);
        assert_eq!(
            profile.iter().map(|item| item.label).collect::<Vec<_>>(),
            vec![
                "Account",
                "Settings",
                "Keymaps",
                "Themes",
                "Server Configuration"
            ]
        );
        assert!(profile[0].command.is_some());
        assert!(profile[1..].iter().all(|item| item.command.is_none()));

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
            assert!(!WorkspaceShell::app_bar_menu_items(menu).is_empty());
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
            .any(|command| command.id == "query.execute-statement"));
        assert!(commands
            .iter()
            .any(|command| command.id == "query.execute-document"));
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
                assert_eq!(
                    configuration["engine_specific"]["trust_server_certificate"],
                    true
                );
            }
            _ => panic!("expected profile creation command"),
        }
    }

    #[gpui::test]
    fn toast_expires_and_current_local_server_is_a_no_op(cx: &mut TestAppContext) {
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
        assert!(workspace.read_with(&cx, |shell, _| !shell.toasts.is_empty()));
        cx.background_executor
            .advance_clock(std::time::Duration::from_secs(5));
        cx.run_until_parked();
        assert!(workspace.read_with(&cx, |shell, _| shell.toasts.is_empty()));
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
            shell.open_server_picker(cx);
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

        workspace.update(&mut cx, |shell, cx| shell.open_server_picker(cx));
        cx.simulate_mouse_down(
            point(px(10.), px(500.)),
            MouseButton::Left,
            Modifiers::default(),
        );
        assert!(workspace.read_with(&cx, |shell, _| shell.modal().is_none()));

        workspace.update(&mut cx, |shell, cx| shell.open_account(cx));
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

        workspace.update(&mut cx, |shell, cx| shell.open_server_picker(cx));
        cx.run_until_parked();
        let account_bounds = cx
            .debug_bounds("toolbar-account")
            .expect("account button should be rendered");
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
                    memberships: Vec::new(),
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
