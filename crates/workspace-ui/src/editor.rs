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
use sift_ui::{ActiveTheme, Theme};

mod vim;
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
        Self {
            replica,
            text,
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
        Ok(Self {
            replica,
            text,
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
        let before = &self.text[..self.cursor()];
        let line_start = before.rfind('\n').map_or(0, |index| index + 1);
        (
            before.matches('\n').count() + 1,
            before[line_start..].chars().count() + 1,
        )
    }

    /// Apply a text splice against the replica and refresh the cached text. Only
    /// touches CRDT state; selection and history are the caller's concern.
    fn splice(&mut self, start: usize, end: usize, new_text: &str) {
        let since = self.replica.version_vector();
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
            line: self.text[..start]
                .bytes()
                .filter(|byte| *byte == b'\n')
                .count(),
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
            line: self.text[..start]
                .bytes()
                .filter(|byte| *byte == b'\n')
                .count(),
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
            line: self.text[..start]
                .bytes()
                .filter(|byte| *byte == b'\n')
                .count(),
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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditorLanguage {
    Sql,
    Toml,
    PlainText,
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
        Redo,
        ExitInsertMode
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
    revision: u64,
    line_starts: Arc<Vec<usize>>,
    lines: HashMap<usize, ShapedLine>,
}

/// Multi-line GPUI editor over a [`QueryDocument`]. Character and IME input flow
/// through the platform via [`EntityInputHandler`]; editing commands arrive as
/// typed actions the workspace keymap binds under the `SiftEditor` context.
pub struct QueryEditor {
    focus_handle: FocusHandle,
    document: QueryDocument,
    language: EditorLanguage,
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
}

impl QueryEditor {
    pub fn new(document: QueryDocument, cx: &mut Context<Self>) -> Self {
        let cursor_blink = cx.new(|_| CursorBlink::new());
        cx.observe(&cursor_blink, |_, _, cx| cx.notify()).detach();
        let vim_store = shared_vim_store(cx);
        Self {
            focus_handle: cx.focus_handle(),
            document,
            language: EditorLanguage::Sql,
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
        }
    }

    pub fn with_language(mut self, language: EditorLanguage) -> Self {
        self.language = language;
        self
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
        self.marked_range = None;
        self.revision = self.revision.wrapping_add(1);
        self.line_cache.borrow_mut().lines.clear();
        cx.notify();
    }

    pub fn keymap(&self) -> EditorKeymap {
        self.keymap
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
        self.revision = self.revision.wrapping_add(1);
        self.line_cache.borrow_mut().lines.clear();
        self.marked_range = None;
        self.reveal_cursor();
        cx.emit(EditorEvent::CursorChanged);
        cx.notify();
    }

    pub fn cursor_position(&self) -> (usize, usize) {
        self.document.cursor_position()
    }

    fn edited(&mut self, cx: &mut Context<Self>) {
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
        let line = self.document.text()[..self.document.cursor()]
            .bytes()
            .filter(|byte| *byte == b'\n')
            .count();
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
        let line_count = self.document.text().split('\n').count().max(1);
        let content_height = EDITOR_VERTICAL_INSET * 2. + EDITOR_LINE_HEIGHT * line_count as f32;
        let max_scroll = (content_height - viewport.size.height).max(px(0.));
        offset.y = offset.y.min(px(0.)).max(-max_scroll);
        self.scroll_handle.set_offset(offset);
    }

    fn apply_vim_snapshot(&mut self, snapshot: VimSnapshot, cx: &mut Context<Self>) {
        let open_command_palette = snapshot.open_command_palette;
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
        let Some(vim) = self.vim.as_mut() else {
            return false;
        };
        let rows = (f32::from(self.scroll_handle.bounds().size.height)
            / f32::from(EDITOR_LINE_HEIGHT)) as usize;
        vim.set_viewport_rows(rows);
        let snapshot = vim.input_key(code);
        self.apply_vim_snapshot(snapshot, cx);
        true
    }

    fn vim_text(&mut self, text: &str, cx: &mut Context<Self>) -> bool {
        let Some(vim) = self.vim.as_mut() else {
            return false;
        };
        let snapshot = vim.input_text(text);
        self.apply_vim_snapshot(snapshot, cx);
        true
    }

    fn backspace(&mut self, _: &Backspace, _: &mut Window, cx: &mut Context<Self>) {
        if self.read_only {
            return;
        }
        if self.vim_key(modalkit::crossterm::event::KeyCode::Backspace, cx) {
            return;
        }
        self.document.backspace();
        self.edited(cx);
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
        if self.vim_key(modalkit::crossterm::event::KeyCode::Enter, cx) {
            return;
        }
        self.document.insert("\n");
        self.edited(cx);
    }

    fn indent(&mut self, _: &Indent, _: &mut Window, cx: &mut Context<Self>) {
        if self.read_only {
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
        if self.vim_key(modalkit::crossterm::event::KeyCode::Up, cx) {
            return;
        }
        self.document.move_up(false);
        self.selection_changed(cx);
    }

    fn move_down(&mut self, _: &MoveDown, _: &mut Window, cx: &mut Context<Self>) {
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

    fn redo(&mut self, _: &Redo, _: &mut Window, cx: &mut Context<Self>) {
        if self.read_only {
            return;
        }
        if self.document.redo() {
            self.edited(cx);
        }
    }

    fn exit_insert_mode(&mut self, _: &ExitInsertMode, _: &mut Window, cx: &mut Context<Self>) {
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
        let runs = match self.language {
            EditorLanguage::Sql => sql_text_runs(line_text, style.font(), theme),
            EditorLanguage::Toml => toml_text_runs(line_text, style.font(), theme),
            EditorLanguage::PlainText => plain_text_runs(line_text, style.font(), theme),
        };
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
        self.selection_changed(cx);
    }

    fn replace_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.read_only {
            return;
        }
        if self.vim_text(new_text, cx) {
            return;
        }
        let range = range_utf16
            .as_ref()
            .map(|range| self.range_from_utf16(range))
            .or_else(|| self.marked_range.clone());
        match range {
            Some(range) => self.document.replace_range(range, new_text),
            None => self.document.insert(new_text),
        }
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
        let focused = self.focus_handle.is_focused(window);
        let blink_enabled = self.cursor_blink.read(cx).enabled;
        if focused != blink_enabled {
            if focused {
                self.cursor_blink.update(cx, CursorBlink::enable);
            } else {
                self.cursor_blink.update(cx, CursorBlink::disable);
            }
        }
        div()
            .id("sift-query-editor")
            .key_context("SiftEditor")
            .role(Role::TextInput)
            .aria_label(match self.language {
                EditorLanguage::Sql => "SQL query editor",
                EditorLanguage::Toml => "TOML configuration editor",
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
            // Clicking the editor focuses it directly (synchronously), so the
            // SiftEditor key context is active and editing keys route.
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|editor, event: &gpui::MouseDownEvent, window, cx| {
                    editor.focus_handle.clone().focus(window, cx);
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
            .on_action(cx.listener(Self::redo))
            .on_action(cx.listener(Self::exit_insert_mode))
            .on_action(cx.listener(Self::execute_statement))
            .on_action(cx.listener(Self::execute_document))
            .children(
                (self.language == EditorLanguage::Toml)
                    .then(|| toml_diagnostic(self.document.text()))
                    .flatten()
                    .map(|diagnostic| {
                        div()
                            .px_3()
                            .py_1()
                            .border_b_1()
                            .border_color(colors.danger)
                            .bg(colors.danger_muted)
                            .text_xs()
                            .text_color(colors.danger)
                            .child(diagnostic)
                    }),
            )
            .child(
                div()
                    .id("editor-scroll")
                    .flex_1()
                    .min_h_0()
                    .bg(colors.background)
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll_handle)
                    .child(QueryEditorElement {
                        editor: cx.entity(),
                    }),
            )
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
    active_line: Option<PaintQuad>,
    selections: Vec<PaintQuad>,
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
        let line_count = self.editor.read(cx).document.text().split('\n').count();
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
        let block_cursor = editor.keymap == EditorKeymap::Vim && editor.vim_mode == VimMode::Normal;
        let cursor_visible = editor.cursor_blink.read(cx).visible;
        let editor_focused = editor.focus_handle.is_focused(window);
        let viewport = editor.scroll_handle.bounds();
        let style = window.text_style();
        let font_size = style.font_size.to_pixels(window.rem_size());
        let line_height = EDITOR_LINE_HEIGHT;

        let mut lines = Vec::new();
        let mut line_numbers = Vec::new();
        let line_starts = {
            let mut cache = editor.line_cache.borrow_mut();
            if cache.revision != editor.revision || cache.line_starts.is_empty() {
                cache.revision = editor.revision;
                let mut line_starts = vec![0];
                line_starts.extend(
                    text.match_indices('\n')
                        .map(|(offset, _)| offset.saturating_add(1)),
                );
                cache.line_starts = Arc::new(line_starts);
            }
            cache.line_starts.clone()
        };
        let mut selections = Vec::new();
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
                let runs = match language {
                    EditorLanguage::Sql => sql_text_runs(line, style.font(), theme),
                    EditorLanguage::Toml => toml_text_runs(line, style.font(), theme),
                    EditorLanguage::PlainText => plain_text_runs(line, style.font(), theme),
                };
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
                theme.colors.muted_text
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
            active_line: active_line_quad,
            selections,
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
        for selection in prepaint.selections.drain(..) {
            window.paint_quad(selection);
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
