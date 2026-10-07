//! Selection for styled, wrapped, read-only text, including across blocks.
use std::{cell::RefCell, ops::Range, rc::Rc};

use gpui::{
    fill, App, Bounds, CursorStyle, DispatchPhase, Element, ElementId, FocusHandle,
    GlobalElementId, Hitbox, HitboxBehavior, InspectorElementId, IntoElement, LayoutId,
    MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, SharedString, StyledText,
    Window,
};

use crate::ActiveTheme;

#[derive(Default)]
pub struct TextSelection {
    document: String,
    text: SharedString,
    anchor: usize,
    cursor: usize,
    dragging: bool,
}

impl TextSelection {
    pub fn selected_text(&self) -> Option<String> {
        let range = self.anchor.min(self.cursor)..self.anchor.max(self.cursor);
        if range.is_empty() {
            None
        } else {
            self.text.get(range).map(str::to_owned)
        }
    }

    pub fn selected_text_in(&self, document: &str) -> Option<String> {
        if self.document == document {
            self.selected_text()
        } else {
            None
        }
    }

    pub fn clear(&mut self) {
        *self = Self::default();
    }

    fn range_in(&self, document: &str, range: Range<usize>) -> Range<usize> {
        if self.document != document {
            return 0..0;
        }
        let start = self.anchor.min(self.cursor).clamp(range.start, range.end);
        let end = self.anchor.max(self.cursor).clamp(range.start, range.end);
        start - range.start..end - range.start
    }
}

pub struct SelectableText {
    id: ElementId,
    text: StyledText,
    document: String,
    document_text: SharedString,
    range: Range<usize>,
    selection: Rc<RefCell<TextSelection>>,
    focus: FocusHandle,
    links: Vec<(Range<usize>, String)>,
}

impl SelectableText {
    pub fn new(
        id: impl Into<ElementId>,
        text: StyledText,
        document: String,
        document_text: SharedString,
        range: Range<usize>,
        selection: Rc<RefCell<TextSelection>>,
        focus: FocusHandle,
    ) -> Self {
        Self {
            id: id.into(),
            text,
            document,
            document_text,
            range,
            selection,
            focus,
            links: vec![],
        }
    }

    pub fn links(mut self, links: Vec<(Range<usize>, String)>) -> Self {
        self.links = links;
        self
    }
}

impl IntoElement for SelectableText {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}

impl Element for SelectableText {
    type RequestLayoutState = ();
    type PrepaintState = Hitbox;

    fn id(&self) -> Option<ElementId> {
        Some(self.id.clone())
    }
    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        inspector: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        self.text.request_layout(None, inspector, window, cx)
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        inspector: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        state: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) -> Hitbox {
        self.text
            .prepaint(None, inspector, bounds, state, window, cx);
        window.insert_hitbox(bounds, HitboxBehavior::Normal)
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        inspector: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        state: &mut (),
        hitbox: &mut Hitbox,
        window: &mut Window,
        cx: &mut App,
    ) {
        let layout = self.text.layout().clone();
        let selected = self
            .selection
            .borrow()
            .range_in(&self.document, self.range.clone());
        let style = window.text_style();
        let font_size = style.font_size.to_pixels(window.rem_size());
        let height = style
            .line_height
            .to_pixels(font_size.into(), window.rem_size());
        let local = &self.document_text[self.range.clone()];
        for (index, ch) in local
            .char_indices()
            .filter(|(index, _)| selected.contains(index))
        {
            if ch == '\n' {
                continue;
            }
            if let Some(start) = layout.position_for_index(index) {
                let right = layout
                    .position_for_index(index + ch.len_utf8())
                    .filter(|end| end.y == start.y)
                    .map_or(bounds.right(), |end| end.x);
                window.paint_quad(fill(
                    Bounds::new(
                        start,
                        gpui::size((right - start.x).max(gpui::px(1.)), height),
                    ),
                    cx.theme().colors.selected_surface,
                ));
            }
        }
        self.text
            .paint(None, inspector, bounds, state, &mut (), window, cx);
        window.set_cursor_style(CursorStyle::IBeam, hitbox);
        let selection = self.selection.clone();
        let document = self.document.clone();
        let document_text = self.document_text.clone();
        let range = self.range.clone();
        let focus = self.focus.clone();
        let down_layout = layout.clone();
        let down_hitbox = hitbox.clone();
        window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
            if phase == DispatchPhase::Bubble
                && event.button == MouseButton::Left
                && down_hitbox.is_hovered(window)
            {
                let index = down_layout
                    .index_for_position(event.position)
                    .unwrap_or_else(|index| index)
                    .min(range.len());
                *selection.borrow_mut() = TextSelection {
                    document: document.clone(),
                    text: document_text.clone(),
                    anchor: range.start + index,
                    cursor: range.start + index,
                    dragging: true,
                };
                focus.focus(window, cx);
                cx.stop_propagation();
                window.refresh();
            }
        });
        let selection = self.selection.clone();
        let document = self.document.clone();
        let range = self.range.clone();
        let move_layout = layout.clone();
        let move_hitbox = hitbox.clone();
        window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, _| {
            if phase == DispatchPhase::Bubble
                && event.pressed_button == Some(MouseButton::Left)
                && move_hitbox.is_hovered(window)
            {
                let mut selected = selection.borrow_mut();
                if selected.dragging && selected.document == document {
                    selected.cursor = range.start
                        + move_layout
                            .index_for_position(event.position)
                            .unwrap_or_else(|index| index)
                            .min(range.len());
                    window.refresh();
                }
            }
        });
        let selection = self.selection.clone();
        let document = self.document.clone();
        let range = self.range.clone();
        let up_hitbox = hitbox.clone();
        let links = std::mem::take(&mut self.links);
        window.on_mouse_event(move |event: &MouseUpEvent, phase, window, cx| {
            if phase == DispatchPhase::Bubble && event.button == MouseButton::Left {
                let mut selected = selection.borrow_mut();
                if selected.document == document {
                    selected.dragging = false;
                    if up_hitbox.is_hovered(window) && selected.anchor == selected.cursor {
                        let index = layout
                            .index_for_position(event.position)
                            .unwrap_or_else(|index| index);
                        if selected.anchor == range.start + index {
                            if let Some((_, url)) =
                                links.iter().find(|(range, _)| range.contains(&index))
                            {
                                cx.open_url(url);
                            }
                        }
                    }
                    window.refresh();
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn selection_crosses_blocks_and_keeps_unicode_boundaries() {
        let selection = TextSelection {
            document: "response".into(),
            text: "αβ\n\nSQL".into(),
            anchor: 2,
            cursor: 8,
            dragging: false,
        };
        assert_eq!(selection.selected_text().as_deref(), Some("β\n\nSQ"));
        assert_eq!(selection.range_in("response", 6..9), 0..2);
        assert_eq!(selection.range_in("other", 6..9), 0..0);
    }
}
