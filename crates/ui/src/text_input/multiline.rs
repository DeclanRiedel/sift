//! Opt-in wrapped prompt editing using GPUI's native text layout and IME boundary.
use super::*;

impl TextInput {
    pub(super) fn index_at_position(&self, position: gpui::Point<Pixels>) -> usize {
        if let Some(layout) = &self.multiline_layout {
            layout
                .index_for_position(position)
                .unwrap_or_else(|index| index)
                .min(self.content.len())
        } else {
            self.index_at_x(position.x)
        }
    }

    pub(super) fn handle_multiline_key(
        &mut self,
        event: &gpui::KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.multiline {
            return;
        }
        let key = event.keystroke.key.as_str();
        let modifiers = event.keystroke.modifiers;
        if key == "enter"
            && modifiers.shift
            && !modifiers.control
            && !modifiers.platform
            && !modifiers.alt
        {
            self.replace_text_in_range(None, "\n", window, cx);
            cx.stop_propagation();
        } else if matches!(key, "up" | "down")
            && !modifiers.control
            && !modifiers.platform
            && !modifiers.alt
        {
            let Some(layout) = &self.multiline_layout else {
                return;
            };
            let Some(mut point) = layout.position_for_index(self.cursor_offset()) else {
                return;
            };
            point.y += layout.line_height() * if key == "up" { -1. } else { 1. };
            point.y += layout.line_height() / 2.;
            let cursor = self.index_at_position(point);
            if modifiers.shift {
                let anchor = if self.selection_reversed {
                    self.selected_range.end
                } else {
                    self.selected_range.start
                };
                self.selected_range = anchor.min(cursor)..anchor.max(cursor);
                self.selection_reversed = cursor < anchor;
                self.reveal_cursor = true;
                cx.notify();
            } else {
                self.move_to(cursor, cx);
            }
            cx.stop_propagation();
        }
    }
}

pub(super) fn element(input: &TextInput, cx: &Context<TextInput>) -> impl IntoElement {
    let text = if input.content.is_empty() {
        input.placeholder.clone()
    } else {
        input.content.clone()
    };
    div()
        .id("multiline-input-scroll")
        .debug_selector(|| "multiline-input-scroll".into())
        .w_full()
        .min_w_0()
        .max_h(px(120.))
        .overflow_y_scroll()
        .track_scroll(&input.multiline_scroll)
        .whitespace_normal()
        .child(MultilineElement {
            input: cx.entity(),
            text: gpui::StyledText::new(text),
        })
}

struct MultilineElement {
    input: Entity<TextInput>,
    text: gpui::StyledText,
}

impl IntoElement for MultilineElement {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}

impl Element for MultilineElement {
    type RequestLayoutState = ();
    type PrepaintState = ();
    fn id(&self) -> Option<ElementId> {
        None
    }
    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        inspector: Option<&gpui::InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        let input = self.input.read(cx);
        let text = if input.content.is_empty() {
            input.placeholder.clone()
        } else {
            input.content.clone()
        };
        let mut run = window.text_style().to_run(text.len());
        if input.content.is_empty() {
            run.color = cx.theme().colors.muted_text;
        }
        self.text = gpui::StyledText::new(text).with_runs(vec![run]);
        self.text.request_layout(None, inspector, window, cx)
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        inspector: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        state: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        self.text
            .prepaint(None, inspector, bounds, state, window, cx);
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        inspector: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        state: &mut (),
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        let layout = self.text.layout().clone();
        let input = self.input.read(cx);
        let focus = input.focus_handle.clone();
        let selected = input.selected_range.clone();
        let cursor = input.cursor_offset();
        let content = input.content.clone();
        let marked = input.marked_range.clone();
        window.handle_input(
            &focus,
            ElementInputHandler::new(bounds, self.input.clone()),
            cx,
        );
        for (index, ch) in content
            .char_indices()
            .filter(|(index, _)| selected.contains(index))
        {
            if let Some(start) = layout.position_for_index(index) {
                let right = layout
                    .position_for_index(index + ch.len_utf8())
                    .filter(|end| end.y == start.y)
                    .map_or(bounds.right(), |end| end.x);
                window.paint_quad(fill(
                    Bounds::new(
                        start,
                        size((right - start.x).max(px(1.)), layout.line_height()),
                    ),
                    cx.theme().colors.selected_surface,
                ));
            }
        }
        self.text
            .paint(None, inspector, bounds, state, &mut (), window, cx);
        if let Some(marked) = marked {
            for (index, ch) in content
                .char_indices()
                .filter(|(index, _)| marked.contains(index))
            {
                if let Some(mut start) = layout.position_for_index(index) {
                    let right = layout
                        .position_for_index(index + ch.len_utf8())
                        .filter(|end| end.y == start.y)
                        .map_or(bounds.right(), |end| end.x);
                    start.y += layout.line_height() - px(1.);
                    window.paint_quad(fill(
                        Bounds::new(start, size((right - start.x).max(px(1.)), px(1.))),
                        window.text_style().color,
                    ));
                }
            }
        }
        if focus.is_focused(window) && selected.is_empty() {
            if let Some(start) = layout.position_for_index(cursor) {
                window.paint_quad(fill(
                    Bounds::new(start, size(px(1.5), layout.line_height())),
                    window.text_style().color,
                ));
            }
        }
        self.input.update(cx, |input, cx| {
            if input.reveal_cursor {
                if let Some(start) = layout.position_for_index(cursor) {
                    let viewport = input.multiline_scroll.bounds();
                    let delta = if start.y < viewport.top() {
                        viewport.top() - start.y
                    } else if start.y + layout.line_height() > viewport.bottom() {
                        viewport.bottom() - start.y - layout.line_height()
                    } else {
                        px(0.)
                    };
                    if delta != px(0.) {
                        let offset = input.multiline_scroll.offset();
                        input
                            .multiline_scroll
                            .set_offset(point(px(0.), (offset.y + delta).min(px(0.))));
                        cx.notify();
                    }
                }
                input.reveal_cursor = false;
            }
            input.multiline_layout = Some(layout);
            input.last_bounds = Some(bounds);
        });
    }
}
