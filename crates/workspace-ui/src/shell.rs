use std::sync::Arc;

use gpui::{
    actions, div, prelude::*, px, App, Context, Entity, FocusHandle, Focusable, IntoElement, Role,
    Subscription, Task, Window, WindowBounds,
};
use sift_api_types::RoomId;
use sift_ui::{button, ControlState, ControlTone, TextInput, Theme};

use crate::presentation::{
    DockPresentation, ItemKind, ItemPresentation, PanePresentation, PresentationState,
    PresentationStore, WindowPresentation, WorkspacePresentation,
};
use crate::{
    LifecycleEvent, LifecycleProjection, PresenceEvent, RoomPresenceProjection, WorkspaceNavEntry,
};

actions!(
    sift_shell,
    [
        OpenCommandPalette,
        DismissModal,
        SplitPane,
        FocusNextPane,
        CloseActiveItem,
        SaveActiveItem,
        ConfirmCloseWithoutSaving,
        ToggleLeftDock,
        ToggleRightDock,
        ToggleBottomDock,
        ToggleShellTheme
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
    ConfirmClose { title: String },
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

/// A pane owns its ordered items and focus handle. The workspace owns panes;
/// items never reach sideways into sibling panes.
pub struct Pane {
    id: u64,
    items: Vec<ItemPresentation>,
    active_item: usize,
    focus_handle: FocusHandle,
    theme: Theme,
}

impl Pane {
    fn from_presentation(pane: PanePresentation, theme: Theme, cx: &mut Context<Self>) -> Self {
        Self {
            id: pane.id,
            items: pane.items,
            active_item: pane.active_item,
            focus_handle: cx.focus_handle(),
            theme,
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
}

impl Focusable for Pane {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl gpui::Render for Pane {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let active = self.active_item().cloned();
        div()
            .id(("pane", self.id as usize))
            .key_context("SiftPane")
            .track_focus(&self.focus_handle)
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .child(
                div()
                    .h(px(34.))
                    .flex()
                    .items_center()
                    .gap_1()
                    .px_2()
                    .border_b_1()
                    .children(self.items.iter().enumerate().map(|(index, item)| {
                        let dirty = if item.dirty { " ●" } else { "" };
                        div()
                            .id(("tab", item.id as usize))
                            .px_2()
                            .py_1()
                            .rounded_sm()
                            .when(index == self.active_item, |tab| {
                                tab.bg(self.theme.colors.selected_surface)
                            })
                            .child(format!("{}{dirty}", item.title))
                    })),
            )
            .child(
                div()
                    .flex_1()
                    .p_4()
                    .children(active.map(|item| match item.kind {
                        ItemKind::Query => format!("Query editor · {}", item.title),
                        ItemKind::Schema => format!("Schema view · {}", item.title),
                        ItemKind::Welcome => "Welcome to Sift".into(),
                    })),
            )
    }
}

pub struct WorkspaceShell {
    focus_handle: FocusHandle,
    query_input: Entity<TextInput>,
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
        panes[active_pane].focus_handle(cx).focus(window, cx);
        let bounds_subscription = cx.observe_window_bounds(window, |shell, window, cx| {
            shell.capture_window_bounds(window.window_bounds());
            shell.persist(cx);
        });
        Self {
            focus_handle: cx.focus_handle(),
            query_input,
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
        vec![
            CommandSpec {
                id: "workspace.split-pane",
                label: "Split Pane",
                shortcut: "Ctrl+\\",
                disabled_reason: None,
            },
            CommandSpec {
                id: "workspace.close-item",
                label: "Close Active Item",
                shortcut: "Ctrl+W",
                disabled_reason: (!has_item).then_some("No active item"),
            },
            CommandSpec {
                id: "workspace.toggle-left-dock",
                label: "Toggle Connections Dock",
                shortcut: "Ctrl+Shift+B",
                disabled_reason: None,
            },
        ]
    }

    pub fn pane_count(&self) -> usize {
        self.panes.len()
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
                        if let LifecycleEvent::Selected(instance) = &event {
                            shell.selected_instance_id = Some(instance.id.clone());
                        }
                        shell.lifecycle.apply(event);
                        shell.status.connection = shell.lifecycle.status_label();
                        shell.reconcile_restored_workspace(cx);
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
        self.panes.push(pane);
        self.active_pane = self.panes.len() - 1;
        self.panes[self.active_pane]
            .focus_handle(cx)
            .focus(window, cx);
        self.persist(cx);
        cx.notify();
    }

    fn focus_next_pane(&mut self, _: &FocusNextPane, window: &mut Window, cx: &mut Context<Self>) {
        self.active_pane = (self.active_pane + 1) % self.panes.len();
        self.panes[self.active_pane]
            .focus_handle(cx)
            .focus(window, cx);
        cx.notify();
    }

    fn close_active_item(&mut self, _: &CloseActiveItem, _: &mut Window, cx: &mut Context<Self>) {
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
        self.remove_active_item(cx);
    }

    fn remove_active_item(&mut self, cx: &mut Context<Self>) {
        if let Some(pane) = self.panes.get(self.active_pane) {
            pane.update(cx, |pane, _| {
                if !pane.items.is_empty() {
                    pane.items.remove(pane.active_item);
                    pane.active_item = pane.active_item.min(pane.items.len().saturating_sub(1));
                }
            });
        }
        self.modal = None;
        self.persist(cx);
        cx.notify();
    }

    fn save_active_item(&mut self, _: &SaveActiveItem, _: &mut Window, cx: &mut Context<Self>) {
        let close_after_save = matches!(self.modal, Some(Modal::ConfirmClose { .. }));
        self.mark_active_item_dirty(false, cx);
        self.toasts.push(Toast {
            message: "Presentation saved".into(),
        });
        if close_after_save {
            self.remove_active_item(cx);
        } else {
            self.modal = None;
            self.persist(cx);
            cx.notify();
        }
    }

    fn confirm_close_without_saving(
        &mut self,
        _: &ConfirmCloseWithoutSaving,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.remove_active_item(cx);
    }

    fn open_command_palette(
        &mut self,
        _: &OpenCommandPalette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.modal = Some(Modal::CommandPalette);
        self.query_input.focus_handle(cx).focus(window, cx);
        cx.notify();
    }

    fn dismiss_modal(&mut self, _: &DismissModal, _: &mut Window, cx: &mut Context<Self>) {
        self.modal = None;
        cx.notify();
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

    fn toggle_theme(&mut self, _: &ToggleShellTheme, _: &mut Window, cx: &mut Context<Self>) {
        self.dark_theme = !self.dark_theme;
        self.theme = if self.dark_theme {
            Theme::dark()
        } else {
            Theme::light()
        };
        for pane in &self.panes {
            pane.update(cx, |pane, _| pane.theme = self.theme);
        }
        self.persist(cx);
        cx.notify();
    }

    fn render_dock(&self, dock: &Dock) -> impl IntoElement {
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
                dock_view.children(self.lifecycle.tenants.iter().flat_map(|tenant| {
                    let tenant_name = div()
                        .mt_2()
                        .text_color(colors.muted_text)
                        .child(tenant.name.clone());
                    let connections = tenant
                        .connection_names
                        .iter()
                        .map(|name| div().pl_2().child(format!("● {name}")));
                    let workspaces = tenant.rooms.iter().flat_map(|room| {
                        room.workspaces.iter().map(move |workspace| {
                            let features =
                                match (workspace.git_enabled, workspace.scheduling_enabled) {
                                    (true, true) => " · Git · Runs",
                                    (true, false) => " · Git",
                                    (false, true) => " · Runs",
                                    (false, false) => "",
                                };
                            div()
                                .pl_2()
                                .child(format!("{} / {}{features}", room.name, workspace.name))
                        })
                    });
                    std::iter::once(tenant_name)
                        .chain(connections)
                        .chain(workspaces)
                        .collect::<Vec<_>>()
                }))
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

    fn render_modal(&self, cx: &App) -> Option<impl IntoElement> {
        let colors = self.theme.colors;
        self.modal.as_ref().map(|modal| {
            let content = match modal {
                Modal::CommandPalette => div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(self.query_input.clone())
                    .children(self.command_specs(cx).into_iter().map(|command| {
                        let state = if command.enabled() {
                            ControlState::Rest
                        } else {
                            ControlState::Disabled
                        };
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .child(button(
                                command.id,
                                command.label,
                                self.theme,
                                ControlTone::Neutral,
                                state,
                            ))
                            .child(command.disabled_reason.unwrap_or(command.shortcut))
                    }))
                    .into_any_element(),
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
        div()
            .id("sift-shell")
            .key_context("SiftWorkspace")
            .role(Role::Application)
            .aria_label("Sift database workspace")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::open_command_palette))
            .on_action(cx.listener(Self::dismiss_modal))
            .on_action(cx.listener(Self::split_pane))
            .on_action(cx.listener(Self::focus_next_pane))
            .on_action(cx.listener(Self::close_active_item))
            .on_action(cx.listener(Self::save_active_item))
            .on_action(cx.listener(Self::confirm_close_without_saving))
            .on_action(cx.listener(Self::toggle_left_dock))
            .on_action(cx.listener(Self::toggle_right_dock))
            .on_action(cx.listener(Self::toggle_bottom_dock))
            .on_action(cx.listener(Self::toggle_theme))
            .relative()
            .flex()
            .flex_col()
            .size_full()
            .bg(colors.background)
            .text_color(colors.text)
            .child(
                div()
                    .id("integrated-titlebar")
                    .key_context("SiftWindow")
                    .h(px(38.))
                    .flex()
                    .items_center()
                    .justify_between()
                    .px_3()
                    .border_b_1()
                    .border_color(colors.border)
                    .bg(colors.surface)
                    .child(div().font_weight(gpui::FontWeight::SEMIBOLD).child("sift"))
                    .child("Local workspace · Ctrl+Shift+P"),
            )
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .when(self.left_dock.presentation.open, |row| {
                        row.child(self.render_dock(&self.left_dock))
                    })
                    .child(
                        div()
                            .flex()
                            .flex_1()
                            .min_w_0()
                            .children(self.panes.iter().cloned()),
                    )
                    .when(self.right_dock.presentation.open, |row| {
                        row.child(self.render_dock(&self.right_dock))
                    }),
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
                        .child("Data   Messages   Explain   History"),
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
