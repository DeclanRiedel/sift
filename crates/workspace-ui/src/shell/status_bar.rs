//! Status-bar projection and rendering.

use super::*;

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

pub(super) fn render_status_bar(
    status: &StatusBar,
    connection_status: &ConnectionStatus,
    theme: Theme,
) -> gpui::AnyElement {
    let colors = theme.colors;
    div()
        .id("status-bar")
        .role(Role::Toolbar)
        .aria_label("Workspace status")
        .tab_group()
        .h(theme.metrics.status_height)
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
                        .child(
                            div()
                                .size(px(6.))
                                .rounded_full()
                                .bg(match connection_status {
                                    ConnectionStatus::Connected { .. } => colors.success,
                                    ConnectionStatus::Connecting { .. } => colors.warning,
                                    ConnectionStatus::Failed { .. } => colors.danger,
                                    ConnectionStatus::Disconnected => colors.muted_text,
                                }),
                        )
                        .child(status.connection.clone()),
                )
                .child(div().min_w_0().truncate().child(status.database.clone()))
                .child(div().flex_none().child(status.transaction.clone())),
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
                        .child(status.room.clone()),
                )
                .child(
                    div()
                        .flex_none()
                        .px_1()
                        .rounded(px(3.))
                        .bg(colors.hovered_surface)
                        .child(status.execution.clone()),
                ),
        )
        .into_any_element()
}
