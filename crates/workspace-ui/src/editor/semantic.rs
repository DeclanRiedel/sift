//! Client-side projection of the shared SQL semantic service (ADR-032).
//!
//! The desktop never parses SQL itself. Every diagnostic, completion, format
//! result, quick fix, and usage list comes from the server-owned semantic
//! document. This module holds the editor-local projection of those answers
//! plus the offset arithmetic needed to map wire ranges onto client bytes.
//!
//! Staleness is decided by one number: the editor's text revision. A request
//! carries the revision it was issued against and any answer whose revision no
//! longer matches the buffer is dropped instead of being applied late. That is
//! the whole of "semantic revision cancellation" on the client side.

use std::ops::Range;

use sift_protocol::completion::{CompletionCandidate, CompletionKind};
use sift_protocol::{
    DiagnosticSeverity, SemanticDiagnostic, SqlUsage, SqlUsageKind, TextEdit, TextRange,
};

/// Clamp a wire offset onto a byte offset that is a char boundary of `text`.
fn clamp_offset(text: &str, offset: u32) -> usize {
    let mut offset = (offset as usize).min(text.len());
    while !text.is_char_boundary(offset) {
        offset -= 1;
    }
    offset
}

/// Project a wire range onto client byte offsets, ordering the ends.
pub(crate) fn clamp_range(text: &str, range: TextRange) -> Range<usize> {
    let start = clamp_offset(text, range.start);
    let end = clamp_offset(text, range.end);
    start.min(end)..start.max(end)
}

/// What the editor wants the semantic service to do for one buffer revision.
/// Every variant is self-sufficient: the dispatcher pairs it with the exact
/// text it was issued against, so the server document can be resynchronized
/// before the request runs without a separate ordering protocol.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SemanticRequestKind {
    /// Re-parse and re-diagnose. Debounced by the dispatcher.
    Analyze,
    Complete {
        cursor: u32,
    },
    Hover {
        position: u32,
    },
    ExpandStar {
        position: u32,
    },
    Format {
        range: Option<TextRange>,
    },
    QuickFix {
        fix_id: String,
    },
    Usages {
        position: u32,
    },
    Rename {
        position: u32,
        new_name: String,
    },
    /// List every statement intersecting the full document range.
    Outline {
        end: u32,
    },
}

impl SemanticRequestKind {
    /// Analyze is the only request the dispatcher may delay; the rest are
    /// direct responses to a keystroke the user is waiting on.
    pub const fn is_debounced(&self) -> bool {
        matches!(self, Self::Analyze)
    }
}

/// One completed semantic answer, addressed to the revision it describes.
#[derive(Debug, Clone)]
pub enum SemanticOutcome {
    Diagnostics {
        diagnostics: Vec<SemanticDiagnostic>,
        incomplete: bool,
    },
    Completions {
        replaced: TextRange,
        candidates: Vec<CompletionCandidate>,
    },
    Hover(sift_protocol::SemanticHoverResponse),
    StarExpansion(sift_protocol::StarExpansionPreview),
    Edits {
        edits: Vec<TextEdit>,
        warnings: Vec<String>,
    },
    RenamePreview {
        edits: Vec<TextEdit>,
        warnings: Vec<String>,
    },
    Usages {
        usages: Vec<SqlUsage>,
        is_complete: bool,
    },
    Outline {
        statements: Vec<sift_protocol::SemanticStatement>,
        symbols: Vec<sift_protocol::SemanticOutlineSymbol>,
    },
    OutlineFailed(String),
    Failed(String),
}

/// One server diagnostic projected onto client byte offsets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditorDiagnostic {
    pub id: String,
    pub severity: DiagnosticSeverity,
    pub code: String,
    pub message: String,
    pub range: Range<usize>,
    pub quick_fix_ids: Vec<String>,
}

impl EditorDiagnostic {
    fn project(text: &str, diagnostic: SemanticDiagnostic) -> Self {
        let range = clamp_range(text, diagnostic.range);
        Self {
            id: diagnostic.id,
            severity: diagnostic.severity,
            code: diagnostic.code,
            message: diagnostic.message,
            range,
            quick_fix_ids: diagnostic.quick_fix_ids,
        }
    }

    /// Diagnostics are half-open ranges; an empty range still owns the caret
    /// sitting on it so end-of-statement errors remain reachable.
    pub fn contains(&self, offset: usize) -> bool {
        if self.range.is_empty() {
            return offset == self.range.start;
        }
        self.range.contains(&offset)
    }
}

/// The completion menu currently offered for one buffer revision.
#[derive(Debug, Clone)]
pub struct CompletionMenu {
    /// Range the accepted candidate replaces, in client byte offsets.
    pub replace: Range<usize>,
    pub candidates: Vec<CompletionCandidate>,
    pub selected: usize,
}

impl CompletionMenu {
    pub fn selected(&self) -> Option<&CompletionCandidate> {
        self.candidates.get(self.selected)
    }

    fn move_selection(&mut self, delta: isize) {
        if self.candidates.is_empty() {
            return;
        }
        let count = self.candidates.len() as isize;
        let next = (self.selected as isize + delta).rem_euclid(count);
        self.selected = next as usize;
    }
}

/// Everything the editor knows about the server's view of its text.
#[derive(Debug, Default)]
pub struct SemanticState {
    diagnostics: Vec<EditorDiagnostic>,
    diagnostics_by_line: Vec<Vec<usize>>,
    /// Text revision the current diagnostics describe. `None` means the buffer
    /// has never been analysed.
    diagnostics_revision: Option<u64>,
    diagnostics_incomplete: bool,
    completion: Option<CompletionMenu>,
    /// Revision a completion request is outstanding for, used to keep the
    /// menu from flickering open on a stale answer.
    pending_completion: Option<u64>,
    hover: Option<sift_protocol::SemanticHoverResponse>,
    pending_hover: Option<(u64, u32)>,
    star_expansion: Option<sift_protocol::StarExpansionPreview>,
    pending_star_expansion: Option<u64>,
    usages: Vec<(Range<usize>, SqlUsageKind)>,
    usages_by_line: Vec<Vec<usize>>,
    usages_revision: Option<u64>,
    /// Last non-fatal semantic message (format warnings, partial usages,
    /// service failures). Surfaced in the editor status strip.
    notice: Option<String>,
}

impl SemanticState {
    pub fn diagnostics(&self) -> &[EditorDiagnostic] {
        &self.diagnostics
    }

    pub fn diagnostics_incomplete(&self) -> bool {
        self.diagnostics_incomplete
    }

    pub fn completion(&self) -> Option<&CompletionMenu> {
        self.completion.as_ref()
    }

    pub fn hover(&self) -> Option<&sift_protocol::SemanticHoverResponse> {
        self.hover.as_ref()
    }

    pub fn star_expansion(&self) -> Option<&sift_protocol::StarExpansionPreview> {
        self.star_expansion.as_ref()
    }

    pub fn usages(&self) -> &[(Range<usize>, SqlUsageKind)] {
        &self.usages
    }

    pub fn diagnostic_indexes_on_line(&self, line: usize) -> &[usize] {
        self.diagnostics_by_line
            .get(line)
            .map_or(&[], Vec::as_slice)
    }

    pub fn usage_indexes_on_line(&self, line: usize) -> &[usize] {
        self.usages_by_line.get(line).map_or(&[], Vec::as_slice)
    }

    pub fn notice(&self) -> Option<&str> {
        self.notice.as_deref()
    }

    pub fn error_count(&self) -> usize {
        self.diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.severity == DiagnosticSeverity::Error)
            .count()
    }

    pub fn warning_count(&self) -> usize {
        self.diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.severity == DiagnosticSeverity::Warning)
            .count()
    }

    /// The diagnostic owning `offset`, preferring the most severe one.
    pub fn diagnostic_at(&self, offset: usize) -> Option<&EditorDiagnostic> {
        self.diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.contains(offset))
            .min_by_key(|diagnostic| severity_rank(diagnostic.severity))
    }

    /// Drop everything that describes an older buffer. Diagnostics are kept
    /// (greyed, not moved) so the editor does not flash empty on every
    /// keystroke; positions become approximate until the next answer lands.
    pub fn invalidate(&mut self) {
        self.completion = None;
        self.pending_completion = None;
        self.hover = None;
        self.pending_hover = None;
        self.star_expansion = None;
        self.pending_star_expansion = None;
        self.usages.clear();
        self.usages_by_line.clear();
        self.usages_revision = None;
    }

    pub fn cancel_completion(&mut self) -> bool {
        let had_menu = self.completion.is_some() || self.pending_completion.is_some();
        self.completion = None;
        self.pending_completion = None;
        had_menu
    }

    pub fn expect_completion(&mut self, revision: u64) {
        self.pending_completion = Some(revision);
        self.completion = None;
    }

    pub fn expect_hover(&mut self, revision: u64, position: u32) -> bool {
        if self.pending_hover == Some((revision, position))
            || self.hover.as_ref().is_some_and(|hover| {
                hover.revision == revision
                    && hover.range.start <= position
                    && position <= hover.range.end
            })
        {
            return false;
        }
        self.pending_hover = Some((revision, position));
        self.hover = None;
        true
    }

    pub fn clear_hover(&mut self) -> bool {
        let changed = self.hover.is_some() || self.pending_hover.is_some();
        self.hover = None;
        self.pending_hover = None;
        changed
    }

    pub fn expect_star_expansion(&mut self, revision: u64) {
        self.pending_star_expansion = Some(revision);
        self.star_expansion = None;
    }

    pub fn clear_star_expansion(&mut self) -> bool {
        let changed = self.star_expansion.is_some() || self.pending_star_expansion.is_some();
        self.star_expansion = None;
        self.pending_star_expansion = None;
        changed
    }

    pub fn move_completion_selection(&mut self, delta: isize) -> bool {
        match self.completion.as_mut() {
            Some(menu) => {
                menu.move_selection(delta);
                true
            }
            None => false,
        }
    }

    /// Apply a diagnostics answer. Returns false when it is stale.
    pub fn set_diagnostics(
        &mut self,
        text: &str,
        revision: u64,
        current_revision: u64,
        diagnostics: Vec<SemanticDiagnostic>,
        incomplete: bool,
    ) -> bool {
        if revision != current_revision {
            return false;
        }
        self.diagnostics = diagnostics
            .into_iter()
            .map(|diagnostic| EditorDiagnostic::project(text, diagnostic))
            .collect();
        self.diagnostics.sort_by_key(|diagnostic| {
            (
                diagnostic.range.start,
                severity_rank(diagnostic.severity),
                diagnostic.range.end,
            )
        });
        self.diagnostics_by_line = span_line_index(
            text,
            self.diagnostics.iter().map(|diagnostic| &diagnostic.range),
        );
        self.diagnostics_revision = Some(revision);
        self.diagnostics_incomplete = incomplete;
        true
    }

    /// Apply a completion answer. Returns false when it is stale or empty.
    pub fn set_completions(
        &mut self,
        text: &str,
        revision: u64,
        current_revision: u64,
        replaced: TextRange,
        candidates: Vec<CompletionCandidate>,
    ) -> bool {
        if revision != current_revision || self.pending_completion != Some(revision) {
            return false;
        }
        self.pending_completion = None;
        if candidates.is_empty() {
            self.completion = None;
            return false;
        }
        self.completion = Some(CompletionMenu {
            replace: clamp_range(text, replaced),
            candidates,
            selected: 0,
        });
        true
    }

    pub fn set_hover(
        &mut self,
        revision: u64,
        current_revision: u64,
        hover: sift_protocol::SemanticHoverResponse,
    ) -> bool {
        if revision != current_revision
            || hover.revision != revision
            || !self
                .pending_hover
                .is_some_and(|(pending_revision, position)| {
                    pending_revision == revision
                        && hover.range.start <= position
                        && position <= hover.range.end
                })
        {
            return false;
        }
        self.pending_hover = None;
        self.hover = Some(hover);
        true
    }

    pub fn set_star_expansion(
        &mut self,
        revision: u64,
        current_revision: u64,
        preview: sift_protocol::StarExpansionPreview,
    ) -> bool {
        if revision != current_revision
            || preview.revision != revision
            || self.pending_star_expansion != Some(revision)
            || !preview.exact
        {
            return false;
        }
        self.pending_star_expansion = None;
        self.star_expansion = Some(preview);
        true
    }

    pub fn set_usages(
        &mut self,
        text: &str,
        revision: u64,
        current_revision: u64,
        usages: Vec<SqlUsage>,
        is_complete: bool,
    ) -> bool {
        if revision != current_revision {
            return false;
        }
        self.usages = usages
            .into_iter()
            .map(|usage| (clamp_range(text, usage.range), usage.kind))
            .collect();
        self.usages
            .sort_by_key(|(range, _)| (range.start, range.end));
        self.usages_by_line = span_line_index(text, self.usages.iter().map(|(range, _)| range));
        self.usages_revision = Some(revision);
        self.notice = (!is_complete).then(|| {
            format!(
                "{} usage(s) shown; the search was truncated.",
                self.usages.len()
            )
        });
        true
    }

    pub fn set_notice(&mut self, notice: Option<String>) {
        self.notice = notice;
    }

    /// True while the editor's diagnostics describe an older buffer.
    pub fn diagnostics_stale(&self, current_revision: u64) -> bool {
        self.diagnostics_revision
            .is_some_and(|revision| revision != current_revision)
    }
}

fn span_line_index<'a>(
    text: &str,
    ranges: impl Iterator<Item = &'a Range<usize>>,
) -> Vec<Vec<usize>> {
    let mut line_starts = vec![0];
    line_starts.extend(text.match_indices('\n').map(|(offset, _)| offset + 1));
    let mut index = vec![Vec::new(); line_starts.len()];
    for (span_index, range) in ranges.enumerate() {
        let start = line_starts
            .partition_point(|line_start| *line_start <= range.start.min(text.len()))
            .saturating_sub(1);
        let end = line_starts
            .partition_point(|line_start| *line_start <= range.end.min(text.len()))
            .saturating_sub(1);
        for line in index.iter_mut().take(end + 1).skip(start) {
            line.push(span_index);
        }
    }
    index
}

const fn severity_rank(severity: DiagnosticSeverity) -> u8 {
    match severity {
        DiagnosticSeverity::Error => 0,
        DiagnosticSeverity::Warning => 1,
        DiagnosticSeverity::Information => 2,
        DiagnosticSeverity::Hint => 3,
    }
}

/// Server text edits, ordered so applying them back-to-front keeps every
/// remaining range valid, and clamped onto client char boundaries.
///
/// Overlapping edits are a server bug rather than a recoverable state, so the
/// later of two overlapping edits is dropped instead of corrupting the buffer.
pub(crate) fn ordered_edits(text: &str, edits: Vec<TextEdit>) -> Vec<(Range<usize>, String)> {
    let mut ordered: Vec<(Range<usize>, String)> = edits
        .into_iter()
        .map(|edit| (clamp_range(text, edit.range), edit.new_text))
        .collect();
    ordered.sort_by_key(|(range, _)| (range.start, range.end));
    let mut applied: Vec<(Range<usize>, String)> = Vec::with_capacity(ordered.len());
    for (range, new_text) in ordered {
        if applied
            .last()
            .is_some_and(|(previous, _)| previous.end > range.start)
        {
            continue;
        }
        applied.push((range, new_text));
    }
    applied.reverse();
    applied
}

/// Short single-character badge for a candidate kind. Keeps the menu readable
/// without shipping an icon set for something this dense.
pub(crate) const fn completion_kind_badge(kind: CompletionKind) -> &'static str {
    match kind {
        CompletionKind::Keyword => "K",
        CompletionKind::Function => "ƒ",
        CompletionKind::Schema => "S",
        CompletionKind::Table => "T",
        CompletionKind::View => "V",
        CompletionKind::MaterializedView => "M",
        CompletionKind::Column => "C",
        CompletionKind::Alias => "A",
        CompletionKind::Procedure => "P",
        CompletionKind::Type => "Y",
        CompletionKind::Snippet => "⋯",
    }
}

pub(crate) fn completion_candidate_metadata(candidate: &CompletionCandidate) -> Option<String> {
    match (&candidate.qualified_name, &candidate.detail) {
        (Some(qualified), Some(detail))
            if qualified
                .split('.')
                .any(|part| part.eq_ignore_ascii_case(detail)) =>
        {
            Some(qualified.clone())
        }
        (Some(qualified), Some(detail)) => Some(format!("{qualified} · {detail}")),
        (Some(qualified), None) => Some(qualified.clone()),
        (None, Some(detail)) => Some(detail.clone()),
        (None, None) => None,
    }
}

pub(crate) const fn usage_kind_label(kind: SqlUsageKind) -> &'static str {
    match kind {
        SqlUsageKind::Definition => "definition",
        SqlUsageKind::Read => "read",
        SqlUsageKind::Write => "write",
        SqlUsageKind::Call => "call",
        SqlUsageKind::TypeReference => "type",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn diagnostic(start: u32, end: u32, severity: DiagnosticSeverity) -> SemanticDiagnostic {
        SemanticDiagnostic {
            id: format!("d{start}"),
            severity,
            code: "E1".into(),
            message: "boom".into(),
            range: TextRange { start, end },
            related_ranges: Vec::new(),
            source: "test".into(),
            quick_fix_ids: vec!["fix".into()],
        }
    }

    #[test]
    fn stale_answers_are_dropped_instead_of_applied_late() {
        let mut state = SemanticState::default();
        assert!(!state.set_diagnostics(
            "select 1",
            4,
            5,
            vec![diagnostic(0, 6, DiagnosticSeverity::Error)],
            false,
        ));
        assert!(state.diagnostics().is_empty());
        assert!(state.set_diagnostics(
            "select 1",
            5,
            5,
            vec![diagnostic(0, 6, DiagnosticSeverity::Error)],
            false,
        ));
        assert_eq!(state.error_count(), 1);
    }

    #[test]
    fn wire_ranges_clamp_onto_char_boundaries() {
        let text = "sélect";
        let range = clamp_range(text, TextRange { start: 2, end: 999 });
        assert!(text.is_char_boundary(range.start));
        assert_eq!(range.end, text.len());
    }

    #[test]
    fn completion_menu_only_opens_for_the_revision_it_was_requested_for() {
        let mut state = SemanticState::default();
        let candidate = CompletionCandidate {
            label: "users".into(),
            insert: "users".into(),
            kind: CompletionKind::Table,
            detail: None,
            qualified_name: None,
            score: 1,
        };
        // No request outstanding: an unsolicited answer never opens a menu.
        assert!(!state.set_completions(
            "sel",
            3,
            3,
            TextRange { start: 0, end: 3 },
            vec![candidate.clone()],
        ));
        state.expect_completion(3);
        assert!(state.set_completions(
            "sel",
            3,
            3,
            TextRange { start: 0, end: 3 },
            vec![candidate],
        ));
        assert_eq!(
            state.completion().map(|menu| menu.candidates.len()),
            Some(1)
        );
    }

    #[test]
    fn completion_metadata_combines_owner_and_type() {
        let candidate = CompletionCandidate {
            label: "id".into(),
            insert: "id".into(),
            kind: CompletionKind::Column,
            detail: Some("int4 NOT NULL".into()),
            qualified_name: Some("app.public.users".into()),
            score: 1,
        };
        assert_eq!(
            completion_candidate_metadata(&candidate).as_deref(),
            Some("app.public.users · int4 NOT NULL")
        );
    }

    #[test]
    fn hover_requires_matching_revision_and_pending_word() {
        let mut state = SemanticState::default();
        let response = sift_protocol::SemanticHoverResponse {
            document_id: sift_protocol::SemanticDocumentId(
                "00000000-0000-0000-0000-000000000000".parse().unwrap(),
            ),
            revision: 3,
            range: TextRange { start: 7, end: 12 },
            kind: sift_protocol::SemanticHoverKind::Object,
            display_name: "users".into(),
            qualified_name: Some("app.public.users".into()),
            type_ref: None,
            nullability: None,
            object_kind: Some(sift_protocol::CatalogNodeKind::Table),
            comment: None,
            detail: None,
            uncertain: false,
            catalog_revision: Some(sift_protocol::CatalogRevision(1)),
        };
        assert!(state.expect_hover(3, 7));
        assert!(!state.set_hover(3, 4, response.clone()));
        assert!(state.set_hover(3, 3, response));
        assert_eq!(state.hover().unwrap().display_name, "users");
        assert!(!state.expect_hover(3, 8));
        assert!(state.clear_hover());
    }

    #[test]
    fn most_severe_diagnostic_wins_at_a_shared_offset() {
        let mut state = SemanticState::default();
        state.set_diagnostics(
            "select 1",
            1,
            1,
            vec![
                diagnostic(0, 6, DiagnosticSeverity::Warning),
                diagnostic(0, 6, DiagnosticSeverity::Error),
            ],
            false,
        );
        assert_eq!(
            state.diagnostic_at(2).map(|found| found.severity),
            Some(DiagnosticSeverity::Error)
        );
    }

    #[test]
    fn edits_apply_back_to_front_and_drop_overlaps() {
        let edits = vec![
            TextEdit {
                range: TextRange { start: 0, end: 6 },
                new_text: "SELECT".into(),
            },
            TextEdit {
                range: TextRange { start: 3, end: 8 },
                new_text: "overlap".into(),
            },
            TextEdit {
                range: TextRange { start: 7, end: 8 },
                new_text: "2".into(),
            },
        ];
        let ordered = ordered_edits("select 1", edits);
        assert_eq!(ordered.len(), 2);
        assert_eq!(ordered[0].0, 7..8);
        assert_eq!(ordered[1].0, 0..6);
    }

    #[test]
    fn an_empty_diagnostic_range_still_owns_its_caret() {
        let mut state = SemanticState::default();
        state.set_diagnostics(
            "select",
            1,
            1,
            vec![diagnostic(6, 6, DiagnosticSeverity::Error)],
            false,
        );
        assert!(state.diagnostic_at(6).is_some());
        assert!(state.diagnostic_at(5).is_none());
    }

    #[test]
    fn span_index_projects_only_overlapping_lines() {
        let ranges = [0..3, 2..8, 9..9];
        let index = span_line_index("one\ntwo\nthree", ranges.iter());
        assert_eq!(index, vec![vec![0, 1], vec![1], vec![1, 2]]);
    }
}
