//! Host-owned bottom output panel.

use super::*;

pub(super) fn render_output_panel(dock: &Dock, theme: Theme) -> gpui::AnyElement {
    debug_assert_eq!(dock.id, DockId::Output);
    let colors = theme.colors;
    div()
        .h(px(dock.presentation.size.min(160.0)))
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
                .child(dock.definition().title.to_uppercase()),
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
        )
        .into_any_element()
}
