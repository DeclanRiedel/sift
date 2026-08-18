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
        store::Store,
    },
    env::vim::{
        keybindings::{default_vim_keys, VimMachine},
        VimMode as ModalVimMode,
    },
    key::TerminalKey,
    keybindings::BindingMachine,
    prelude::ViewportContext,
};

use super::VimMode;

pub(super) struct VimSnapshot {
    pub text: Option<String>,
    pub cursor: (usize, usize),
    pub selection: Option<((usize, usize), (usize, usize))>,
    pub mode: VimMode,
}

/// A complete Vim keybinding machine plus ModalKit's editing buffer. Sift
/// mirrors each completed command back into its CRDT as a minimal splice.
pub(super) struct VimEngine {
    bindings: VimMachine<TerminalKey>,
    buffer: EditBuffer<EmptyInfo>,
    cursor_group: CursorGroupId,
    store: Store<EmptyInfo>,
    viewport: ViewportContext<Cursor>,
}

impl VimEngine {
    pub fn new(text: &str, cursor: usize) -> Self {
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
            store: Store::default(),
            viewport: ViewportContext::default(),
        }
    }

    pub fn set_viewport_rows(&mut self, rows: usize) {
        self.viewport.dimensions.1 = rows.max(1);
    }

    pub fn set_cursor(&mut self, text: &str, cursor: usize) {
        self.buffer
            .set_leader(self.cursor_group, cursor_from_byte(text, cursor));
    }

    pub fn input_text(&mut self, text: &str) -> VimSnapshot {
        let mut text_changed = false;
        for character in text.chars() {
            text_changed |=
                self.input_key_event(KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE));
        }
        self.snapshot(text_changed)
    }

    pub fn input_key(&mut self, code: KeyCode) -> VimSnapshot {
        let text_changed = self.input_key_event(KeyEvent::new(code, KeyModifiers::NONE));
        self.snapshot(text_changed)
    }

    fn input_key_event(&mut self, event: KeyEvent) -> bool {
        let mut text_changed = false;
        self.bindings.input_key(event.into());
        while let Some((action, context)) = self.bindings.pop() {
            let editor_context = (self.cursor_group, &self.viewport, &context);
            match action {
                Action::Editor(action) => {
                    text_changed |= editor_action_changes_text(&action, &context);
                    let _ = self
                        .buffer
                        .editor_command(&action, &editor_context, &mut self.store);
                }
                Action::Jump(list, direction, count) => {
                    let count = context.resolve(&count);
                    let _ = self.buffer.jump(list, direction, count, &editor_context);
                }
                Action::Search(direction, count) => {
                    let _ = self
                        .buffer
                        .search(direction, count, &editor_context, &mut self.store);
                }
                Action::Repeat(sequence) => self.bindings.repeat(sequence, Some(context)),
                _ => {}
            }
        }
        text_changed
    }

    fn snapshot(&mut self, text_changed: bool) -> VimSnapshot {
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
    fn package_repeats_completed_edits() {
        let mut vim = VimEngine::new("one\ntwo\nthree\nfour", 0);
        assert_eq!(vim.input_text("2dd").text.as_deref(), Some("three\nfour"));
        assert_eq!(vim.input_text(".").text.as_deref(), Some(""));
    }

    #[test]
    fn sentinel_preserves_user_trailing_newline() {
        let mut vim = VimEngine::new("select 1;\n", 0);
        assert_eq!(vim.input_text("l").text, None);
    }
}
