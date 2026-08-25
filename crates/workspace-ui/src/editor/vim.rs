//! Adapter between ModalKit's Vim state machine and Sift's collaborative
//! document. ModalKit owns key interpretation and transient Vim state; the
//! Loro-backed `QueryDocument` remains the canonical text model.

use modalkit::{
    actions::{Action, Editable, EditorAction, HistoryAction, Jumpable, Searchable},
    crossterm::event::{KeyCode, KeyEvent, KeyModifiers},
    editing::{
        application::EmptyInfo,
        buffer::{CursorGroupId, EditBuffer},
        context::Resolve,
        cursor::Cursor,
        store::{RegisterCell, RegisterPutFlags, SharedStore},
    },
    env::vim::{
        keybindings::{default_vim_keys, VimMachine},
        VimMode as ModalVimMode,
    },
    key::TerminalKey,
    keybindings::BindingMachine,
    prelude::{EditTarget, MoveDir1D, MoveType, Register, TargetShape, ViewportContext},
};

use super::VimMode;

pub(super) struct VimSnapshot {
    pub text: Option<String>,
    pub cursor: (usize, usize),
    pub selection: Option<((usize, usize), (usize, usize))>,
    pub mode: VimMode,
    pub entered: String,
    pub open_command_palette: bool,
    /// New unnamed-register text produced by this command. The GPUI editor
    /// publishes it to the platform clipboard after applying the snapshot.
    pub clipboard: Option<String>,
}

/// A complete Vim keybinding machine plus ModalKit's editing buffer. Sift
/// mirrors each completed command back into its CRDT as a minimal splice.
pub(super) struct VimEngine {
    bindings: VimMachine<TerminalKey>,
    buffer: EditBuffer<EmptyInfo>,
    cursor_group: CursorGroupId,
    store: SharedStore<EmptyInfo>,
    viewport: ViewportContext<Cursor>,
    entered: String,
    /// Cursor after entering Insert. ModalKit moves left on an immediate Esc;
    /// real editor UX should keep an empty insert session position-stable.
    empty_insert_origin: Option<Cursor>,
}

impl VimEngine {
    #[cfg(test)]
    pub fn new(text: &str, cursor: usize) -> Self {
        Self::with_store(
            text,
            cursor,
            modalkit::editing::store::Store::default().shared(),
        )
    }

    pub fn with_store(text: &str, cursor: usize, store: SharedStore<EmptyInfo>) -> Self {
        // ModalKit buffers always retain a final newline. Add a dedicated
        // sentinel newline so a user-authored trailing newline is preserved.
        let modal_text = format!("{text}\n");
        let mut buffer = EditBuffer::from_str("sift-editor".into(), &modal_text);
        let cursor_group = buffer.create_group();
        buffer.set_leader(cursor_group, cursor_from_byte(text, cursor));
        Self {
            bindings: default_vim_keys(),
            buffer,
            cursor_group,
            store,
            viewport: ViewportContext::default(),
            entered: String::new(),
            empty_insert_origin: None,
        }
    }

    pub fn set_viewport_rows(&mut self, rows: usize) {
        self.viewport.dimensions.1 = rows.max(1);
    }

    pub fn set_cursor(&mut self, text: &str, cursor: usize) {
        self.buffer
            .set_leader(self.cursor_group, cursor_from_byte(text, cursor));
    }

    /// Make platform clipboard text the unnamed Vim register. A trailing
    /// newline is the portable representation of a linewise yank.
    pub fn set_clipboard(&mut self, text: &str) {
        let shape = if text.ends_with('\n') {
            TargetShape::LineWise
        } else {
            TargetShape::CharWise
        };
        let mut store = self
            .store
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let _ = store.registers.put(
            &Register::Unnamed,
            RegisterCell::from((shape, text)),
            RegisterPutFlags::NONE,
        );
    }

    pub fn input_text(&mut self, text: &str) -> VimSnapshot {
        let mut text_changed = false;
        let mut clipboard_changed = false;
        let mut open_command_palette = false;
        for character in text.chars() {
            if character == ':' && self.bindings.mode() == ModalVimMode::Normal {
                self.entered.clear();
                open_command_palette = true;
                break;
            }
            let (changed, register_changed) =
                self.input_key_event(KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE));
            text_changed |= changed;
            clipboard_changed |= register_changed;
        }
        self.snapshot(text_changed, open_command_palette, clipboard_changed)
    }

    pub fn input_key(&mut self, code: KeyCode) -> VimSnapshot {
        if code == KeyCode::Char(':') && self.bindings.mode() == ModalVimMode::Normal {
            self.entered.clear();
            return self.snapshot(false, true, false);
        }
        let (text_changed, clipboard_changed) =
            self.input_key_event(KeyEvent::new(code, KeyModifiers::NONE));
        self.snapshot(text_changed, false, clipboard_changed)
    }

    fn input_key_event(&mut self, event: KeyEvent) -> (bool, bool) {
        let mut text_changed = false;
        let register_before = self.unnamed_register();
        let mode_before = self.bindings.mode();
        if mode_before == ModalVimMode::Insert && event.code != KeyCode::Esc {
            self.empty_insert_origin = None;
        }
        let entered_key = entered_key(&event);
        if let Some(key) = entered_key.as_deref() {
            if self.bindings.mode() != ModalVimMode::Insert {
                self.entered.push_str(key);
            }
        } else {
            self.entered.clear();
        }
        self.bindings.input_key(event.into());
        let mut action_completed = false;
        let store = self.store.clone();
        let mut store = store
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        while let Some((action, context)) = self.bindings.pop() {
            action_completed = true;
            match action {
                Action::Editor(action) => {
                    text_changed |= editor_action_changes_text(&action, &context);
                    if !self.apply_compatibility_motion(&action, &context) {
                        let editor_context = (self.cursor_group, &self.viewport, &context);
                        let _ = self
                            .buffer
                            .editor_command(&action, &editor_context, &mut store);
                    }
                }
                Action::Jump(list, direction, count) => {
                    let count = context.resolve(&count);
                    let editor_context = (self.cursor_group, &self.viewport, &context);
                    let _ = self.buffer.jump(list, direction, count, &editor_context);
                }
                Action::Search(direction, count) => {
                    let editor_context = (self.cursor_group, &self.viewport, &context);
                    let _ = self
                        .buffer
                        .search(direction, count, &editor_context, &mut store);
                }
                Action::Repeat(sequence) => self.bindings.repeat(sequence, Some(context)),
                _ => {}
            }
        }
        if action_completed && self.bindings.mode() != ModalVimMode::OperationPending {
            self.entered.clear();
        }
        let mode_after = self.bindings.mode();
        if mode_before != ModalVimMode::Insert && mode_after == ModalVimMode::Insert {
            self.empty_insert_origin = Some(self.buffer.get_leader(self.cursor_group));
        } else if mode_before == ModalVimMode::Insert && mode_after != ModalVimMode::Insert {
            if let Some(origin) = self.empty_insert_origin.take() {
                self.buffer.set_leader(self.cursor_group, origin);
            }
        }
        drop(store);
        (text_changed, self.unnamed_register() != register_before)
    }

    /// ModalKit 0.0.25 maps these Vim keys but leaves paragraph movement as a
    /// backend TODO. Keep the workaround at the adapter boundary so it can be
    /// removed as soon as the package implements the motion itself.
    fn apply_compatibility_motion(
        &mut self,
        action: &EditorAction,
        context: &modalkit::editing::context::EditContext,
    ) -> bool {
        let EditorAction::Edit(operation, EditTarget::Motion(movement, count)) = action else {
            return false;
        };
        if context.resolve(operation) != modalkit::actions::EditAction::Motion {
            return false;
        }
        let MoveType::ParagraphBegin(direction) = movement else {
            return false;
        };
        let cursor = self.buffer.get_leader(self.cursor_group);
        let target = paragraph_boundary(
            &self.buffer.get_text(),
            cursor.y,
            *direction,
            context.resolve(count),
        );
        self.buffer
            .set_leader(self.cursor_group, Cursor::new(target, 0));
        true
    }

    fn unnamed_register(&self) -> String {
        let store = self
            .store
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        store
            .registers
            .get(&Register::Unnamed)
            .map(|cell| cell.value.to_string())
            .unwrap_or_default()
    }

    fn snapshot(
        &mut self,
        text_changed: bool,
        open_command_palette: bool,
        clipboard_changed: bool,
    ) -> VimSnapshot {
        let text = text_changed.then(|| {
            let mut text = self.buffer.get_text();
            // Remove only the sentinel newline introduced in `new`.
            text.pop();
            text
        });
        let cursor = self.buffer.get_leader(self.cursor_group);
        let cursor = (cursor.y, cursor.x);
        let selection = self
            .buffer
            .get_leader_selection(self.cursor_group)
            .map(|(start, end, _)| ((start.y, start.x), (end.y, end.x)));
        let mode = match self.bindings.mode() {
            ModalVimMode::Insert => VimMode::Insert,
            ModalVimMode::Visual => VimMode::Visual,
            ModalVimMode::Select => VimMode::Select,
            ModalVimMode::OperationPending => VimMode::OperatorPending,
            ModalVimMode::Command => VimMode::Command,
            _ => VimMode::Normal,
        };
        VimSnapshot {
            text,
            cursor,
            selection,
            mode,
            entered: self.entered.clone(),
            open_command_palette,
            clipboard: clipboard_changed.then(|| self.unnamed_register()),
        }
    }
}

fn entered_key(event: &KeyEvent) -> Option<String> {
    match event.code {
        KeyCode::Char(character) if !event.modifiers.contains(KeyModifiers::CONTROL) => {
            Some(character.to_string())
        }
        _ => None,
    }
}

fn paragraph_boundary(text: &str, row: usize, direction: MoveDir1D, count: usize) -> usize {
    let lines = text.split('\n').collect::<Vec<_>>();
    let last_row = lines.len().saturating_sub(2);
    let mut remaining = count.max(1);
    let mut found_non_empty = false;
    match direction {
        MoveDir1D::Next => {
            for (candidate, line) in lines
                .iter()
                .enumerate()
                .take(last_row + 1)
                .skip(row.min(last_row))
            {
                if found_non_empty && line.is_empty() {
                    if remaining == 1 {
                        return candidate;
                    }
                    remaining -= 1;
                    found_non_empty = false;
                }
                found_non_empty |= !line.is_empty();
            }
            last_row
        }
        MoveDir1D::Previous => {
            for candidate in (0..=row.min(last_row)).rev() {
                let empty = lines[candidate].is_empty();
                if found_non_empty && empty {
                    if remaining == 1 {
                        return candidate;
                    }
                    remaining -= 1;
                    found_non_empty = false;
                }
                found_non_empty |= !empty;
            }
            0
        }
    }
}

fn editor_action_changes_text(
    action: &EditorAction,
    context: &modalkit::editing::context::EditContext,
) -> bool {
    match action {
        EditorAction::Edit(operation, _) => !context.resolve(operation).is_readonly(),
        EditorAction::History(HistoryAction::Checkpoint) => false,
        EditorAction::History(_) | EditorAction::InsertText(_) | EditorAction::Complete(..) => true,
        _ => false,
    }
}

fn cursor_from_byte(text: &str, byte: usize) -> Cursor {
    let byte = byte.min(text.len());
    let before = &text[..byte];
    let line_start = before.rfind('\n').map_or(0, |offset| offset + 1);
    Cursor::new(
        before.bytes().filter(|byte| *byte == b'\n').count(),
        before[line_start..].chars().count(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn package_handles_counts_operators_and_insert_mode() {
        let mut vim = VimEngine::new("one\ntwo\nthree", 0);
        let snapshot = vim.input_text("2dd");
        assert_eq!(snapshot.text.as_deref(), Some("three"));
        let snapshot = vim.input_text("ihello ");
        assert_eq!(snapshot.mode, VimMode::Insert);
        assert_eq!(snapshot.text.as_deref(), Some("hello three"));
        let snapshot = vim.input_key(KeyCode::Esc);
        assert_eq!(snapshot.mode, VimMode::Normal);
        assert_eq!(snapshot.text, None);
    }

    #[test]
    fn empty_insert_mode_cycles_preserve_the_cursor() {
        let mut vim = VimEngine::new("abcdef", 4);
        for _ in 0..8 {
            let snapshot = vim.input_text("i");
            assert_eq!(snapshot.mode, VimMode::Insert);
            let snapshot = vim.input_key(KeyCode::Esc);
            assert_eq!(snapshot.mode, VimMode::Normal);
            assert_eq!(snapshot.cursor, (0, 4));
            assert_eq!(snapshot.text, None);
        }
    }

    #[test]
    fn package_repeats_completed_edits() {
        let mut vim = VimEngine::new("one\ntwo\nthree\nfour", 0);
        assert_eq!(vim.input_text("2dd").text.as_deref(), Some("three\nfour"));
        assert_eq!(vim.input_text(".").text.as_deref(), Some(""));
    }

    #[test]
    fn package_moves_between_paragraphs_with_braces() {
        let mut vim = VimEngine::new("select 1;\n\nselect 2;\n\nselect 3;", 0);
        let snapshot = vim.input_text("}");
        assert_eq!(snapshot.cursor, (1, 0));
        let snapshot = vim.input_text("}");
        assert_eq!(snapshot.cursor, (3, 0));
        let snapshot = vim.input_text("{");
        assert_eq!(snapshot.cursor, (1, 0));
    }

    #[test]
    fn sentinel_preserves_user_trailing_newline() {
        let mut vim = VimEngine::new("select 1;\n", 0);
        assert_eq!(vim.input_text("l").text, None);
    }

    #[test]
    fn reports_incomplete_key_sequences() {
        let mut vim = VimEngine::new("one\ntwo\nthree", 0);
        assert_eq!(vim.input_text("g").entered, "g");
        assert_eq!(vim.input_text("g").entered, "");

        assert_eq!(vim.input_text("2d").entered, "2d");
        assert_eq!(vim.input_text("d").entered, "");
    }

    #[test]
    fn unnamed_register_is_shared_between_editors() {
        let store = modalkit::editing::store::Store::<EmptyInfo>::default().shared();
        let mut first = VimEngine::with_store("one\ntwo", 0, store.clone());
        let mut second = VimEngine::with_store("alpha\nbeta", 0, store);

        assert_eq!(first.input_text("yy").text, None);
        assert_eq!(
            second.input_text("p").text.as_deref(),
            Some("alpha\none\nbeta")
        );
    }

    #[test]
    fn unnamed_register_round_trips_through_platform_clipboard_text() {
        let mut vim = VimEngine::new("one\ntwo", 0);
        let yank = vim.input_text("yy");
        assert_eq!(yank.clipboard.as_deref(), Some("one\n"));

        vim.set_clipboard("external");
        let paste = vim.input_text("p");
        assert_eq!(paste.text.as_deref(), Some("oexternalne\ntwo"));
        assert_eq!(paste.clipboard, None);
    }

    #[test]
    fn platform_clipboard_trailing_newline_pastes_linewise() {
        let mut vim = VimEngine::new("one\ntwo", 0);
        vim.set_clipboard("external\n");
        let paste = vim.input_text("p");
        assert_eq!(paste.text.as_deref(), Some("one\nexternal\ntwo"));
    }

    #[test]
    fn colon_requests_sift_command_palette_without_entering_ex_mode() {
        let mut vim = VimEngine::new("select 1;", 0);
        let snapshot = vim.input_text(":");
        assert!(snapshot.open_command_palette);
        assert_eq!(snapshot.mode, VimMode::Normal);
        assert_eq!(snapshot.entered, "");
    }
}
