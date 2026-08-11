//! The SQL query editor: a Loro-backed document model plus a multi-line GPUI
//! view. The document is the M3 vertical-slice core — selections, editing with
//! undo/redo, statement targeting, and find — kept free of GPUI so it is fully
//! unit-testable. The view renders it and bridges platform text/IME input.

use std::ops::Range;

use gpui::{
    actions, div, fill, point, prelude::*, px, size, App, Bounds, ClipboardItem, Context,
    CursorStyle, Element, ElementId, ElementInputHandler, Entity, EntityInputHandler, EventEmitter,
    FocusHandle, Focusable, GlobalElementId, IntoElement, LayoutId, MouseButton, PaintQuad, Pixels,
    Role, ShapedLine, Style, TextRun, UTF16Selection, Window,
};
use sift_doc::{random_peer_id, TextReplica};
use sift_ui::Theme;

/// A single applied edit, retained so it can be inverted for undo/redo. Offsets
/// are byte offsets into the materialized text at the time the edit applied.
#[derive(Debug, Clone)]
struct Edit {
    at: usize,
    removed: String,
    inserted: String,
    selection_before: Range<usize>,
    reversed_before: bool,
}

/// A collaborative SQL document. Text lives in a Loro [`TextReplica`] so the
/// same buffer can later sync with the server room; the model caches the
/// materialized text and owns a byte-offset selection. Loro indexes by Unicode
/// scalar, so every replica call converts from the byte offsets the view uses.
pub struct QueryDocument {
    replica: TextReplica,
    text: String,
    selection: Range<usize>,
    reversed: bool,
    /// Sticky column for vertical movement, so up/down over short lines keeps
    /// the caret's horizontal intent. Cleared by any horizontal move or edit.
    goal_column: Option<usize>,
    undo: Vec<Edit>,
    redo: Vec<Edit>,
}

impl QueryDocument {
    /// Build a document authored under `peer_id`, seeded with `initial` text.
    pub fn new(peer_id: u64, initial: &str) -> Self {
        let replica = TextReplica::new(peer_id).expect("non-zero peer id");
        if !initial.is_empty() {
            replica.insert(0, initial).expect("seed insert");
        }
        let text = replica.text();
        let end = text.len();
        Self {
            replica,
            text,
            selection: end..end,
            reversed: false,
            goal_column: None,
            undo: Vec::new(),
            redo: Vec::new(),
        }
    }

    /// A document with a fresh random peer id — the desktop's default.
    pub fn with_random_peer(initial: &str) -> Self {
        Self::new(random_peer_id(), initial)
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn selection(&self) -> Range<usize> {
        self.selection.clone()
    }

    pub fn is_reversed(&self) -> bool {
        self.reversed
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    /// The fixed end of the selection (the point movement pivots around).
    fn anchor(&self) -> usize {
        if self.reversed {
            self.selection.end
        } else {
            self.selection.start
        }
    }

    /// The moving end of the selection (the caret).
    pub fn cursor(&self) -> usize {
        if self.reversed {
            self.selection.start
        } else {
            self.selection.end
        }
    }

    /// Apply a text splice against the replica and refresh the cached text. Only
    /// touches CRDT state; selection and history are the caller's concern.
    fn splice(&mut self, start: usize, end: usize, new_text: &str) {
        let char_start = self.text[..start].chars().count();
        let removed_chars = self.text[start..end].chars().count();
        if removed_chars > 0 {
            self.replica
                .delete(char_start, removed_chars)
                .expect("delete within bounds");
        }
        if !new_text.is_empty() {
            self.replica
                .insert(char_start, new_text)
                .expect("insert within bounds");
        }
        self.text = self.replica.text();
    }

    /// Replace `range` with `new_text`, recording the edit for undo and
    /// collapsing the caret after the inserted text. `range` must lie on char
    /// boundaries; out-of-range values are clamped.
    pub fn replace_range(&mut self, range: Range<usize>, new_text: &str) {
        let start = range.start.min(self.text.len());
        let end = range.end.clamp(start, self.text.len());
        let removed = self.text[start..end].to_string();
        let edit = Edit {
            at: start,
            removed,
            inserted: new_text.to_string(),
            selection_before: self.selection.clone(),
            reversed_before: self.reversed,
        };
        self.splice(start, end, new_text);
        self.undo.push(edit);
        self.redo.clear();
        let cursor = start + new_text.len();
        self.selection = cursor..cursor;
        self.reversed = false;
        self.goal_column = None;
    }

    /// Replace the current selection (or insert at the caret) with `new_text`.
    pub fn insert(&mut self, new_text: &str) {
        self.replace_range(self.selection.clone(), new_text);
    }

    /// Delete the selection, or the char before the caret when empty.
    pub fn backspace(&mut self) {
        if self.selection.is_empty() {
            let cursor = self.cursor();
            if cursor == 0 {
                return;
            }
            let start = self.prev_boundary(cursor);
            self.replace_range(start..cursor, "");
        } else {
            self.replace_range(self.selection.clone(), "");
        }
    }

    /// Delete the selection, or the char after the caret when empty.
    pub fn delete_forward(&mut self) {
        if self.selection.is_empty() {
            let cursor = self.cursor();
            if cursor == self.text.len() {
                return;
            }
            let end = self.next_boundary(cursor);
            self.replace_range(cursor..end, "");
        } else {
            self.replace_range(self.selection.clone(), "");
        }
    }

    pub fn undo(&mut self) -> bool {
        let Some(edit) = self.undo.pop() else {
            return false;
        };
        let start = edit.at;
        let end = start + edit.inserted.len();
        self.splice(start, end, &edit.removed);
        self.selection = edit.selection_before.clone();
        self.reversed = edit.reversed_before;
        self.goal_column = None;
        self.redo.push(edit);
        true
    }

    pub fn redo(&mut self) -> bool {
        let Some(edit) = self.redo.pop() else {
            return false;
        };
        let start = edit.at;
        let end = start + edit.removed.len();
        self.splice(start, end, &edit.inserted);
        let cursor = start + edit.inserted.len();
        self.selection = cursor..cursor;
        self.reversed = false;
        self.goal_column = None;
        self.undo.push(edit);
        true
    }

    // --- selection and movement (byte offsets over `text`) ---

    pub fn set_selection(&mut self, range: Range<usize>, reversed: bool) {
        let start = range.start.min(self.text.len());
        let end = range.end.clamp(start, self.text.len());
        self.selection = start..end;
        self.reversed = reversed && start != end;
        self.goal_column = None;
    }

    pub fn select_all(&mut self) {
        self.selection = 0..self.text.len();
        self.reversed = false;
        self.goal_column = None;
    }

    fn move_caret(&mut self, to: usize, extend: bool) {
        let to = to.min(self.text.len());
        self.goal_column = None;
        if extend {
            let anchor = self.anchor();
            if to >= anchor {
                self.selection = anchor..to;
                self.reversed = false;
            } else {
                self.selection = to..anchor;
                self.reversed = true;
            }
        } else {
            self.selection = to..to;
            self.reversed = false;
        }
    }

    fn prev_boundary(&self, offset: usize) -> usize {
        self.text[..offset]
            .char_indices()
            .next_back()
            .map_or(0, |(index, _)| index)
    }

    fn next_boundary(&self, offset: usize) -> usize {
        self.text[offset..]
            .chars()
            .next()
            .map_or(offset, |c| offset + c.len_utf8())
    }

    fn line_start(&self, offset: usize) -> usize {
        self.text[..offset].rfind('\n').map_or(0, |index| index + 1)
    }

    fn line_end(&self, offset: usize) -> usize {
        self.text[offset..]
            .find('\n')
            .map_or(self.text.len(), |index| offset + index)
    }

    /// Caret column, counted in characters from the start of its line.
    fn column(&self, offset: usize) -> usize {
        let start = self.line_start(offset);
        self.text[start..offset].chars().count()
    }

    /// Byte offset of `column` characters into the line beginning at `line_start`.
    fn offset_for_column(&self, line_start: usize, column: usize) -> usize {
        let line_end = self.line_end(line_start);
        let mut offset = line_start;
        for _ in 0..column {
            if offset >= line_end {
                break;
            }
            offset = self.next_boundary(offset);
        }
        offset
    }

    pub fn move_left(&mut self, extend: bool) {
        if !extend && !self.selection.is_empty() {
            self.move_caret(self.selection.start, false);
            return;
        }
        let cursor = self.cursor();
        self.move_caret(self.prev_boundary(cursor), extend);
    }

    pub fn move_right(&mut self, extend: bool) {
        if !extend && !self.selection.is_empty() {
            self.move_caret(self.selection.end, false);
            return;
        }
        let cursor = self.cursor();
        self.move_caret(self.next_boundary(cursor), extend);
    }

    pub fn move_up(&mut self, extend: bool) {
        let cursor = self.cursor();
        let column = self.goal_column.unwrap_or_else(|| self.column(cursor));
        let line_start = self.line_start(cursor);
        let target = if line_start == 0 {
            0
        } else {
            let prev_line_start = self.line_start(line_start - 1);
            self.offset_for_column(prev_line_start, column)
        };
        self.move_caret(target, extend);
        self.goal_column = Some(column);
    }

    pub fn move_down(&mut self, extend: bool) {
        let cursor = self.cursor();
        let column = self.goal_column.unwrap_or_else(|| self.column(cursor));
        let line_end = self.line_end(cursor);
        let target = if line_end == self.text.len() {
            self.text.len()
        } else {
            self.offset_for_column(line_end + 1, column)
        };
        self.move_caret(target, extend);
        self.goal_column = Some(column);
    }

    pub fn move_line_start(&mut self, extend: bool) {
        self.move_caret(self.line_start(self.cursor()), extend);
    }

    pub fn move_line_end(&mut self, extend: bool) {
        self.move_caret(self.line_end(self.cursor()), extend);
    }

    pub fn selected_text(&self) -> &str {
        &self.text[self.selection.clone()]
    }

    // --- statement targeting ---

    /// Byte ranges of each `;`-separated SQL statement, ignoring semicolons
    /// inside single-quoted strings, line comments, and block comments. Empty
    /// (whitespace-only) statements are omitted. Ranges exclude the terminator.
    pub fn statements(&self) -> Vec<Range<usize>> {
        let bytes = self.text.as_bytes();
        let mut ranges = Vec::new();
        let mut segment_start = 0usize;
        let mut in_string = false;
        let mut in_line_comment = false;
        let mut in_block_comment = false;
        let mut index = 0usize;
        while index < bytes.len() {
            let byte = bytes[index];
            if in_line_comment {
                if byte == b'\n' {
                    in_line_comment = false;
                }
                index += 1;
                continue;
            }
            if in_block_comment {
                if byte == b'*' && bytes.get(index + 1) == Some(&b'/') {
                    in_block_comment = false;
                    index += 2;
                    continue;
                }
                index += 1;
                continue;
            }
            if in_string {
                if byte == b'\'' {
                    in_string = false;
                }
                index += 1;
                continue;
            }
            match byte {
                b'\'' => in_string = true,
                b'-' if bytes.get(index + 1) == Some(&b'-') => {
                    in_line_comment = true;
                    index += 2;
                    continue;
                }
                b'/' if bytes.get(index + 1) == Some(&b'*') => {
                    in_block_comment = true;
                    index += 2;
                    continue;
                }
                b';' => {
                    push_statement(&self.text, segment_start, index, &mut ranges);
                    segment_start = index + 1;
                }
                _ => {}
            }
            index += 1;
        }
        push_statement(&self.text, segment_start, bytes.len(), &mut ranges);
        ranges
    }

    /// The statement range containing `offset`, preferring the one whose body
    /// the caret sits within or immediately after.
    pub fn statement_at(&self, offset: usize) -> Option<Range<usize>> {
        let statements = self.statements();
        statements
            .iter()
            .find(|range| offset >= range.start && offset <= range.end)
            .or_else(|| statements.iter().find(|range| offset < range.start))
            .cloned()
    }

    /// The statement the caret currently targets, if any.
    pub fn active_statement(&self) -> Option<Range<usize>> {
        self.statement_at(self.cursor())
    }

    // --- find ---

    /// Byte ranges of every occurrence of `needle`, overlapping matches skipped.
    pub fn find_matches(&self, needle: &str, case_sensitive: bool) -> Vec<Range<usize>> {
        if needle.is_empty() {
            return Vec::new();
        }
        if case_sensitive {
            self.text
                .match_indices(needle)
                .map(|(start, matched)| start..start + matched.len())
                .collect()
        } else {
            let haystack = self.text.to_lowercase();
            let needle = needle.to_lowercase();
            // Lowercasing can change byte length, so map matches back through a
            // parallel scan of the original text by character count instead of
            // trusting lowered byte offsets.
            find_ci(&self.text, &haystack, &needle)
        }
    }
}

fn push_statement(text: &str, start: usize, end: usize, ranges: &mut Vec<Range<usize>>) {
    let trimmed_start = start + (text[start..end].len() - text[start..end].trim_start().len());
    let trimmed_end = trimmed_start + text[trimmed_start..end].trim_end().len();
    if trimmed_end > trimmed_start {
        ranges.push(trimmed_start..trimmed_end);
    }
}

/// Case-insensitive search that reports offsets into the ORIGINAL text.
/// `lowered` is `original.to_lowercase()`; because a single source char can
/// lower to several, we walk both in lockstep by character.
fn find_ci(original: &str, lowered: &str, needle: &str) -> Vec<Range<usize>> {
    let mut matches = Vec::new();
    let orig_chars: Vec<(usize, char)> = original.char_indices().collect();
    let lower_starts: Vec<usize> = {
        // Byte offset in `lowered` where each original char's lowering begins.
        let mut starts = Vec::with_capacity(orig_chars.len() + 1);
        let mut cursor = 0usize;
        for (_, c) in &orig_chars {
            starts.push(cursor);
            cursor += c.to_lowercase().map(char::len_utf8).sum::<usize>();
        }
        starts.push(cursor);
        starts
    };
    for char_index in 0..orig_chars.len() {
        let lower_start = lower_starts[char_index];
        if lowered[lower_start..].starts_with(needle) {
            let orig_start = orig_chars[char_index].0;
            // Advance original chars until their lowering covers the needle.
            let target = lower_start + needle.len();
            let mut probe = char_index;
            while probe < orig_chars.len() && lower_starts[probe] < target {
                probe += 1;
            }
            let orig_end = orig_chars
                .get(probe)
                .map_or(original.len(), |(offset, _)| *offset);
            matches.push(orig_start..orig_end);
        }
    }
    matches
}

/// Raised by the editor for its owning query item to act on. The editor never
/// talks to the SDK directly; it reports intent and the workspace dispatches.
#[derive(Debug, Clone)]
pub enum EditorEvent {
    /// Run this SQL (the statement under the caret, or the whole document).
    Execute { sql: String },
}

actions!(
    sift_editor,
    [
        Backspace,
        DeleteForward,
        ExecuteStatement,
        ExecuteDocument,
        MoveLeft,
        MoveRight,
        MoveUp,
        MoveDown,
        SelectLeft,
        SelectRight,
        SelectUp,
        SelectDown,
        LineStart,
        LineEnd,
        SelectAll,
        Newline,
        Indent,
        Copy,
        Cut,
        Paste,
        Undo,
        Redo
    ]
);

/// Multi-line GPUI editor over a [`QueryDocument`]. Character and IME input flow
/// through the platform via [`EntityInputHandler`]; editing commands arrive as
/// typed actions the workspace keymap binds under the `SiftEditor` context.
pub struct QueryEditor {
    focus_handle: FocusHandle,
    document: QueryDocument,
    theme: Theme,
    marked_range: Option<Range<usize>>,
    line_layouts: Vec<ShapedLine>,
    line_starts: Vec<usize>,
    line_height: Pixels,
    last_bounds: Option<Bounds<Pixels>>,
}

impl QueryEditor {
    pub fn new(document: QueryDocument, theme: Theme, cx: &mut Context<Self>) -> Self {
        Self {
            focus_handle: cx.focus_handle(),
            document,
            theme,
            marked_range: None,
            line_layouts: Vec::new(),
            line_starts: Vec::new(),
            line_height: px(20.),
            last_bounds: None,
        }
    }

    pub fn document(&self) -> &QueryDocument {
        &self.document
    }

    pub fn set_theme(&mut self, theme: Theme, cx: &mut Context<Self>) {
        self.theme = theme;
        cx.notify();
    }

    fn edited(&mut self, cx: &mut Context<Self>) {
        self.marked_range = None;
        cx.notify();
    }

    fn backspace(&mut self, _: &Backspace, _: &mut Window, cx: &mut Context<Self>) {
        self.document.backspace();
        self.edited(cx);
    }

    fn delete_forward(&mut self, _: &DeleteForward, _: &mut Window, cx: &mut Context<Self>) {
        self.document.delete_forward();
        self.edited(cx);
    }

    fn newline(&mut self, _: &Newline, _: &mut Window, cx: &mut Context<Self>) {
        self.document.insert("\n");
        self.edited(cx);
    }

    fn indent(&mut self, _: &Indent, _: &mut Window, cx: &mut Context<Self>) {
        self.document.insert("  ");
        self.edited(cx);
    }

    fn move_left(&mut self, _: &MoveLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.document.move_left(false);
        cx.notify();
    }

    fn move_right(&mut self, _: &MoveRight, _: &mut Window, cx: &mut Context<Self>) {
        self.document.move_right(false);
        cx.notify();
    }

    fn move_up(&mut self, _: &MoveUp, _: &mut Window, cx: &mut Context<Self>) {
        self.document.move_up(false);
        cx.notify();
    }

    fn move_down(&mut self, _: &MoveDown, _: &mut Window, cx: &mut Context<Self>) {
        self.document.move_down(false);
        cx.notify();
    }

    fn select_left(&mut self, _: &SelectLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.document.move_left(true);
        cx.notify();
    }

    fn select_right(&mut self, _: &SelectRight, _: &mut Window, cx: &mut Context<Self>) {
        self.document.move_right(true);
        cx.notify();
    }

    fn select_up(&mut self, _: &SelectUp, _: &mut Window, cx: &mut Context<Self>) {
        self.document.move_up(true);
        cx.notify();
    }

    fn select_down(&mut self, _: &SelectDown, _: &mut Window, cx: &mut Context<Self>) {
        self.document.move_down(true);
        cx.notify();
    }

    fn line_start(&mut self, _: &LineStart, _: &mut Window, cx: &mut Context<Self>) {
        self.document.move_line_start(false);
        cx.notify();
    }

    fn line_end(&mut self, _: &LineEnd, _: &mut Window, cx: &mut Context<Self>) {
        self.document.move_line_end(false);
        cx.notify();
    }

    fn select_all(&mut self, _: &SelectAll, _: &mut Window, cx: &mut Context<Self>) {
        self.document.select_all();
        cx.notify();
    }

    fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        let selected = self.document.selected_text();
        if !selected.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(selected.to_string()));
        }
    }

    fn cut(&mut self, _: &Cut, window: &mut Window, cx: &mut Context<Self>) {
        self.copy(&Copy, window, cx);
        if !self.document.selection().is_empty() {
            self.document.insert("");
            self.edited(cx);
        }
    }

    fn paste(&mut self, _: &Paste, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
            self.document.insert(&text);
            self.edited(cx);
        }
    }

    fn undo(&mut self, _: &Undo, _: &mut Window, cx: &mut Context<Self>) {
        if self.document.undo() {
            self.edited(cx);
        }
    }

    fn redo(&mut self, _: &Redo, _: &mut Window, cx: &mut Context<Self>) {
        if self.document.redo() {
            self.edited(cx);
        }
    }

    fn execute_statement(&mut self, _: &ExecuteStatement, _: &mut Window, cx: &mut Context<Self>) {
        let sql = self
            .document
            .active_statement()
            .map(|range| self.document.text()[range].to_string())
            .filter(|sql| !sql.trim().is_empty())
            .unwrap_or_else(|| self.document.text().trim().to_string());
        if !sql.trim().is_empty() {
            cx.emit(EditorEvent::Execute { sql });
        }
    }

    fn execute_document(&mut self, _: &ExecuteDocument, _: &mut Window, cx: &mut Context<Self>) {
        let sql = self.document.text().trim().to_string();
        if !sql.is_empty() {
            cx.emit(EditorEvent::Execute { sql });
        }
    }

    fn offset_from_utf16(&self, offset: usize) -> usize {
        offset_from_utf16(self.document.text(), offset)
    }

    fn offset_to_utf16(&self, offset: usize) -> usize {
        offset_to_utf16(self.document.text(), offset)
    }

    fn range_from_utf16(&self, range: &Range<usize>) -> Range<usize> {
        self.offset_from_utf16(range.start)..self.offset_from_utf16(range.end)
    }

    fn range_to_utf16(&self, range: &Range<usize>) -> Range<usize> {
        self.offset_to_utf16(range.start)..self.offset_to_utf16(range.end)
    }

    /// Line index and byte offset of a caret within its line.
    fn line_of(&self, offset: usize) -> (usize, usize) {
        let text = self.document.text();
        let line = text[..offset].matches('\n').count();
        let line_start = text[..offset].rfind('\n').map_or(0, |index| index + 1);
        (line, line_start)
    }
}

impl EventEmitter<EditorEvent> for QueryEditor {}

impl Focusable for QueryEditor {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl EntityInputHandler for QueryEditor {
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        actual_range: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        let range = self.range_from_utf16(&range_utf16);
        actual_range.replace(self.range_to_utf16(&range));
        Some(self.document.text()[range].to_string())
    }

    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: self.range_to_utf16(&self.document.selection()),
            reversed: self.document.is_reversed(),
        })
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        self.marked_range
            .as_ref()
            .map(|range| self.range_to_utf16(range))
    }

    fn unmark_text(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        self.marked_range = None;
        cx.notify();
    }

    fn replace_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = range_utf16
            .as_ref()
            .map(|range| self.range_from_utf16(range))
            .or_else(|| self.marked_range.clone());
        match range {
            Some(range) => self.document.replace_range(range, new_text),
            None => self.document.insert(new_text),
        }
        self.marked_range = None;
        cx.notify();
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        new_selected_range_utf16: Option<Range<usize>>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = range_utf16
            .as_ref()
            .map(|range| self.range_from_utf16(range))
            .or_else(|| self.marked_range.clone())
            .unwrap_or_else(|| self.document.selection());
        self.document.replace_range(range.clone(), new_text);
        self.marked_range =
            (!new_text.is_empty()).then(|| range.start..range.start + new_text.len());
        if let Some(selection) = new_selected_range_utf16 {
            let start = offset_from_utf16(new_text, selection.start) + range.start;
            let end = offset_from_utf16(new_text, selection.end) + range.start;
            self.document.set_selection(start..end, false);
        }
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        bounds: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let range = self.range_from_utf16(&range_utf16);
        let (line, line_start) = self.line_of(range.start);
        let layout = self.line_layouts.get(line)?;
        let x = layout.x_for_index(range.start - line_start);
        let top = bounds.top() + self.line_height * line as f32;
        Some(Bounds::from_corners(
            point(bounds.left() + x, top),
            point(bounds.left() + x, top + self.line_height),
        ))
    }

    fn character_index_for_point(
        &mut self,
        point: gpui::Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        let bounds = self.last_bounds?;
        let relative_y = f32::from(point.y - bounds.top()).max(0.0);
        let line = ((relative_y / f32::from(self.line_height)) as usize)
            .min(self.line_starts.len().saturating_sub(1));
        let line_start = *self.line_starts.get(line)?;
        let layout = self.line_layouts.get(line)?;
        let within = layout.index_for_x(point.x - bounds.left()).unwrap_or(0);
        Some(self.offset_to_utf16(line_start + within))
    }
}

impl gpui::Render for QueryEditor {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("sift-query-editor")
            .key_context("SiftEditor")
            .role(Role::TextInput)
            .aria_label("SQL query editor")
            .track_focus(&self.focus_handle)
            .cursor(CursorStyle::IBeam)
            .size_full()
            .min_h_0()
            .overflow_hidden()
            .px_3()
            .py_2()
            .text_color(self.theme.colors.text)
            // Clicking the editor focuses it directly (synchronously), so the
            // SiftEditor key context is active and editing keys route.
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|editor, _, window, cx| {
                    editor.focus_handle.clone().focus(window, cx);
                }),
            )
            .on_action(cx.listener(Self::backspace))
            .on_action(cx.listener(Self::delete_forward))
            .on_action(cx.listener(Self::newline))
            .on_action(cx.listener(Self::indent))
            .on_action(cx.listener(Self::move_left))
            .on_action(cx.listener(Self::move_right))
            .on_action(cx.listener(Self::move_up))
            .on_action(cx.listener(Self::move_down))
            .on_action(cx.listener(Self::select_left))
            .on_action(cx.listener(Self::select_right))
            .on_action(cx.listener(Self::select_up))
            .on_action(cx.listener(Self::select_down))
            .on_action(cx.listener(Self::line_start))
            .on_action(cx.listener(Self::line_end))
            .on_action(cx.listener(Self::select_all))
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::cut))
            .on_action(cx.listener(Self::paste))
            .on_action(cx.listener(Self::undo))
            .on_action(cx.listener(Self::redo))
            .on_action(cx.listener(Self::execute_statement))
            .on_action(cx.listener(Self::execute_document))
            .child(QueryEditorElement {
                editor: cx.entity(),
            })
    }
}

struct QueryEditorElement {
    editor: Entity<QueryEditor>,
}

struct EditorPrepaint {
    lines: Vec<ShapedLine>,
    line_starts: Vec<usize>,
    selections: Vec<PaintQuad>,
    cursor: Option<PaintQuad>,
    line_height: Pixels,
}

impl IntoElement for QueryEditorElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for QueryEditorElement {
    type RequestLayoutState = ();
    type PrepaintState = EditorPrepaint;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let line_count = self.editor.read(cx).document.text().split('\n').count();
        let mut style = Style::default();
        style.size.width = gpui::relative(1.).into();
        style.size.height = (window.line_height() * line_count.max(1) as f32).into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let editor = self.editor.read(cx);
        let text = editor.document.text().to_string();
        let selection = editor.document.selection();
        let cursor = editor.document.cursor();
        let theme = editor.theme;
        let style = window.text_style();
        let font_size = style.font_size.to_pixels(window.rem_size());
        let line_height = window.line_height();

        let run_color = theme.colors.text;
        let mut lines = Vec::new();
        let mut line_starts = Vec::new();
        let mut selections = Vec::new();
        let mut cursor_quad = None;

        let mut offset = 0usize;
        for (line_index, line) in text.split('\n').enumerate() {
            line_starts.push(offset);
            let line_end = offset + line.len();
            let run = TextRun {
                len: line.len(),
                font: style.font(),
                color: run_color,
                background_color: None,
                underline: None,
                strikethrough: None,
            };
            let runs = if line.is_empty() { vec![] } else { vec![run] };
            let shaped =
                window
                    .text_system()
                    .shape_line(line.to_string().into(), font_size, &runs, None);
            let top = bounds.top() + line_height * line_index as f32;

            // Selection rectangle for the portion of this line inside the range.
            if !selection.is_empty() {
                let sel_start = selection.start.clamp(offset, line_end);
                let sel_end = selection.end.clamp(offset, line_end);
                let spans_newline = selection.end > line_end;
                if sel_start < sel_end || (selection.start <= offset && spans_newline) {
                    let x0 = shaped.x_for_index(sel_start - offset);
                    let x1 = if spans_newline {
                        shaped.x_for_index(line.len()) + px(6.)
                    } else {
                        shaped.x_for_index(sel_end - offset)
                    };
                    selections.push(fill(
                        Bounds::from_corners(
                            point(bounds.left() + x0, top),
                            point(bounds.left() + x1, top + line_height),
                        ),
                        theme.colors.selected_surface,
                    ));
                }
            }

            if selection.is_empty() && cursor >= offset && cursor <= line_end {
                let x = shaped.x_for_index(cursor - offset);
                cursor_quad = Some(fill(
                    Bounds::new(point(bounds.left() + x, top), size(px(1.5), line_height)),
                    theme.colors.accent,
                ));
            }

            lines.push(shaped);
            offset = line_end + 1;
        }

        EditorPrepaint {
            lines,
            line_starts,
            selections,
            cursor: cursor_quad,
            line_height,
        }
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let focus_handle = self.editor.read(cx).focus_handle.clone();
        window.handle_input(
            &focus_handle,
            ElementInputHandler::new(bounds, self.editor.clone()),
            cx,
        );
        for selection in prepaint.selections.drain(..) {
            window.paint_quad(selection);
        }
        let line_height = prepaint.line_height;
        for (index, line) in prepaint.lines.iter().enumerate() {
            let origin = point(bounds.left(), bounds.top() + line_height * index as f32);
            line.paint(origin, line_height, gpui::TextAlign::Left, None, window, cx)
                .expect("editor line paint succeeds");
        }
        if focus_handle.is_focused(window) {
            if let Some(cursor) = prepaint.cursor.take() {
                window.paint_quad(cursor);
            }
        }
        let lines = std::mem::take(&mut prepaint.lines);
        let line_starts = std::mem::take(&mut prepaint.line_starts);
        self.editor.update(cx, |editor, _| {
            editor.line_layouts = lines;
            editor.line_starts = line_starts;
            editor.line_height = line_height;
            editor.last_bounds = Some(bounds);
        });
    }
}

fn offset_from_utf16(text: &str, offset: usize) -> usize {
    let mut utf8_offset = 0;
    let mut utf16_count = 0;
    for character in text.chars() {
        if utf16_count >= offset {
            break;
        }
        utf16_count += character.len_utf16();
        utf8_offset += character.len_utf8();
    }
    utf8_offset
}

fn offset_to_utf16(text: &str, offset: usize) -> usize {
    let mut utf16_offset = 0;
    let mut utf8_count = 0;
    for character in text.chars() {
        if utf8_count >= offset {
            break;
        }
        utf8_count += character.len_utf8();
        utf16_offset += character.len_utf16();
    }
    utf16_offset
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{TestAppContext, VisualTestContext};

    fn doc(text: &str) -> QueryDocument {
        QueryDocument::new(7, text)
    }

    #[gpui::test]
    fn editor_view_routes_input_and_editing_actions(cx: &mut TestAppContext) {
        let window = cx
            .update(|cx| {
                cx.open_window(Default::default(), |_window, cx| {
                    cx.new(|cx| QueryEditor::new(doc("select"), Theme::dark(), cx))
                })
            })
            .unwrap();
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        let editor = window.root(&mut cx).unwrap();

        // Platform text input flows through the entity's input handler.
        editor.update_in(&mut cx, |editor, window, cx| {
            editor.document.set_selection(6..6, false);
            editor.replace_text_in_range(None, " 1", window, cx);
        });
        assert_eq!(
            editor.read_with(&cx, |editor, _| editor.document().text().to_string()),
            "select 1"
        );

        // Typed editing actions dispatch to the focused editor.
        let focus = editor.read_with(&cx, |editor, cx| editor.focus_handle(cx));
        cx.update(|window, cx| focus.dispatch_action(&Backspace, window, cx));
        assert_eq!(
            editor.read_with(&cx, |editor, _| editor.document().text().to_string()),
            "select "
        );
        cx.update(|window, cx| focus.dispatch_action(&Undo, window, cx));
        assert_eq!(
            editor.read_with(&cx, |editor, _| editor.document().text().to_string()),
            "select 1"
        );
        cx.update(|window, cx| focus.dispatch_action(&Newline, window, cx));
        assert_eq!(
            editor.read_with(&cx, |editor, _| editor.document().text().to_string()),
            "select 1\n"
        );
    }

    #[test]
    fn insert_and_replica_stay_consistent() {
        let mut document = doc("select");
        document.set_selection(6..6, false);
        document.insert(" 1");
        assert_eq!(document.text(), "select 1");
        assert_eq!(document.replica.text(), "select 1");
        assert_eq!(document.selection(), 8..8);
    }

    #[test]
    fn undo_and_redo_round_trip_text_and_selection() {
        let mut document = doc("a");
        document.set_selection(1..1, false);
        document.insert("bc");
        assert_eq!(document.text(), "abc");
        assert!(document.undo());
        assert_eq!(document.text(), "a");
        assert_eq!(document.selection(), 1..1);
        assert!(document.redo());
        assert_eq!(document.text(), "abc");
        assert_eq!(document.replica.text(), "abc");
        assert!(!document.redo());
    }

    #[test]
    fn backspace_on_selection_deletes_the_range() {
        let mut document = doc("hello world");
        document.set_selection(5..11, false);
        document.backspace();
        assert_eq!(document.text(), "hello");
        assert!(document.undo());
        assert_eq!(document.text(), "hello world");
    }

    #[test]
    fn vertical_movement_preserves_column() {
        let mut document = doc("abcd\nef\nghij");
        document.set_selection(3..3, false); // after 'c' on line 0
        document.move_down(false);
        assert_eq!(document.cursor(), 7); // clamped to end of short line "ef"
        document.move_down(false);
        assert_eq!(document.cursor(), 11); // column 3 on "ghij" -> before 'j'
        document.move_up(false);
        document.move_up(false);
        assert_eq!(document.cursor(), 3);
    }

    #[test]
    fn extend_selection_tracks_anchor_across_directions() {
        let mut document = doc("abcdef");
        document.set_selection(3..3, false);
        document.move_left(true);
        document.move_left(true);
        assert_eq!(document.selection(), 1..3);
        assert!(document.is_reversed());
        document.move_right(true);
        document.move_right(true);
        document.move_right(true);
        assert_eq!(document.selection(), 3..4);
        assert!(!document.is_reversed());
    }

    #[test]
    fn statements_ignore_semicolons_in_strings_and_comments() {
        let document = doc("select 1; -- a; b\nselect ';' ; select 3");
        let statements = document.statements();
        let rendered: Vec<&str> = statements
            .iter()
            .map(|range| &document.text()[range.clone()])
            .collect();
        assert_eq!(
            rendered,
            vec!["select 1", "-- a; b\nselect ';'", "select 3"]
        );
    }

    #[test]
    fn statement_at_targets_the_caret_statement() {
        let document = doc("select 1;\nselect 2;\nselect 3");
        let target = document.statement_at(12).unwrap();
        assert_eq!(&document.text()[target], "select 2");
    }

    #[test]
    fn find_is_case_insensitive_by_default() {
        let document = doc("Select id FROM users where id = 1 select");
        let matches = document.find_matches("select", false);
        assert_eq!(matches.len(), 2);
        assert_eq!(&document.text()[matches[0].clone()], "Select");
        assert_eq!(&document.text()[matches[1].clone()], "select");
        let sensitive = document.find_matches("select", true);
        assert_eq!(sensitive.len(), 1);
    }

    #[test]
    fn find_handles_multibyte_text() {
        let document = doc("café CAFÉ café");
        let matches = document.find_matches("café", false);
        assert_eq!(matches.len(), 3);
        for range in matches {
            assert!(document.text().is_char_boundary(range.start));
            assert!(document.text().is_char_boundary(range.end));
        }
    }
}
