//! Native large-results window. The grid entity stays owned by its query pane.
use super::*;

pub(super) struct DataWindow {
    pub title: String,
    pub results: Entity<ResultsView>,
}

impl Render for DataWindow {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = cx.theme().colors;
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(colors.background)
            .text_color(colors.text)
            .track_focus(&self.results.focus_handle(cx))
            .on_key_down(|event, window, _| {
                if event.keystroke.key == "escape" {
                    window.remove_window();
                }
            })
            .on_action(|_: &DismissModal, window, _| window.remove_window())
            .child(
                div()
                    .h(px(36.))
                    .flex_none()
                    .px_2()
                    .flex()
                    .items_center()
                    .bg(colors.toolbar)
                    .child(
                        div()
                            .id("data-window-heading")
                            .flex_1()
                            .h_full()
                            .flex()
                            .items_center()
                            .window_control_area(WindowControlArea::Drag)
                            .on_mouse_down(MouseButton::Left, |_, window, _| {
                                window.start_window_move()
                            })
                            .child(self.title.clone()),
                    )
                    .child(
                        IconButton::new(
                            "close-data-window",
                            IconName::Close,
                            "Close large Data view",
                        )
                        .on_click(|_, window, _| window.remove_window()),
                    ),
            )
            .child(
                div()
                    .debug_selector(|| "data-results-window-body".into())
                    .flex_1()
                    .min_h_0()
                    .overflow_hidden()
                    .child(self.results.clone()),
            )
    }
}
