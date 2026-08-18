//! GPUI-owned projection of one server-authoritative Sift workspace.

pub mod editor;
mod lifecycle;
mod presentation;
pub mod results;
mod shell;

pub use editor::{QueryDocument, QueryEditor};
pub use results::{ResultData, ResultState, ResultsView};

pub use lifecycle::{
    create_virtual_workspace, load_instance, stream_room_presence, ConnectionNavEntry,
    ConnectionPhase, DegradedReason, InstanceCatalog, InstanceKind, InstanceSpec, LifecycleEvent,
    LifecycleProjection, LoadedInstance, PresenceEvent, RoomNavEntry, RoomPresenceProjection,
    TenantNavEntry, WorkspaceNavEntry,
};
pub use presentation::{
    BottomTool, DockPresentation, ItemKind, ItemPresentation, LeftPanel, PanePresentation,
    PresentationState, PresentationStore, Rect, WindowPresentation, WorkspacePresentation,
};
pub use shell::{
    CloseActiveItem, CloseActivePane, CommandDefinition, CommandId, CommandRegistry, CommandSpec,
    ConnectionStatus, DismissModal, Dock, DockDefinition, DockId, DockPlacement, DockRegistry,
    ExecutorCommand, ExecutorEvent, FocusNextPane, InstanceCommand,
    InstanceConfigurationPresentation, InstanceCredentialKind, InstanceCredentialPresentation,
    InstanceManagerEvent, InstancePlanPresentation, ItemDefinition, ItemRegistry, ItemRuntimeKind,
    Modal, OpenCommandPalette, OpenServerConnection, PaletteConfirm, PaletteDown, PaletteUp, Pane,
    PaneEvent, SaveActiveItem, SavedInstanceRoot, SavedServerProfile, SplitPane, StatusBar, Toast,
    ToggleBottomDock, ToggleLeftDock, ToggleRightDock, Tooltip, WorkspaceShell,
};

use std::{ops::Range, time::Duration};

use gpui::{
    actions, div, prelude::*, px, uniform_list, Context, Entity, Focusable, IntoElement, Role,
    Task, Window,
};
use sift_ui::{TextInput, Theme};

pub const FEASIBILITY_ROW_COUNT: usize = 100_000;

actions!(sift_workspace, [ToggleTheme, RefreshProbe, FocusQueryInput]);

/// M0 root entity proving the ownership, action, async, input, and
/// virtualization primitives required by the product workspace.
pub struct FeasibilityWorkspace {
    theme: Theme,
    dark: bool,
    query_input: Entity<TextInput>,
    probe_generation: u64,
    probe_complete: bool,
    _probe_task: Task<()>,
}

impl FeasibilityWorkspace {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let query_input =
            cx.new(|cx| TextInput::new("select * from workspace_files", "Write SQL…", cx));
        query_input.focus_handle(cx).focus(window, cx);
        let probe_task = Self::spawn_probe(cx);
        Self {
            theme: Theme::dark(),
            dark: true,
            query_input,
            probe_generation: 1,
            probe_complete: false,
            _probe_task: probe_task,
        }
    }

    fn spawn_probe(cx: &mut Context<Self>) -> Task<()> {
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(1))
                .await;
            this.update(cx, |workspace, cx| {
                workspace.probe_complete = true;
                cx.notify();
            })
            .ok();
        })
    }

    fn toggle_theme(&mut self, _: &ToggleTheme, _: &mut Window, cx: &mut Context<Self>) {
        self.dark = !self.dark;
        self.theme = if self.dark {
            Theme::dark()
        } else {
            Theme::light()
        };
        cx.notify();
    }

    fn refresh_probe(&mut self, _: &RefreshProbe, _: &mut Window, cx: &mut Context<Self>) {
        self.probe_generation += 1;
        self.probe_complete = false;
        self._probe_task = Self::spawn_probe(cx);
        cx.notify();
    }

    fn focus_query_input(
        &mut self,
        _: &FocusQueryInput,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.query_input.focus_handle(cx).focus(window, cx);
    }

    pub fn probe_complete(&self) -> bool {
        self.probe_complete
    }

    pub fn probe_generation(&self) -> u64 {
        self.probe_generation
    }
}

impl gpui::Render for FeasibilityWorkspace {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = self.theme.colors;
        let status = if self.probe_complete {
            "GPUI async bridge ready"
        } else {
            "Checking async bridge…"
        };

        div()
            .id("sift-workspace")
            .key_context("SiftWorkspace")
            .role(Role::Application)
            .aria_label("Sift database workspace")
            .on_action(cx.listener(Self::toggle_theme))
            .on_action(cx.listener(Self::refresh_probe))
            .on_action(cx.listener(Self::focus_query_input))
            .flex()
            .flex_col()
            .size_full()
            .bg(colors.background)
            .text_color(colors.text)
            .child(
                div()
                    .id("titlebar")
                    .h(px(38.))
                    .flex()
                    .items_center()
                    .justify_between()
                    .px_3()
                    .border_b_1()
                    .border_color(colors.border)
                    .bg(colors.surface)
                    .child(div().font_weight(gpui::FontWeight::SEMIBOLD).child("sift"))
                    .child(
                        div()
                            .text_sm()
                            .text_color(colors.muted_text)
                            .child("Local / Phase M feasibility"),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .child(
                        div()
                            .id("left-dock")
                            .w(px(230.))
                            .flex()
                            .flex_col()
                            .border_r_1()
                            .border_color(colors.border)
                            .bg(colors.surface)
                            .p_3()
                            .gap_2()
                            .text_sm()
                            .child(
                                div()
                                    .text_color(colors.muted_text)
                                    .child("CONNECTIONS"),
                            )
                            .child("Local PostgreSQL")
                            .child(
                                div()
                                    .mt_3()
                                    .text_color(colors.muted_text)
                                    .child("WORKSPACE"),
                            )
                            .child("Queries")
                            .child("Schema")
                            .child("Git")
                            .child("Runs"),
                    )
                    .child(
                        div()
                            .id("center-pane")
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .child(
                                div()
                                    .h(px(34.))
                                    .flex()
                                    .items_center()
                                    .px_3()
                                    .border_b_1()
                                    .border_color(colors.border)
                                    .bg(colors.surface)
                                    .text_sm()
                                    .child("query.sql"),
                            )
                            .child(
                                div()
                                    .p_3()
                                    .border_b_1()
                                    .border_color(colors.border)
                                    .bg(colors.background)
                                    .child(
                                        div()
                                            .rounded_md()
                                            .border_1()
                                            .border_color(colors.border)
                                            .bg(colors.elevated_surface)
                                            .child(self.query_input.clone()),
                                    ),
                            )
                            .child(
                                div()
                                    .h(px(32.))
                                    .flex()
                                    .items_center()
                                    .gap_4()
                                    .px_3()
                                    .border_b_1()
                                    .border_color(colors.border)
                                    .bg(colors.surface)
                                    .text_sm()
                                    .child("Data")
                                    .child(
                                        div().text_color(colors.muted_text).child("Messages"),
                                    )
                                    .child(
                                        div().text_color(colors.muted_text).child("Explain"),
                                    )
                                    .child(
                                        div().text_color(colors.muted_text).child("History"),
                                    ),
                            )
                            .child(
                                uniform_list(
                                    "feasibility-result-rows",
                                    FEASIBILITY_ROW_COUNT,
                                    cx.processor(move |_, range: Range<usize>, _, _| {
                                        range
                                            .map(|row| {
                                                div()
                                                    .id(row)
                                                    .h(px(28.))
                                                    .flex()
                                                    .items_center()
                                                    .px_3()
                                                    .border_b_1()
                                                    .border_color(colors.border)
                                                    .text_sm()
                                                    .child(format!(
                                                        "{:06}    workspace_file_{row}.sql    ready",
                                                        row + 1
                                                    ))
                                            })
                                            .collect()
                                    }),
                                )
                                .flex_1()
                                .min_h_0(),
                            ),
                    )
                    .child(
                        div()
                            .id("right-dock")
                            .w(px(220.))
                            .border_l_1()
                            .border_color(colors.border)
                            .bg(colors.surface)
                            .p_3()
                            .text_sm()
                            .child(
                                div()
                                    .text_color(colors.muted_text)
                                    .child("INSPECTOR"),
                            )
                            .child(div().mt_3().child("PostgreSQL query"))
                            .child(
                                div()
                                    .mt_1()
                                    .text_color(colors.muted_text)
                                    .child("Read-only feasibility surface"),
                            ),
                    ),
            )
            .child(
                div()
                    .id("statusbar")
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
                    .child(status)
                    .child(format!(
                        "100,000 virtual rows · probe {}",
                        self.probe_generation
                    )),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;

    #[gpui::test]
    async fn async_probe_updates_a_live_entity(cx: &mut TestAppContext) {
        let window = cx.update(|cx| {
            cx.open_window(Default::default(), |window, cx| {
                cx.new(|cx| FeasibilityWorkspace::new(window, cx))
            })
            .unwrap()
        });
        cx.background_executor
            .advance_clock(Duration::from_millis(2));
        cx.run_until_parked();
        let workspace = window.root(cx).unwrap();
        assert!(workspace.read_with(cx, |workspace, _| workspace.probe_complete()));
    }

    #[gpui::test]
    fn virtual_surface_cardinality_is_intentionally_large(_: &mut TestAppContext) {
        assert_eq!(FEASIBILITY_ROW_COUNT, 100_000);
    }
}
