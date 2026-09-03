//! The SQL query editor: a Loro-backed document model plus a multi-line GPUI
//! view. The document is the M3 vertical-slice core — selections, editing with
//! undo/redo, statement targeting, and find — kept free of GPUI so it is fully
//! unit-testable. The view renders it and bridges platform text/IME input.

use std::{cell::RefCell, collections::HashMap, ops::Range, sync::Arc, time::Duration};

use gpui::{
    actions, div, fill, outline, point, prelude::*, px, size, App, BorderStyle, Bounds,
    ClipboardItem, Context, CursorStyle, Element, ElementId, ElementInputHandler, Entity,
    EntityInputHandler, EventEmitter, FocusHandle, Focusable, GlobalElementId, IntoElement,
    LayoutId, MouseButton, PaintQuad, Pixels, Role, ScrollHandle, ShapedLine, Style, TextRun,
    UTF16Selection, Window,
};
use modalkit::editing::{application::EmptyInfo, store::SharedStore};
use sift_doc::{random_peer_id, TextReplica};
use sift_ui::{
    ActiveTheme, Button, ButtonTone, Clickable, Disableable, IconButton, IconName, TextInput, Theme,
};

mod semantic;
mod vim;
use self::semantic::{
    completion_candidate_metadata, completion_kind_badge, ordered_edits, usage_kind_label,
};
pub use self::semantic::{
    CompletionMenu, EditorDiagnostic, SemanticOutcome, SemanticRequestKind, SemanticState,
};
use self::vim::{VimEngine, VimSnapshot};

struct GlobalVimStore(SharedStore<EmptyInfo>);

impl gpui::Global for GlobalVimStore {}

fn shared_vim_store(cx: &mut App) -> SharedStore<EmptyInfo> {
    if let Some(store) = cx.try_global::<GlobalVimStore>() {
        return store.0.clone();
    }
    let store = modalkit::editing::store::Store::default().shared();
    cx.set_global(GlobalVimStore(store.clone()));
    store
}

const EDITOR_LINE_HEIGHT: Pixels = px(20.);
const BLOCK_CURSOR_FALLBACK_WIDTH: Pixels = px(7.);
pub(crate) const EDITOR_GUTTER_WIDTH: Pixels = px(48.);
const EDITOR_TEXT_INSET: Pixels = px(12.);
const EDITOR_VERTICAL_INSET: Pixels = px(8.);
const DIAGNOSTIC_UNDERLINE_HEIGHT: Pixels = px(2.);
/// Zero-width server ranges (end-of-statement errors) still need a visible
/// mark, so they paint as a narrow stub rather than nothing.
const EMPTY_SPAN_WIDTH: Pixels = px(6.);
const COMPLETION_MENU_WIDTH: Pixels = px(440.);
const COMPLETION_ROW_HEIGHT: Pixels = px(22.);
const COMPLETION_VISIBLE_ROWS: usize = 9;

#[cfg(test)]
fn line_starts(text: &str) -> Vec<usize> {
    line_indices(text).0
}

fn line_indices(text: &str) -> (Vec<usize>, Vec<usize>) {
    let mut starts = vec![0];
    let mut char_starts = vec![0];
    let mut chars = 0;
    for (offset, character) in text.char_indices() {
        chars += 1;
        if character == '\n' {
            starts.push(offset + 1);
            char_starts.push(chars);
        }
    }
    (starts, char_starts)
}

fn identifier_hover_position(text: &str, offset: usize) -> Option<u32> {
    let mut probe = offset.min(text.len());
    if probe == text.len() {
        probe = text[..probe].char_indices().next_back()?.0;
    } else if !matches!(
        text[probe..].chars().next(),
        Some(character) if identifier_character(character)
    ) {
        return None;
    }
    let character = text[probe..].chars().next()?;
    if !identifier_character(character) {
        return None;
    }
    while let Some((previous, character)) = text[..probe].char_indices().next_back() {
        if !identifier_character(character) {
            break;
        }
        probe = previous;
    }
    u32::try_from(probe).ok()
}

fn identifier_character(character: char) -> bool {
    character.is_alphanumeric() || character == '_' || character as u32 >= 0x80
}

/// Cheap activation guard only. The server parser still owns SQL context and
/// candidate correctness. Keeping this linear scan local prevents catalog or
/// parser work for comments, string literals, and punctuation-heavy edits.
fn should_auto_complete(text: &str, cursor: usize) -> bool {
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum ScanState {
        Sql,
        String,
        QuotedIdentifier,
        BracketIdentifier,
        LineComment,
        BlockComment,
    }

    let cursor = cursor.min(text.len());
    let prefix = &text[..cursor];
    let mut state = ScanState::Sql;
    let mut chars = prefix.char_indices().peekable();
    while let Some((_, character)) = chars.next() {
        let next = chars.peek().map(|(_, next)| *next);
        state = match (state, character, next) {
            (ScanState::Sql, '\'', _) => ScanState::String,
            (ScanState::Sql, '"', _) => ScanState::QuotedIdentifier,
            (ScanState::Sql, '[', _) => ScanState::BracketIdentifier,
            (ScanState::Sql, '-', Some('-')) => {
                chars.next();
                ScanState::LineComment
            }
            (ScanState::Sql, '/', Some('*')) => {
                chars.next();
                ScanState::BlockComment
            }
            (ScanState::String, '\'', Some('\'')) => {
                chars.next();
                ScanState::String
            }
            (ScanState::String, '\'', _) => ScanState::Sql,
            (ScanState::QuotedIdentifier, '"', Some('"')) => {
                chars.next();
                ScanState::QuotedIdentifier
            }
            (ScanState::QuotedIdentifier, '"', _) => ScanState::Sql,
            (ScanState::BracketIdentifier, ']', Some(']')) => {
                chars.next();
                ScanState::BracketIdentifier
            }
            (ScanState::BracketIdentifier, ']', _) => ScanState::Sql,
            (ScanState::LineComment, '\n', _) => ScanState::Sql,
            (ScanState::BlockComment, '*', Some('/')) => {
                chars.next();
                ScanState::Sql
            }
            _ => state,
        };
    }
    if matches!(
        state,
        ScanState::String | ScanState::LineComment | ScanState::BlockComment
    ) {
        return false;
    }

    let Some(last) = prefix.chars().next_back() else {
        return false;
    };
    if last == '.' {
        return true;
    }
    if identifier_character(last) {
        let typed = prefix
            .chars()
            .rev()
            .take_while(|character| identifier_character(*character))
            .count();
        return typed >= 2;
    }
    if last.is_whitespace() {
        let keyword = prefix
            .trim_end()
            .chars()
            .rev()
            .take_while(|character| identifier_character(*character))
            .collect::<String>()
            .chars()
            .rev()
            .collect::<String>();
        return matches!(
            keyword.to_ascii_lowercase().as_str(),
            "select"
                | "from"
                | "join"
                | "update"
                | "into"
                | "table"
                | "where"
                | "on"
                | "set"
                | "by"
        );
    }
    false
}

fn hover_type_display(type_ref: &sift_protocol::TypeRef) -> String {
    match type_ref {
        sift_protocol::TypeRef::Native { name, .. } => name.clone(),
        sift_protocol::TypeRef::Primitive(primitive) => {
            format!("{primitive:?}").to_ascii_lowercase()
        }
    }
}

fn valid_star_expansion_source(source: &str) -> bool {
    let source = source.trim();
    let Some(prefix) = source.strip_suffix('*') else {
        return false;
    };
    let prefix = prefix.trim_end();
    if prefix.is_empty() {
        return true;
    }
    let Some(qualifier) = prefix.strip_suffix('.') else {
        return false;
    };
    let qualifier = qualifier.trim();
    !qualifier.is_empty()
        && qualifier.chars().all(|character| {
            identifier_character(character) || matches!(character, '"' | '[' | ']')
        })
}

/// Pixel bounds of the part of `range` that falls on the line starting at
/// `line_start`, or `None` when the range misses this line entirely.
fn span_bounds(
    range: &Range<usize>,
    line_start: usize,
    line_end: usize,
    shaped: &ShapedLine,
    text_left: Pixels,
    top: Pixels,
    line_height: Pixels,
) -> Option<Bounds<Pixels>> {
    if range.start > line_end || range.end < line_start {
        return None;
    }
    let start = range.start.clamp(line_start, line_end);
    let end = range.end.clamp(line_start, line_end);
    let x0 = shaped.x_for_index(start - line_start);
    let x1 = if end > start {
        shaped
            .x_for_index(end - line_start)
            .max(x0 + EMPTY_SPAN_WIDTH)
    } else {
        x0 + EMPTY_SPAN_WIDTH
    };
    Some(Bounds::from_corners(
        point(text_left + x0, top),
        point(text_left + x1, top + line_height),
    ))
}

fn floor_char_boundary(text: &str, mut offset: usize) -> usize {
    offset = offset.min(text.len());
    while !text.is_char_boundary(offset) {
        offset -= 1;
    }
    offset
}

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

#[derive(Debug, Clone, Copy)]
struct DocumentChange {
    line: usize,
    structural: bool,
}

/// A collaborative SQL document. Text lives in a Loro [`TextReplica`] so the
/// same buffer can later sync with the server room; the model caches the
/// materialized text and owns a byte-offset selection. Loro indexes by Unicode
/// scalar, so every replica call converts from the byte offsets the view uses.
pub struct QueryDocument {
    replica: TextReplica,
    text: String,
    line_starts: Arc<Vec<usize>>,
    line_char_starts: Vec<usize>,
    selection: Range<usize>,
    reversed: bool,
    /// Sticky column for vertical movement, so up/down over short lines keeps
    /// the caret's horizontal intent. Cleared by any horizontal move or edit.
    goal_column: Option<usize>,
    undo: Vec<Edit>,
    redo: Vec<Edit>,
    last_change: Option<DocumentChange>,
    pending_room_update: Option<Vec<u8>>,
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
        let (line_starts, line_char_starts) = line_indices(&text);
        Self {
            replica,
            text,
            line_starts: Arc::new(line_starts),
            line_char_starts,
            selection: end..end,
            reversed: false,
            goal_column: None,
            undo: Vec::new(),
            redo: Vec::new(),
            last_change: None,
            pending_room_update: None,
        }
    }

    /// A document with a fresh random peer id — the desktop's default.
    pub fn with_random_peer(initial: &str) -> Self {
        Self::new(random_peer_id(), initial)
    }

    pub fn from_room_snapshot(snapshot: &[u8]) -> Result<Self, sift_doc::DocError> {
        let replica = TextReplica::from_snapshot(random_peer_id(), snapshot)?;
        let text = replica.text();
        let end = text.len();
        let (line_starts, line_char_starts) = line_indices(&text);
        Ok(Self {
            replica,
            text,
            line_starts: Arc::new(line_starts),
            line_char_starts,
            selection: end..end,
            reversed: false,
            goal_column: None,
            undo: Vec::new(),
            redo: Vec::new(),
            last_change: None,
            pending_room_update: None,
        })
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    fn line_starts(&self) -> Arc<Vec<usize>> {
        self.line_starts.clone()
    }

    fn line_count(&self) -> usize {
        self.line_starts.len()
    }

    fn line_of_offset(&self, offset: usize) -> usize {
        self.line_starts
            .partition_point(|start| *start <= offset.min(self.text.len()))
            .saturating_sub(1)
    }

    /// Replace the materialized buffer after a room replica sync. The room
    /// replica owns merge history; this view replica is rebuilt so remote
    /// changes do not masquerade as locally undoable edits.
    pub fn replace_from_room(&mut self, snapshot: &[u8]) -> Result<bool, sift_doc::DocError> {
        let replacement = Self::from_room_snapshot(snapshot)?;
        if self.text == replacement.text {
            // Even an equal materialization may carry a newer CRDT frontier.
            self.replica = replacement.replica;
            return Ok(false);
        }
        let cursor = self.cursor().min(replacement.text.len());
        *self = replacement;
        let cursor = floor_char_boundary(&self.text, cursor);
        self.selection = cursor..cursor;
        Ok(true)
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

    /// One-based line and Unicode-scalar column for status-bar presentation.
    pub fn cursor_position(&self) -> (usize, usize) {
        let cursor = self.cursor();
        let line = self.line_of_offset(cursor);
        let line_start = self.line_starts[line];
        (line + 1, self.text[line_start..cursor].chars().count() + 1)
    }

    pub fn cursor_offset(&self) -> usize {
        self.cursor()
    }

    /// Apply a text splice against the replica and refresh the cached text. Only
    /// touches CRDT state; selection and history are the caller's concern.
    fn splice(&mut self, start: usize, end: usize, new_text: &str) {
        let since = self.replica.version_vector();
        let line = self.line_of_offset(start);
        let line_start = self.line_starts[line];
        let char_start = self.line_char_starts[line] + self.text[line_start..start].chars().count();
        let removed_chars = self.text[start..end].chars().count();
        let inserted_chars = new_text.chars().count();
        let structural = self.text[start..end].contains('\n') || new_text.contains('\n');
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
        self.text.replace_range(start..end, new_text);
        if structural {
            let (line_starts, line_char_starts) = line_indices(&self.text);
            self.line_starts = Arc::new(line_starts);
            self.line_char_starts = line_char_starts;
        } else {
            let byte_delta = new_text.len() as isize - (end - start) as isize;
            let char_delta = inserted_chars as isize - removed_chars as isize;
            for line_start in Arc::make_mut(&mut self.line_starts)
                .iter_mut()
                .skip(line + 1)
            {
                *line_start = line_start.saturating_add_signed(byte_delta);
            }
            for char_start in self.line_char_starts.iter_mut().skip(line + 1) {
                *char_start = char_start.saturating_add_signed(char_delta);
            }
        }
        self.pending_room_update = self
            .replica
            .updates_since_if_any(&since)
            .expect("export local editor update");
    }

    fn take_room_update(&mut self) -> Option<Vec<u8>> {
        self.pending_room_update.take()
    }

    /// Replace `range` with `new_text`, recording the edit for undo and
    /// collapsing the caret after the inserted text. `range` must lie on char
    /// boundaries; out-of-range values are clamped.
    pub fn replace_range(&mut self, range: Range<usize>, new_text: &str) {
        let start = range.start.min(self.text.len());
        let end = range.end.clamp(start, self.text.len());
        let removed = self.text[start..end].to_string();
        self.last_change = Some(DocumentChange {
            line: self.line_of_offset(start),
            structural: removed.contains('\n') || new_text.contains('\n'),
        });
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
        self.last_change = Some(DocumentChange {
            line: self.line_of_offset(start),
            structural: edit.inserted.contains('\n') || edit.removed.contains('\n'),
        });
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
        self.last_change = Some(DocumentChange {
            line: self.line_of_offset(start),
            structural: edit.inserted.contains('\n') || edit.removed.contains('\n'),
        });
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
    /// Document text changed and the owning tab must become dirty.
    DocumentChanged { update: Vec<u8> },
    /// Cursor or modal state changed; parent chrome may refresh lazily.
    CursorChanged,
    /// Pending Vim keys or mode changed; status chrome should refresh now.
    VimStateChanged,
    /// Vim's command prefix requested the workspace command palette.
    OpenCommandPalette,
    /// Run this SQL (the statement under the caret, or the whole document).
    Execute { sql: String },
    /// Ask the workspace to drive the server semantic document. `revision` is
    /// this editor's text revision; answers that no longer match it are
    /// dropped rather than applied to a buffer that has moved on.
    SemanticRequest {
        revision: u64,
        request: SemanticRequestKind,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditorLanguage {
    Sql,
    Toml,
    Json,
    Markdown,
    PlainText,
}

#[derive(Debug, Clone)]
pub enum JsonSchema {
    Keymaps { command_ids: Arc<[String]> },
}

impl JsonSchema {
    pub fn keymaps(command_ids: impl IntoIterator<Item = String>) -> Self {
        Self::Keymaps {
            command_ids: command_ids.into_iter().collect(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditorKeymap {
    Standard,
    Vim,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VimMode {
    Normal,
    Insert,
    Visual,
    Select,
    OperatorPending,
    Command,
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
        VimUndo,
        Redo,
        ExitInsertMode,
        Complete,
        ExpandStar,
        FormatDocument,
        ApplyQuickFix,
        FindUsages,
        GoToNextDiagnostic,
        OpenFind,
        FindNext,
        FindPrevious,
        ReplaceNext,
        ReplaceAll,
        CloseFind
    ]
);

const CURSOR_BLINK_INTERVAL: Duration = Duration::from_millis(530);
const CURSOR_BLINK_PAUSE: Duration = Duration::from_millis(500);

struct CursorBlink {
    epoch: usize,
    visible: bool,
    enabled: bool,
}

impl CursorBlink {
    fn new() -> Self {
        Self {
            epoch: 0,
            visible: true,
            enabled: false,
        }
    }

    fn enable(&mut self, cx: &mut Context<Self>) {
        if self.enabled {
            return;
        }
        self.enabled = true;
        self.visible = true;
        self.schedule(self.epoch, CURSOR_BLINK_INTERVAL, cx);
    }

    fn disable(&mut self, cx: &mut Context<Self>) {
        self.enabled = false;
        self.epoch = self.epoch.wrapping_add(1);
        if !self.visible {
            self.visible = true;
            cx.notify();
        }
    }

    fn pause(&mut self, cx: &mut Context<Self>) {
        if !self.enabled {
            return;
        }
        self.epoch = self.epoch.wrapping_add(1);
        self.visible = true;
        cx.notify();
        self.schedule(self.epoch, CURSOR_BLINK_PAUSE, cx);
    }

    fn schedule(&self, epoch: usize, delay: Duration, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(delay).await;
            let _ = this.update(cx, |this, cx| {
                if this.enabled && this.epoch == epoch {
                    this.visible = !this.visible;
                    cx.notify();
                    this.epoch = this.epoch.wrapping_add(1);
                    this.schedule(this.epoch, CURSOR_BLINK_INTERVAL, cx);
                }
            });
        })
        .detach();
    }
}

#[derive(Default)]
struct LineLayoutCache {
    lines: HashMap<usize, ShapedLine>,
}

#[derive(Default)]
struct FindMatchCache {
    revision: u64,
    query: String,
    case_sensitive: bool,
    matches: Arc<Vec<Range<usize>>>,
}

/// Multi-line GPUI editor over a [`QueryDocument`]. Character and IME input flow
/// through the platform via [`EntityInputHandler`]; editing commands arrive as
/// typed actions the workspace keymap binds under the `SiftEditor` context.
pub struct QueryEditor {
    focus_handle: FocusHandle,
    document: QueryDocument,
    language: EditorLanguage,
    diff_language: Option<EditorLanguage>,
    keymap: EditorKeymap,
    vim_mode: VimMode,
    vim_entered: String,
    vim_store: SharedStore<EmptyInfo>,
    vim: Option<VimEngine>,
    cursor_blink: Entity<CursorBlink>,
    cursor_event_pending: bool,
    revision: u64,
    line_cache: RefCell<LineLayoutCache>,
    marked_range: Option<Range<usize>>,
    line_layouts: Vec<ShapedLine>,
    visible_line_start: usize,
    line_starts: Arc<Vec<usize>>,
    line_height: Pixels,
    last_bounds: Option<Bounds<Pixels>>,
    scroll_handle: ScrollHandle,
    read_only: bool,
    semantic: SemanticState,
    hover_anchor: Option<(Pixels, Pixels)>,
    manifest_schema: bool,
    manifest_hover: Option<sift_instance_config::ManifestHover>,
    manifest_analysis_epoch: u64,
    manifest_lifecycle: Option<ManifestLifecycle>,
    json_schema: Option<JsonSchema>,
    find_open: bool,
    find_query: Entity<TextInput>,
    replace_query: Entity<TextInput>,
    find_case_sensitive: bool,
    find_cache: RefCell<FindMatchCache>,
    snippet_tabstops: Vec<Range<usize>>,
    snippet_tabstop_index: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ManifestLifecycle {
    applied: bool,
    missing_credentials: usize,
}

impl QueryEditor {
    pub fn new(document: QueryDocument, cx: &mut Context<Self>) -> Self {
        let cursor_blink = cx.new(|_| CursorBlink::new());
        cx.observe(&cursor_blink, |_, _, cx| cx.notify()).detach();
        let find_query = cx.new(|cx| TextInput::new("", "Find", cx).aria_label("Find text"));
        let replace_query =
            cx.new(|cx| TextInput::new("", "Replace", cx).aria_label("Replacement text"));
        cx.observe(&find_query, |_, _, cx| cx.notify()).detach();
        cx.observe(&replace_query, |_, _, cx| cx.notify()).detach();
        let vim_store = shared_vim_store(cx);
        Self {
            focus_handle: cx.focus_handle(),
            document,
            language: EditorLanguage::Sql,
            diff_language: None,
            keymap: EditorKeymap::Standard,
            vim_mode: VimMode::Insert,
            vim_entered: String::new(),
            vim_store,
            vim: None,
            cursor_blink,
            cursor_event_pending: false,
            revision: 1,
            line_cache: RefCell::new(LineLayoutCache::default()),
            marked_range: None,
            line_layouts: Vec::new(),
            visible_line_start: 0,
            line_starts: Arc::default(),
            line_height: EDITOR_LINE_HEIGHT,
            last_bounds: None,
            scroll_handle: ScrollHandle::new(),
            read_only: false,
            semantic: SemanticState::default(),
            hover_anchor: None,
            manifest_schema: false,
            manifest_hover: None,
            manifest_analysis_epoch: 0,
            manifest_lifecycle: None,
            json_schema: None,
            find_open: false,
            find_query,
            replace_query,
            find_case_sensitive: false,
            find_cache: RefCell::new(FindMatchCache::default()),
            snippet_tabstops: Vec::new(),
            snippet_tabstop_index: 0,
        }
    }

    pub fn with_language(mut self, language: EditorLanguage) -> Self {
        self.language = language;
        self
    }

    pub fn with_diff_language(mut self, language: EditorLanguage) -> Self {
        self.language = EditorLanguage::PlainText;
        self.diff_language = Some(language);
        self
    }

    pub fn with_json_schema(mut self, schema: JsonSchema) -> Self {
        self.json_schema = Some(schema);
        self.refresh_local_diagnostics();
        self
    }

    pub fn with_manifest_schema(mut self) -> Self {
        self.manifest_schema = true;
        self.manifest_lifecycle = Some(ManifestLifecycle {
            applied: false,
            missing_credentials: 0,
        });
        self.refresh_manifest_diagnostics();
        self
    }

    pub fn set_manifest_lifecycle(
        &mut self,
        applied: bool,
        missing_credentials: usize,
        cx: &mut Context<Self>,
    ) {
        self.manifest_lifecycle = Some(ManifestLifecycle {
            applied,
            missing_credentials,
        });
        cx.notify();
    }

    pub fn with_keymap(mut self, keymap: EditorKeymap) -> Self {
        self.apply_keymap(keymap);
        self
    }

    pub fn read_only(mut self) -> Self {
        self.read_only = true;
        self
    }

    /// Replace the complete document from its owning surface without emitting
    /// a collaborative edit. Used for read-only feeds and generated SQL views.
    pub fn replace_text_from_owner(&mut self, text: &str, cx: &mut Context<Self>) {
        if self.document.text() == text {
            return;
        }
        let length = self.document.text().len();
        self.document.replace_range(0..length, text);
        let _ = self.document.take_room_update();
        self.resync_keymap_after_external_change(cx);
        self.marked_range = None;
        self.revision = self.revision.wrapping_add(1);
        self.line_cache.borrow_mut().lines.clear();
        self.semantic.invalidate();
        if self.manifest_schema {
            self.refresh_manifest_diagnostics();
        } else if self.language == EditorLanguage::Json {
            self.refresh_local_diagnostics();
        } else {
            self.request_semantic(SemanticRequestKind::Analyze, cx);
        }
        cx.notify();
    }

    /// External replacements bypass ModalKit's edit path. Rebuild its rope and
    /// cursor group from the canonical document before accepting more input.
    fn resync_keymap_after_external_change(&mut self, cx: &mut Context<Self>) {
        if self.keymap != EditorKeymap::Vim {
            return;
        }
        let preserve_insert = self.vim_mode == VimMode::Insert;
        self.apply_keymap(EditorKeymap::Vim);
        if preserve_insert {
            let snapshot = self
                .vim
                .as_mut()
                .expect("Vim keymap must own an engine")
                .input_text("i");
            self.vim_mode = snapshot.mode;
        }
        cx.emit(EditorEvent::VimStateChanged);
    }

    pub fn keymap(&self) -> EditorKeymap {
        self.keymap
    }

    pub fn set_language(&mut self, language: EditorLanguage, cx: &mut Context<Self>) {
        if self.language == language && self.diff_language.is_none() {
            return;
        }
        self.language = language;
        self.diff_language = None;
        self.semantic.invalidate();
        if self.manifest_schema {
            self.refresh_manifest_diagnostics();
        } else if self.language == EditorLanguage::Json {
            self.refresh_local_diagnostics();
        } else {
            self.request_semantic(SemanticRequestKind::Analyze, cx);
        }
        cx.notify();
    }

    pub fn vim_mode(&self) -> VimMode {
        self.vim_mode
    }

    pub fn vim_entered(&self) -> &str {
        &self.vim_entered
    }

    pub fn toggle_keymap(&mut self, cx: &mut Context<Self>) {
        let keymap = match self.keymap {
            EditorKeymap::Standard => EditorKeymap::Vim,
            EditorKeymap::Vim => EditorKeymap::Standard,
        };
        self.set_keymap(keymap, cx);
    }

    pub fn set_keymap(&mut self, keymap: EditorKeymap, cx: &mut Context<Self>) {
        if self.keymap == keymap {
            return;
        }
        self.apply_keymap(keymap);
        cx.emit(EditorEvent::VimStateChanged);
        self.selection_changed(cx);
    }

    fn apply_keymap(&mut self, keymap: EditorKeymap) {
        self.keymap = keymap;
        self.vim_mode = match self.keymap {
            EditorKeymap::Standard => VimMode::Insert,
            EditorKeymap::Vim => VimMode::Normal,
        };
        self.vim_entered.clear();
        self.vim = (self.keymap == EditorKeymap::Vim).then(|| {
            VimEngine::with_store(
                self.document.text(),
                self.document.cursor(),
                self.vim_store.clone(),
            )
        });
    }

    pub fn document(&self) -> &QueryDocument {
        &self.document
    }

    /// Apply authoritative room text without emitting `DocumentChanged`,
    /// which would otherwise echo the remote change back onto the socket.
    pub fn apply_room_snapshot(&mut self, snapshot: &[u8], cx: &mut Context<Self>) {
        let Ok(changed) = self.document.replace_from_room(snapshot) else {
            return;
        };
        if !changed {
            return;
        }
        self.resync_keymap_after_external_change(cx);
        self.revision = self.revision.wrapping_add(1);
        self.line_cache.borrow_mut().lines.clear();
        self.marked_range = None;
        self.semantic.invalidate();
        self.request_semantic(SemanticRequestKind::Analyze, cx);
        self.reveal_cursor();
        cx.emit(EditorEvent::CursorChanged);
        cx.notify();
    }

    pub fn cursor_position(&self) -> (usize, usize) {
        self.document.cursor_position()
    }

    pub fn cursor_offset(&self) -> usize {
        self.document.cursor()
    }

    pub fn select_range(&mut self, range: Range<usize>, cx: &mut Context<Self>) -> bool {
        if range.start > range.end
            || range.end > self.document.text().len()
            || !self.document.text().is_char_boundary(range.start)
            || !self.document.text().is_char_boundary(range.end)
        {
            return false;
        }
        self.document.set_selection(range, false);
        self.selection_changed(cx);
        true
    }

    pub fn navigate_to_text(&mut self, needle: &str, delta: isize, cx: &mut Context<Self>) -> bool {
        let matches = self.document.find_matches(needle, true);
        self.navigate_to_matches(matches, delta, cx)
    }

    /// Navigate between occurrences that begin a line. Useful for structured
    /// read-only buffers whose delimiter may also occur later on the line.
    pub fn navigate_to_line_prefix(
        &mut self,
        prefix: &str,
        delta: isize,
        cx: &mut Context<Self>,
    ) -> bool {
        let matches = self.line_prefix_matches(prefix);
        self.navigate_to_matches(matches, delta, cx)
    }

    pub fn navigate_to_line_prefix_index(
        &mut self,
        prefix: &str,
        index: usize,
        cx: &mut Context<Self>,
    ) -> bool {
        let matches = self.line_prefix_matches(prefix);
        let Some(range) = matches.get(index).cloned() else {
            return false;
        };
        self.document.set_selection(range, false);
        self.selection_changed(cx);
        true
    }

    fn line_prefix_matches(&self, prefix: &str) -> Vec<Range<usize>> {
        let text = self.document.text();
        self.document
            .find_matches(prefix, true)
            .into_iter()
            .filter(|range| {
                range.start == 0 || text.as_bytes().get(range.start - 1) == Some(&b'\n')
            })
            .collect()
    }

    fn navigate_to_matches(
        &mut self,
        matches: Vec<Range<usize>>,
        delta: isize,
        cx: &mut Context<Self>,
    ) -> bool {
        if matches.is_empty() {
            return false;
        }
        let cursor = self.document.cursor();
        let index = if delta < 0 {
            matches
                .iter()
                .rposition(|range| range.start < cursor)
                .unwrap_or(matches.len() - 1)
        } else {
            matches
                .iter()
                .position(|range| range.start > cursor)
                .unwrap_or(0)
        };
        self.document.set_selection(matches[index].clone(), false);
        self.selection_changed(cx);
        true
    }

    pub fn set_cursor_offset(&mut self, offset: usize, cx: &mut Context<Self>) {
        let text = self.document.text();
        let mut target = offset.min(text.len());
        while !text.is_char_boundary(target) {
            target -= 1;
        }
        self.document.set_selection(target..target, false);
        if let Some(vim) = self.vim.as_mut() {
            vim.set_cursor(self.document.text(), target);
        }
        self.selection_changed(cx);
    }

    pub fn go_to_line(&mut self, line: usize, cx: &mut Context<Self>) -> bool {
        if line == 0 {
            return false;
        }
        let starts = self.document.line_starts();
        let Some(offset) = starts.get(line.saturating_sub(1).min(starts.len() - 1)) else {
            return false;
        };
        self.set_cursor_offset(*offset, cx);
        true
    }

    /// Monotonic identity of the current buffer contents. Every semantic
    /// request is tagged with it and every answer is checked against it, so a
    /// slow server reply can never be applied to text the user has since
    /// changed.
    pub fn text_revision(&self) -> u64 {
        self.revision
    }

    pub fn semantic(&self) -> &SemanticState {
        &self.semantic
    }

    pub fn manifest_outline(&self) -> Vec<sift_instance_config::ManifestOutlineItem> {
        if self.manifest_schema {
            sift_instance_config::manifest_outline(self.document.text())
        } else {
            Vec::new()
        }
    }

    pub fn go_to_offset(&mut self, offset: usize, cx: &mut Context<Self>) {
        self.set_cursor_offset(offset, cx);
    }

    /// Semantics are a SQL-only, writable-buffer concern. Read-only DDL
    /// previews and TOML settings buffers never open a server document.
    pub fn semantic_enabled(&self) -> bool {
        self.language == EditorLanguage::Sql && !self.read_only
    }

    fn request_semantic(&mut self, request: SemanticRequestKind, cx: &mut Context<Self>) {
        if !self.semantic_enabled() {
            return;
        }
        cx.emit(EditorEvent::SemanticRequest {
            revision: self.revision,
            request,
        });
    }

    /// Apply one semantic answer. Returns whether it was fresh enough to use.
    pub fn apply_semantic_outcome(
        &mut self,
        revision: u64,
        outcome: SemanticOutcome,
        cx: &mut Context<Self>,
    ) -> bool {
        let current = self.revision;
        let applied = match outcome {
            SemanticOutcome::Diagnostics {
                diagnostics,
                incomplete,
            } => self.semantic.set_diagnostics(
                self.document.text(),
                revision,
                current,
                diagnostics,
                incomplete,
            ),
            SemanticOutcome::Completions {
                replaced,
                candidates,
            } => self.semantic.set_completions(
                self.document.text(),
                revision,
                current,
                replaced,
                candidates,
            ),
            SemanticOutcome::Hover(hover) => self.semantic.set_hover(revision, current, hover),
            SemanticOutcome::StarExpansion(preview) => {
                self.semantic.set_star_expansion(revision, current, preview)
            }
            SemanticOutcome::Usages {
                usages,
                is_complete,
            } => self.semantic.set_usages(
                self.document.text(),
                revision,
                current,
                usages,
                is_complete,
            ),
            SemanticOutcome::Edits { edits, warnings } => {
                self.apply_semantic_edits(revision, edits, warnings, cx)
            }
            SemanticOutcome::RenamePreview { .. } => false,
            SemanticOutcome::Outline { .. } | SemanticOutcome::OutlineFailed(_) => false,
            SemanticOutcome::Failed(message) => {
                self.semantic.clear_hover();
                self.semantic.clear_star_expansion();
                self.semantic.set_notice(Some(message));
                true
            }
        };
        cx.notify();
        applied
    }

    /// Apply a server `WorkspaceEdit` to this buffer. Applying back-to-front
    /// keeps each remaining range valid; the edits land as separate undo
    /// entries, which is the same granularity the document model uses for
    /// ordinary typing.
    fn apply_semantic_edits(
        &mut self,
        revision: u64,
        edits: Vec<sift_protocol::TextEdit>,
        warnings: Vec<String>,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.read_only {
            return false;
        }
        if revision != self.revision {
            self.semantic.set_notice(Some(
                "Buffer changed; the server edit was discarded.".into(),
            ));
            return false;
        }
        let ordered = ordered_edits(self.document.text(), edits);
        if ordered.is_empty() {
            self.semantic.set_notice(
                warnings
                    .first()
                    .cloned()
                    .or_else(|| Some("Nothing to change.".into())),
            );
            return true;
        }
        for (range, new_text) in ordered {
            self.document.replace_range(range, &new_text);
        }
        self.resync_keymap_after_external_change(cx);
        self.semantic.set_notice(warnings.first().cloned());
        self.edited(cx);
        true
    }

    pub fn apply_prepared_semantic_edits(
        &mut self,
        revision: u64,
        edits: Vec<sift_protocol::TextEdit>,
        warnings: Vec<String>,
        cx: &mut Context<Self>,
    ) -> bool {
        self.apply_semantic_edits(revision, edits, warnings, cx)
    }

    fn complete(&mut self, _: &Complete, _: &mut Window, cx: &mut Context<Self>) {
        if self.manifest_schema {
            self.open_manifest_completion(cx);
            return;
        }
        if self.language == EditorLanguage::Json {
            self.open_json_completion(cx);
            return;
        }
        if !self.semantic_enabled() {
            return;
        }
        self.semantic.expect_completion(self.revision);
        let cursor = self.document.cursor() as u32;
        self.request_semantic(SemanticRequestKind::Complete { cursor }, cx);
        cx.notify();
    }

    fn expand_star(&mut self, _: &ExpandStar, _: &mut Window, cx: &mut Context<Self>) {
        if !self.semantic_enabled() {
            return;
        }
        self.semantic.expect_star_expansion(self.revision);
        self.request_semantic(
            SemanticRequestKind::ExpandStar {
                position: self.document.cursor() as u32,
            },
            cx,
        );
        cx.notify();
    }

    fn request_hover_at(
        &mut self,
        position: gpui::Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.semantic_enabled() && !self.manifest_schema {
            return;
        }
        let Some(offset) = self.byte_index_for_point(position, cx.theme(), window) else {
            if self.semantic.clear_hover() || self.manifest_hover.take().is_some() {
                self.hover_anchor = None;
                cx.notify();
            }
            return;
        };
        if self.manifest_schema {
            let hover = sift_instance_config::manifest_hover(self.document.text(), offset);
            if hover == self.manifest_hover {
                return;
            }
            self.manifest_hover = hover;
            let viewport = self.scroll_handle.bounds();
            let scroll = self.scroll_handle.offset();
            self.hover_anchor = self.manifest_hover.as_ref().map(|_| {
                (
                    position.x - viewport.left(),
                    position.y - viewport.top() - scroll.y + px(16.),
                )
            });
            cx.notify();
            return;
        }
        let Some(hover_position) = identifier_hover_position(self.document.text(), offset) else {
            if self.semantic.clear_hover() {
                self.hover_anchor = None;
                cx.notify();
            }
            return;
        };
        if !self.semantic.expect_hover(self.revision, hover_position) {
            return;
        }
        let viewport = self.scroll_handle.bounds();
        let scroll = self.scroll_handle.offset();
        self.hover_anchor = Some((
            position.x - viewport.left(),
            position.y - viewport.top() - scroll.y + px(16.),
        ));
        self.request_semantic(
            SemanticRequestKind::Hover {
                position: hover_position,
            },
            cx,
        );
    }

    /// Returns whether a menu was open and consumed the keystroke.
    fn accept_active_completion(&mut self, cx: &mut Context<Self>) -> bool {
        if self.read_only {
            return false;
        }
        let Some(menu) = self.semantic.completion() else {
            return false;
        };
        let Some(candidate) = menu.selected() else {
            self.semantic.cancel_completion();
            return false;
        };
        let insert = candidate.insert.to_string();
        let snippet = (candidate.kind == sift_protocol::completion::CompletionKind::Snippet)
            .then(|| sift_snippets::expand(&insert))
            .transpose();
        let replace = menu.replace.clone();
        self.semantic.cancel_completion();
        match snippet {
            Ok(Some(expansion)) => {
                let base = replace.start;
                self.document.replace_range(replace, &expansion.text);
                self.snippet_tabstops = expansion
                    .tabstops
                    .into_iter()
                    .map(|tabstop| base + tabstop.range.start..base + tabstop.range.end)
                    .collect();
                self.snippet_tabstop_index = 0;
                if let Some(range) = self.snippet_tabstops.first().cloned() {
                    self.document.set_selection(range, false);
                }
            }
            Ok(None) | Err(_) => {
                self.document.replace_range(replace, &insert);
                self.snippet_tabstops.clear();
            }
        }
        self.resync_keymap_after_external_change(cx);
        // Acceptance is terminal for this popup. Re-triggering automatic
        // completion here makes the accepted token immediately reappear.
        self.edited_with_auto_completion(false, cx);
        true
    }

    fn advance_snippet_tabstop(&mut self, cx: &mut Context<Self>) -> bool {
        if self.snippet_tabstops.is_empty() {
            return false;
        }
        self.snippet_tabstop_index += 1;
        let Some(range) = self
            .snippet_tabstops
            .get(self.snippet_tabstop_index)
            .cloned()
        else {
            self.snippet_tabstops.clear();
            return false;
        };
        self.document.set_selection(range, false);
        self.resync_keymap_after_external_change(cx);
        self.selection_changed(cx);
        true
    }

    fn adjust_snippet_tabstops(&mut self, edited: Range<usize>, inserted_len: usize) {
        if self.snippet_tabstops.is_empty() {
            return;
        }
        let removed_len = edited.end.saturating_sub(edited.start);
        let delta = inserted_len as isize - removed_len as isize;
        for range in &mut self.snippet_tabstops {
            if range.start >= edited.end {
                range.start = range.start.saturating_add_signed(delta);
                range.end = range.end.saturating_add_signed(delta);
            } else if range.end > edited.start {
                *range = edited.start..edited.start + inserted_len;
            }
        }
    }

    fn accept_star_expansion(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(preview) = self.semantic.star_expansion().cloned() else {
            return false;
        };
        let range = preview.range.start as usize..preview.range.end as usize;
        if range.end > self.document.text().len()
            || !self.document.text().is_char_boundary(range.start)
            || !self.document.text().is_char_boundary(range.end)
            || !valid_star_expansion_source(&self.document.text()[range.clone()])
        {
            self.semantic.clear_star_expansion();
            return false;
        }
        self.semantic.clear_star_expansion();
        self.document.replace_range(range, &preview.replacement);
        self.resync_keymap_after_external_change(cx);
        self.edited(cx);
        true
    }

    fn format_document(&mut self, _: &FormatDocument, _: &mut Window, cx: &mut Context<Self>) {
        if self.read_only {
            return;
        }
        if self.manifest_schema {
            self.schedule_manifest_diagnostics(cx);
        } else if self.language == EditorLanguage::Json {
            match format_json_document(self.document.text()) {
                Ok(formatted) if formatted != self.document.text() => {
                    let length = self.document.text().len();
                    self.document.replace_range(0..length, &formatted);
                    self.semantic.set_notice(Some("Formatted JSON.".into()));
                    self.edited(cx);
                }
                Ok(_) => {
                    self.semantic
                        .set_notice(Some("JSON is already formatted.".into()));
                    cx.notify();
                }
                Err(message) => {
                    self.semantic.set_notice(Some(message));
                    cx.notify();
                }
            }
            return;
        }
        let selection = self.document.selection();
        let range = (!selection.is_empty()).then_some(sift_protocol::TextRange {
            start: selection.start as u32,
            end: selection.end as u32,
        });
        self.request_semantic(SemanticRequestKind::Format { range }, cx);
    }

    fn apply_quick_fix(&mut self, _: &ApplyQuickFix, _: &mut Window, cx: &mut Context<Self>) {
        if self.read_only {
            return;
        }
        let fix_id = self
            .semantic
            .diagnostic_at(self.document.cursor())
            .and_then(|diagnostic| diagnostic.quick_fix_ids.first().cloned());
        match fix_id {
            Some(fix_id) => self.request_semantic(SemanticRequestKind::QuickFix { fix_id }, cx),
            None => {
                self.semantic
                    .set_notice(Some("No quick fix at the caret.".into()));
                cx.notify();
            }
        }
    }

    fn find_usages(&mut self, _: &FindUsages, _: &mut Window, cx: &mut Context<Self>) {
        let position = self.document.cursor() as u32;
        self.request_semantic(SemanticRequestKind::Usages { position }, cx);
    }

    fn open_find(&mut self, _: &OpenFind, window: &mut Window, cx: &mut Context<Self>) {
        self.find_open = true;
        let selected = self.document.selected_text();
        if !selected.is_empty() && !selected.contains('\n') {
            self.find_query
                .update(cx, |input, cx| input.set_text(selected, cx));
        }
        self.find_query.focus_handle(cx).focus(window, cx);
        cx.notify();
    }

    fn close_find(&mut self, _: &CloseFind, window: &mut Window, cx: &mut Context<Self>) {
        self.find_open = false;
        self.focus_handle.focus(window, cx);
        cx.notify();
    }

    fn find_next(&mut self, _: &FindNext, _: &mut Window, cx: &mut Context<Self>) {
        self.move_find_selection(1, cx);
    }

    fn find_previous(&mut self, _: &FindPrevious, _: &mut Window, cx: &mut Context<Self>) {
        self.move_find_selection(-1, cx);
    }

    fn move_find_selection(&mut self, delta: isize, cx: &mut Context<Self>) -> bool {
        let matches = self.current_find_matches(cx);
        if matches.is_empty() {
            return false;
        }
        let selection = self.document.selection();
        let current = matches.iter().position(|range| *range == selection);
        let index = match current {
            Some(index) => (index as isize + delta).rem_euclid(matches.len() as isize) as usize,
            None if delta < 0 => matches
                .iter()
                .rposition(|range| range.start < self.document.cursor())
                .unwrap_or(matches.len() - 1),
            None => matches
                .iter()
                .position(|range| range.start >= self.document.cursor())
                .unwrap_or(0),
        };
        self.document.set_selection(matches[index].clone(), false);
        self.selection_changed(cx);
        true
    }

    fn replace_next(&mut self, _: &ReplaceNext, _: &mut Window, cx: &mut Context<Self>) {
        if self.read_only {
            return;
        }
        let matches = self.current_find_matches(cx);
        if matches.is_empty() {
            return;
        }
        let selection = self.document.selection();
        let target = matches
            .iter()
            .find(|range| **range == selection)
            .cloned()
            .or_else(|| {
                matches
                    .iter()
                    .find(|range| range.start >= self.document.cursor())
                    .cloned()
            })
            .unwrap_or_else(|| matches[0].clone());
        let replacement = self.replace_query.read(cx).text().to_owned();
        self.document.replace_range(target, &replacement);
        self.edited(cx);
        self.move_find_selection(1, cx);
    }

    fn replace_all(&mut self, _: &ReplaceAll, _: &mut Window, cx: &mut Context<Self>) {
        if self.read_only {
            return;
        }
        let matches = self.current_find_matches(cx);
        if matches.is_empty() {
            return;
        }
        let replacement = self.replace_query.read(cx).text().to_owned();
        let mut updated = self.document.text().to_owned();
        for range in matches.iter().rev() {
            updated.replace_range(range.clone(), &replacement);
        }
        let length = self.document.text().len();
        self.document.replace_range(0..length, &updated);
        self.edited(cx);
    }

    fn current_find_matches(&self, cx: &App) -> Arc<Vec<Range<usize>>> {
        let query = self.find_query.read(cx).text();
        let mut cache = self.find_cache.borrow_mut();
        if cache.revision != self.revision
            || cache.query != query
            || cache.case_sensitive != self.find_case_sensitive
        {
            cache.revision = self.revision;
            cache.query.clear();
            cache.query.push_str(query);
            cache.case_sensitive = self.find_case_sensitive;
            cache.matches = Arc::new(self.document.find_matches(query, self.find_case_sensitive));
        }
        cache.matches.clone()
    }

    fn toggle_find_case(&mut self, cx: &mut Context<Self>) {
        self.find_case_sensitive = !self.find_case_sensitive;
        cx.notify();
    }

    /// Move the caret to the next diagnostic, wrapping once. Keeps a
    /// keyboard-only path to every problem the server reported.
    fn go_to_next_diagnostic(
        &mut self,
        _: &GoToNextDiagnostic,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let cursor = self.document.cursor();
        let target = self
            .semantic
            .diagnostics()
            .iter()
            .map(|diagnostic| diagnostic.range.start)
            .find(|start| *start > cursor)
            .or_else(|| {
                self.semantic
                    .diagnostics()
                    .first()
                    .map(|diagnostic| diagnostic.range.start)
            });
        let Some(target) = target else {
            return;
        };
        let target = target.min(self.document.text().len());
        self.document.set_selection(target..target, false);
        if let Some(vim) = self.vim.as_mut() {
            vim.set_cursor(self.document.text(), target);
        }
        self.selection_changed(cx);
    }

    fn edited(&mut self, cx: &mut Context<Self>) {
        self.edited_with_auto_completion(true, cx);
    }

    fn edited_with_auto_completion(&mut self, allow_auto_completion: bool, cx: &mut Context<Self>) {
        self.marked_range = None;
        self.revision = self.revision.wrapping_add(1);
        let mut cache = self.line_cache.borrow_mut();
        match self.document.last_change {
            Some(change) if !change.structural => {
                cache.lines.remove(&change.line);
            }
            _ => cache.lines.clear(),
        }
        drop(cache);
        self.reveal_cursor();
        self.cursor_blink.update(cx, CursorBlink::pause);
        if let Some(update) = self.document.take_room_update() {
            cx.emit(EditorEvent::DocumentChanged { update });
        }
        // An open menu stays open across typing by re-requesting against the
        // new revision; the server owns the filtering, the client never
        // narrows a stale candidate list itself.
        let reopen_completion = self.semantic.completion().is_some();
        self.semantic.invalidate();
        if self.language == EditorLanguage::Json {
            self.refresh_local_diagnostics();
        } else {
            self.request_semantic(SemanticRequestKind::Analyze, cx);
        }
        let auto_complete = allow_auto_completion
            && self.keymap == EditorKeymap::Vim
            && self.vim_mode == VimMode::Insert
            && (self.manifest_schema
                || should_auto_complete(self.document.text(), self.document.cursor()));
        if reopen_completion || auto_complete {
            if self.manifest_schema {
                self.open_manifest_completion(cx);
            } else if self.language == EditorLanguage::Json {
                self.open_json_completion(cx);
            } else {
                self.semantic.expect_completion(self.revision);
                let cursor = self.document.cursor() as u32;
                self.request_semantic(SemanticRequestKind::AutoComplete { cursor }, cx);
            }
        }
        cx.notify();
    }

    fn refresh_local_diagnostics(&mut self) {
        let diagnostics = json_schema_diagnostics(self.document.text(), self.json_schema.as_ref());
        self.semantic.set_diagnostics(
            self.document.text(),
            self.revision,
            self.revision,
            diagnostics,
            false,
        );
    }

    fn schedule_manifest_diagnostics(&mut self, cx: &mut Context<Self>) {
        self.manifest_analysis_epoch = self.manifest_analysis_epoch.wrapping_add(1);
        let epoch = self.manifest_analysis_epoch;
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(250))
                .await;
            let _ = this.update(cx, |editor, cx| {
                if editor.manifest_schema && editor.manifest_analysis_epoch == epoch {
                    editor.refresh_manifest_diagnostics();
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn refresh_manifest_diagnostics(&mut self) {
        let diagnostics = sift_instance_config::manifest_diagnostics(self.document.text())
            .into_iter()
            .enumerate()
            .map(|(index, diagnostic)| sift_protocol::SemanticDiagnostic {
                id: format!("manifest-{index}"),
                severity: sift_protocol::DiagnosticSeverity::Error,
                code: "sift.toml".into(),
                message: diagnostic.message,
                range: sift_protocol::TextRange {
                    start: diagnostic.range.start as u32,
                    end: diagnostic.range.end as u32,
                },
                related_ranges: Vec::new(),
                source: "sift.toml".into(),
                quick_fix_ids: Vec::new(),
            })
            .collect();
        self.semantic.set_diagnostics(
            self.document.text(),
            self.revision,
            self.revision,
            diagnostics,
            false,
        );
    }

    fn open_manifest_completion(&mut self, cx: &mut Context<Self>) {
        let (replaced, completions) = sift_instance_config::manifest_completions(
            self.document.text(),
            self.document.cursor(),
        );
        let candidates = completions
            .into_iter()
            .enumerate()
            .map(
                |(index, completion)| sift_protocol::completion::CompletionCandidate {
                    label: completion.label.into(),
                    insert: completion.insertion.into(),
                    kind: sift_protocol::completion::CompletionKind::Keyword,
                    detail: Some(completion.detail),
                    qualified_name: None,
                    score: 10_000 - index as i32,
                },
            )
            .collect();
        self.semantic.expect_completion(self.revision);
        self.semantic.set_completions(
            self.document.text(),
            self.revision,
            self.revision,
            sift_protocol::TextRange {
                start: replaced.start as u32,
                end: replaced.end as u32,
            },
            candidates,
        );
        cx.notify();
    }

    fn open_json_completion(&mut self, cx: &mut Context<Self>) {
        let Some(schema) = self.json_schema.as_ref() else {
            return;
        };
        let cursor = self.document.cursor();
        let (replaced, candidates) = json_schema_completions(self.document.text(), cursor, schema);
        self.semantic.expect_completion(self.revision);
        self.semantic.set_completions(
            self.document.text(),
            self.revision,
            self.revision,
            sift_protocol::TextRange {
                start: replaced.start as u32,
                end: replaced.end as u32,
            },
            candidates,
        );
        cx.notify();
    }

    fn selection_changed(&mut self, cx: &mut Context<Self>) {
        self.reveal_cursor();
        self.cursor_blink.update(cx, CursorBlink::pause);
        if !self.cursor_event_pending {
            self.cursor_event_pending = true;
            cx.spawn(async move |this, cx| {
                cx.background_executor()
                    .timer(Duration::from_millis(33))
                    .await;
                let _ = this.update(cx, |editor, cx| {
                    editor.cursor_event_pending = false;
                    cx.emit(EditorEvent::CursorChanged);
                });
            })
            .detach();
        }
        cx.notify();
    }

    fn reveal_cursor(&self) {
        let viewport = self.scroll_handle.bounds();
        if viewport.size.height <= px(0.) {
            return;
        }
        let line = self.document.line_of_offset(self.document.cursor());
        let caret_top = EDITOR_VERTICAL_INSET + EDITOR_LINE_HEIGHT * line as f32;
        let caret_bottom = caret_top + EDITOR_LINE_HEIGHT;
        let mut offset = self.scroll_handle.offset();
        let visible_top = -offset.y;
        let visible_bottom = visible_top + viewport.size.height;
        if caret_top < visible_top + EDITOR_VERTICAL_INSET {
            offset.y = -(caret_top - EDITOR_VERTICAL_INSET).max(px(0.));
        } else if caret_bottom > visible_bottom - EDITOR_VERTICAL_INSET {
            offset.y = -(caret_bottom + EDITOR_VERTICAL_INSET - viewport.size.height);
        }
        let line_count = self.document.line_count();
        let content_height = EDITOR_VERTICAL_INSET * 2. + EDITOR_LINE_HEIGHT * line_count as f32;
        let max_scroll = (content_height - viewport.size.height).max(px(0.));
        offset.y = offset.y.min(px(0.)).max(-max_scroll);
        self.scroll_handle.set_offset(offset);
    }

    fn apply_vim_snapshot(&mut self, snapshot: VimSnapshot, cx: &mut Context<Self>) {
        let open_command_palette = snapshot.open_command_palette;
        let clipboard = snapshot.clipboard;
        let vim_state_changed =
            self.vim_mode != snapshot.mode || self.vim_entered != snapshot.entered;
        self.vim_entered = snapshot.entered;
        let mut document_changed = false;
        if let Some(snapshot_text) = snapshot
            .text
            .filter(|text| !self.read_only && text != self.document.text())
        {
            let old = self.document.text();
            let prefix = old
                .char_indices()
                .zip(snapshot_text.char_indices())
                .take_while(|((_, left), (_, right))| left == right)
                .last()
                .map_or(0, |((offset, character), _)| offset + character.len_utf8());
            let old_tail = &old[prefix..];
            let new_tail = &snapshot_text[prefix..];
            let suffix_chars = old_tail
                .chars()
                .rev()
                .zip(new_tail.chars().rev())
                .take_while(|(left, right)| left == right)
                .count();
            let old_suffix = if suffix_chars == 0 {
                old.len()
            } else {
                old_tail
                    .char_indices()
                    .rev()
                    .nth(suffix_chars - 1)
                    .map_or(old.len(), |(offset, _)| prefix + offset)
            };
            let new_suffix = if suffix_chars == 0 {
                snapshot_text.len()
            } else {
                new_tail
                    .char_indices()
                    .rev()
                    .nth(suffix_chars - 1)
                    .map_or(snapshot_text.len(), |(offset, _)| prefix + offset)
            };
            self.document
                .replace_range(prefix..old_suffix, &snapshot_text[prefix..new_suffix]);
            self.adjust_snippet_tabstops(
                prefix..old_suffix,
                snapshot_text[prefix..new_suffix].len(),
            );
            document_changed = true;
        }
        let cursor = byte_from_line_column(self.document.text(), snapshot.cursor);
        if let Some((start, end)) = snapshot.selection {
            let start = byte_from_line_column(self.document.text(), start);
            let end = byte_from_line_column(self.document.text(), end);
            self.document
                .set_selection(start.min(end)..start.max(end), cursor == start.min(end));
        } else {
            self.document.set_selection(cursor..cursor, false);
        }
        self.vim_mode = snapshot.mode;
        if vim_state_changed {
            cx.emit(EditorEvent::VimStateChanged);
        }
        if open_command_palette {
            cx.emit(EditorEvent::OpenCommandPalette);
        }
        if let Some(text) = clipboard {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
        if document_changed {
            self.edited(cx);
        } else {
            self.selection_changed(cx);
        }
    }

    fn vim_key(
        &mut self,
        code: modalkit::crossterm::event::KeyCode,
        cx: &mut Context<Self>,
    ) -> bool {
        let clipboard = cx.read_from_clipboard().and_then(|item| item.text());
        let Some(vim) = self.vim.as_mut() else {
            return false;
        };
        if let Some(text) = clipboard.as_deref() {
            vim.set_clipboard(text);
        }
        let rows = (f32::from(self.scroll_handle.bounds().size.height)
            / f32::from(EDITOR_LINE_HEIGHT)) as usize;
        vim.set_viewport_rows(rows);
        let snapshot = vim.input_key(code);
        self.apply_vim_snapshot(snapshot, cx);
        true
    }

    fn vim_text(&mut self, text: &str, cx: &mut Context<Self>) -> bool {
        let clipboard = cx.read_from_clipboard().and_then(|item| item.text());
        let Some(vim) = self.vim.as_mut() else {
            return false;
        };
        if let Some(text) = clipboard.as_deref() {
            vim.set_clipboard(text);
        }
        let snapshot = vim.input_text(text);
        self.apply_vim_snapshot(snapshot, cx);
        true
    }

    fn backspace(&mut self, _: &Backspace, _: &mut Window, cx: &mut Context<Self>) {
        if self.read_only {
            return;
        }
        if self.keymap == EditorKeymap::Vim
            && self.vim_mode == VimMode::Insert
            && self.document.selection().is_empty()
        {
            let snapshot = self
                .vim
                .as_mut()
                .expect("Vim keymap must own an engine")
                .backspace_without_text_snapshot();
            self.document.backspace();
            self.apply_vim_snapshot(snapshot, cx);
            self.edited_with_auto_completion(false, cx);
            return;
        }
        if self.vim_key(modalkit::crossterm::event::KeyCode::Backspace, cx) {
            return;
        }
        self.document.backspace();
        self.edited_with_auto_completion(false, cx);
    }

    fn delete_forward(&mut self, _: &DeleteForward, _: &mut Window, cx: &mut Context<Self>) {
        if self.read_only {
            return;
        }
        if self.vim_key(modalkit::crossterm::event::KeyCode::Delete, cx) {
            return;
        }
        self.document.delete_forward();
        self.edited(cx);
    }

    fn newline(&mut self, _: &Newline, _: &mut Window, cx: &mut Context<Self>) {
        if self.read_only {
            return;
        }
        if self.accept_active_completion(cx) {
            return;
        }
        if self.accept_star_expansion(cx) {
            return;
        }
        if self.vim_key(modalkit::crossterm::event::KeyCode::Enter, cx) {
            return;
        }
        if self.language == EditorLanguage::Json && self.document.selection().is_empty() {
            let (insert, caret_back) = json_newline(self.document.text(), self.document.cursor());
            self.document.insert(&insert);
            if caret_back > 0 {
                let cursor = self.document.cursor().saturating_sub(caret_back);
                self.document.set_selection(cursor..cursor, false);
            }
        } else {
            self.document.insert("\n");
        }
        self.edited(cx);
    }

    fn indent(&mut self, _: &Indent, _: &mut Window, cx: &mut Context<Self>) {
        if self.read_only {
            return;
        }
        if self.accept_active_completion(cx) {
            return;
        }
        if self.advance_snippet_tabstop(cx) {
            return;
        }
        if self.accept_star_expansion(cx) {
            return;
        }
        if self.vim_key(modalkit::crossterm::event::KeyCode::Tab, cx) {
            return;
        }
        self.document.insert("  ");
        self.edited(cx);
    }

    fn move_left(&mut self, _: &MoveLeft, _: &mut Window, cx: &mut Context<Self>) {
        if self.vim_key(modalkit::crossterm::event::KeyCode::Left, cx) {
            return;
        }
        self.document.move_left(false);
        self.selection_changed(cx);
    }

    fn move_right(&mut self, _: &MoveRight, _: &mut Window, cx: &mut Context<Self>) {
        if self.vim_key(modalkit::crossterm::event::KeyCode::Right, cx) {
            return;
        }
        self.document.move_right(false);
        self.selection_changed(cx);
    }

    fn move_up(&mut self, _: &MoveUp, _: &mut Window, cx: &mut Context<Self>) {
        if self.semantic.move_completion_selection(-1) {
            cx.notify();
            return;
        }
        if self.vim_key(modalkit::crossterm::event::KeyCode::Up, cx) {
            return;
        }
        self.document.move_up(false);
        self.selection_changed(cx);
    }

    fn move_down(&mut self, _: &MoveDown, _: &mut Window, cx: &mut Context<Self>) {
        if self.semantic.move_completion_selection(1) {
            cx.notify();
            return;
        }
        if self.vim_key(modalkit::crossterm::event::KeyCode::Down, cx) {
            return;
        }
        self.document.move_down(false);
        self.selection_changed(cx);
    }

    fn select_left(&mut self, _: &SelectLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.document.move_left(true);
        self.selection_changed(cx);
    }

    fn select_right(&mut self, _: &SelectRight, _: &mut Window, cx: &mut Context<Self>) {
        self.document.move_right(true);
        self.selection_changed(cx);
    }

    fn select_up(&mut self, _: &SelectUp, _: &mut Window, cx: &mut Context<Self>) {
        self.document.move_up(true);
        self.selection_changed(cx);
    }

    fn select_down(&mut self, _: &SelectDown, _: &mut Window, cx: &mut Context<Self>) {
        self.document.move_down(true);
        self.selection_changed(cx);
    }

    fn line_start(&mut self, _: &LineStart, _: &mut Window, cx: &mut Context<Self>) {
        self.document.move_line_start(false);
        self.selection_changed(cx);
    }

    fn line_end(&mut self, _: &LineEnd, _: &mut Window, cx: &mut Context<Self>) {
        self.document.move_line_end(false);
        self.selection_changed(cx);
    }

    fn select_all(&mut self, _: &SelectAll, _: &mut Window, cx: &mut Context<Self>) {
        self.document.select_all();
        self.selection_changed(cx);
    }

    fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        let selected = self.document.selected_text();
        if !selected.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(selected.to_string()));
        }
    }

    fn cut(&mut self, _: &Cut, window: &mut Window, cx: &mut Context<Self>) {
        if self.read_only {
            self.copy(&Copy, window, cx);
            return;
        }
        self.copy(&Copy, window, cx);
        if !self.document.selection().is_empty() {
            self.document.insert("");
            self.edited(cx);
        }
    }

    fn paste(&mut self, _: &Paste, _: &mut Window, cx: &mut Context<Self>) {
        if self.read_only {
            return;
        }
        if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
            self.document.insert(&text);
            self.edited(cx);
        }
    }

    fn undo(&mut self, _: &Undo, _: &mut Window, cx: &mut Context<Self>) {
        if self.read_only {
            return;
        }
        if self.document.undo() {
            self.edited(cx);
        }
    }

    fn vim_undo(&mut self, _: &VimUndo, _: &mut Window, cx: &mut Context<Self>) {
        if self.read_only {
            return;
        }
        if self.document.undo() {
            self.resync_keymap_after_external_change(cx);
            self.edited(cx);
        }
    }

    fn redo(&mut self, _: &Redo, _: &mut Window, cx: &mut Context<Self>) {
        if self.read_only {
            return;
        }
        if self.document.redo() {
            self.edited(cx);
        }
    }

    fn exit_insert_mode(&mut self, _: &ExitInsertMode, _: &mut Window, cx: &mut Context<Self>) {
        // Escape dismisses the completion menu before it reaches Vim, so one
        // press never both closes the menu and leaves insert mode.
        if self.semantic.cancel_completion() || self.semantic.clear_star_expansion() {
            cx.notify();
            return;
        }
        self.vim_key(modalkit::crossterm::event::KeyCode::Esc, cx);
    }

    fn execute_statement(&mut self, _: &ExecuteStatement, _: &mut Window, cx: &mut Context<Self>) {
        if self.read_only {
            return;
        }
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
        if self.read_only {
            return;
        }
        let sql = self.document.text().trim().to_string();
        if !sql.is_empty() {
            cx.emit(EditorEvent::Execute { sql });
        }
    }

    fn offset_from_utf16(&self, offset: usize) -> usize {
        offset_from_utf16(self.document.text(), offset)
    }

    /// Convert a viewport coordinate directly into a document byte offset.
    /// This deliberately does not use the cached visible row layouts: scroll
    /// events can move the viewport between frames, so cached rows are not a
    /// reliable coordinate system for pointer input.
    fn byte_index_for_point(
        &self,
        position: gpui::Point<Pixels>,
        theme: Theme,
        window: &mut Window,
    ) -> Option<usize> {
        let viewport = self.scroll_handle.bounds();
        if !viewport.contains(&position) {
            return None;
        }
        let offset = self.scroll_handle.offset();
        let content_y = position.y - viewport.top() - offset.y - EDITOR_VERTICAL_INSET;
        let line_count = self.document.text().split('\n').count().max(1);
        let content_height = EDITOR_LINE_HEIGHT * line_count as f32;
        if content_y < px(0.) || content_y >= content_height {
            return None;
        }
        let line = (f32::from(content_y) / f32::from(EDITOR_LINE_HEIGHT)) as usize;
        let line_start = self
            .document
            .text()
            .match_indices('\n')
            .take(line)
            .last()
            .map_or(0, |(offset, _)| offset + 1);
        let line_end = self.document.text()[line_start..]
            .find('\n')
            .map_or(self.document.text().len(), |offset| line_start + offset);
        let line_text = &self.document.text()[line_start..line_end];
        // Blank rows have no glyph target. Treating their full viewport width
        // as a valid hit makes the caret appear to jump to an arbitrary byte
        // boundary, especially after scrolling. Preserve the current caret.
        if line_text.trim().is_empty() {
            return None;
        }
        let style = window.text_style();
        let font_size = style.font_size.to_pixels(window.rem_size());
        let runs = editor_text_runs(
            line_text,
            style.font(),
            theme,
            self.language,
            self.diff_language,
        );
        let layout =
            window
                .text_system()
                .shape_line(line_text.to_string().into(), font_size, &runs, None);
        let text_left = viewport.left() + EDITOR_GUTTER_WIDTH + EDITOR_TEXT_INSET;
        let text_x = position.x - text_left;
        if text_x < px(0.) {
            return None;
        }
        if text_x > layout.width() {
            return Some(line_end);
        }
        let within = layout.index_for_x(text_x).unwrap_or(0);
        Some(line_start + within.min(line_text.len()))
    }

    fn line_range_for_gutter_point(&self, position: gpui::Point<Pixels>) -> Option<Range<usize>> {
        let viewport = self.scroll_handle.bounds();
        if !viewport.contains(&position) || position.x >= viewport.left() + EDITOR_GUTTER_WIDTH {
            return None;
        }
        let content_y =
            position.y - viewport.top() - self.scroll_handle.offset().y - EDITOR_VERTICAL_INSET;
        if content_y < px(0.) {
            return None;
        }
        let line = (f32::from(content_y) / f32::from(EDITOR_LINE_HEIGHT)) as usize;
        let starts = self.document.line_starts();
        let start = *starts.get(line)?;
        let end = starts
            .get(line + 1)
            .copied()
            .unwrap_or(self.document.text().len());
        Some(start..end)
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

    /// Caret position in scroll-content coordinates, used to anchor the
    /// completion menu to the text rather than to the viewport. `None` before
    /// the first paint, or when the caret's line is not currently laid out.
    fn caret_content_origin(&self) -> Option<(Pixels, Pixels)> {
        let cursor = self.document.cursor();
        let (line, line_start) = self.line_of(cursor);
        let layout = self
            .line_layouts
            .get(line.checked_sub(self.visible_line_start)?)?;
        let x = layout.x_for_index(cursor.saturating_sub(line_start));
        Some((
            EDITOR_GUTTER_WIDTH + EDITOR_TEXT_INSET + x,
            EDITOR_VERTICAL_INSET + self.line_height * (line + 1) as f32,
        ))
    }

    fn render_completion_menu(&self, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        let menu = self.semantic.completion()?;
        let (left, top) = self.caret_content_origin()?;
        let colors = cx.theme().colors;
        let selected = menu.selected;
        let rows = menu
            .candidates
            .iter()
            .enumerate()
            .skip(selected.saturating_sub(COMPLETION_VISIBLE_ROWS - 1))
            .take(COMPLETION_VISIBLE_ROWS)
            .map(|(index, candidate)| {
                let active = index == selected;
                div()
                    .id(("completion-row", index))
                    .debug_selector(move || format!("completion-row-{index}"))
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_2()
                    .h(COMPLETION_ROW_HEIGHT)
                    .text_xs()
                    .line_height(COMPLETION_ROW_HEIGHT)
                    .when(active, |row| row.bg(colors.selected_surface))
                    .child(
                        div()
                            .debug_selector(move || format!("completion-kind-{index}"))
                            .flex_none()
                            .w(px(18.))
                            .h_full()
                            .flex()
                            .items_center()
                            .justify_center()
                            .text_color(colors.accent)
                            .child(completion_kind_badge(candidate.kind)),
                    )
                    .child(
                        div()
                            .debug_selector(move || format!("completion-label-{index}"))
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .flex()
                            .items_center()
                            .overflow_hidden()
                            .text_color(colors.text)
                            .child(candidate.label.to_string()),
                    )
                    .children(completion_candidate_metadata(candidate).map(|metadata| {
                        div()
                            .debug_selector(move || format!("completion-metadata-{index}"))
                            .flex_none()
                            .h_full()
                            .flex()
                            .items_center()
                            .max_w(px(250.))
                            .overflow_hidden()
                            .text_color(colors.muted_text)
                            .child(metadata)
                    }))
            })
            .collect::<Vec<_>>();
        Some(
            div()
                .absolute()
                .left(left)
                .top(top)
                .w(COMPLETION_MENU_WIDTH)
                .border_1()
                .border_color(colors.border)
                .bg(colors.elevated_surface)
                .rounded(cx.theme().metrics.radius)
                .overflow_hidden()
                .flex()
                .flex_col()
                .children(rows)
                .into_any_element(),
        )
    }

    fn render_hover_card(&self, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        let hover = self.semantic.hover()?;
        let (left, top) = self.hover_anchor?;
        let colors = cx.theme().colors;
        let type_text = hover.type_ref.as_ref().map(hover_type_display);
        let nullable = hover.nullability.map(|nullable| match nullable {
            sift_protocol::Nullability::Nullable => "nullable",
            sift_protocol::Nullability::NotNullable => "not null",
            sift_protocol::Nullability::Unknown => "nullability unknown",
        });
        Some(
            div()
                .absolute()
                .left(left)
                .top(top)
                .w(px(380.))
                .p_3()
                .border_1()
                .border_color(colors.border)
                .bg(colors.elevated_surface)
                .rounded(cx.theme().metrics.radius)
                .shadow_md()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .text_sm()
                        .text_color(colors.text)
                        .child(hover.display_name.clone()),
                )
                .children(
                    hover
                        .qualified_name
                        .clone()
                        .map(|name| div().text_xs().text_color(colors.muted_text).child(name)),
                )
                .children(type_text.map(|type_text| {
                    let text = nullable.map_or(type_text.clone(), |nullable| {
                        format!("{type_text} · {nullable}")
                    });
                    div().text_xs().text_color(colors.accent).child(text)
                }))
                .children(
                    hover
                        .comment
                        .clone()
                        .map(|comment| div().text_xs().text_color(colors.text).child(comment)),
                )
                .children(
                    hover
                        .detail
                        .clone()
                        .map(|detail| div().text_xs().text_color(colors.muted_text).child(detail)),
                )
                .when(hover.uncertain, |card| {
                    card.child(
                        div()
                            .text_xs()
                            .text_color(colors.warning)
                            .child("Metadata is incomplete or inferred"),
                    )
                })
                .into_any_element(),
        )
    }

    fn render_manifest_hover_card(&self, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        let hover = self.manifest_hover.as_ref()?;
        let (left, top) = self.hover_anchor?;
        let colors = cx.theme().colors;
        let choices =
            (!hover.choices.is_empty()).then(|| format!("Choices: {}", hover.choices.join(", ")));
        Some(
            div()
                .absolute()
                .left(left)
                .top(top)
                .w(px(380.))
                .p_3()
                .border_1()
                .border_color(colors.border)
                .bg(colors.elevated_surface)
                .rounded(cx.theme().metrics.radius)
                .shadow_md()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .text_sm()
                        .text_color(colors.text)
                        .child(hover.path.clone()),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(colors.accent)
                        .child(hover.value_type),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(colors.text)
                        .child(hover.documentation),
                )
                .children(
                    choices.map(|choices| {
                        div().text_xs().text_color(colors.muted_text).child(choices)
                    }),
                )
                .into_any_element(),
        )
    }

    fn render_manifest_lifecycle(&self, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        let lifecycle = self.manifest_lifecycle?;
        let colors = cx.theme().colors;
        let valid = self.semantic.diagnostics().is_empty();
        let ready = lifecycle.applied && lifecycle.missing_credentials == 0;
        let stages = [
            ("Edited", true),
            ("Validated", valid),
            ("Applied", lifecycle.applied),
            ("Ready", ready),
        ];
        Some(
            div()
                .id("manifest-lifecycle")
                .h(px(30.))
                .flex_none()
                .px_3()
                .flex()
                .items_center()
                .gap_2()
                .border_b_1()
                .border_color(colors.subtle_border)
                .bg(colors.toolbar)
                .children(
                    stages
                        .into_iter()
                        .enumerate()
                        .flat_map(|(index, (label, complete))| {
                            let mut elements = Vec::new();
                            if index > 0 {
                                elements.push(
                                    div()
                                        .text_xs()
                                        .text_color(colors.muted_text)
                                        .child("→")
                                        .into_any_element(),
                                );
                            }
                            elements.push(
                                div()
                                    .text_xs()
                                    .text_color(if complete {
                                        colors.success
                                    } else {
                                        colors.muted_text
                                    })
                                    .child(if complete {
                                        format!("✓ {label}")
                                    } else {
                                        label.into()
                                    })
                                    .into_any_element(),
                            );
                            elements
                        }),
                )
                .children(
                    (lifecycle.applied && lifecycle.missing_credentials > 0).then(|| {
                        div()
                            .ml_auto()
                            .text_xs()
                            .text_color(colors.warning)
                            .child(format!(
                                "{} credential slot(s) need attention",
                                lifecycle.missing_credentials
                            ))
                    }),
                )
                .into_any_element(),
        )
    }

    fn render_star_expansion_card(&self, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        let preview = self.semantic.star_expansion()?;
        let (left, top) = self.caret_content_origin()?;
        let colors = cx.theme().colors;
        let replacement = if preview.replacement.chars().count() > 240 {
            format!(
                "{}…",
                preview.replacement.chars().take(240).collect::<String>()
            )
        } else {
            preview.replacement.clone()
        };
        Some(
            div()
                .absolute()
                .left(left)
                .top(top)
                .w(px(440.))
                .p_3()
                .border_1()
                .border_color(colors.accent)
                .bg(colors.elevated_surface)
                .rounded(cx.theme().metrics.radius)
                .shadow_md()
                .flex()
                .flex_col()
                .gap_1()
                .child(div().text_sm().text_color(colors.text).child(format!(
                    "Expand {} columns from {}",
                    preview.columns.len(),
                    preview.relation
                )))
                .child(
                    div()
                        .text_xs()
                        .text_color(colors.muted_text)
                        .child(replacement),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(colors.accent)
                        .child("Enter or Tab to apply · Esc to dismiss"),
                )
                .into_any_element(),
        )
    }

    fn render_find_bar(&self, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        if !self.find_open {
            return None;
        }
        let colors = cx.theme().colors;
        let matches = self.current_find_matches(cx);
        let current = matches
            .iter()
            .position(|range| *range == self.document.selection())
            .map_or(0, |index| index + 1);
        Some(
            div()
                .id("editor-find-bar")
                .debug_selector(|| "editor-find-bar".into())
                .flex()
                .items_center()
                .gap_1()
                .px_2()
                .py_1()
                .border_b_1()
                .border_color(colors.subtle_border)
                .bg(colors.toolbar)
                .on_key_down(
                    cx.listener(|editor, event: &gpui::KeyDownEvent, window, cx| {
                        match event.keystroke.key.as_str() {
                            "escape" => editor.close_find(&CloseFind, window, cx),
                            "enter" if event.keystroke.modifiers.shift => {
                                editor.find_previous(&FindPrevious, window, cx)
                            }
                            "enter" => editor.find_next(&FindNext, window, cx),
                            _ => return,
                        }
                        cx.stop_propagation();
                    }),
                )
                .child(div().w(px(220.)).child(self.find_query.clone()))
                .child(
                    div()
                        .w(px(180.))
                        .children((!self.read_only).then(|| self.replace_query.clone())),
                )
                .child(
                    Button::new("editor-find-case", "Aa")
                        .tone(if self.find_case_sensitive {
                            ButtonTone::Accent
                        } else {
                            ButtonTone::Ghost
                        })
                        .on_click(cx.listener(|editor, _, _, cx| editor.toggle_find_case(cx))),
                )
                .child(
                    div()
                        .w(px(58.))
                        .text_xs()
                        .text_center()
                        .text_color(colors.muted_text)
                        .child(format!("{current}/{}", matches.len())),
                )
                .child(
                    IconButton::new(
                        "editor-find-previous",
                        IconName::ChevronLeft,
                        "Previous match",
                    )
                    .square(px(24.))
                    .on_click(cx.listener(|editor, _, window, cx| {
                        editor.find_previous(&FindPrevious, window, cx)
                    })),
                )
                .child(
                    IconButton::new("editor-find-next", IconName::ChevronRight, "Next match")
                        .square(px(24.))
                        .on_click(cx.listener(|editor, _, window, cx| {
                            editor.find_next(&FindNext, window, cx)
                        })),
                )
                .child(
                    Button::new("editor-replace-next", "Replace")
                        .tone(ButtonTone::Ghost)
                        .disabled(self.read_only || matches.is_empty())
                        .on_click(cx.listener(|editor, _, window, cx| {
                            editor.replace_next(&ReplaceNext, window, cx)
                        })),
                )
                .child(
                    Button::new("editor-replace-all", "All")
                        .tone(ButtonTone::Ghost)
                        .disabled(self.read_only || matches.is_empty())
                        .on_click(cx.listener(|editor, _, window, cx| {
                            editor.replace_all(&ReplaceAll, window, cx)
                        })),
                )
                .child(div().flex_1())
                .child(
                    IconButton::new("editor-find-close", IconName::Close, "Close find")
                        .square(px(24.))
                        .on_click(cx.listener(|editor, _, window, cx| {
                            editor.close_find(&CloseFind, window, cx)
                        })),
                )
                .into_any_element(),
        )
    }

    /// One-line semantic status: the diagnostic under the caret if there is
    /// one, else the last notice, else the aggregate counts. Never blocks
    /// editing and never claims freshness it does not have.
    fn semantic_status(&self) -> Option<(String, bool)> {
        let errors = self.semantic.error_count();
        let warnings = self.semantic.warning_count();
        let counts = format!("{errors} errors, {warnings} warnings");
        if let Some(diagnostic) = self.semantic.diagnostic_at(self.document.cursor()) {
            let suffix = if diagnostic.quick_fix_ids.is_empty() {
                String::new()
            } else {
                " · quick fix available".into()
            };
            return Some((
                format!(
                    "{}: {}{suffix} · {counts}",
                    diagnostic.code, diagnostic.message,
                ),
                diagnostic.severity == sift_protocol::DiagnosticSeverity::Error,
            ));
        }
        if let Some(notice) = self.semantic.notice() {
            return Some((notice.to_owned(), false));
        }
        if !self.semantic.usages().is_empty() {
            let cursor = self.document.cursor();
            let here = self
                .semantic
                .usages()
                .iter()
                .find(|(range, _)| range.contains(&cursor))
                .map(|(_, kind)| usage_kind_label(*kind));
            let total = self.semantic.usages().len();
            return Some((
                match here {
                    Some(kind) => format!("{total} usage(s) · caret is a {kind}"),
                    None => format!("{total} usage(s) highlighted"),
                },
                false,
            ));
        }
        if errors == 0 && warnings == 0 {
            return None;
        }
        let incomplete = if self.semantic.diagnostics_incomplete() {
            " · catalog checks incomplete"
        } else {
            ""
        };
        Some((format!("{counts}{incomplete}"), errors > 0))
    }

    fn format_problem(&self, diagnostic: &EditorDiagnostic) -> String {
        let line = self.document.line_of_offset(diagnostic.range.start) + 1;
        let severity = match diagnostic.severity {
            sift_protocol::DiagnosticSeverity::Error => "error",
            sift_protocol::DiagnosticSeverity::Warning => "warning",
            sift_protocol::DiagnosticSeverity::Information => "info",
            sift_protocol::DiagnosticSeverity::Hint => "hint",
        };
        format!(
            "Line {line} [{severity}] {}: {}",
            diagnostic.code, diagnostic.message
        )
    }

    fn current_problem_text(&self) -> Option<String> {
        self.semantic
            .diagnostic_at(self.document.cursor())
            .or_else(|| {
                (self.semantic.diagnostics().len() == 1).then(|| &self.semantic.diagnostics()[0])
            })
            .map(|diagnostic| self.format_problem(diagnostic))
    }

    fn all_problems_text(&self) -> Option<String> {
        (!self.semantic.diagnostics().is_empty()).then(|| {
            self.semantic
                .diagnostics()
                .iter()
                .map(|diagnostic| self.format_problem(diagnostic))
                .collect::<Vec<_>>()
                .join("\n")
        })
    }

    fn copy_current_problem(
        &mut self,
        _: &gpui::ClickEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(problem) = self.current_problem_text() {
            cx.write_to_clipboard(ClipboardItem::new_string(problem));
        }
    }

    fn copy_all_problems(&mut self, _: &gpui::ClickEvent, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(problems) = self.all_problems_text() {
            cx.write_to_clipboard(ClipboardItem::new_string(problems));
        }
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
        self.selection_changed(cx);
    }

    fn replace_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.vim_text(new_text, cx) {
            return;
        }
        if self.read_only {
            return;
        }
        let range = range_utf16
            .as_ref()
            .map(|range| self.range_from_utf16(range))
            .or_else(|| self.marked_range.clone())
            .unwrap_or_else(|| self.document.selection());
        self.adjust_snippet_tabstops(range.clone(), new_text.len());
        self.document.replace_range(range, new_text);
        self.marked_range = None;
        self.edited(cx);
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        new_selected_range_utf16: Option<Range<usize>>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.read_only {
            return;
        }
        let range = range_utf16
            .as_ref()
            .map(|range| self.range_from_utf16(range))
            .or_else(|| self.marked_range.clone())
            .unwrap_or_else(|| self.document.selection());
        self.adjust_snippet_tabstops(range.clone(), new_text.len());
        self.document.replace_range(range.clone(), new_text);
        self.marked_range =
            (!new_text.is_empty()).then(|| range.start..range.start + new_text.len());
        if let Some(selection) = new_selected_range_utf16 {
            let start = offset_from_utf16(new_text, selection.start) + range.start;
            let end = offset_from_utf16(new_text, selection.end) + range.start;
            self.document.set_selection(start..end, false);
        }
        self.edited(cx);
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
        let layout = self
            .line_layouts
            .get(line.checked_sub(self.visible_line_start)?)?;
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
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<usize> {
        self.byte_index_for_point(point, cx.theme(), window)
            .map(|offset| self.offset_to_utf16(offset))
    }
}

impl gpui::Render for QueryEditor {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = cx.theme().colors;
        let status = if self.manifest_schema {
            self.semantic_status()
        } else {
            match self.language {
                EditorLanguage::Toml => {
                    toml_diagnostic(self.document.text()).map(|message| (message, true))
                }
                EditorLanguage::Json
                | EditorLanguage::Sql
                | EditorLanguage::Markdown
                | EditorLanguage::PlainText => self.semantic_status(),
            }
        };
        let focused = self.focus_handle.is_focused(window);
        let has_current_problem = self.current_problem_text().is_some();
        let problem_count = self.semantic.diagnostics().len();
        let blink_enabled = self.cursor_blink.read(cx).enabled;
        if focused != blink_enabled {
            if focused {
                self.cursor_blink.update(cx, CursorBlink::enable);
            } else {
                self.cursor_blink.update(cx, CursorBlink::disable);
            }
        }
        let key_context = match (self.keymap, self.vim_mode) {
            (EditorKeymap::Vim, VimMode::Normal) => "SiftEditor vim_mode=normal",
            (EditorKeymap::Vim, VimMode::Visual | VimMode::Select) => "SiftEditor vim_mode=visual",
            (EditorKeymap::Vim, VimMode::OperatorPending) => "SiftEditor vim_mode=operator_pending",
            (EditorKeymap::Vim, VimMode::Command) => "SiftEditor vim_mode=command",
            (EditorKeymap::Vim, VimMode::Insert) | (EditorKeymap::Standard, _) => {
                "SiftEditor vim_mode=insert"
            }
        };
        div()
            .id("sift-query-editor")
            .key_context(key_context)
            .role(Role::TextInput)
            .aria_label(match self.language {
                _ if self.diff_language.is_some() => "Git file diff",
                EditorLanguage::Sql => "SQL query editor",
                EditorLanguage::Toml => "TOML configuration editor",
                EditorLanguage::Json => "JSON editor",
                EditorLanguage::Markdown => "Markdown editor",
                EditorLanguage::PlainText => "Read-only text editor",
            })
            .track_focus(&self.focus_handle)
            .cursor(CursorStyle::IBeam)
            .size_full()
            .min_h_0()
            .overflow_hidden()
            .flex()
            .flex_col()
            .font_family("monospace")
            .text_color(colors.text)
            .on_hover(cx.listener(|editor, hovered: &bool, _, cx| {
                if !*hovered
                    && (editor.semantic.clear_hover() || editor.manifest_hover.take().is_some())
                {
                    editor.hover_anchor = None;
                    cx.notify();
                }
            }))
            .on_mouse_move(
                cx.listener(|editor, event: &gpui::MouseMoveEvent, window, cx| {
                    if !event.dragging() {
                        editor.request_hover_at(event.position, window, cx);
                    }
                }),
            )
            // Clicking the editor focuses it directly (synchronously), so the
            // SiftEditor key context is active and editing keys route.
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|editor, event: &gpui::MouseDownEvent, window, cx| {
                    editor.focus_handle.clone().focus(window, cx);
                    if let Some(range) = editor.line_range_for_gutter_point(event.position) {
                        editor.document.set_selection(range, false);
                        editor.selection_changed(cx);
                        return;
                    }
                    let Some(cursor) =
                        editor.byte_index_for_point(event.position, cx.theme(), window)
                    else {
                        return;
                    };
                    editor.document.set_selection(cursor..cursor, false);
                    if let Some(vim) = editor.vim.as_mut() {
                        vim.set_cursor(editor.document.text(), cursor);
                    }
                    editor.selection_changed(cx);
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
            .on_action(cx.listener(Self::vim_undo))
            .on_action(cx.listener(Self::redo))
            .on_action(cx.listener(Self::exit_insert_mode))
            .on_action(cx.listener(Self::execute_statement))
            .on_action(cx.listener(Self::execute_document))
            .on_action(cx.listener(Self::complete))
            .on_action(cx.listener(Self::expand_star))
            .on_action(cx.listener(Self::format_document))
            .on_action(cx.listener(Self::apply_quick_fix))
            .on_action(cx.listener(Self::find_usages))
            .on_action(cx.listener(Self::go_to_next_diagnostic))
            .on_action(cx.listener(Self::open_find))
            .on_action(cx.listener(Self::find_next))
            .on_action(cx.listener(Self::find_previous))
            .on_action(cx.listener(Self::replace_next))
            .on_action(cx.listener(Self::replace_all))
            .on_action(cx.listener(Self::close_find))
            .children(self.render_manifest_lifecycle(cx))
            .children(self.render_find_bar(cx))
            .child(
                div()
                    .id("editor-scroll")
                    .debug_selector(|| "editor-scroll".to_string())
                    .flex_1()
                    .min_h_0()
                    .bg(colors.background)
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll_handle)
                    .child(
                        // Relative-positioned wrapper so the completion menu
                        // anchors to the text. Its height must stay auto or the
                        // scroll container would clamp the editor's content.
                        div()
                            .relative()
                            .w_full()
                            .child(QueryEditorElement {
                                editor: cx.entity(),
                            })
                            .children(self.render_completion_menu(cx))
                            .children(self.render_hover_card(cx))
                            .children(self.render_manifest_hover_card(cx))
                            .children(self.render_star_expansion_card(cx)),
                    ),
            )
            .children(status.map(|(message, error)| {
                div()
                    .id("editor-status-line")
                    .debug_selector(|| "editor-status-line".to_string())
                    .flex_none()
                    .h(px(24.))
                    .px_3()
                    .flex()
                    .items_center()
                    .overflow_hidden()
                    .border_t_1()
                    .border_color(colors.subtle_border)
                    .bg(colors.surface)
                    .text_xs()
                    .text_color(if error {
                        colors.danger
                    } else {
                        colors.muted_text
                    })
                    .child(div().flex_1().min_w_0().truncate().child(message))
                    .child(
                        div()
                            .flex_none()
                            .flex()
                            .items_center()
                            .gap_1()
                            .child(
                                IconButton::new(
                                    "editor-copy-current-problem",
                                    IconName::Document,
                                    "Copy current problem",
                                )
                                .square(px(20.))
                                .disabled(!has_current_problem)
                                .tooltip("Copy current problem with line number")
                                .on_click(cx.listener(Self::copy_current_problem)),
                            )
                            .children((problem_count > 1).then(|| {
                                IconButton::new(
                                    "editor-copy-all-problems",
                                    IconName::Copy,
                                    "Copy all problems",
                                )
                                .square(px(20.))
                                .badge(problem_count)
                                .disabled(problem_count == 0)
                                .tooltip("Copy all problems with line numbers")
                                .on_click(cx.listener(Self::copy_all_problems))
                            })),
                    )
            }))
    }
}

struct QueryEditorElement {
    editor: Entity<QueryEditor>,
}

struct EditorPrepaint {
    lines: Vec<(usize, ShapedLine)>,
    line_numbers: Vec<(usize, ShapedLine)>,
    line_starts: Arc<Vec<usize>>,
    visible_line_start: usize,
    gutter_diagnostics: Vec<PaintQuad>,
    active_line: Option<PaintQuad>,
    find_matches: Vec<PaintQuad>,
    selections: Vec<PaintQuad>,
    usages: Vec<PaintQuad>,
    diagnostics: Vec<PaintQuad>,
    cursor: Option<PaintQuad>,
    text_bounds: Bounds<Pixels>,
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
        let line_count = self.editor.read(cx).document.line_count();
        let mut style = Style::default();
        style.size.width = gpui::relative(1.).into();
        style.size.height =
            (EDITOR_VERTICAL_INSET * 2. + EDITOR_LINE_HEIGHT * line_count.max(1) as f32).into();
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
        let text = editor.document.text();
        let selection = editor.document.selection();
        let cursor = editor.document.cursor();
        let theme = cx.theme();
        let language = editor.language;
        let diff_language = editor.diff_language;
        let block_cursor = editor.keymap == EditorKeymap::Vim && editor.vim_mode == VimMode::Normal;
        let cursor_visible = editor.cursor_blink.read(cx).visible;
        let editor_focused = editor.focus_handle.is_focused(window);
        let find_matches = if editor.find_open {
            editor.current_find_matches(cx)
        } else {
            Arc::new(Vec::new())
        };
        let viewport = editor.scroll_handle.bounds();
        let style = window.text_style();
        let font_size = style.font_size.to_pixels(window.rem_size());
        let line_height = EDITOR_LINE_HEIGHT;

        let mut lines = Vec::new();
        let mut line_numbers = Vec::new();
        let line_starts = editor.document.line_starts();
        let mut selections = Vec::new();
        let mut find_quads = Vec::new();
        let mut usage_quads = Vec::new();
        let mut diagnostic_quads = Vec::new();
        let mut gutter_diagnostic_quads = Vec::new();
        let mut cursor_quad = None;
        let mut active_line_quad = None;
        let text_top = bounds.top() + EDITOR_VERTICAL_INSET;
        let text_left = bounds.left() + EDITOR_GUTTER_WIDTH + EDITOR_TEXT_INSET;
        let text_bounds = Bounds::new(
            point(text_left, text_top),
            size(
                (bounds.size.width - EDITOR_GUTTER_WIDTH - EDITOR_TEXT_INSET).max(px(0.)),
                EDITOR_LINE_HEIGHT * line_starts.len().max(1) as f32,
            ),
        );
        let scroll_top = -editor.scroll_handle.offset().y;
        let visible_start = if viewport.size.height > px(0.) {
            (f32::from((scroll_top - EDITOR_VERTICAL_INSET).max(px(0.))) / f32::from(line_height))
                .floor() as usize
        } else {
            0
        }
        .saturating_sub(2)
        .min(line_starts.len().saturating_sub(1));
        let visible_end = if viewport.size.height > px(0.) {
            ((f32::from((scroll_top + viewport.size.height - EDITOR_VERTICAL_INSET).max(px(0.)))
                / f32::from(line_height))
            .ceil() as usize)
                + 2
        } else {
            100
        }
        .min(line_starts.len());

        for line_index in visible_start..visible_end {
            let offset = line_starts[line_index];
            let line_end = line_starts
                .get(line_index + 1)
                .map_or(text.len(), |next| next.saturating_sub(1));
            let line = &text[offset..line_end];
            let cached = editor.line_cache.borrow().lines.get(&line_index).cloned();
            let shaped = if let Some(line) = cached {
                line
            } else {
                let runs = editor_text_runs(line, style.font(), theme, language, diff_language);
                let shaped = window.text_system().shape_line(
                    line.to_string().into(),
                    font_size,
                    &runs,
                    None,
                );
                editor
                    .line_cache
                    .borrow_mut()
                    .lines
                    .insert(line_index, shaped.clone());
                shaped
            };
            let number_color = if cursor >= offset && cursor <= line_end {
                theme.colors.accent
            } else {
                theme.colors.disabled_text
            };
            let number = (line_index + 1).to_string();
            let number_runs = [TextRun {
                len: number.len(),
                font: style.font(),
                color: number_color,
                background_color: None,
                underline: None,
                strikethrough: None,
            }];
            line_numbers.push((
                line_index,
                window
                    .text_system()
                    .shape_line(number.into(), font_size, &number_runs, None),
            ));
            let top = text_top + line_height * line_index as f32;

            if let Some(diagnostic) = editor
                .semantic
                .diagnostic_indexes_on_line(line_index)
                .iter()
                .map(|index| &editor.semantic.diagnostics()[*index])
                .min_by_key(|diagnostic| match diagnostic.severity {
                    sift_protocol::DiagnosticSeverity::Error => 0,
                    sift_protocol::DiagnosticSeverity::Warning => 1,
                    sift_protocol::DiagnosticSeverity::Information => 2,
                    sift_protocol::DiagnosticSeverity::Hint => 3,
                })
            {
                let color = match diagnostic.severity {
                    sift_protocol::DiagnosticSeverity::Error => theme.colors.danger,
                    sift_protocol::DiagnosticSeverity::Warning => theme.colors.warning,
                    _ => theme.colors.muted_text,
                };
                gutter_diagnostic_quads.push(fill(
                    Bounds::new(
                        point(bounds.left() + px(4.), top + px(5.)),
                        size(px(3.), line_height - px(10.)),
                    ),
                    color,
                ));
            }

            if cursor >= offset && cursor <= line_end {
                active_line_quad = Some(fill(
                    Bounds::new(
                        point(bounds.left(), top),
                        size(bounds.size.width, line_height),
                    ),
                    theme.colors.editor_active_line,
                ));
            }

            // Selection rectangle for the portion of this line inside the range.
            if editor.find_open {
                for range in find_matches.iter() {
                    if let Some(bounds) = span_bounds(
                        range,
                        offset,
                        line_end,
                        &shaped,
                        text_left,
                        top,
                        line_height,
                    ) {
                        find_quads.push(fill(bounds, theme.colors.accent_muted));
                    }
                }
            }

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
                            point(text_left + x0, top),
                            point(text_left + x1, top + line_height),
                        ),
                        theme.colors.selected_surface,
                    ));
                }
            }

            // Usage highlights sit behind the glyphs; diagnostics underline
            // them. Both are clipped to this line's span of the range.
            for index in editor.semantic.usage_indexes_on_line(line_index) {
                let (range, _) = &editor.semantic.usages()[*index];
                if let Some(bounds) = span_bounds(
                    range,
                    offset,
                    line_end,
                    &shaped,
                    text_left,
                    top,
                    line_height,
                ) {
                    usage_quads.push(fill(bounds, theme.colors.accent_muted));
                }
            }
            for index in editor.semantic.diagnostic_indexes_on_line(line_index) {
                let diagnostic = &editor.semantic.diagnostics()[*index];
                let color = match diagnostic.severity {
                    sift_protocol::DiagnosticSeverity::Error => theme.colors.danger,
                    sift_protocol::DiagnosticSeverity::Warning => theme.colors.warning,
                    _ => theme.colors.muted_text,
                };
                if let Some(bounds) = span_bounds(
                    &diagnostic.range,
                    offset,
                    line_end,
                    &shaped,
                    text_left,
                    top,
                    line_height,
                ) {
                    diagnostic_quads.push(fill(
                        Bounds::new(
                            point(bounds.left(), bounds.bottom() - DIAGNOSTIC_UNDERLINE_HEIGHT),
                            size(bounds.size.width, DIAGNOSTIC_UNDERLINE_HEIGHT),
                        ),
                        color,
                    ));
                }
            }

            if selection.is_empty()
                && (!editor_focused || cursor_visible)
                && cursor >= offset
                && cursor <= line_end
            {
                let cursor_in_line = cursor - offset;
                let x = shaped.x_for_index(cursor_in_line);
                let cursor_width = if block_cursor || !editor_focused {
                    line[cursor_in_line..]
                        .chars()
                        .next()
                        .map(|character| {
                            let next_x = shaped.x_for_index(cursor_in_line + character.len_utf8());
                            (next_x - x).max(BLOCK_CURSOR_FALLBACK_WIDTH)
                        })
                        .unwrap_or(BLOCK_CURSOR_FALLBACK_WIDTH)
                } else {
                    px(1.5)
                };
                let cursor_bounds =
                    Bounds::new(point(text_left + x, top), size(cursor_width, line_height));
                cursor_quad = Some(if editor_focused {
                    fill(cursor_bounds, theme.colors.accent)
                } else {
                    outline(cursor_bounds, theme.colors.accent, BorderStyle::default())
                });
            }

            lines.push((line_index, shaped));
        }

        {
            let mut cache = editor.line_cache.borrow_mut();
            if cache.lines.len() > 512 {
                let keep_start = visible_start.saturating_sub(128);
                let keep_end = (visible_end + 128).min(line_starts.len());
                cache
                    .lines
                    .retain(|line, _| *line >= keep_start && *line < keep_end);
            }
        }

        EditorPrepaint {
            lines,
            line_numbers,
            line_starts,
            visible_line_start: visible_start,
            gutter_diagnostics: gutter_diagnostic_quads,
            active_line: active_line_quad,
            find_matches: find_quads,
            selections,
            usages: usage_quads,
            diagnostics: diagnostic_quads,
            cursor: cursor_quad,
            text_bounds,
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
            ElementInputHandler::new(prepaint.text_bounds, self.editor.clone()),
            cx,
        );
        if let Some(active_line) = prepaint.active_line.take() {
            window.paint_quad(active_line);
        }
        for found in prepaint.find_matches.drain(..) {
            window.paint_quad(found);
        }
        for selection in prepaint.selections.drain(..) {
            window.paint_quad(selection);
        }
        for usage in prepaint.usages.drain(..) {
            window.paint_quad(usage);
        }
        let line_height = prepaint.line_height;
        for (index, line) in &prepaint.lines {
            let origin = point(
                prepaint.text_bounds.left(),
                prepaint.text_bounds.top() + line_height * *index as f32,
            );
            line.paint(origin, line_height, gpui::TextAlign::Left, None, window, cx)
                .expect("editor line paint succeeds");
        }
        for (index, number) in &prepaint.line_numbers {
            let origin = point(
                bounds.left() + EDITOR_GUTTER_WIDTH - px(8.) - number.width(),
                prepaint.text_bounds.top() + line_height * *index as f32,
            );
            number
                .paint(origin, line_height, gpui::TextAlign::Left, None, window, cx)
                .expect("line number paint succeeds");
        }
        for marker in prepaint.gutter_diagnostics.drain(..) {
            window.paint_quad(marker);
        }
        for diagnostic in prepaint.diagnostics.drain(..) {
            window.paint_quad(diagnostic);
        }
        if let Some(cursor) = prepaint.cursor.take() {
            window.paint_quad(cursor);
        }
        let lines = std::mem::take(&mut prepaint.lines)
            .into_iter()
            .map(|(_, line)| line)
            .collect();
        let line_starts = std::mem::take(&mut prepaint.line_starts);
        self.editor.update(cx, |editor, _| {
            editor.line_layouts = lines;
            editor.visible_line_start = prepaint.visible_line_start;
            editor.line_starts = line_starts;
            editor.line_height = line_height;
            editor.last_bounds = Some(prepaint.text_bounds);
        });
    }
}

fn editor_text_runs(
    line: &str,
    font: gpui::Font,
    theme: Theme,
    language: EditorLanguage,
    diff_language: Option<EditorLanguage>,
) -> Vec<TextRun> {
    if let Some(diff_language) = diff_language {
        return diff_text_runs(line, font, theme, diff_language);
    }
    language_text_runs(line, font, theme, language)
}

fn language_text_runs(
    line: &str,
    font: gpui::Font,
    theme: Theme,
    language: EditorLanguage,
) -> Vec<TextRun> {
    match language {
        EditorLanguage::Sql => sql_text_runs(line, font, theme),
        EditorLanguage::Toml => toml_text_runs(line, font, theme),
        EditorLanguage::Json => json_text_runs(line, font, theme),
        EditorLanguage::Markdown => markdown_text_runs(line, font, theme),
        EditorLanguage::PlainText => plain_text_runs(line, font, theme),
    }
}

fn diff_text_runs(
    line: &str,
    font: gpui::Font,
    theme: Theme,
    language: EditorLanguage,
) -> Vec<TextRun> {
    if line.starts_with("@@ ") {
        return vec![TextRun {
            len: line.len(),
            font,
            color: theme.colors.accent,
            background_color: Some(theme.colors.accent_muted),
            underline: None,
            strikethrough: None,
        }];
    }
    let Some(marker) = line.as_bytes().first().copied() else {
        return plain_text_runs(line, font, theme);
    };
    let color = match marker {
        b'+' => theme.colors.success,
        b'-' => theme.colors.danger,
        b'\\' | b' ' => theme.colors.muted_text,
        _ => return plain_text_runs(line, font, theme),
    };
    let mut runs = vec![TextRun {
        len: 1,
        font: font.clone(),
        color,
        background_color: None,
        underline: None,
        strikethrough: None,
    }];
    runs.extend(language_text_runs(&line[1..], font, theme, language));
    runs
}

fn markdown_text_runs(line: &str, font: gpui::Font, theme: Theme) -> Vec<TextRun> {
    let trimmed = line.trim_start();
    let color = if trimmed.starts_with('#') {
        theme.colors.syntax_keyword
    } else if trimmed.starts_with('>') {
        theme.colors.syntax_comment
    } else if trimmed.starts_with("```") || trimmed.contains('`') {
        theme.colors.syntax_string
    } else {
        theme.colors.text
    };
    vec![TextRun {
        len: line.len(),
        font,
        color,
        background_color: None,
        underline: None,
        strikethrough: None,
    }]
}

fn sql_text_runs(line: &str, font: gpui::Font, theme: Theme) -> Vec<TextRun> {
    let mut runs = Vec::new();
    let bytes = line.as_bytes();
    let mut start = 0;
    while start < bytes.len() {
        let (end, color) = if bytes[start..].starts_with(b"--") {
            (bytes.len(), theme.colors.syntax_comment)
        } else if bytes[start] == b'\'' {
            let mut end = start + 1;
            while end < bytes.len() {
                if bytes[end] == b'\'' {
                    end += 1;
                    if end < bytes.len() && bytes[end] == b'\'' {
                        end += 1;
                        continue;
                    }
                    break;
                }
                end += line[end..].chars().next().map_or(1, char::len_utf8);
            }
            (end, theme.colors.syntax_string)
        } else if bytes[start].is_ascii_digit() {
            let end = bytes[start..]
                .iter()
                .position(|byte| !byte.is_ascii_digit() && *byte != b'.' && *byte != b'_')
                .map_or(bytes.len(), |offset| start + offset);
            (end, theme.colors.syntax_number)
        } else if bytes[start].is_ascii_alphabetic() || bytes[start] == b'_' {
            let end = bytes[start..]
                .iter()
                .position(|byte| !byte.is_ascii_alphanumeric() && *byte != b'_')
                .map_or(bytes.len(), |offset| start + offset);
            let word = &line[start..end];
            let color = if is_sql_keyword(word) {
                theme.colors.syntax_keyword
            } else {
                theme.colors.text
            };
            (end, color)
        } else {
            (
                start + line[start..].chars().next().map_or(1, char::len_utf8),
                theme.colors.text,
            )
        };
        runs.push(TextRun {
            len: end - start,
            font: font.clone(),
            color,
            background_color: None,
            underline: None,
            strikethrough: None,
        });
        start = end;
    }
    runs
}

fn plain_text_runs(line: &str, font: gpui::Font, theme: Theme) -> Vec<TextRun> {
    vec![TextRun {
        len: line.len(),
        font,
        color: theme.colors.text,
        background_color: None,
        underline: None,
        strikethrough: None,
    }]
}

fn toml_diagnostic(source: &str) -> Option<String> {
    toml::from_str::<toml::Value>(source).err().map(|error| {
        let line = error.span().map_or(1, |span| {
            source[..span.start.min(source.len())]
                .bytes()
                .filter(|byte| *byte == b'\n')
                .count()
                + 1
        });
        format!("Invalid TOML near line {line}")
    })
}

fn json_schema_diagnostics(
    source: &str,
    schema: Option<&JsonSchema>,
) -> Vec<sift_protocol::SemanticDiagnostic> {
    let value = match serde_json::from_str::<serde_json::Value>(source) {
        Ok(value) => value,
        Err(error) => {
            let start = json_error_token_start(
                source,
                json_error_offset(source, error.line(), error.column()),
            );
            return vec![json_diagnostic_at(
                "json.syntax",
                error.to_string(),
                start..next_char_boundary(source, start),
            )];
        }
    };
    let Some(schema) = schema else {
        return Vec::new();
    };
    match schema {
        JsonSchema::Keymaps { command_ids } => {
            let Some(root) = value.as_object() else {
                return vec![json_diagnostic_at(
                    "json.schema.root",
                    "keymaps.json must contain an object".into(),
                    0..source.len(),
                )];
            };
            let mut diagnostics = Vec::new();
            for key in root.keys() {
                if !matches!(key.as_str(), "version" | "bindings") {
                    diagnostics.push(json_diagnostic_at(
                        "json.schema.unknown-property",
                        format!("Unknown keymaps.json property {key:?}"),
                        json_key_span(source, key).unwrap_or(0..0),
                    ));
                }
            }
            match root.get("version") {
                None => diagnostics.push(json_diagnostic_at(
                    "json.schema.required",
                    "Missing required property \"version\"".into(),
                    0..0,
                )),
                Some(serde_json::Value::Number(version)) if version.as_u64() == Some(1) => {}
                Some(_) => diagnostics.push(json_diagnostic_at(
                    "json.schema.version",
                    "version must be the integer 1".into(),
                    json_value_span(source, "version").unwrap_or(0..0),
                )),
            }
            match root.get("bindings") {
                None => diagnostics.push(json_diagnostic_at(
                    "json.schema.required",
                    "Missing required property \"bindings\"".into(),
                    0..0,
                )),
                Some(serde_json::Value::Object(bindings)) => {
                    for (command, sequence) in bindings {
                        let range = json_key_span(source, command).unwrap_or(0..0);
                        if !command_ids.iter().any(|known| known == command) {
                            diagnostics.push(json_diagnostic_at(
                                "json.schema.command",
                                format!("Unknown command id {command:?}"),
                                range,
                            ));
                        }
                        if !sequence.is_string() {
                            diagnostics.push(json_diagnostic_at(
                                "json.schema.binding",
                                format!("Binding for {command:?} must be a string"),
                                json_value_span(source, command).unwrap_or(0..0),
                            ));
                        }
                    }
                }
                Some(_) => diagnostics.push(json_diagnostic_at(
                    "json.schema.bindings",
                    "bindings must be an object".into(),
                    json_value_span(source, "bindings").unwrap_or(0..0),
                )),
            }
            diagnostics
        }
    }
}

fn json_schema_completions(
    source: &str,
    cursor: usize,
    schema: &JsonSchema,
) -> (
    Range<usize>,
    Vec<sift_protocol::completion::CompletionCandidate>,
) {
    let cursor = cursor.min(source.len());
    let string_range = json_string_contents_at(source, cursor);
    let replace = string_range.clone().unwrap_or(cursor..cursor);
    let bare_key = string_range.is_some();
    let object_start = innermost_json_object(source, cursor);
    let object_name = object_start.and_then(|start| json_object_property_name(source, start));
    let parsed = serde_json::from_str::<serde_json::Value>(source).ok();

    match schema {
        JsonSchema::Keymaps { command_ids } if object_name.as_deref() == Some("bindings") => {
            let used = parsed
                .as_ref()
                .and_then(|value| value.get("bindings"))
                .and_then(serde_json::Value::as_object);
            let candidates = command_ids
                .iter()
                .filter(|command| !used.is_some_and(|used| used.contains_key(command.as_str())))
                .map(|command| {
                    json_completion_candidate(
                        command,
                        if bare_key {
                            command.clone()
                        } else {
                            format!("\"{command}\": \"\"")
                        },
                        "Command binding",
                    )
                })
                .collect();
            (replace, candidates)
        }
        JsonSchema::Keymaps { .. } => {
            let root = parsed.as_ref().and_then(serde_json::Value::as_object);
            let candidates = [
                ("version", "\"version\": 1", "Keymap schema version"),
                ("bindings", "\"bindings\": {}", "Command bindings"),
            ]
            .into_iter()
            .filter(|(key, _, _)| !root.is_some_and(|root| root.contains_key(*key)))
            .map(|(label, insert, detail)| {
                json_completion_candidate(label, if bare_key { label } else { insert }, detail)
            })
            .collect();
            (replace, candidates)
        }
    }
}

fn json_completion_candidate(
    label: impl Into<String>,
    insert: impl Into<String>,
    detail: &str,
) -> sift_protocol::completion::CompletionCandidate {
    let label = label.into();
    sift_protocol::completion::CompletionCandidate {
        label: label.clone().into(),
        insert: insert.into().into(),
        kind: sift_protocol::completion::CompletionKind::Keyword,
        detail: Some(detail.into()),
        qualified_name: None,
        score: 0,
    }
}

fn json_diagnostic_at(
    code: &str,
    message: String,
    range: Range<usize>,
) -> sift_protocol::SemanticDiagnostic {
    sift_protocol::SemanticDiagnostic {
        id: format!("{code}:{}", range.start),
        severity: sift_protocol::DiagnosticSeverity::Error,
        code: code.into(),
        message,
        range: sift_protocol::TextRange {
            start: range.start.min(u32::MAX as usize) as u32,
            end: range.end.min(u32::MAX as usize) as u32,
        },
        related_ranges: Vec::new(),
        source: "json".into(),
        quick_fix_ids: Vec::new(),
    }
}

fn json_error_offset(source: &str, line: usize, column: usize) -> usize {
    let line_start = source
        .match_indices('\n')
        .nth(line.saturating_sub(2))
        .map_or(0, |(offset, _)| offset + 1);
    let mut offset = line_start;
    for _ in 0..column.saturating_sub(1) {
        let Some(character) = source[offset..].chars().next() else {
            break;
        };
        if character == '\n' {
            break;
        }
        offset += character.len_utf8();
    }
    offset.min(source.len())
}

fn json_error_token_start(source: &str, mut offset: usize) -> usize {
    while offset > 0 {
        let Some((previous, character)) = source[..offset].char_indices().next_back() else {
            break;
        };
        if !character.is_ascii_alphanumeric() && character != '_' {
            break;
        }
        offset = previous;
    }
    offset
}

fn next_char_boundary(source: &str, start: usize) -> usize {
    source[start..]
        .chars()
        .next()
        .map_or(start, |character| start + character.len_utf8())
}

fn json_key_span(source: &str, key: &str) -> Option<Range<usize>> {
    let needle = serde_json::to_string(key).ok()?;
    source
        .match_indices(&needle)
        .find(|(start, matched)| {
            source[start + matched.len()..]
                .trim_start()
                .starts_with(':')
        })
        .map(|(start, matched)| start..start + matched.len())
}

fn json_value_span(source: &str, key: &str) -> Option<Range<usize>> {
    let key = json_key_span(source, key)?;
    let colon = source[key.end..].find(':')? + key.end;
    let start = source[colon + 1..].find(|character: char| !character.is_whitespace())? + colon + 1;
    Some(start..next_char_boundary(source, start))
}

fn json_string_contents_at(source: &str, cursor: usize) -> Option<Range<usize>> {
    let mut in_string = false;
    let mut escaped = false;
    let mut start = 0;
    for (index, character) in source.char_indices() {
        if index > cursor {
            break;
        }
        if character == '"' && !escaped {
            if in_string {
                if cursor <= index {
                    return Some(start..index);
                }
                in_string = false;
            } else {
                in_string = true;
                start = index + 1;
            }
        }
        escaped = character == '\\' && !escaped;
        if character != '\\' {
            escaped = false;
        }
    }
    in_string.then_some(start..cursor)
}

fn innermost_json_object(source: &str, cursor: usize) -> Option<usize> {
    let mut stack = Vec::new();
    let mut in_string = false;
    let mut escaped = false;
    for (index, character) in source[..cursor].char_indices() {
        if in_string {
            if character == '"' && !escaped {
                in_string = false;
            }
            escaped = character == '\\' && !escaped;
            if character != '\\' {
                escaped = false;
            }
            continue;
        }
        match character {
            '"' => in_string = true,
            '{' => stack.push(index),
            '}' => {
                stack.pop();
            }
            _ => {}
        }
    }
    stack.last().copied()
}

fn json_object_property_name(source: &str, object_start: usize) -> Option<String> {
    let prefix = source[..object_start].trim_end();
    let colon = prefix.strip_suffix(':')?.trim_end();
    let end = colon.len().checked_sub(1)?;
    (colon.as_bytes().get(end) == Some(&b'"'))
        .then(|| colon[..end].rfind('"'))
        .flatten()
        .map(|start| colon[start + 1..end].to_owned())
}

fn format_json_document(source: &str) -> Result<String, String> {
    serde_json::from_str::<serde_json::Value>(source)
        .map_err(|error| format!("Cannot format invalid JSON near line {}.", error.line()))?;
    let mut formatted = String::with_capacity(source.len() + source.len() / 4);
    let mut depth = 0usize;
    let mut index = 0usize;
    while index < source.len() {
        let character = source[index..].chars().next().expect("index is in bounds");
        let width = character.len_utf8();
        match character {
            '"' => {
                let start = index;
                index += width;
                let mut escaped = false;
                while index < source.len() {
                    let current = source[index..].chars().next().expect("index is in bounds");
                    index += current.len_utf8();
                    if current == '"' && !escaped {
                        break;
                    }
                    escaped = current == '\\' && !escaped;
                    if current != '\\' {
                        escaped = false;
                    }
                }
                formatted.push_str(&source[start..index]);
                continue;
            }
            '{' | '[' => {
                formatted.push(character);
                depth += 1;
                let next = source[index + width..]
                    .chars()
                    .find(|next| !next.is_whitespace());
                let empty = matches!((character, next), ('{', Some('}')) | ('[', Some(']')));
                if !empty {
                    formatted.push('\n');
                    push_json_indent(&mut formatted, depth);
                }
            }
            '}' | ']' => {
                depth = depth.saturating_sub(1);
                let open = if character == '}' { '{' } else { '[' };
                if !formatted.ends_with(open) {
                    formatted.push('\n');
                    push_json_indent(&mut formatted, depth);
                }
                formatted.push(character);
            }
            ',' => {
                formatted.push(',');
                formatted.push('\n');
                push_json_indent(&mut formatted, depth);
            }
            ':' => formatted.push_str(": "),
            whitespace if whitespace.is_whitespace() => {}
            _ => formatted.push(character),
        }
        index += width;
    }
    formatted.push('\n');
    Ok(formatted)
}

fn push_json_indent(output: &mut String, depth: usize) {
    for _ in 0..depth {
        output.push_str("  ");
    }
}

fn json_newline(source: &str, cursor: usize) -> (String, usize) {
    let line_start = source[..cursor].rfind('\n').map_or(0, |index| index + 1);
    let base_indent = source[line_start..cursor]
        .chars()
        .take_while(|character| matches!(character, ' ' | '\t'))
        .collect::<String>();
    let before = source[line_start..cursor].trim_end();
    let next = source[cursor..].trim_start().chars().next();
    let opening = before.ends_with('{') || before.ends_with('[');
    let paired = matches!(
        (before.chars().last(), next),
        (Some('{'), Some('}')) | (Some('['), Some(']'))
    );
    let inner_indent = if opening {
        format!("{base_indent}  ")
    } else {
        base_indent.clone()
    };
    if paired {
        let insert = format!("\n{inner_indent}\n{base_indent}");
        let caret_back = 1 + base_indent.len();
        (insert, caret_back)
    } else {
        (format!("\n{inner_indent}"), 0)
    }
}

fn json_text_runs(line: &str, font: gpui::Font, theme: Theme) -> Vec<TextRun> {
    let mut runs = Vec::new();
    let bytes = line.as_bytes();
    let mut start = 0;
    while start < bytes.len() {
        let (end, color) = if bytes[start] == b'"' {
            let mut end = start + 1;
            let mut escaped = false;
            while end < bytes.len() {
                match bytes[end] {
                    b'"' if !escaped => {
                        end += 1;
                        break;
                    }
                    b'\\' if !escaped => escaped = true,
                    _ => escaped = false,
                }
                end += line[end..].chars().next().map_or(1, char::len_utf8);
            }
            let is_key = line[end..].trim_start().starts_with(':');
            let color = if is_key {
                theme.colors.syntax_keyword
            } else {
                theme.colors.syntax_string
            };
            (end, color)
        } else if bytes[start].is_ascii_digit()
            || (bytes[start] == b'-' && bytes.get(start + 1).is_some_and(u8::is_ascii_digit))
        {
            let end = bytes[start + 1..]
                .iter()
                .position(|byte| !matches!(*byte, b'0'..=b'9' | b'.' | b'e' | b'E' | b'+' | b'-'))
                .map_or(bytes.len(), |offset| start + 1 + offset);
            (end, theme.colors.syntax_number)
        } else if ["true", "false", "null"].into_iter().any(|keyword| {
            line[start..].starts_with(keyword)
                && bytes
                    .get(start + keyword.len())
                    .is_none_or(|byte| !byte.is_ascii_alphanumeric() && *byte != b'_')
        }) {
            let end = ["true", "false", "null"]
                .into_iter()
                .find(|keyword| line[start..].starts_with(keyword))
                .map_or(start + 1, |keyword| start + keyword.len());
            (end, theme.colors.syntax_keyword)
        } else {
            (
                start + line[start..].chars().next().map_or(1, char::len_utf8),
                theme.colors.text,
            )
        };
        runs.push(TextRun {
            len: end - start,
            font: font.clone(),
            color,
            background_color: None,
            underline: None,
            strikethrough: None,
        });
        start = end;
    }
    runs
}

fn toml_text_runs(line: &str, font: gpui::Font, theme: Theme) -> Vec<TextRun> {
    let mut runs = Vec::new();
    let bytes = line.as_bytes();
    let equals = line.find('=');
    let mut start = 0;
    while start < bytes.len() {
        let (end, color) = if bytes[start] == b'#' {
            (bytes.len(), theme.colors.syntax_comment)
        } else if matches!(bytes[start], b'\'' | b'"') {
            let quote = bytes[start];
            let mut end = start + 1;
            while end < bytes.len() {
                if bytes[end] == quote && (quote == b'\'' || end == 0 || bytes[end - 1] != b'\\') {
                    end += 1;
                    break;
                }
                end += line[end..].chars().next().map_or(1, char::len_utf8);
            }
            (end, theme.colors.syntax_string)
        } else if bytes[start].is_ascii_digit()
            || bytes[start..].starts_with(b"true")
            || bytes[start..].starts_with(b"false")
        {
            let end = bytes[start..]
                .iter()
                .position(|byte| byte.is_ascii_whitespace() || matches!(*byte, b',' | b']' | b'#'))
                .map_or(bytes.len(), |offset| start + offset);
            (end, theme.colors.syntax_number)
        } else if equals.is_some_and(|equals| start < equals)
            || (line.trim_start().starts_with('[') && !line.trim_start().starts_with("[]"))
        {
            let end = equals.unwrap_or(bytes.len());
            (end.max(start + 1), theme.colors.syntax_keyword)
        } else {
            (
                start + line[start..].chars().next().map_or(1, char::len_utf8),
                theme.colors.text,
            )
        };
        runs.push(TextRun {
            len: end - start,
            font: font.clone(),
            color,
            background_color: None,
            underline: None,
            strikethrough: None,
        });
        start = end;
    }
    runs
}

fn is_sql_keyword(word: &str) -> bool {
    matches!(
        word.to_ascii_uppercase().as_str(),
        "SELECT"
            | "FROM"
            | "WHERE"
            | "JOIN"
            | "INNER"
            | "LEFT"
            | "RIGHT"
            | "FULL"
            | "OUTER"
            | "ON"
            | "AS"
            | "AND"
            | "OR"
            | "NOT"
            | "NULL"
            | "IS"
            | "IN"
            | "LIKE"
            | "GROUP"
            | "BY"
            | "ORDER"
            | "HAVING"
            | "LIMIT"
            | "OFFSET"
            | "INSERT"
            | "INTO"
            | "VALUES"
            | "UPDATE"
            | "SET"
            | "DELETE"
            | "CREATE"
            | "ALTER"
            | "DROP"
            | "TABLE"
            | "VIEW"
            | "WITH"
            | "UNION"
            | "ALL"
            | "DISTINCT"
            | "CASE"
            | "WHEN"
            | "THEN"
            | "ELSE"
            | "END"
            | "BEGIN"
            | "COMMIT"
            | "ROLLBACK"
            | "RETURNING"
            | "OUTPUT"
    )
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

fn byte_from_line_column(text: &str, (line, column): (usize, usize)) -> usize {
    let mut line_start = 0;
    for _ in 0..line {
        let Some(relative) = text[line_start..].find('\n') else {
            return text.len();
        };
        line_start += relative + 1;
    }
    let line_end = text[line_start..]
        .find('\n')
        .map_or(text.len(), |relative| line_start + relative);
    line_start
        + text[line_start..line_end]
            .char_indices()
            .nth(column)
            .map_or(line_end - line_start, |(offset, _)| offset)
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

    #[test]
    fn automatic_completion_activates_only_in_useful_sql_contexts() {
        for sql in [
            "SELECT ",
            "SELECT * FROM ",
            "SELECT * FROM us",
            "SELECT u.",
            "SELECT * FROM [us",
            "SELECT * FROM \"us",
        ] {
            assert!(should_auto_complete(sql, sql.len()), "{sql}");
        }
        for sql in [
            "",
            "S",
            "SELECT * FROM u",
            "SELECT 'users",
            "SELECT 1 -- users",
            "SELECT /* users",
        ] {
            assert!(!should_auto_complete(sql, sql.len()), "{sql}");
        }
    }

    #[gpui::test]
    fn vim_insert_typing_requests_debounced_completion(cx: &mut TestAppContext) {
        let (mut cx, editor, spy) = editor_with_spy("SELECT * FROM ", cx);
        editor.update_in(&mut cx, |editor, window, cx| {
            editor.set_keymap(EditorKeymap::Vim, cx);
            assert!(editor.vim_key(modalkit::crossterm::event::KeyCode::Char('i'), cx));
            editor.replace_text_in_range(None, "us", window, cx);
        });
        cx.run_until_parked();
        let requests = spy.read_with(&cx, |spy, _| spy.0.clone());
        assert!(requests
            .iter()
            .any(|(_, request)| matches!(request, SemanticRequestKind::Analyze)));
        assert!(requests.iter().any(|(_, request)| matches!(
            request,
            SemanticRequestKind::AutoComplete { cursor: 16 }
        )));
    }

    #[gpui::test]
    fn vim_insert_backspace_avoids_snapshot_and_new_completion_work(cx: &mut TestAppContext) {
        let (mut cx, editor, spy) = editor_with_spy("select users", cx);
        editor.update_in(&mut cx, |editor, window, cx| {
            editor.set_keymap(EditorKeymap::Vim, cx);
            assert!(editor.vim_key(modalkit::crossterm::event::KeyCode::Char('i'), cx));
            editor.backspace(&Backspace, window, cx);
        });
        cx.run_until_parked();
        editor.read_with(&cx, |editor, _| {
            assert_eq!(editor.document().text(), "select user");
            assert_eq!(editor.vim_mode(), VimMode::Insert);
        });
        let requests = spy.read_with(&cx, |spy, _| spy.0.clone());
        assert!(requests
            .iter()
            .any(|(_, request)| matches!(request, SemanticRequestKind::Analyze)));
        assert!(requests
            .iter()
            .all(|(_, request)| !matches!(request, SemanticRequestKind::AutoComplete { .. })));
    }

    /// Collects the semantic intents an editor raises so tests can assert on
    /// what the workspace would have dispatched.
    struct SemanticSpy(Vec<(u64, SemanticRequestKind)>);

    fn editor_with_spy(
        text: &str,
        cx: &mut TestAppContext,
    ) -> (VisualTestContext, Entity<QueryEditor>, Entity<SemanticSpy>) {
        let window = cx
            .update(|cx| {
                let text = text.to_owned();
                cx.open_window(Default::default(), move |_window, cx| {
                    cx.new(|cx| QueryEditor::new(doc(&text), cx))
                })
            })
            .unwrap();
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        let editor = window.root(&mut cx).unwrap();
        let spy = cx.new(|_| SemanticSpy(Vec::new()));
        spy.update(&mut cx, |_, cx| {
            cx.subscribe(&editor, |spy: &mut SemanticSpy, _, event, _| {
                if let EditorEvent::SemanticRequest { revision, request } = event {
                    spy.0.push((*revision, request.clone()));
                }
            })
            .detach();
        });
        cx.run_until_parked();
        (cx, editor, spy)
    }

    fn diagnostic(
        start: u32,
        end: u32,
        quick_fix_ids: Vec<String>,
    ) -> sift_protocol::SemanticDiagnostic {
        sift_protocol::SemanticDiagnostic {
            id: "d1".into(),
            severity: sift_protocol::DiagnosticSeverity::Error,
            code: "SQL001".into(),
            message: "unknown table".into(),
            range: sift_protocol::TextRange { start, end },
            related_ranges: Vec::new(),
            source: "binder".into(),
            quick_fix_ids,
        }
    }

    fn candidate(label: &str) -> sift_protocol::completion::CompletionCandidate {
        sift_protocol::completion::CompletionCandidate {
            label: label.to_owned().into(),
            insert: label.to_owned().into(),
            kind: sift_protocol::completion::CompletionKind::Table,
            detail: None,
            qualified_name: None,
            score: 1,
        }
    }

    #[test]
    fn hover_pointer_coalesces_to_identifier_start() {
        let sql = "select café.id";
        assert_eq!(identifier_hover_position(sql, 9), Some(7));
        assert_eq!(identifier_hover_position(sql, 12), None);
        assert_eq!(identifier_hover_position(sql, 6), None);
        assert!(valid_star_expansion_source("café.*"));
        assert!(valid_star_expansion_source("*"));
        assert!(!valid_star_expansion_source("users; drop table audit; *"));
    }

    #[gpui::test]
    fn editing_asks_for_analysis_of_the_revision_it_produced(cx: &mut TestAppContext) {
        let (mut cx, editor, spy) = editor_with_spy("sel", cx);
        editor.update_in(&mut cx, |editor, window, cx| {
            editor.replace_text_in_range(None, "ect", window, cx);
        });
        cx.run_until_parked();
        let revision = editor.read_with(&cx, |editor, _| editor.text_revision());
        let requests = spy.read_with(&cx, |spy, _| spy.0.clone());
        assert_eq!(
            requests.last(),
            Some(&(revision, SemanticRequestKind::Analyze))
        );
    }

    #[gpui::test]
    fn a_semantic_answer_for_an_older_revision_is_discarded(cx: &mut TestAppContext) {
        let (mut cx, editor, _) = editor_with_spy("select 1", cx);
        editor.update(&mut cx, |editor, cx| {
            let current = editor.text_revision();
            assert!(!editor.apply_semantic_outcome(
                current.wrapping_sub(1),
                SemanticOutcome::Diagnostics {
                    diagnostics: vec![diagnostic(0, 6, Vec::new())],
                    incomplete: false,
                },
                cx,
            ));
            assert!(editor.semantic().diagnostics().is_empty());
            assert!(editor.apply_semantic_outcome(
                current,
                SemanticOutcome::Diagnostics {
                    diagnostics: vec![diagnostic(0, 6, Vec::new())],
                    incomplete: false,
                },
                cx,
            ));
            assert_eq!(editor.semantic().error_count(), 1);
        });
    }

    #[gpui::test]
    fn diagnostic_status_line_is_hidden_until_needed(cx: &mut TestAppContext) {
        let (mut cx, editor, _) = editor_with_spy("select from", cx);
        cx.run_until_parked();
        let before = cx.debug_bounds("editor-scroll").expect("editor viewport");
        assert!(cx.debug_bounds("editor-status-line").is_none());

        editor.update(&mut cx, |editor, cx| {
            let revision = editor.text_revision();
            assert!(editor.apply_semantic_outcome(
                revision,
                SemanticOutcome::Diagnostics {
                    diagnostics: vec![diagnostic(7, 11, Vec::new())],
                    incomplete: false,
                },
                cx,
            ));
        });
        cx.run_until_parked();

        let after = cx.debug_bounds("editor-scroll").expect("editor viewport");
        assert!(after.size.height < before.size.height);
        assert!(cx.debug_bounds("editor-status-line").is_some());
    }

    #[gpui::test]
    fn copied_problems_include_one_based_line_numbers(cx: &mut TestAppContext) {
        let (mut cx, editor, _) = editor_with_spy("select 1;\nfrom missing", cx);
        editor.update(&mut cx, |editor, cx| {
            let mut warning = diagnostic(15, 22, Vec::new());
            warning.id = "d2".into();
            warning.severity = sift_protocol::DiagnosticSeverity::Warning;
            warning.code = "SQL002".into();
            warning.message = "unqualified object".into();
            let revision = editor.text_revision();
            assert!(editor.apply_semantic_outcome(
                revision,
                SemanticOutcome::Diagnostics {
                    diagnostics: vec![diagnostic(0, 6, Vec::new()), warning],
                    incomplete: false,
                },
                cx,
            ));
            editor.document.set_selection(16..16, false);
            assert_eq!(
                editor.current_problem_text().as_deref(),
                Some("Line 2 [warning] SQL002: unqualified object")
            );
            assert_eq!(
                editor.all_problems_text().as_deref(),
                Some(
                    "Line 1 [error] SQL001: unknown table\n\
                     Line 2 [warning] SQL002: unqualified object"
                )
            );
        });
    }

    #[gpui::test]
    fn accepting_a_completion_replaces_the_server_reported_range(cx: &mut TestAppContext) {
        let (mut cx, editor, spy) = editor_with_spy("select * from us", cx);
        editor.update_in(&mut cx, |editor, window, cx| {
            editor.set_keymap(EditorKeymap::Vim, cx);
            assert!(editor.vim_key(modalkit::crossterm::event::KeyCode::Char('i'), cx));
            editor.document.set_selection(16..16, false);
            editor.complete(&Complete, window, cx);
        });
        cx.run_until_parked();
        let (revision, request) = spy
            .read_with(&cx, |spy, _| spy.0.last().cloned())
            .expect("completion request raised");
        assert_eq!(request, SemanticRequestKind::Complete { cursor: 16 });
        editor.update(&mut cx, |editor, cx| {
            assert!(editor.apply_semantic_outcome(
                revision,
                SemanticOutcome::Completions {
                    replaced: sift_protocol::TextRange { start: 14, end: 16 },
                    candidates: vec![candidate("users")],
                },
                cx,
            ));
        });
        // Enter accepts the highlighted candidate without reopening it.
        editor.update_in(&mut cx, |editor, window, cx| {
            editor.newline(&Newline, window, cx);
        });
        cx.run_until_parked();
        editor.read_with(&cx, |editor, _| {
            assert_eq!(editor.document().text(), "select * from users");
            assert!(editor.semantic().completion().is_none());
            assert_eq!(editor.vim_mode(), VimMode::Insert);
        });
        let requests = spy.read_with(&cx, |spy, _| spy.0.clone());
        assert!(requests
            .iter()
            .all(|(_, request)| !matches!(request, SemanticRequestKind::AutoComplete { .. })));
    }

    #[gpui::test]
    fn completion_row_text_and_kind_share_one_vertical_track(cx: &mut TestAppContext) {
        let (mut cx, editor, spy) = editor_with_spy("select * from us", cx);
        editor.update_in(&mut cx, |editor, window, cx| {
            editor.document.set_selection(16..16, false);
            editor.complete(&Complete, window, cx);
        });
        cx.run_until_parked();
        let revision = spy
            .read_with(&cx, |spy, _| spy.0.last().map(|(revision, _)| *revision))
            .expect("completion request");
        let mut users = candidate("users");
        users.detail = Some("public".into());
        editor.update(&mut cx, |editor, cx| {
            assert!(editor.apply_semantic_outcome(
                revision,
                SemanticOutcome::Completions {
                    replaced: sift_protocol::TextRange { start: 14, end: 16 },
                    candidates: vec![users],
                },
                cx,
            ));
        });
        cx.run_until_parked();

        let row = cx.debug_bounds("completion-row-0").expect("completion row");
        for selector in [
            "completion-kind-0",
            "completion-label-0",
            "completion-metadata-0",
        ] {
            let child = cx.debug_bounds(selector).expect("completion row child");
            assert_eq!(child.top(), row.top(), "{selector} top");
            assert_eq!(child.bottom(), row.bottom(), "{selector} bottom");
        }
    }

    #[gpui::test]
    fn star_expansion_previews_then_applies_as_one_vim_undo(cx: &mut TestAppContext) {
        let source = "select u.* from users u";
        let (mut cx, editor, spy) = editor_with_spy(source, cx);
        editor.update_in(&mut cx, |editor, window, cx| {
            editor.set_keymap(EditorKeymap::Vim, cx);
            editor.document.set_selection(10..10, false);
            editor.expand_star(&ExpandStar, window, cx);
        });
        let (revision, request) = spy
            .read_with(&cx, |spy, _| spy.0.last().cloned())
            .expect("star expansion request raised");
        assert_eq!(request, SemanticRequestKind::ExpandStar { position: 10 });
        editor.update(&mut cx, |editor, cx| {
            assert!(editor.apply_semantic_outcome(
                revision,
                SemanticOutcome::StarExpansion(sift_protocol::StarExpansionPreview {
                    document_id: sift_protocol::SemanticDocumentId(
                        "00000000-0000-0000-0000-000000000000".parse().unwrap(),
                    ),
                    revision,
                    catalog_revision: sift_protocol::CatalogRevision(1),
                    range: sift_protocol::TextRange { start: 7, end: 10 },
                    replacement: "u.id, u.email".into(),
                    columns: vec!["id".into(), "email".into()],
                    kind: sift_protocol::StarExpansionKind::QualifiedRelation,
                    relation: "app.public.users".into(),
                    exact: true,
                }),
                cx,
            ));
        });
        editor.update_in(&mut cx, |editor, window, cx| {
            editor.indent(&Indent, window, cx);
        });
        editor.read_with(&cx, |editor, _| {
            assert_eq!(
                editor.document().text(),
                "select u.id, u.email from users u"
            );
        });
        editor.update_in(&mut cx, |editor, window, cx| {
            editor.vim_undo(&VimUndo, window, cx);
        });
        editor.read_with(&cx, |editor, _| {
            assert_eq!(editor.document().text(), source);
            assert_eq!(editor.keymap(), EditorKeymap::Vim);
        });
    }

    #[gpui::test]
    fn snippet_completion_has_tabstops_and_is_one_vim_undo(cx: &mut TestAppContext) {
        let source = "sel";
        let (mut cx, editor, spy) = editor_with_spy(source, cx);
        editor.update_in(&mut cx, |editor, window, cx| {
            editor.set_keymap(EditorKeymap::Vim, cx);
            editor.document.set_selection(3..3, false);
            editor.complete(&Complete, window, cx);
        });
        cx.run_until_parked();
        let (revision, request) = spy
            .read_with(&cx, |spy, _| spy.0.last().cloned())
            .expect("completion request raised");
        assert_eq!(request, SemanticRequestKind::Complete { cursor: 3 });
        editor.update(&mut cx, |editor, cx| {
            assert!(editor.apply_semantic_outcome(
                revision,
                SemanticOutcome::Completions {
                    replaced: sift_protocol::TextRange { start: 0, end: 3 },
                    candidates: vec![sift_protocol::completion::CompletionCandidate {
                        label: "sel".into(),
                        insert: "SELECT ${1:*} FROM ${2:table};$0".into(),
                        kind: sift_protocol::completion::CompletionKind::Snippet,
                        detail: Some("snippet".into()),
                        qualified_name: None,
                        score: 2_000,
                    }],
                },
                cx,
            ));
        });
        editor.update_in(&mut cx, |editor, window, cx| {
            editor.indent(&Indent, window, cx);
            assert_eq!(editor.document.selected_text(), "*");
            editor.indent(&Indent, window, cx);
            assert_eq!(editor.document.selected_text(), "table");
            editor.vim_undo(&VimUndo, window, cx);
        });
        editor.read_with(&cx, |editor, _| {
            assert_eq!(editor.document().text(), source);
            assert_eq!(editor.keymap(), EditorKeymap::Vim);
        });
    }

    #[gpui::test]
    fn server_edits_apply_back_to_front_over_the_whole_buffer(cx: &mut TestAppContext) {
        let (mut cx, editor, _) = editor_with_spy("select 1", cx);
        editor.update(&mut cx, |editor, cx| {
            let revision = editor.text_revision();
            assert!(editor.apply_semantic_outcome(
                revision,
                SemanticOutcome::Edits {
                    edits: vec![
                        sift_protocol::TextEdit {
                            range: sift_protocol::TextRange { start: 0, end: 6 },
                            new_text: "SELECT".into(),
                        },
                        sift_protocol::TextEdit {
                            range: sift_protocol::TextRange { start: 7, end: 8 },
                            new_text: "2".into(),
                        },
                    ],
                    warnings: Vec::new(),
                },
                cx,
            ));
        });
        editor.read_with(&cx, |editor, _| {
            assert_eq!(editor.document().text(), "SELECT 2");
        });
    }

    #[gpui::test]
    fn quick_fix_uses_the_diagnostic_under_the_caret(cx: &mut TestAppContext) {
        let (mut cx, editor, spy) = editor_with_spy("select * from usrs", cx);
        editor.update_in(&mut cx, |editor, window, cx| {
            let revision = editor.text_revision();
            editor.apply_semantic_outcome(
                revision,
                SemanticOutcome::Diagnostics {
                    diagnostics: vec![diagnostic(14, 18, vec!["create-table".into()])],
                    incomplete: false,
                },
                cx,
            );
            editor.document.set_selection(15..15, false);
            editor.apply_quick_fix(&ApplyQuickFix, window, cx);
        });
        cx.run_until_parked();
        let request = spy.read_with(&cx, |spy, _| spy.0.last().cloned());
        assert_eq!(
            request.map(|(_, request)| request),
            Some(SemanticRequestKind::QuickFix {
                fix_id: "create-table".into()
            })
        );

        // Outside the diagnostic there is nothing to prepare, and the editor
        // says so instead of issuing a request the server would reject.
        editor.update_in(&mut cx, |editor, window, cx| {
            editor.document.set_selection(0..0, false);
            editor.apply_quick_fix(&ApplyQuickFix, window, cx);
        });
        cx.run_until_parked();
        editor.read_with(&cx, |editor, _| {
            assert_eq!(
                editor.semantic().notice(),
                Some("No quick fix at the caret.")
            );
        });
        let last = spy.read_with(&cx, |spy, _| spy.0.last().cloned());
        assert_eq!(
            last.map(|(_, request)| request),
            Some(SemanticRequestKind::QuickFix {
                fix_id: "create-table".into()
            })
        );
    }

    #[gpui::test]
    fn editor_view_routes_input_and_editing_actions(cx: &mut TestAppContext) {
        let window = cx
            .update(|cx| {
                cx.open_window(Default::default(), |_window, cx| {
                    cx.new(|cx| QueryEditor::new(doc("select"), cx))
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

    #[gpui::test]
    fn find_bar_navigates_and_replaces_through_document_edits(cx: &mut TestAppContext) {
        let window = cx
            .update(|cx| {
                cx.open_window(Default::default(), |_window, cx| {
                    cx.new(|cx| QueryEditor::new(doc("one two one"), cx))
                })
            })
            .unwrap();
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        let editor = window.root(&mut cx).unwrap();
        editor.update_in(&mut cx, |editor, window, cx| {
            editor.open_find(&OpenFind, window, cx);
            editor
                .find_query
                .update(cx, |input, cx| input.set_text("one", cx));
            editor
                .replace_query
                .update(cx, |input, cx| input.set_text("three", cx));
            editor.find_next(&FindNext, window, cx);
        });
        cx.run_until_parked();
        assert!(cx.debug_bounds("editor-find-bar").is_some());
        assert_eq!(
            editor.read_with(&cx, |editor, _| editor.document.selection()),
            0..3
        );

        editor.update_in(&mut cx, |editor, window, cx| {
            editor.replace_next(&ReplaceNext, window, cx)
        });
        assert_eq!(
            editor.read_with(&cx, |editor, _| editor.document.text().to_owned()),
            "three two one"
        );
        editor.update_in(&mut cx, |editor, window, cx| {
            editor.replace_all(&ReplaceAll, window, cx);
            editor.close_find(&CloseFind, window, cx);
        });
        assert_eq!(
            editor.read_with(&cx, |editor, _| editor.document.text().to_owned()),
            "three two three"
        );
        assert!(editor.read_with(&cx, |editor, _| !editor.find_open));
    }

    #[gpui::test]
    fn large_editor_virtualizes_rows_and_scrolls(cx: &mut TestAppContext) {
        let text = (0..2_000)
            .map(|line| format!("select {line};"))
            .collect::<Vec<_>>()
            .join("\n");
        let window = cx
            .update(|cx| {
                cx.open_window(Default::default(), |_window, cx| {
                    cx.new(|cx| QueryEditor::new(doc(&text), cx))
                })
            })
            .unwrap();
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        let editor = window.root(&mut cx).unwrap();
        cx.run_until_parked();
        editor.read_with(&cx, |editor, _| {
            assert_eq!(editor.line_starts.len(), 2_000);
            assert!(editor.line_layouts.len() < 100);
        });

        let viewport = editor.read_with(&cx, |editor, _| editor.scroll_handle.bounds());
        cx.simulate_event(gpui::ScrollWheelEvent {
            position: viewport.center(),
            delta: gpui::ScrollDelta::Pixels(point(px(0.), px(-600.))),
            ..Default::default()
        });
        cx.run_until_parked();
        editor.read_with(&cx, |editor, _| {
            assert!(editor.scroll_handle.offset().y < px(0.));
            assert!(editor.visible_line_start > 0);
            assert!(editor.line_layouts.len() < 100);
        });
    }

    #[gpui::test]
    fn clicking_after_scroll_maps_viewport_to_the_visible_document_row(cx: &mut TestAppContext) {
        let text = (0..300)
            .map(|line| format!("select {line};"))
            .collect::<Vec<_>>()
            .join("\n");
        let window = cx
            .update(|cx| {
                cx.open_window(Default::default(), |_window, cx| {
                    cx.new(|cx| QueryEditor::new(doc(&text), cx))
                })
            })
            .unwrap();
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        let editor = window.root(&mut cx).unwrap();
        cx.run_until_parked();
        let viewport = editor.read_with(&cx, |editor, _| editor.scroll_handle.bounds());
        cx.simulate_event(gpui::ScrollWheelEvent {
            position: viewport.center(),
            delta: gpui::ScrollDelta::Pixels(point(px(0.), px(-720.))),
            ..Default::default()
        });
        cx.run_until_parked();
        let (position, expected_line) = editor.read_with(&cx, |editor, _| {
            let viewport = editor.scroll_handle.bounds();
            let y = viewport.top() + px(42.);
            let content_y =
                y - viewport.top() - editor.scroll_handle.offset().y - EDITOR_VERTICAL_INSET;
            let line = (f32::from(content_y) / f32::from(EDITOR_LINE_HEIGHT)) as usize;
            (
                point(
                    viewport.left() + EDITOR_GUTTER_WIDTH + EDITOR_TEXT_INSET + px(8.),
                    y,
                ),
                line + 1,
            )
        });
        cx.simulate_click(position, gpui::Modifiers::default());
        assert_eq!(
            editor.read_with(&cx, |editor, _| editor.cursor_position().0),
            expected_line
        );
    }

    #[gpui::test]
    fn clicking_the_scrolled_gutter_selects_the_visible_line(cx: &mut TestAppContext) {
        let text = (0..300)
            .map(|line| format!("select {line};"))
            .collect::<Vec<_>>()
            .join("\n");
        let expected_text = text.clone();
        let window = cx
            .update(|cx| {
                cx.open_window(Default::default(), |_window, cx| {
                    cx.new(|cx| QueryEditor::new(doc(&text), cx))
                })
            })
            .unwrap();
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        let editor = window.root(&mut cx).unwrap();
        cx.run_until_parked();
        let viewport = editor.read_with(&cx, |editor, _| editor.scroll_handle.bounds());
        cx.simulate_event(gpui::ScrollWheelEvent {
            position: viewport.center(),
            delta: gpui::ScrollDelta::Pixels(point(px(0.), px(-720.))),
            ..Default::default()
        });
        cx.run_until_parked();
        let (position, expected) = editor.read_with(&cx, |editor, _| {
            let viewport = editor.scroll_handle.bounds();
            let y = viewport.top() + px(42.);
            let content_y =
                y - viewport.top() - editor.scroll_handle.offset().y - EDITOR_VERTICAL_INSET;
            let line = (f32::from(content_y) / f32::from(EDITOR_LINE_HEIGHT)) as usize;
            let starts = line_starts(&expected_text);
            (
                point(viewport.left() + px(12.), y),
                starts[line]..starts.get(line + 1).copied().unwrap_or(expected_text.len()),
            )
        });
        cx.simulate_click(position, gpui::Modifiers::default());
        assert_eq!(
            editor.read_with(&cx, |editor, _| editor.document.selection()),
            expected
        );
    }

    #[gpui::test]
    fn clicking_a_blank_row_preserves_the_caret(cx: &mut TestAppContext) {
        let window = cx
            .update(|cx| {
                cx.open_window(Default::default(), |_window, cx| {
                    cx.new(|cx| QueryEditor::new(doc("select 1;\n   \nselect 3;"), cx))
                })
            })
            .unwrap();
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        let editor = window.root(&mut cx).unwrap();
        editor.update(&mut cx, |editor, _| {
            editor.document.set_selection(4..4, false)
        });
        cx.run_until_parked();
        let position = editor.read_with(&cx, |editor, _| {
            let viewport = editor.scroll_handle.bounds();
            point(
                viewport.left() + EDITOR_GUTTER_WIDTH + EDITOR_TEXT_INSET + px(8.),
                viewport.top() + EDITOR_VERTICAL_INSET + EDITOR_LINE_HEIGHT * 1.5,
            )
        });
        cx.simulate_click(position, gpui::Modifiers::default());
        assert_eq!(
            editor.read_with(&cx, |editor, _| editor.document.cursor()),
            4
        );
    }

    #[gpui::test]
    fn read_only_text_rejects_edits_but_accepts_feed_updates(cx: &mut TestAppContext) {
        let window = cx
            .update(|cx| {
                cx.open_window(Default::default(), |_window, cx| {
                    cx.new(|cx| QueryEditor::new(doc("first"), cx).read_only())
                })
            })
            .unwrap();
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        let editor = window.root(&mut cx).unwrap();
        cx.write_to_clipboard(ClipboardItem::new_string("mutated".into()));
        editor.update_in(&mut cx, |editor, window, cx| {
            editor.paste(&Paste, window, cx);
            assert_eq!(editor.document().text(), "first");
            editor.replace_text_from_owner("second", cx);
            assert_eq!(editor.document().text(), "second");
        });
    }

    #[gpui::test]
    fn owner_replacement_resyncs_vim_buffer_before_next_edit(cx: &mut TestAppContext) {
        let window = cx
            .update(|cx| {
                cx.open_window(Default::default(), |_window, cx| {
                    cx.new(|cx| QueryEditor::new(doc("one\ntwo\nthree\nfour"), cx))
                })
            })
            .unwrap();
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        let editor = window.root(&mut cx).unwrap();
        editor.update_in(&mut cx, |editor, window, cx| {
            editor.toggle_keymap(cx);
            let old_text = editor.document().text().to_owned();
            let old_end = old_text.len();
            editor.vim.as_mut().unwrap().set_cursor(&old_text, old_end);

            editor.replace_text_from_owner("select 1;\n", cx);
            editor.replace_text_in_range(None, "i", window, cx);
            editor.replace_text_in_range(None, "x", window, cx);
        });
        editor.read_with(&cx, |editor, _| {
            assert_eq!(editor.document().text(), "select 1;\nx");
            assert_eq!(editor.keymap(), EditorKeymap::Vim);
        });
    }

    #[gpui::test]
    fn clicking_past_text_moves_to_line_end_and_below_document_is_ignored(cx: &mut TestAppContext) {
        let window = cx
            .update(|cx| {
                cx.open_window(Default::default(), |_window, cx| {
                    cx.new(|cx| QueryEditor::new(doc("select 1;\nselect 2;"), cx))
                })
            })
            .unwrap();
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        let editor = window.root(&mut cx).unwrap();
        editor.update(&mut cx, |editor, _| {
            editor.document.set_selection(4..4, false)
        });
        cx.run_until_parked();
        let (past_text, below_document) = editor.read_with(&cx, |editor, _| {
            let viewport = editor.scroll_handle.bounds();
            (
                point(
                    viewport.right() - px(8.),
                    viewport.top() + EDITOR_VERTICAL_INSET + EDITOR_LINE_HEIGHT * 0.5,
                ),
                point(
                    viewport.left() + EDITOR_GUTTER_WIDTH + EDITOR_TEXT_INSET + px(8.),
                    viewport.top() + EDITOR_VERTICAL_INSET + EDITOR_LINE_HEIGHT * 3.5,
                ),
            )
        });

        cx.simulate_click(past_text, gpui::Modifiers::default());
        assert_eq!(
            editor.read_with(&cx, |editor, _| editor.document.cursor()),
            "select 1;".len()
        );
        cx.simulate_click(below_document, gpui::Modifiers::default());

        assert_eq!(
            editor.read_with(&cx, |editor, _| editor.document.cursor()),
            "select 1;".len()
        );
    }

    #[gpui::test]
    fn moving_down_reveals_the_caret_beyond_the_initial_viewport(cx: &mut TestAppContext) {
        let text = (0..300)
            .map(|line| format!("select {line};"))
            .collect::<Vec<_>>()
            .join("\n");
        let window = cx
            .update(|cx| {
                cx.open_window(Default::default(), |_window, cx| {
                    cx.new(|cx| QueryEditor::new(doc(&text), cx))
                })
            })
            .unwrap();
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        let editor = window.root(&mut cx).unwrap();
        cx.run_until_parked();
        editor.update_in(&mut cx, |editor, window, cx| {
            editor.document.set_selection(0..0, false);
            editor.selection_changed(cx);
            for _ in 0..120 {
                editor.move_down(&MoveDown, window, cx);
            }
        });
        editor.read_with(&cx, |editor, _| {
            assert_eq!(editor.cursor_position().0, 121);
            assert!(editor.scroll_handle.offset().y < px(0.));
        });
    }

    #[gpui::test]
    fn vim_normal_mode_routes_commands_without_inserting_them(cx: &mut TestAppContext) {
        let window = cx
            .update(|cx| {
                cx.open_window(Default::default(), |_window, cx| {
                    cx.new(|cx| QueryEditor::new(doc("abc"), cx))
                })
            })
            .unwrap();
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        let editor = window.root(&mut cx).unwrap();
        editor.update_in(&mut cx, |editor, window, cx| {
            editor.toggle_keymap(cx);
            editor.replace_text_in_range(None, "h", window, cx);
            assert_eq!(editor.document.cursor(), 2);
            assert_eq!(editor.document.text(), "abc");
            editor.replace_text_in_range(None, "i", window, cx);
            editor.replace_text_in_range(None, "Z", window, cx);
        });
        editor.read_with(&cx, |editor, _| {
            assert_eq!(editor.keymap(), EditorKeymap::Vim);
            assert_eq!(editor.vim_mode(), VimMode::Insert);
            assert_eq!(editor.document.text(), "abZc");
        });
    }

    #[gpui::test]
    fn vim_braces_move_between_blank_line_paragraph_boundaries(cx: &mut TestAppContext) {
        let window = cx
            .update(|cx| {
                cx.open_window(Default::default(), |_window, cx| {
                    cx.new(|cx| QueryEditor::new(doc("select 1;\n\nselect 2;\n\nselect 3;"), cx))
                })
            })
            .unwrap();
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        let editor = window.root(&mut cx).unwrap();
        editor.update_in(&mut cx, |editor, window, cx| {
            editor.document.set_selection(0..0, false);
            editor.toggle_keymap(cx);
            editor.replace_text_in_range(None, "}", window, cx);
            assert_eq!(editor.cursor_position(), (2, 1));
            editor.replace_text_in_range(None, "}", window, cx);
            assert_eq!(editor.cursor_position(), (4, 1));
            editor.replace_text_in_range(None, "{", window, cx);
            assert_eq!(editor.cursor_position(), (2, 1));
        });
    }

    #[gpui::test]
    fn cursor_blink_pauses_after_input_and_resumes(cx: &mut TestAppContext) {
        let blink = cx.update(|cx| cx.new(|_| CursorBlink::new()));
        blink.update(cx, CursorBlink::enable);
        assert!(blink.read_with(cx, |blink, _| blink.visible));
        cx.executor().advance_clock(CURSOR_BLINK_INTERVAL);
        cx.run_until_parked();
        assert!(!blink.read_with(cx, |blink, _| blink.visible));
        blink.update(cx, CursorBlink::pause);
        assert!(blink.read_with(cx, |blink, _| blink.visible));
        cx.executor().advance_clock(CURSOR_BLINK_PAUSE);
        cx.run_until_parked();
        assert!(!blink.read_with(cx, |blink, _| blink.visible));
    }

    #[gpui::test]
    fn cursor_motion_reuses_cached_visible_line_layouts(cx: &mut TestAppContext) {
        let text = (0..200)
            .map(|line| format!("select {line};"))
            .collect::<Vec<_>>()
            .join("\n");
        let window = cx
            .update(|cx| {
                cx.open_window(Default::default(), |_window, cx| {
                    cx.new(|cx| QueryEditor::new(doc(&text), cx))
                })
            })
            .unwrap();
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        let editor = window.root(&mut cx).unwrap();
        cx.run_until_parked();
        let (cached_before, revision_before) = editor.read_with(&cx, |editor, _| {
            (editor.line_cache.borrow().lines.len(), editor.revision)
        });
        editor.update_in(&mut cx, |editor, window, cx| {
            editor.document.set_selection(0..0, false);
            editor.move_down(&MoveDown, window, cx);
        });
        cx.run_until_parked();
        editor.read_with(&cx, |editor, _| {
            assert_eq!(editor.revision, revision_before);
            assert_eq!(editor.line_cache.borrow().lines.len(), cached_before);
        });
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
    fn incremental_line_index_tracks_unicode_and_structural_edits() {
        let mut document = doc("αβ\nxyz\nlast");
        assert_eq!(&*document.line_starts(), &[0, 5, 9]);
        assert_eq!(document.line_char_starts, vec![0, 3, 7]);

        document.replace_range(0..0, "!");
        assert_eq!(document.text(), "!αβ\nxyz\nlast");
        assert_eq!(&*document.line_starts(), &[0, 6, 10]);
        assert_eq!(document.line_char_starts, vec![0, 4, 8]);

        document.replace_range(6..9, "x\ny");
        assert_eq!(document.text(), "!αβ\nx\ny\nlast");
        assert_eq!(&*document.line_starts(), &[0, 6, 8, 10]);
        assert_eq!(document.line_char_starts, vec![0, 4, 6, 8]);
        assert_eq!(document.replica.text(), document.text());
    }

    #[test]
    fn room_snapshot_preserves_lineage_for_the_next_native_update() {
        let seed = TextReplica::new(41).unwrap();
        seed.insert(0, "select 1").unwrap();
        let snapshot = seed.export_snapshot().unwrap();
        let mut document = QueryDocument::from_room_snapshot(&snapshot).unwrap();

        document.insert("0");
        let update = document.take_room_update().unwrap();
        let receiver = TextReplica::from_snapshot(42, &snapshot).unwrap();
        receiver.import(&update).unwrap();

        assert_eq!(receiver.text(), "select 10");
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
    fn cursor_position_is_one_based_and_unicode_aware() {
        let mut document = doc("αβ\nxyz");
        document.set_selection(0..0, false);
        assert_eq!(document.cursor_position(), (1, 1));
        document.set_selection(4..4, false);
        assert_eq!(document.cursor_position(), (1, 3));
        document.set_selection(6..6, false);
        assert_eq!(document.cursor_position(), (2, 2));
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
#[test]
fn sql_presentation_runs_cover_text_and_classify_keywords() {
    let theme = Theme::dark();
    let text = "select 42 -- rows";
    let runs = sql_text_runs(text, gpui::font("monospace"), theme);
    assert_eq!(runs.iter().map(|run| run.len).sum::<usize>(), text.len());
    assert_eq!(runs[0].color, theme.colors.syntax_keyword);
    assert!(runs
        .iter()
        .any(|run| run.color == theme.colors.syntax_number));
    assert!(runs
        .iter()
        .any(|run| run.color == theme.colors.syntax_comment));
}

#[test]
fn diff_presentation_keeps_git_markers_and_payload_syntax() {
    let theme = Theme::dark();
    let text = "+select 42";
    let runs = diff_text_runs(text, gpui::font("monospace"), theme, EditorLanguage::Sql);
    assert_eq!(runs.iter().map(|run| run.len).sum::<usize>(), text.len());
    assert_eq!(runs[0].color, theme.colors.success);
    assert!(runs
        .iter()
        .any(|run| run.color == theme.colors.syntax_keyword));
    assert!(runs
        .iter()
        .any(|run| run.color == theme.colors.syntax_number));

    let header = diff_text_runs(
        "@@ -1,1 +1,1 @@",
        gpui::font("monospace"),
        theme,
        EditorLanguage::PlainText,
    );
    assert_eq!(header[0].color, theme.colors.accent);
    assert_eq!(header[0].background_color, Some(theme.colors.accent_muted));
}

#[test]
fn toml_presentation_and_diagnostics_are_lightweight_and_source_safe() {
    let theme = Theme::dark();
    let text = "name = \"demo\" # instance";
    let runs = toml_text_runs(text, gpui::font("monospace"), theme);
    assert_eq!(runs.iter().map(|run| run.len).sum::<usize>(), text.len());
    assert_eq!(runs[0].color, theme.colors.syntax_keyword);
    assert!(runs
        .iter()
        .any(|run| run.color == theme.colors.syntax_comment));
    assert!(
        toml_diagnostic("name = \"unterminated-secret").is_some_and(|message| {
            message.contains("line 1") && !message.contains("unterminated-secret")
        })
    );
    assert!(toml_diagnostic("name = \"demo\"").is_none());
}

#[test]
fn json_presentation_classifies_keys_values_and_literals() {
    let theme = Theme::dark();
    let text = r#"{"name": "demo", "enabled": true, "count": -2.5, "empty": null}"#;
    let runs = json_text_runs(text, gpui::font("monospace"), theme);
    assert_eq!(runs.iter().map(|run| run.len).sum::<usize>(), text.len());
    assert!(runs
        .iter()
        .any(|run| run.color == theme.colors.syntax_string));
    assert!(runs
        .iter()
        .any(|run| run.color == theme.colors.syntax_number));
    assert!(
        runs.iter()
            .filter(|run| run.color == theme.colors.syntax_keyword)
            .count()
            >= 6
    );
}

#[test]
fn json_formatting_is_pretty_stable_and_source_safe_on_errors() {
    let formatted = format_json_document(r#"{"z":0,"items":[1,true],"name":"demo"}"#).unwrap();
    assert_eq!(
        formatted,
        "{\n  \"z\": 0,\n  \"items\": [\n    1,\n    true\n  ],\n  \"name\": \"demo\"\n}\n"
    );
    assert_eq!(format_json_document(&formatted).unwrap(), formatted);
    assert!(format_json_document("{\"secret\": nope}")
        .unwrap_err()
        .contains("line 1"));
    assert!(!format_json_document("{\"secret\": nope}")
        .unwrap_err()
        .contains("secret"));
}

#[test]
fn json_newlines_follow_two_space_nesting_and_split_paired_delimiters() {
    assert_eq!(json_newline("{", 1), ("\n  ".into(), 0));
    assert_eq!(json_newline("{\n  \"items\": [", 14), ("\n    ".into(), 0));
    assert_eq!(
        json_newline("{\n  \"items\": []\n}", 14),
        ("\n    \n  ".into(), 3)
    );
}

#[test]
fn keymaps_json_schema_reports_precise_syntax_and_property_errors() {
    let schema = JsonSchema::keymaps(["workspace.focus-editor".into()]);
    let syntax = json_schema_diagnostics("{\n  \"version\": nope\n}", Some(&schema));
    assert_eq!(syntax.len(), 1);
    assert_eq!(syntax[0].code, "json.syntax");
    assert_eq!(syntax[0].range.start, 15);

    let source = r#"{
  "version": 2,
  "bindings": {
    "workspace.unknown": 4
  },
  "extra": true
}"#;
    let diagnostics = json_schema_diagnostics(source, Some(&schema));
    assert!(diagnostics
        .iter()
        .any(|diagnostic| diagnostic.code == "json.schema.version"));
    assert!(diagnostics
        .iter()
        .any(|diagnostic| diagnostic.code == "json.schema.command"));
    assert!(diagnostics
        .iter()
        .any(|diagnostic| diagnostic.code == "json.schema.binding"));
    assert!(diagnostics
        .iter()
        .any(|diagnostic| diagnostic.code == "json.schema.unknown-property"));
    assert!(diagnostics
        .iter()
        .all(|diagnostic| diagnostic.range.start <= diagnostic.range.end));
}

#[test]
fn keymaps_json_completion_follows_the_current_object_schema() {
    let schema = JsonSchema::keymaps([
        "workspace.focus-editor".into(),
        "workspace.focus-results".into(),
    ]);
    let source = "{\n  \"ver\"\n}";
    let cursor = source.find("ver").unwrap() + 3;
    let (replace, candidates) = json_schema_completions(source, cursor, &schema);
    assert_eq!(&source[replace], "ver");
    assert!(candidates
        .iter()
        .any(|candidate| candidate.label == "version"));

    let source = "{\n  \"version\": 1,\n  \"bindings\": {\n    \"work\"\n  }\n}";
    let cursor = source.find("work").unwrap() + 4;
    let (replace, candidates) = json_schema_completions(source, cursor, &schema);
    assert_eq!(&source[replace], "work");
    assert_eq!(candidates.len(), 2);
    assert!(candidates
        .iter()
        .any(|candidate| candidate.label == "workspace.focus-editor"));
}
