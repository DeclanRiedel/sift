//! Query results surface: a `ResultSet` projection of the server's execution
//! outcome and a `ResultsView` that renders it as query-owned Data / Messages /
//! Explain / History tabs. The model maps real `sift_protocol` result types and
//! is GPUI-free so cell formatting and state transitions are unit-testable. The
//! grid virtualizes rows so paint cost tracks the viewport, not cardinality.

use std::collections::HashMap;
use std::ops::Range;

use gpui::{
    actions, anchored, canvas, deferred, div, prelude::*, px, uniform_list, App, ClipboardItem,
    Context, CursorStyle, Div, DragMoveEvent, Entity, FocusHandle, Focusable, IntoElement,
    MouseButton, Pixels, Point, ScrollStrategy, ShapedLine, SharedString, Stateful, Subscription,
    TextAlign, TextRun, UniformListScrollHandle, Window,
};
use sift_api_types::QueryHistory;
use sift_protocol::{
    ColumnMetadata, DriverWarning, ExecuteResponse, ExplainResponse, Nullability, Page, PlanNode,
    Row, TypeRef, Value,
};
use sift_ui::{
    icon, ActiveTheme, Badge, Button, ButtonTone, Clickable, Disableable, ErrorBanner, IconButton,
    IconName, TextInput, TextInputEvent, ThemeColors,
};

use crate::presentation::ResultReference;

const MIN_COLUMN_WIDTH: f32 = 144.0;
const DEFAULT_COLUMN_WIDTH: f32 = 184.0;
const MAX_COLUMN_WIDTH: f32 = 960.0;
const COLUMN_RESIZE_HANDLE_WIDTH: f32 = 7.0;
pub(crate) const ROW_NUMBER_WIDTH: f32 = 46.0;
pub(crate) const ROW_HEIGHT: f32 = 24.0;
const HEADER_HEIGHT: f32 = 40.0;
/// Hard UI retention bound. WebSocket ACK backpressure limits pages in flight;
/// this separately prevents an arbitrarily large completed query from growing
/// desktop memory without bound.
pub const MAX_RETAINED_ROWS: usize = 10_000;

fn plan_number(value: f64) -> String {
    if value.abs() >= 1_000.0 || value.fract().abs() < f64::EPSILON {
        format!("{value:.0}")
    } else {
        format!("{value:.2}")
    }
}

/// How a single cell's value should be classified for rendering. Keeps color
/// and alignment decisions in the view while the model stays presentation-free.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CellClass {
    Null,
    Number,
    Bool,
    Text,
    Temporal,
    Binary,
    Structured,
}

/// A cell rendered to display text plus its class.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CellRender {
    pub text: String,
    pub class: CellClass,
}

#[derive(Debug, Clone)]
struct CachedCellRender {
    text: SharedString,
    paint_text: SharedString,
    class: CellClass,
    shaped: Option<CachedShapedCell>,
}

#[derive(Debug, Clone)]
struct CachedShapedCell {
    line: ShapedLine,
    run: TextRun,
    font_size: Pixels,
}

#[derive(Debug, Clone)]
struct CachedColumnRender {
    name: SharedString,
    type_label: SharedString,
    nullable: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct ResultFieldInspectorRow {
    pub source_column: usize,
    pub name: SharedString,
    pub type_label: SharedString,
    pub nullable: bool,
    pub included: bool,
}

pub(crate) struct SelectedCellEdit {
    pub column: String,
    pub original: Value,
    pub original_row: Vec<(String, Value)>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SelectedRowJson {
    pub row_index: usize,
    pub value: serde_json::Value,
}

fn value_for_row_json(value: &Value) -> serde_json::Value {
    match value {
        Value::Null | Value::TypedNull { .. } => serde_json::Value::Null,
        Value::Bool(value) => (*value).into(),
        Value::Int16(value) => (*value).into(),
        Value::Int32(value) => (*value).into(),
        Value::Int64(value) => (*value).into(),
        Value::Float32(value) => serde_json::Number::from_f64(f64::from(*value))
            .map_or_else(|| value.to_string().into(), serde_json::Value::Number),
        Value::Float64(value) => serde_json::Number::from_f64(*value)
            .map_or_else(|| value.to_string().into(), serde_json::Value::Number),
        // Keep arbitrary precision exact rather than silently rounding it to
        // the JSON implementation's floating-point representation.
        Value::Decimal(value) => value.clone().into(),
        Value::Text(value) => {
            let trimmed = value.trim();
            let looks_structured = (trimmed.starts_with('{') && trimmed.ends_with('}'))
                || (trimmed.starts_with('[') && trimmed.ends_with(']'));
            if looks_structured {
                serde_json::from_str(trimmed).unwrap_or_else(|_| value.clone().into())
            } else {
                value.clone().into()
            }
        }
        Value::Blob(value) => format!("<{} bytes>", value.len()).into(),
        Value::Date(value) => value.to_string().into(),
        Value::Time(value) => value.to_string().into(),
        Value::Timestamp(value) => value.to_string().into(),
        Value::TimestampTz(value) => value.to_rfc3339().into(),
        Value::Interval(value) => format!("{} ms", value.num_milliseconds()).into(),
        Value::Uuid(value) => value.to_string().into(),
        Value::Json(value) => value.clone(),
        Value::Native { display_text, .. } => display_text.clone().into(),
    }
}

#[derive(Debug, Clone)]
struct ColumnDrag {
    index: usize,
    name: SharedString,
}

impl gpui::Render for ColumnDrag {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = cx.theme().colors;
        div()
            .px_2()
            .h(px(HEADER_HEIGHT))
            .min_w(px(MIN_COLUMN_WIDTH))
            .flex()
            .items_center()
            .bg(colors.toolbar)
            .border_1()
            .border_color(colors.accent)
            .text_sm()
            .font_weight(gpui::FontWeight::SEMIBOLD)
            .child(self.name.clone())
    }
}

/// A result column's presentation projection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResultColumn {
    pub name: String,
    pub type_label: String,
    pub nullable: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GridSelection {
    Cell {
        row: usize,
        column: usize,
    },
    Range {
        anchor_row: usize,
        anchor_column: usize,
        focus_row: usize,
        focus_column: usize,
    },
    Row(usize),
    Column(usize),
    All,
}

#[derive(Debug, Clone, Copy)]
struct ColumnResizeDrag {
    column: usize,
}

impl GridSelection {
    fn highlights_row(self, row: usize) -> bool {
        matches!(
            self,
            Self::Cell { row: selected, .. } | Self::Row(selected) if selected == row
        ) || matches!(self, Self::Range { anchor_row, focus_row, .. } if (anchor_row.min(focus_row)..=anchor_row.max(focus_row)).contains(&row))
            || self == Self::All
    }

    fn highlights_column(self, column: usize) -> bool {
        matches!(
            self,
            Self::Cell {
                column: selected,
                ..
            } | Self::Column(selected) if selected == column
        ) || matches!(self, Self::Range { anchor_column, focus_column, .. } if (anchor_column.min(focus_column)..=anchor_column.max(focus_column)).contains(&column))
            || self == Self::All
    }
}

impl ResultColumn {
    fn from_metadata(column: &ColumnMetadata) -> Self {
        let type_label = match &column.type_ref {
            TypeRef::Primitive(primitive) => format!("{primitive:?}").to_lowercase(),
            TypeRef::Native { name, .. } => name.clone(),
        };
        Self {
            name: column.name.clone(),
            type_label,
            nullable: matches!(column.nullable, Nullability::Nullable),
        }
    }
}

/// A materialized, ready result set: the current page window of rows plus the
/// batch's completion facts. Row memory is bounded by the page, never by total
/// cardinality — additional pages replace or extend this within budget.
#[derive(Debug, Clone, Default)]
pub struct ResultData {
    pub columns: Vec<ResultColumn>,
    pub rows: Vec<Row>,
    pub affected_rows: Option<u64>,
    pub duration_ms: Option<u64>,
    pub warnings: Vec<DriverWarning>,
    pub has_more: bool,
    /// Set when a multi-result batch was truncated to its first result set.
    pub truncated_extra_results: bool,
}

/// The distinct outcome states a result surface can be in. Each is a separate,
/// non-collapsible UI state per the Phase M error/trust model.
#[derive(Debug, Clone)]
pub enum ResultState {
    /// Nothing has been run yet.
    Idle,
    /// A previous run is known by reference only — its rows were never
    /// persisted and are gone. Distinct from `Idle`: something *did* run, and
    /// the user needs to be told the rows are not what they are looking at.
    Detached(ResultReference),
    /// A run is in flight.
    Pending,
    /// Streamed rows are available while the query remains in flight.
    Streaming(ResultData),
    /// Rows (or an affected-row count) are available.
    Ready(ResultData),
    /// The server rejected or could not run the query (offline, no connection,
    /// policy). Carries a human reason.
    Unavailable(String),
    /// The driver returned an error.
    Failed(String),
    /// The user cancelled the run.
    Cancelled,
    /// The run exceeded its deadline.
    TimedOut,
    /// Transport was lost with the outcome indeterminate — never reported as
    /// success and never auto-retried.
    OutcomeUnknown,
}

impl ResultState {
    /// Build from a bounded HTTP execute response (first page already present).
    pub fn from_execute(response: ExecuteResponse) -> Self {
        ResultState::Ready(ResultData {
            columns: response
                .columns
                .iter()
                .map(ResultColumn::from_metadata)
                .collect(),
            rows: response.rows,
            affected_rows: response.affected_rows,
            duration_ms: None,
            warnings: response.warnings,
            has_more: response.has_more,
            truncated_extra_results: false,
        })
    }

    /// Build from a streamed page sequence, taking the first result set of a
    /// multi-result batch and flagging the truncation.
    pub fn from_pages(pages: Vec<Page>) -> Self {
        let mut data = ResultData::default();
        let mut seen_result = false;
        for page in pages {
            match page {
                Page::NextResult { columns } => {
                    if seen_result {
                        data.truncated_extra_results = true;
                        break;
                    }
                    seen_result = true;
                    data.columns = columns.iter().map(ResultColumn::from_metadata).collect();
                }
                Page::Rows { rows } => data.rows.extend(rows),
                Page::Error { error } => return ResultState::Failed(error.message),
                Page::Done {
                    affected_rows,
                    warnings,
                } => {
                    data.affected_rows = affected_rows;
                    data.warnings = warnings;
                }
            }
        }
        ResultState::Ready(data)
    }

    /// Classify a failed execution into a distinct state. `transport` marks a
    /// request that never received a definite server answer, so its outcome is
    /// indeterminate and must never be reported as success or auto-retried.
    pub fn from_execution_error(transport: bool, message: impl Into<String>) -> Self {
        if transport {
            return ResultState::OutcomeUnknown;
        }
        let message = message.into();
        let lower = message.to_lowercase();
        if lower.contains("cancel") {
            ResultState::Cancelled
        } else if lower.contains("timed out") || lower.contains("timeout") {
            ResultState::TimedOut
        } else {
            ResultState::Failed(message)
        }
    }

    /// Short label for the status strip.
    /// The durable reference a finished run leaves on its tab, or `None` while
    /// nothing has completed. Rows are deliberately excluded.
    pub fn reference(
        &self,
        cursor_id: Option<u64>,
        completed_at_ms: u64,
    ) -> Option<ResultReference> {
        let data = match self {
            ResultState::Ready(data) => data,
            _ => return None,
        };
        Some(ResultReference {
            cursor_id,
            row_count: data.rows.len() as u64,
            affected_rows: data.affected_rows,
            has_more: data.has_more,
            completed_at_ms,
        })
    }

    pub fn status_label(&self) -> String {
        match self {
            ResultState::Idle => "Ready".into(),
            ResultState::Detached(reference) => match reference.affected_rows {
                Some(affected) => format!("{affected} row(s) affected · previous session"),
                None => {
                    let more = if reference.has_more { "+" } else { "" };
                    format!(
                        "{}{more} row(s) last run · re-run to view",
                        reference.row_count
                    )
                }
            },
            ResultState::Pending => "Running…".into(),
            ResultState::Streaming(data) => format!("{}+ row(s) · Running…", data.rows.len()),
            ResultState::Ready(data) => match (data.rows.len(), data.affected_rows) {
                (0, Some(affected)) => match data.duration_ms {
                    Some(duration) => format!("{affected} row(s) affected · {duration} ms"),
                    None => format!("{affected} row(s) affected"),
                },
                (rows, _) => {
                    let more = if data.has_more { "+" } else { "" };
                    match data.duration_ms {
                        Some(duration) => format!("{rows}{more} row(s) · {duration} ms"),
                        None => format!("{rows}{more} row(s)"),
                    }
                }
            },
            ResultState::Unavailable(reason) => reason.clone(),
            ResultState::Failed(message) => format!("Failed: {message}"),
            ResultState::Cancelled => "Cancelled".into(),
            ResultState::TimedOut => "Timed out".into(),
            ResultState::OutcomeUnknown => "Outcome unknown".into(),
        }
    }

    fn ready(&self) -> Option<&ResultData> {
        match self {
            ResultState::Streaming(data) | ResultState::Ready(data) => Some(data),
            _ => None,
        }
    }
}

/// Render a protocol [`Value`] into display text and a class. Null and binary
/// get explicit, non-empty presentations rather than blank cells.
pub fn render_value(value: &Value) -> CellRender {
    let (text, class) = match value {
        Value::Null => ("NULL".to_string(), CellClass::Null),
        Value::TypedNull { .. } => ("NULL".to_string(), CellClass::Null),
        Value::Bool(b) => (b.to_string(), CellClass::Bool),
        Value::Int16(n) => (n.to_string(), CellClass::Number),
        Value::Int32(n) => (n.to_string(), CellClass::Number),
        Value::Int64(n) => (n.to_string(), CellClass::Number),
        Value::Float32(n) => (n.to_string(), CellClass::Number),
        Value::Float64(n) => (n.to_string(), CellClass::Number),
        Value::Decimal(s) => (s.clone(), CellClass::Number),
        Value::Text(s) => (s.clone(), CellClass::Text),
        Value::Blob(bytes) => (format!("⟨{} bytes⟩", bytes.len()), CellClass::Binary),
        Value::Date(d) => (d.to_string(), CellClass::Temporal),
        Value::Time(t) => (t.to_string(), CellClass::Temporal),
        Value::Timestamp(ts) => (ts.to_string(), CellClass::Temporal),
        Value::TimestampTz(ts) => (ts.to_rfc3339(), CellClass::Temporal),
        Value::Interval(d) => (format!("{} ms", d.num_milliseconds()), CellClass::Temporal),
        Value::Uuid(u) => (u.to_string(), CellClass::Text),
        Value::Json(j) => (j.to_string(), CellClass::Structured),
        Value::Native { display_text, .. } => (display_text.clone(), CellClass::Text),
    };
    CellRender { text, class }
}

fn single_line_text(text: &SharedString) -> SharedString {
    if !text.contains('\n') && !text.contains('\r') {
        return text.clone();
    }
    text.chars()
        .map(|character| match character {
            '\n' | '\r' => ' ',
            other => other,
        })
        .collect::<String>()
        .into()
}

/// The result tabs a query item owns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResultTab {
    Data,
    Messages,
    Explain,
    History,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MessageSeverity {
    Info,
    Warning,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ResultMessage {
    severity: MessageSeverity,
    text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ResultPlacement {
    Bottom,
    Right,
}

impl ResultTab {
    const ALL: [ResultTab; 4] = [
        ResultTab::Data,
        ResultTab::Messages,
        ResultTab::Explain,
        ResultTab::History,
    ];

    fn label(self) -> &'static str {
        match self {
            ResultTab::Data => "Data",
            ResultTab::Messages => "Messages",
            ResultTab::Explain => "Explain",
            ResultTab::History => "History",
        }
    }
}

actions!(
    sift_results,
    [
        CopySelectedCell,
        CopySelectedWithHeaders,
        EditSelectedCell,
        ToggleVisualSelection,
        ExitVisualSelection,
        PasteSelectedCell,
        RevertSelectedCell,
        MoveCellLeft,
        MoveCellRight,
        MoveCellUp,
        MoveCellDown,
        PreviousResultTab,
        NextResultTab
    ]
);

/// What consuming one streamed page did, so the dispatcher knows whether to
/// acknowledge it and let the server send the next one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamProgress {
    /// The page was consumed; acknowledge it.
    Consumed,
    /// The retained window is full. Nothing was consumed and the page must be
    /// held unacknowledged until the user asks for the next window, which is
    /// what keeps a large result from streaming through the client unseen.
    WindowFull,
    /// The stream ended.
    Terminal,
}

/// Raised for the owning query tab. The results surface never reaches the
/// executor itself; it reports intent and the workspace dispatches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResultsEvent {
    /// Discard the retained window and consume the held page onwards.
    LoadNextWindowRequested,
    /// Present this live Data grid in the workspace's near-fullscreen modal.
    OpenDataModalRequested,
    ExportRequested {
        format: sift_protocol::ExportFormat,
    },
    EditSelectedCellRequested,
    PasteSelectedCellRequested {
        text: String,
    },
    RevertSelectedCellRequested,
    ReviewStagedEditsRequested,
    SubmitCellEdit {
        text: String,
    },
    CancelCellEdit,
    SelectionChanged,
    RowJsonViewerChanged,
    OpenSelectedRowJsonRequested,
    /// Explain the editor's targeted statement. Analyze is explicit because it
    /// executes the statement to collect runtime counters.
    ExplainRequested {
        analyze: bool,
    },
    CapturePlanRequested,
    OpenPlanCapturesRequested,
    HistoryRequested {
        cursor: Option<String>,
    },
    RerunHistory {
        sql: String,
    },
}

#[derive(Debug, Clone, Default)]
struct HistoryState {
    rows: Vec<QueryHistory>,
    next_cursor: Option<String>,
    loading: bool,
    error: Option<String>,
    loaded: bool,
}

#[derive(Debug, Clone)]
enum ExplainState {
    Empty,
    Pending { analyze: bool },
    Ready(Box<ExplainResponse>),
    Failed(String),
}

struct InlineCellEdit {
    row: usize,
    column: usize,
    input: Entity<TextInput>,
    _subscription: Subscription,
    error: bool,
    pending: bool,
}

#[derive(Debug, Clone)]
struct RenderedPlanNode {
    depth: usize,
    op: SharedString,
    relation: Option<SharedString>,
    estimated: SharedString,
    actual: Option<SharedString>,
}

fn flatten_plan(root: &PlanNode) -> Vec<RenderedPlanNode> {
    let mut rows = Vec::new();
    let mut stack = vec![(root, 0_usize)];
    while let Some((node, depth)) = stack.pop() {
        let estimated = [
            node.est_rows
                .map(|value| format!("{} rows", plan_number(value))),
            node.est_cost
                .map(|value| format!("cost {}", plan_number(value))),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join("  ·  ");
        let actual = [
            node.actual_rows
                .map(|value| format!("{} actual", plan_number(value))),
            node.actual_ms
                .map(|value| format!("{} ms", plan_number(value))),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join("  ·  ");
        rows.push(RenderedPlanNode {
            depth,
            op: node.op.clone().into(),
            relation: node.relation.clone().map(Into::into),
            estimated: estimated.into(),
            actual: (!actual.is_empty()).then(|| actual.into()),
        });
        stack.extend(
            node.children
                .iter()
                .rev()
                .map(|child| (child, depth.saturating_add(1))),
        );
    }
    rows
}

/// The query-owned results surface.
pub struct ResultsView {
    focus_handle: FocusHandle,
    state: ResultState,
    /// Display-ready values are built once per result update, not repeatedly
    /// for every visible row during scroll and selection paints.
    rendered_columns: Vec<CachedColumnRender>,
    rendered_rows: Vec<Vec<CachedCellRender>>,
    column_widths: Vec<f32>,
    /// Stable source-column indices in current display order. Visibility is a
    /// separate projection so excluded fields can return in the same position.
    column_order: Vec<usize>,
    included_columns: Vec<bool>,
    tab: ResultTab,
    selected: Option<GridSelection>,
    visual_selection: bool,
    editing_cell: Option<(usize, usize)>,
    inline_cell_edit: Option<InlineCellEdit>,
    staged_cells: HashMap<(usize, usize), Value>,
    restore_grid_focus: bool,
    query_started_at: Option<std::time::Instant>,
    row_json_filter_input: Entity<TextInput>,
    _row_json_filter_subscription: Subscription,
    row_json_folded: bool,
    row_json_wrapped: bool,
    context_menu_position: Option<Point<Pixels>>,
    messages: Vec<ResultMessage>,
    selected_message: Option<usize>,
    row_scroll_handle: UniformListScrollHandle,
    grid_scroll_handle: gpui::ScrollHandle,
    placement: ResultPlacement,
    bottom_height: f32,
    right_width: f32,
    stream_result_seen: bool,
    /// Absolute index of the first retained row within the whole result, so
    /// row numbers keep describing the result rather than the window.
    window_start: usize,
    /// A page is being held because the window is full.
    window_held: bool,
    explain: ExplainState,
    rendered_plan_nodes: Vec<RenderedPlanNode>,
    plan_scroll_handle: UniformListScrollHandle,
    large_view: bool,
    history: HistoryState,
}

impl ResultsView {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let row_json_filter_input = cx.new(|cx| {
            TextInput::new("", "Filter keys or /regex/", cx).aria_label("Filter selected row JSON")
        });
        let row_json_filter_subscription = cx.subscribe(
            &row_json_filter_input,
            |_, _, event: &TextInputEvent, cx| {
                if *event == TextInputEvent::Changed {
                    cx.emit(ResultsEvent::RowJsonViewerChanged);
                }
            },
        );
        Self {
            focus_handle: cx.focus_handle(),
            state: ResultState::Idle,
            rendered_columns: Vec::new(),
            rendered_rows: Vec::new(),
            column_widths: Vec::new(),
            column_order: Vec::new(),
            included_columns: Vec::new(),
            tab: ResultTab::Data,
            selected: None,
            visual_selection: false,
            editing_cell: None,
            inline_cell_edit: None,
            staged_cells: HashMap::new(),
            restore_grid_focus: false,
            query_started_at: None,
            row_json_filter_input,
            _row_json_filter_subscription: row_json_filter_subscription,
            row_json_folded: false,
            row_json_wrapped: false,
            context_menu_position: None,
            messages: Vec::new(),
            selected_message: None,
            row_scroll_handle: UniformListScrollHandle::new(),
            grid_scroll_handle: gpui::ScrollHandle::new(),
            placement: ResultPlacement::Bottom,
            bottom_height: 240.0,
            right_width: 420.0,
            stream_result_seen: false,
            window_start: 0,
            window_held: false,
            explain: ExplainState::Empty,
            rendered_plan_nodes: Vec::new(),
            plan_scroll_handle: UniformListScrollHandle::new(),
            large_view: false,
            history: HistoryState::default(),
        }
    }

    pub fn state(&self) -> &ResultState {
        &self.state
    }

    pub fn active_tab(&self) -> ResultTab {
        self.tab
    }

    pub(crate) fn focus_data(&mut self, cx: &mut Context<Self>) {
        self.select_tab(ResultTab::Data, cx);
    }

    pub fn set_large_view(&mut self, active: bool, cx: &mut Context<Self>) {
        if self.large_view != active {
            self.large_view = active;
            cx.notify();
        }
    }

    #[cfg(test)]
    pub(crate) fn selected_cell(&self) -> Option<(usize, usize)> {
        match self.selected {
            Some(GridSelection::Cell { row, column }) => self
                .visible_column_indices()
                .iter()
                .position(|source| *source == column)
                .map(|display_column| (row, display_column)),
            _ => None,
        }
    }

    #[cfg(test)]
    pub(crate) fn inline_cell_edit_status(&self, cx: &App) -> Option<(String, bool)> {
        self.inline_cell_edit
            .as_ref()
            .map(|edit| (edit.input.read(cx).text().to_string(), edit.pending))
    }

    #[cfg(test)]
    pub(crate) fn inline_cell_edit_focus(&self, cx: &App) -> Option<FocusHandle> {
        self.inline_cell_edit
            .as_ref()
            .map(|edit| edit.input.focus_handle(cx))
    }

    #[cfg(test)]
    pub(crate) fn staged_cell_count(&self) -> usize {
        self.staged_cells.len()
    }

    pub(crate) fn placement(&self) -> ResultPlacement {
        self.placement
    }

    pub(crate) fn selected_cell_edit(&self) -> Option<SelectedCellEdit> {
        let GridSelection::Cell { row, column } = self.selected? else {
            return None;
        };
        let data = self.state.ready()?;
        let selected = data.rows.get(row)?.values.get(column)?.clone();
        let column_name = data.columns.get(column)?.name.clone();
        let original_row = data
            .columns
            .iter()
            .zip(data.rows.get(row)?.values.iter())
            .map(|(column, value)| (column.name.clone(), value.clone()))
            .collect();
        Some(SelectedCellEdit {
            column: column_name,
            original: selected,
            original_row,
        })
    }

    pub(crate) fn apply_saved_cell_values<'a>(
        &mut self,
        edits: impl IntoIterator<Item = (&'a [(String, Value)], &'a str, &'a Value)>,
        cx: &mut Context<Self>,
    ) -> usize {
        // Resolve every coordinate against the unchanged result snapshot first.
        // Mutating one cell can otherwise make later edits on the same row fail
        // their original-row lookup and leave stale staged highlights behind.
        let resolved = {
            let Some(data) = self.state.ready() else {
                return 0;
            };
            edits
                .into_iter()
                .filter_map(|(original_row, column, value)| {
                    let column_index = data
                        .columns
                        .iter()
                        .position(|candidate| candidate.name == column)?;
                    let expected_columns = original_row
                        .iter()
                        .map(|(name, expected)| {
                            data.columns
                                .iter()
                                .position(|column| column.name == *name)
                                .map(|index| (index, expected))
                        })
                        .collect::<Option<Vec<_>>>()?;
                    let row_index = data.rows.iter().position(|row| {
                        expected_columns
                            .iter()
                            .all(|(index, expected)| row.values.get(*index) == Some(*expected))
                    })?;
                    Some((row_index, column_index, value.clone()))
                })
                .collect::<Vec<_>>()
        };
        let data = match &mut self.state {
            ResultState::Streaming(data) | ResultState::Ready(data) => data,
            _ => return 0,
        };
        for (row, column, value) in &resolved {
            data.rows[*row].values[*column] = value.clone();
        }
        for (row, column, value) in &resolved {
            self.staged_cells.remove(&(*row, *column));
            self.replace_rendered_cell(*row, *column, value);
        }
        cx.notify();
        resolved.len()
    }

    pub(crate) fn stage_cell_value(
        &mut self,
        original_row: &[(String, Value)],
        column: &str,
        value: Value,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(data) = self.state.ready() else {
            return false;
        };
        let Some(column_index) = data
            .columns
            .iter()
            .position(|candidate| candidate.name == column)
        else {
            return false;
        };
        let expected_columns = original_row
            .iter()
            .map(|(name, expected)| {
                data.columns
                    .iter()
                    .position(|column| column.name == *name)
                    .map(|index| (index, expected))
            })
            .collect::<Option<Vec<_>>>();
        let Some(expected_columns) = expected_columns else {
            return false;
        };
        let Some(row_index) = data.rows.iter().position(|row| {
            expected_columns
                .iter()
                .all(|(index, expected)| row.values.get(*index) == Some(*expected))
        }) else {
            return false;
        };
        self.staged_cells
            .insert((row_index, column_index), value.clone());
        self.replace_rendered_cell(row_index, column_index, &value);
        cx.notify();
        true
    }

    pub(crate) fn clear_staged_cells(&mut self, cx: &mut Context<Self>) {
        let coordinates = self.staged_cells.keys().copied().collect::<Vec<_>>();
        self.staged_cells.clear();
        let originals = self.state.ready().map(|data| {
            coordinates
                .iter()
                .filter_map(|(row, column)| {
                    data.rows
                        .get(*row)
                        .and_then(|row| row.values.get(*column))
                        .map(|value| (*row, *column, value.clone()))
                })
                .collect::<Vec<_>>()
        });
        for (row, column, value) in originals.unwrap_or_default() {
            self.replace_rendered_cell(row, column, &value);
        }
        cx.notify();
    }

    fn replace_rendered_cell(&mut self, row: usize, column: usize, value: &Value) {
        let Some(cell) = self
            .rendered_rows
            .get_mut(row)
            .and_then(|row| row.get_mut(column))
        else {
            return;
        };
        let rendered = render_value(value);
        let text: SharedString = rendered.text.into();
        *cell = CachedCellRender {
            paint_text: single_line_text(&text),
            text,
            class: rendered.class,
            shaped: None,
        };
    }

    pub fn selected_row_json(&self) -> Option<SelectedRowJson> {
        let row_index = match self.selected? {
            GridSelection::Cell { row, .. } | GridSelection::Row(row) => row,
            GridSelection::Range { focus_row, .. } => focus_row,
            GridSelection::Column(_) | GridSelection::All => 0,
        };
        let data = self.state.ready()?;
        let row = data.rows.get(row_index)?;
        let value = data
            .columns
            .iter()
            .zip(&row.values)
            .map(|(column, value)| (column.name.clone(), value_for_row_json(value)))
            .collect::<serde_json::Map<_, _>>()
            .into();
        Some(SelectedRowJson { row_index, value })
    }

    pub(crate) fn row_json_filter_input(&self) -> Entity<TextInput> {
        self.row_json_filter_input.clone()
    }

    pub(crate) fn row_json_folded(&self) -> bool {
        self.row_json_folded
    }

    pub(crate) fn row_json_wrapped(&self) -> bool {
        self.row_json_wrapped
    }

    pub(crate) fn toggle_row_json_folded(&mut self, cx: &mut Context<Self>) {
        self.row_json_folded = !self.row_json_folded;
        cx.notify();
    }

    pub(crate) fn toggle_row_json_wrapped(&mut self, cx: &mut Context<Self>) {
        self.row_json_wrapped = !self.row_json_wrapped;
        cx.notify();
    }

    pub(crate) fn begin_selected_cell_edit(
        &mut self,
        text: String,
        cx: &mut Context<Self>,
    ) -> Option<FocusHandle> {
        let GridSelection::Cell { row, column } = self.selected? else {
            return None;
        };
        let input = cx.new(|cx| {
            TextInput::new(text, "New cell value", cx).aria_label("Edit selected result cell")
        });
        let focus = input.focus_handle(cx);
        let subscription = cx.subscribe(&input, |view, _, event: &TextInputEvent, cx| {
            if *event == TextInputEvent::Submitted {
                view.submit_inline_cell_edit(cx);
            }
        });
        self.inline_cell_edit = Some(InlineCellEdit {
            row,
            column,
            input,
            _subscription: subscription,
            error: false,
            pending: false,
        });
        self.editing_cell = Some((row, column));
        cx.notify();
        Some(focus)
    }

    pub(crate) fn mark_selected_cell_editing(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(GridSelection::Cell { row, column }) = self.selected else {
            return false;
        };
        self.editing_cell = Some((row, column));
        cx.notify();
        true
    }

    #[cfg(test)]
    pub(crate) fn set_inline_cell_edit_pending(&mut self, pending: bool, cx: &mut Context<Self>) {
        if let Some(edit) = self.inline_cell_edit.as_mut() {
            if edit.pending != pending {
                edit.pending = pending;
                cx.notify();
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn set_inline_cell_edit_text(&mut self, text: &str, cx: &mut Context<Self>) {
        if let Some(edit) = self.inline_cell_edit.as_ref() {
            if edit.input.read(cx).text() != text {
                edit.input
                    .update(cx, |input, cx| input.set_text(text.to_owned(), cx));
            }
        }
    }

    pub(crate) fn set_inline_cell_edit_error(&mut self, cx: &mut Context<Self>) {
        if let Some(edit) = self.inline_cell_edit.as_mut() {
            edit.error = true;
            edit.pending = false;
            cx.notify();
        }
    }

    pub(crate) fn finish_inline_cell_edit(&mut self, cx: &mut Context<Self>) {
        let had_inline_edit = self.inline_cell_edit.take().is_some();
        let had_editing_cell = self.editing_cell.take().is_some();
        let needs_notify = had_inline_edit || had_editing_cell || !self.restore_grid_focus;
        self.restore_grid_focus = true;
        if needs_notify {
            cx.notify();
        }
    }

    fn submit_inline_cell_edit(&mut self, cx: &mut Context<Self>) {
        let Some(edit) = self.inline_cell_edit.as_ref() else {
            return;
        };
        if edit.pending {
            return;
        }
        cx.emit(ResultsEvent::SubmitCellEdit {
            text: edit.input.read(cx).text().to_string(),
        });
    }

    fn cancel_inline_cell_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.inline_cell_edit.take().is_none() {
            return;
        }
        self.editing_cell = None;
        self.focus_handle.focus(window, cx);
        cx.emit(ResultsEvent::CancelCellEdit);
        cx.notify();
    }

    pub(crate) fn extent(&self) -> f32 {
        match self.placement {
            ResultPlacement::Bottom => self.bottom_height,
            ResultPlacement::Right => self.right_width,
        }
    }

    pub(crate) fn set_extent(&mut self, extent: f32, cx: &mut Context<Self>) {
        match self.placement {
            ResultPlacement::Bottom => self.bottom_height = extent,
            ResultPlacement::Right => self.right_width = extent,
        }
        cx.notify();
    }

    #[cfg(test)]
    pub(crate) fn toggle_placement(&mut self, cx: &mut Context<Self>) {
        self.placement = match self.placement {
            ResultPlacement::Bottom => ResultPlacement::Right,
            ResultPlacement::Right => ResultPlacement::Bottom,
        };
        cx.notify();
    }

    /// Adopt a new outcome, resetting selection and opening failures as text.
    pub fn set_state(&mut self, mut state: ResultState, cx: &mut Context<Self>) {
        match &mut state {
            ResultState::Pending => self.query_started_at = Some(std::time::Instant::now()),
            ResultState::Streaming(_) => {
                self.query_started_at
                    .get_or_insert_with(std::time::Instant::now);
            }
            ResultState::Ready(data) => {
                if data.duration_ms.is_none() {
                    data.duration_ms = self.query_started_at.take().map(|started| {
                        u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
                    });
                } else {
                    self.query_started_at = None;
                }
            }
            _ => self.query_started_at = None,
        }
        self.rendered_columns = match &state {
            ResultState::Streaming(data) | ResultState::Ready(data) => data
                .columns
                .iter()
                .map(|column| CachedColumnRender {
                    name: column.name.clone().into(),
                    type_label: column.type_label.clone().into(),
                    nullable: column.nullable,
                })
                .collect(),
            _ => Vec::new(),
        };
        self.rendered_rows = match &state {
            ResultState::Streaming(data) | ResultState::Ready(data) => data
                .rows
                .iter()
                .map(|row| {
                    row.values
                        .iter()
                        .map(|value| {
                            let rendered = render_value(value);
                            let text: SharedString = rendered.text.into();
                            CachedCellRender {
                                paint_text: single_line_text(&text),
                                text,
                                class: rendered.class,
                                shaped: None,
                            }
                        })
                        .collect()
                })
                .collect(),
            _ => Vec::new(),
        };
        self.column_widths = vec![DEFAULT_COLUMN_WIDTH; self.rendered_columns.len()];
        self.column_order = (0..self.rendered_columns.len()).collect();
        self.included_columns = vec![true; self.rendered_columns.len()];
        self.messages = Self::messages_for_state(&state);
        self.selected_message = None;
        self.tab = if Self::is_error_state(&state) {
            ResultTab::Messages
        } else {
            ResultTab::Data
        };
        self.state = state;
        self.stream_result_seen = false;
        self.window_start = 0;
        self.window_held = false;
        self.selected = None;
        self.inline_cell_edit = None;
        self.staged_cells.clear();
        cx.notify();
    }

    fn is_error_state(state: &ResultState) -> bool {
        matches!(
            state,
            ResultState::Unavailable(_)
                | ResultState::Failed(_)
                | ResultState::TimedOut
                | ResultState::OutcomeUnknown
        )
    }

    fn messages_for_state(state: &ResultState) -> Vec<ResultMessage> {
        let mut messages = Vec::new();
        match state {
            ResultState::Streaming(data) | ResultState::Ready(data) => {
                if let Some(affected) = data.affected_rows {
                    messages.push(ResultMessage {
                        severity: MessageSeverity::Info,
                        text: format!("{affected} row(s) affected"),
                    });
                }
                if data.truncated_extra_results {
                    messages.push(ResultMessage {
                        severity: MessageSeverity::Warning,
                        text: "Additional result sets were truncated to the first.".into(),
                    });
                }
                messages.extend(data.warnings.iter().map(|warning| ResultMessage {
                    severity: MessageSeverity::Warning,
                    text: warning.message.clone(),
                }));
            }
            ResultState::Unavailable(message) | ResultState::Failed(message) => {
                messages.push(ResultMessage {
                    severity: MessageSeverity::Error,
                    text: message.clone(),
                });
            }
            ResultState::TimedOut => messages.push(ResultMessage {
                severity: MessageSeverity::Error,
                text: "Query timed out".into(),
            }),
            ResultState::OutcomeUnknown => messages.push(ResultMessage {
                severity: MessageSeverity::Error,
                text: "Query outcome is unknown".into(),
            }),
            ResultState::Cancelled => messages.push(ResultMessage {
                severity: MessageSeverity::Info,
                text: "Query cancelled".into(),
            }),
            ResultState::Detached(reference) => messages.push(ResultMessage {
                severity: MessageSeverity::Info,
                text: format!(
                    "This tab last returned {}{} row(s). Result data is never saved \
                     locally, so re-run the query to see it.",
                    reference.row_count,
                    if reference.has_more { "+" } else { "" }
                ),
            }),
            ResultState::Idle | ResultState::Pending => {}
        }
        messages
    }

    /// Seed a restored tab with the reference to its last run. Kept separate
    /// from `set_state` so restoring never selects the Messages tab or clears
    /// a live result that arrived first.
    pub fn restore_reference(&mut self, reference: ResultReference, cx: &mut Context<Self>) {
        if !matches!(self.state, ResultState::Idle) {
            return;
        }
        self.state = ResultState::Detached(reference);
        self.messages = Self::messages_for_state(&self.state);
        self.selected_message = None;
        cx.notify();
    }

    pub fn set_pending(&mut self, cx: &mut Context<Self>) {
        self.set_state(ResultState::Pending, cx);
    }

    /// Reset this surface for a cursor-backed stream. Subsequent pages append
    /// incrementally and retain at most [`MAX_RETAINED_ROWS`].
    pub fn begin_stream(&mut self, cx: &mut Context<Self>) {
        self.set_state(ResultState::Streaming(ResultData::default()), cx);
    }

    /// Consume one server page.
    ///
    /// A page is taken whole or not at all. Splitting one would mean carrying a
    /// row remainder across the pause, and the retained bound is a memory
    /// budget rather than an exact window size, so a window may end slightly
    /// short of [`MAX_RETAINED_ROWS`] instead.
    pub fn apply_stream_page(&mut self, page: Page, cx: &mut Context<Self>) -> StreamProgress {
        if !matches!(self.state, ResultState::Streaming(_)) {
            self.begin_stream(cx);
        }

        match page {
            Page::NextResult { columns } => {
                let ResultState::Streaming(data) = &mut self.state else {
                    unreachable!("stream initialized above")
                };
                if self.stream_result_seen {
                    data.truncated_extra_results = true;
                } else {
                    self.stream_result_seen = true;
                    data.columns = columns.iter().map(ResultColumn::from_metadata).collect();
                    self.rendered_columns = columns
                        .iter()
                        .map(|column| CachedColumnRender {
                            name: column.name.clone().into(),
                            type_label: ResultColumn::from_metadata(column).type_label.into(),
                            nullable: matches!(column.nullable, Nullability::Nullable),
                        })
                        .collect();
                    self.column_widths = vec![DEFAULT_COLUMN_WIDTH; self.rendered_columns.len()];
                    self.column_order = (0..self.rendered_columns.len()).collect();
                    self.included_columns = vec![true; self.rendered_columns.len()];
                }
                cx.notify();
                StreamProgress::Consumed
            }
            Page::Rows { rows } => {
                let ResultState::Streaming(data) = &mut self.state else {
                    unreachable!("stream initialized above")
                };
                if data.truncated_extra_results {
                    cx.notify();
                    return StreamProgress::Consumed;
                }
                if data.rows.len() + rows.len() > MAX_RETAINED_ROWS && !data.rows.is_empty() {
                    self.window_held = true;
                    data.has_more = true;
                    cx.notify();
                    return StreamProgress::WindowFull;
                }
                for row in rows {
                    self.rendered_rows.push(
                        row.values
                            .iter()
                            .map(|value| {
                                let rendered = render_value(value);
                                let text: SharedString = rendered.text.into();
                                CachedCellRender {
                                    paint_text: single_line_text(&text),
                                    text,
                                    class: rendered.class,
                                    shaped: None,
                                }
                            })
                            .collect(),
                    );
                    data.rows.push(row);
                }
                cx.notify();
                StreamProgress::Consumed
            }
            Page::Error { error } => {
                let state = match error.code {
                    sift_protocol::Code::QueryCanceled => ResultState::Cancelled,
                    sift_protocol::Code::QueryTimedOut => ResultState::TimedOut,
                    _ => ResultState::Failed(error.message),
                };
                self.set_state(state, cx);
                StreamProgress::Terminal
            }
            Page::Done {
                affected_rows,
                warnings,
            } => {
                let state = std::mem::replace(&mut self.state, ResultState::Idle);
                let mut data = match state {
                    ResultState::Streaming(data) => data,
                    _ => unreachable!("stream initialized above"),
                };
                data.affected_rows = affected_rows;
                data.duration_ms = self.query_started_at.take().map(|started| {
                    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
                });
                data.warnings = warnings;
                self.state = ResultState::Ready(data);
                self.messages = Self::messages_for_state(&self.state);
                self.selected_message = None;
                self.stream_result_seen = false;
                self.window_held = false;
                cx.notify();
                StreamProgress::Terminal
            }
        }
    }

    /// True while a page is held because the retained window is full.
    pub fn window_held(&self) -> bool {
        self.window_held
    }

    /// Absolute row numbers currently retained, 1-based and inclusive. `None`
    /// when no rows are retained.
    pub fn window_rows(&self) -> Option<(usize, usize)> {
        let count = self.state.ready().map_or(0, |data| data.rows.len());
        (count > 0).then(|| (self.window_start + 1, self.window_start + count))
    }

    /// Drop the retained window and continue from where the stream paused.
    ///
    /// Server cursors are forward-only (ADR-011), so this is explicitly a
    /// one-way move: the previous window cannot be scrolled back to without
    /// re-running the query, and the UI says so rather than implying paging.
    pub fn advance_window(&mut self, cx: &mut Context<Self>) -> bool {
        if !self.window_held {
            return false;
        }
        let ResultState::Streaming(data) = &mut self.state else {
            return false;
        };
        self.window_start += data.rows.len();
        data.rows.clear();
        self.rendered_rows.clear();
        self.window_held = false;
        self.selected = None;
        self.row_scroll_handle
            .scroll_to_item(0, ScrollStrategy::Top);
        cx.notify();
        true
    }

    pub fn set_unavailable(&mut self, reason: impl Into<String>, cx: &mut Context<Self>) {
        self.set_state(ResultState::Unavailable(reason.into()), cx);
    }

    fn select_tab(&mut self, tab: ResultTab, cx: &mut Context<Self>) {
        self.tab = if tab == ResultTab::Data && Self::is_error_state(&self.state) {
            ResultTab::Messages
        } else {
            tab
        };
        if self.tab == ResultTab::History && !self.history.loaded && !self.history.loading {
            self.request_history(None, cx);
        }
        cx.notify();
    }

    fn request_history(&mut self, cursor: Option<String>, cx: &mut Context<Self>) {
        if self.history.loading {
            return;
        }
        self.history.loading = true;
        self.history.error = None;
        cx.emit(ResultsEvent::HistoryRequested { cursor });
        cx.notify();
    }

    fn refresh_history(&mut self, cx: &mut Context<Self>) {
        self.request_history(None, cx);
    }

    pub fn set_history_page(
        &mut self,
        page: Result<sift_protocol::CursorPage<QueryHistory>, String>,
        append: bool,
        cx: &mut Context<Self>,
    ) {
        self.history.loading = false;
        self.history.loaded = true;
        match page {
            Ok(page) => {
                if append {
                    self.history.rows.extend(page.items);
                } else {
                    self.history.rows = page.items;
                }
                self.history.next_cursor = page.next_cursor;
                self.history.error = None;
            }
            Err(message) => self.history.error = Some(message),
        }
        cx.notify();
    }

    pub(crate) fn select_relative_tab(&mut self, delta: isize, cx: &mut Context<Self>) {
        let current = ResultTab::ALL
            .iter()
            .position(|tab| *tab == self.tab)
            .unwrap_or(0);
        let next = (current as isize + delta).rem_euclid(ResultTab::ALL.len() as isize) as usize;
        self.select_tab(ResultTab::ALL[next], cx);
    }

    fn request_explain(&mut self, analyze: bool, cx: &mut Context<Self>) {
        if matches!(self.explain, ExplainState::Pending { .. }) {
            return;
        }
        self.explain = ExplainState::Pending { analyze };
        self.tab = ResultTab::Explain;
        cx.emit(ResultsEvent::ExplainRequested { analyze });
        cx.notify();
    }

    pub fn set_explain_result(
        &mut self,
        result: Result<Box<ExplainResponse>, String>,
        cx: &mut Context<Self>,
    ) {
        self.rendered_plan_nodes = result
            .as_ref()
            .map(|response| flatten_plan(&response.root))
            .unwrap_or_default();
        self.plan_scroll_handle
            .scroll_to_item(0, ScrollStrategy::Top);
        self.explain = match result {
            Ok(response) => ExplainState::Ready(response),
            Err(message) => ExplainState::Failed(message),
        };
        self.tab = ResultTab::Explain;
        cx.notify();
    }

    #[cfg(test)]
    pub(crate) fn explain_failure(&self) -> Option<&str> {
        match &self.explain {
            ExplainState::Failed(message) => Some(message),
            ExplainState::Empty | ExplainState::Pending { .. } | ExplainState::Ready(_) => None,
        }
    }

    fn copy_raw_plan(&mut self, cx: &mut Context<Self>) {
        if let ExplainState::Ready(response) = &self.explain {
            cx.write_to_clipboard(ClipboardItem::new_string(response.raw.clone()));
        }
    }

    fn select_message(&mut self, index: usize, cx: &mut Context<Self>) {
        self.selected_message = (self.selected_message != Some(index)).then_some(index);
        cx.notify();
    }

    fn copy_message(&mut self, index: usize, cx: &mut Context<Self>) {
        if let Some(message) = self.messages.get(index) {
            cx.write_to_clipboard(ClipboardItem::new_string(message.text.clone()));
        }
    }

    fn copy_selected_message(&mut self, cx: &mut Context<Self>) {
        if let Some(index) = self.selected_message {
            self.copy_message(index, cx);
        }
    }

    fn copy_all_errors(&mut self, cx: &mut Context<Self>) {
        let errors = self
            .messages
            .iter()
            .filter(|message| message.severity == MessageSeverity::Error)
            .map(|message| message.text.as_str())
            .collect::<Vec<_>>();
        if !errors.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(errors.join("\n")));
        }
    }

    #[cfg(test)]
    pub(crate) fn select_cell(&mut self, row: usize, column: usize, cx: &mut Context<Self>) {
        self.toggle_selection(GridSelection::Cell { row, column }, cx);
    }

    fn extend_cell_selection(&mut self, row: usize, column: usize, cx: &mut Context<Self>) {
        let (anchor_row, anchor_column) = match self.selected {
            Some(GridSelection::Cell { row, column }) => (row, column),
            Some(GridSelection::Range {
                anchor_row,
                anchor_column,
                ..
            }) => (anchor_row, anchor_column),
            _ => {
                self.set_selection(GridSelection::Cell { row, column }, cx);
                return;
            }
        };
        self.set_selection(
            GridSelection::Range {
                anchor_row,
                anchor_column,
                focus_row: row,
                focus_column: column,
            },
            cx,
        );
    }

    fn select_row(&mut self, row: usize, cx: &mut Context<Self>) {
        self.toggle_selection(GridSelection::Row(row), cx);
    }

    fn select_column(&mut self, column: usize, cx: &mut Context<Self>) {
        self.toggle_selection(GridSelection::Column(column), cx);
    }

    fn select_all(&mut self, cx: &mut Context<Self>) {
        self.toggle_selection(GridSelection::All, cx);
    }

    fn toggle_selection(&mut self, selection: GridSelection, cx: &mut Context<Self>) {
        self.selected = (self.selected != Some(selection)).then_some(selection);
        cx.emit(ResultsEvent::SelectionChanged);
        cx.notify();
    }

    fn set_selection(&mut self, selection: GridSelection, cx: &mut Context<Self>) {
        if self.selected != Some(selection) {
            self.selected = Some(selection);
            cx.emit(ResultsEvent::SelectionChanged);
            cx.notify();
        }
    }

    fn set_column_width(&mut self, column: usize, width: f32, cx: &mut Context<Self>) {
        let Some(current) = self.column_widths.get_mut(column) else {
            return;
        };
        let width = width.clamp(MIN_COLUMN_WIDTH, MAX_COLUMN_WIDTH).round();
        if *current != width {
            *current = width;
            cx.notify();
        }
    }

    fn resize_column(
        &mut self,
        event: &DragMoveEvent<ColumnResizeDrag>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let column = event.drag(cx).column;
        let pointer_x: f32 = (event.event.position.x - event.bounds.left()).into();
        let left_edge = ROW_NUMBER_WIDTH
            + self
                .visible_column_indices()
                .into_iter()
                .take_while(|visible| *visible != column)
                .map(|visible| {
                    self.column_widths
                        .get(visible)
                        .copied()
                        .unwrap_or(DEFAULT_COLUMN_WIDTH)
                })
                .sum::<f32>();
        self.set_column_width(column, pointer_x - left_edge, cx);
    }

    fn visible_column_indices(&self) -> Vec<usize> {
        self.column_order
            .iter()
            .copied()
            .filter(|column| self.included_columns.get(*column).copied().unwrap_or(false))
            .collect()
    }

    pub(crate) fn set_column_included(
        &mut self,
        column: usize,
        included: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(current) = self.included_columns.get_mut(column) else {
            return;
        };
        if *current == included {
            return;
        }
        *current = included;
        if !included
            && self
                .selected
                .is_some_and(|selection| selection.highlights_column(column))
        {
            self.selected = None;
        }
        cx.notify();
    }

    pub(crate) fn set_all_columns_included(&mut self, included: bool, cx: &mut Context<Self>) {
        self.included_columns.fill(included);
        if !included
            && matches!(
                self.selected,
                Some(
                    GridSelection::Cell { .. }
                        | GridSelection::Range { .. }
                        | GridSelection::Column(_)
                )
            )
        {
            self.selected = None;
        }
        cx.notify();
    }

    pub(crate) fn inspector_fields(&self) -> Vec<ResultFieldInspectorRow> {
        self.column_order
            .iter()
            .filter_map(|source_column| {
                let column = self.rendered_columns.get(*source_column)?;
                Some(ResultFieldInspectorRow {
                    source_column: *source_column,
                    name: column.name.clone(),
                    type_label: column.type_label.clone(),
                    nullable: column.nullable,
                    included: self
                        .included_columns
                        .get(*source_column)
                        .copied()
                        .unwrap_or(false),
                })
            })
            .collect()
    }

    /// Move one source column onto another in display order. Selection stores
    /// source coordinates, so logical cells remain selected without remapping.
    fn reorder_column(&mut self, source: usize, target: usize, cx: &mut Context<Self>) {
        if source == target {
            return;
        }
        let Some(source_position) = self
            .column_order
            .iter()
            .position(|column| *column == source)
        else {
            return;
        };
        let Some(target_position) = self
            .column_order
            .iter()
            .position(|column| *column == target)
        else {
            return;
        };
        let column = self.column_order.remove(source_position);
        self.column_order
            .insert(target_position.min(self.column_order.len()), column);
        cx.notify();
    }

    fn move_selection(&mut self, row_delta: isize, column_delta: isize, cx: &mut Context<Self>) {
        let Some(data) = self.state.ready() else {
            return;
        };
        let visible_columns = self.visible_column_indices();
        if data.rows.is_empty() || visible_columns.is_empty() {
            return;
        }
        let (row, column) = match self.selected {
            Some(GridSelection::Cell { row, column }) => (row, column),
            Some(GridSelection::Range {
                focus_row,
                focus_column,
                ..
            }) => (focus_row, focus_column),
            Some(GridSelection::Row(row)) => (row, visible_columns[0]),
            Some(GridSelection::Column(column)) => (0, column),
            Some(GridSelection::All) | None => (0, visible_columns[0]),
        };
        let previous_row = row;
        let previous_column = column;
        let row = row
            .saturating_add_signed(row_delta)
            .min(data.rows.len() - 1);
        let display_column = visible_columns
            .iter()
            .position(|visible| *visible == column)
            .unwrap_or(0)
            .saturating_add_signed(column_delta)
            .min(visible_columns.len() - 1);
        let column = visible_columns[display_column];
        if self.visual_selection {
            let (anchor_row, anchor_column) = match self.selected {
                Some(GridSelection::Range {
                    anchor_row,
                    anchor_column,
                    ..
                }) => (anchor_row, anchor_column),
                _ => (previous_row, previous_column),
            };
            self.set_selection(
                GridSelection::Range {
                    anchor_row,
                    anchor_column,
                    focus_row: row,
                    focus_column: column,
                },
                cx,
            );
        } else {
            self.set_selection(GridSelection::Cell { row, column }, cx);
        }
        // Most repeated arrow events stay inside the viewport. Do not make the
        // uniform list resolve a deferred scroll request and relayout its rows
        // until selection actually crosses a visible edge.
        if row != previous_row && self.row_needs_reveal(row) {
            self.row_scroll_handle
                .scroll_to_item(row, ScrollStrategy::Nearest);
        }
        if column != previous_column {
            self.reveal_column(column, cx);
        }
    }

    fn reveal_column(&mut self, source_column: usize, cx: &mut Context<Self>) {
        let visible_columns = self.visible_column_indices();
        let Some(display_column) = visible_columns
            .iter()
            .position(|column| *column == source_column)
        else {
            return;
        };
        let viewport_width = self.grid_scroll_handle.bounds().size.width;
        if viewport_width <= px(0.) {
            return;
        }
        let column_left = px(ROW_NUMBER_WIDTH
            + visible_columns
                .iter()
                .take(display_column)
                .map(|column| {
                    self.column_widths
                        .get(*column)
                        .copied()
                        .unwrap_or(DEFAULT_COLUMN_WIDTH)
                })
                .sum::<f32>());
        let column_right = column_left
            + px(self
                .column_widths
                .get(source_column)
                .copied()
                .unwrap_or(DEFAULT_COLUMN_WIDTH));
        let current = self.grid_scroll_handle.offset();
        let visible_left = -current.x;
        let visible_right = visible_left + viewport_width;
        let target_left = if column_left < visible_left {
            column_left
        } else if column_right > visible_right {
            column_right - viewport_width
        } else {
            return;
        };
        let max = self.grid_scroll_handle.max_offset();
        let next = gpui::point((-target_left).clamp(-max.x, px(0.)), current.y);
        if next != current {
            self.grid_scroll_handle.set_offset(next);
            cx.notify();
        }
    }

    fn select_cell_from_pointer(
        &mut self,
        row: usize,
        column: usize,
        shift: bool,
        click_count: usize,
        cx: &mut Context<Self>,
    ) -> bool {
        if click_count >= 2 {
            self.set_selection(GridSelection::Cell { row, column }, cx);
            let inline_editor_open = self
                .inline_cell_edit
                .as_ref()
                .is_some_and(|edit| edit.row == row && edit.column == column);
            if !inline_editor_open {
                cx.emit(ResultsEvent::EditSelectedCellRequested);
                return true;
            }
        } else if shift {
            self.extend_cell_selection(row, column, cx);
        } else {
            // Pointer selection is idempotent. Toggling the current cell off
            // made a subsequent double-click lose its edit target.
            self.set_selection(GridSelection::Cell { row, column }, cx);
        }
        false
    }

    fn open_cell_context_menu(
        &mut self,
        row: usize,
        column: usize,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.focus_handle.focus(window, cx);
        self.set_selection(GridSelection::Cell { row, column }, cx);
        self.context_menu_position = Some(position);
        cx.notify();
    }

    fn row_needs_reveal(&self, row: usize) -> bool {
        let state = self.row_scroll_handle.0.borrow();
        let viewport_height = state.base_handle.bounds().size.height;
        if viewport_height <= px(0.) {
            return true;
        }
        let visible_top = -state.base_handle.offset().y;
        let visible_bottom = visible_top + viewport_height;
        let row_top = px(ROW_HEIGHT * row as f32);
        let row_bottom = row_top + px(ROW_HEIGHT);
        row_top < visible_top || row_bottom > visible_bottom
    }

    fn move_cell_left(&mut self, _: &MoveCellLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.move_selection(0, -1, cx);
    }

    fn move_cell_right(&mut self, _: &MoveCellRight, _: &mut Window, cx: &mut Context<Self>) {
        self.move_selection(0, 1, cx);
    }

    fn move_cell_up(&mut self, _: &MoveCellUp, _: &mut Window, cx: &mut Context<Self>) {
        self.move_selection(-1, 0, cx);
    }

    fn move_cell_down(&mut self, _: &MoveCellDown, _: &mut Window, cx: &mut Context<Self>) {
        self.move_selection(1, 0, cx);
    }

    fn edit_selected_cell(&mut self, _: &EditSelectedCell, _: &mut Window, cx: &mut Context<Self>) {
        if matches!(self.selected, Some(GridSelection::Cell { .. })) {
            cx.emit(ResultsEvent::EditSelectedCellRequested);
        }
    }

    fn toggle_visual_selection(
        &mut self,
        _: &ToggleVisualSelection,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.visual_selection = !self.visual_selection;
        if self.visual_selection {
            if self.selected.is_none() {
                self.move_selection(0, 0, cx);
            }
        } else if let Some(GridSelection::Range {
            focus_row,
            focus_column,
            ..
        }) = self.selected
        {
            self.set_selection(
                GridSelection::Cell {
                    row: focus_row,
                    column: focus_column,
                },
                cx,
            );
        }
        cx.notify();
    }

    fn exit_visual_selection(
        &mut self,
        _: &ExitVisualSelection,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.visual_selection = false;
        if let Some(GridSelection::Range {
            focus_row,
            focus_column,
            ..
        }) = self.selected
        {
            self.set_selection(
                GridSelection::Cell {
                    row: focus_row,
                    column: focus_column,
                },
                cx,
            );
        }
        cx.notify();
    }

    fn paste_selected_cell(
        &mut self,
        _: &PasteSelectedCell,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !matches!(self.selected, Some(GridSelection::Cell { .. })) {
            return;
        }
        if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
            cx.emit(ResultsEvent::PasteSelectedCellRequested { text });
        }
    }

    fn revert_selected_cell(
        &mut self,
        _: &RevertSelectedCell,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if matches!(self.selected, Some(GridSelection::Cell { .. })) {
            cx.emit(ResultsEvent::RevertSelectedCellRequested);
        }
    }

    fn previous_result_tab(
        &mut self,
        _: &PreviousResultTab,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.select_relative_tab(-1, cx);
    }

    fn next_result_tab(&mut self, _: &NextResultTab, _: &mut Window, cx: &mut Context<Self>) {
        self.select_relative_tab(1, cx);
    }

    fn copy_selected_cell(&mut self, _: &CopySelectedCell, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = self.selected_text() {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
            self.visual_selection = false;
            cx.notify();
        }
    }

    fn copy_selected_with_headers(
        &mut self,
        _: &CopySelectedWithHeaders,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(text) = self.selected_text_with_headers() {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
    }

    fn range_coordinates(
        &self,
        anchor_row: usize,
        anchor_column: usize,
        focus_row: usize,
        focus_column: usize,
    ) -> (std::ops::RangeInclusive<usize>, Vec<usize>) {
        let visible = self.visible_column_indices();
        let anchor = visible
            .iter()
            .position(|column| *column == anchor_column)
            .unwrap_or(0);
        let focus = visible
            .iter()
            .position(|column| *column == focus_column)
            .unwrap_or(anchor);
        (
            anchor_row.min(focus_row)..=anchor_row.max(focus_row),
            visible[anchor.min(focus)..=anchor.max(focus)].to_vec(),
        )
    }

    fn selected_text(&self) -> Option<String> {
        let visible_columns = self.visible_column_indices();
        let row_text = |row: &[CachedCellRender]| {
            visible_columns
                .iter()
                .filter_map(|column| row.get(*column))
                .map(|cell| cell.text.to_string())
                .collect::<Vec<_>>()
                .join("\t")
        };
        match self.selected? {
            GridSelection::Cell { row, column } => self
                .rendered_rows
                .get(row)?
                .get(column)
                .map(|cell| cell.text.to_string()),
            GridSelection::Range {
                anchor_row,
                anchor_column,
                focus_row,
                focus_column,
            } => {
                let (rows, columns) =
                    self.range_coordinates(anchor_row, anchor_column, focus_row, focus_column);
                Some(
                    rows.filter_map(|row| self.rendered_rows.get(row))
                        .map(|row| {
                            columns
                                .iter()
                                .filter_map(|column| row.get(*column))
                                .map(|cell| cell.text.to_string())
                                .collect::<Vec<_>>()
                                .join("\t")
                        })
                        .collect::<Vec<_>>()
                        .join("\n"),
                )
            }
            GridSelection::Row(row) if !visible_columns.is_empty() => {
                self.rendered_rows.get(row).map(|row| row_text(row))
            }
            GridSelection::Row(_) => None,
            GridSelection::Column(column) => {
                let values = self
                    .rendered_rows
                    .iter()
                    .filter_map(|row| row.get(column))
                    .map(|cell| cell.text.to_string())
                    .collect::<Vec<_>>();
                (!values.is_empty()).then(|| values.join("\n"))
            }
            GridSelection::All => (!visible_columns.is_empty()).then(|| {
                self.rendered_rows
                    .iter()
                    .map(|row| row_text(row))
                    .collect::<Vec<_>>()
                    .join("\n")
            }),
        }
    }

    fn selected_text_with_headers(&self) -> Option<String> {
        let columns = match self.selected? {
            GridSelection::Cell { column, .. } | GridSelection::Column(column) => vec![column],
            GridSelection::Range {
                anchor_row,
                anchor_column,
                focus_row,
                focus_column,
            } => {
                self.range_coordinates(anchor_row, anchor_column, focus_row, focus_column)
                    .1
            }
            GridSelection::Row(_) | GridSelection::All => self.visible_column_indices(),
        };
        if columns.is_empty() {
            return None;
        }
        let headers = columns
            .iter()
            .filter_map(|column| self.rendered_columns.get(*column))
            .map(|column| column.name.to_string())
            .collect::<Vec<_>>()
            .join("\t");
        self.selected_text()
            .map(|values| format!("{headers}\n{values}"))
    }

    fn cell_color(colors: ThemeColors, class: CellClass) -> gpui::Hsla {
        match class {
            CellClass::Null => colors.muted_text,
            CellClass::Number => colors.syntax_number,
            CellClass::Temporal => colors.syntax_string,
            CellClass::Bool => colors.accent,
            CellClass::Binary => colors.muted_text,
            CellClass::Structured => colors.warning,
            CellClass::Text => colors.text,
        }
    }

    fn shape_cell(
        cell: &mut CachedCellRender,
        color: gpui::Hsla,
        window: &Window,
    ) -> (ShapedLine, bool) {
        let mut text_style = window.text_style();
        text_style.color = color;
        let font_size = text_style.font_size.to_pixels(window.rem_size());
        let run = text_style.to_run(cell.paint_text.len());
        let cache_matches = cell
            .shaped
            .as_ref()
            .is_some_and(|cached| cached.run == run && cached.font_size == font_size);
        let cache_miss = !cache_matches;
        if cache_miss {
            cell.shaped = Some(CachedShapedCell {
                line: window.text_system().shape_line(
                    cell.paint_text.clone(),
                    font_size,
                    std::slice::from_ref(&run),
                    None,
                ),
                run,
                font_size,
            });
        }
        (
            cell.shaped
                .as_ref()
                .expect("shape cache was populated")
                .line
                .clone(),
            cache_miss,
        )
    }

    /// One tab row shared by the horizontal and vertical result tab bars.
    fn tab_row(tab: ResultTab, selected: bool, colors: ThemeColors) -> Stateful<Div> {
        div()
            .id(("result-tab", tab as usize))
            .debug_selector(move || format!("result-tab-{}", tab.label().to_lowercase()))
            .flex_none()
            .flex()
            .items_center()
            .h_full()
            .px_2()
            .relative()
            .text_sm()
            .when(selected, |el| el.text_color(colors.text))
            .when(selected, |el| {
                el.child(
                    div()
                        .absolute()
                        .left_1()
                        .right_1()
                        .bottom_0()
                        .h(px(1.))
                        .bg(colors.accent),
                )
            })
            .when(!selected, |el| el.text_color(colors.muted_text))
            .hover(|el| el.text_color(colors.text))
            .child(tab.label())
    }

    fn render_tab_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = cx.theme().colors;
        let tab_height = cx.theme().metrics.tab_height;
        div()
            .h(tab_height)
            .flex_none()
            .flex()
            .items_stretch()
            .border_b_1()
            .border_color(colors.subtle_border)
            .bg(colors.toolbar)
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_w_0()
                    .overflow_x_hidden()
                    .items_stretch()
                    .child(
                        div()
                            .id("result-tabs-scroll")
                            .flex()
                            .h_full()
                            .overflow_x_scroll()
                            .children(ResultTab::ALL.into_iter().map(|tab| {
                                Self::tab_row(tab, tab == self.tab, colors).on_click(
                                    cx.listener(move |view, _, _, cx| view.select_tab(tab, cx)),
                                )
                            })),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_shrink_0()
                    .max_w(px(460.))
                    .min_w_0()
                    .overflow_hidden()
                    .items_center()
                    .gap_2()
                    .px_2()
                    .text_xs()
                    .text_color(colors.muted_text)
                    .children(
                        (!Self::is_error_state(&self.state))
                            .then(|| Badge::new(self.state.status_label())),
                    )
                    .children((self.tab == ResultTab::Data).then(|| {
                        div()
                            .flex()
                            .items_center()
                            .gap_1()
                            .children((!self.staged_cells.is_empty()).then(|| {
                                Button::new(
                                    "review-staged-result-edits",
                                    format!("Review {} staged", self.staged_cells.len()),
                                )
                                .debug_selector("review-staged-result-edits")
                                .tone(ButtonTone::Neutral)
                                .on_click(cx.listener(
                                    |_, _, _, cx| cx.emit(ResultsEvent::ReviewStagedEditsRequested),
                                ))
                            }))
                            .children((!self.large_view).then(|| {
                                div()
                                    .debug_selector(|| "open-result-data-modal".into())
                                    .child(
                                        IconButton::new(
                                            "open-result-data-modal",
                                            IconName::Maximize,
                                            "Open Data in large view",
                                        )
                                        .square(px(24.))
                                        .icon_size(13.)
                                        .tooltip("Open Data in large view")
                                        .on_click(
                                            cx.listener(|_, _, _, cx| {
                                                cx.emit(ResultsEvent::OpenDataModalRequested)
                                            }),
                                        ),
                                    )
                            }))
                            .child(
                                Button::new("export-result-csv", "CSV")
                                    .tone(ButtonTone::Ghost)
                                    .on_click(cx.listener(|_, _, _, cx| {
                                        cx.emit(ResultsEvent::ExportRequested {
                                            format: sift_protocol::ExportFormat::Csv,
                                        })
                                    })),
                            )
                            .child(
                                Button::new("export-result-json", "JSON")
                                    .tone(ButtonTone::Ghost)
                                    .on_click(cx.listener(|_, _, _, cx| {
                                        cx.emit(ResultsEvent::ExportRequested {
                                            format: sift_protocol::ExportFormat::JsonArray,
                                        })
                                    })),
                            )
                            .child(
                                Button::new("copy-result-with-headers", "Copy + headers")
                                    .tone(ButtonTone::Neutral)
                                    .disabled(self.selected.is_none())
                                    .on_click(cx.listener(|view, _, window, cx| {
                                        view.copy_selected_with_headers(
                                            &CopySelectedWithHeaders,
                                            window,
                                            cx,
                                        )
                                    })),
                            )
                            .child(
                                IconButton::new(
                                    "copy-result-cell",
                                    IconName::Copy,
                                    "Copy highlighted fields",
                                )
                                .square(px(24.))
                                .icon_size(13.)
                                .tooltip("Copy highlighted fields")
                                .on_click(cx.listener(
                                    |view, _, window, cx| {
                                        view.copy_selected_cell(&CopySelectedCell, window, cx)
                                    },
                                )),
                            )
                    })),
            )
    }

    fn render_vertical_tab_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = cx.theme().colors;
        let tab_height = cx.theme().metrics.tab_height;
        div()
            .w(px(92.))
            .h_full()
            .flex_none()
            .flex()
            .flex_col()
            .border_r_1()
            .border_color(colors.subtle_border)
            .bg(colors.toolbar)
            .children(ResultTab::ALL.into_iter().map(|tab| {
                let selected = tab == self.tab;
                div()
                    .id(("result-tab-vertical", tab as usize))
                    .relative()
                    .h(tab_height)
                    .px_2()
                    .flex_none()
                    .flex()
                    .items_center()
                    .when(selected, |tab| {
                        tab.bg(colors.background).text_color(colors.text).child(
                            div()
                                .absolute()
                                .left_0()
                                .top_1()
                                .bottom_1()
                                .w(px(1.))
                                .bg(colors.accent),
                        )
                    })
                    .when(!selected, |tab| tab.text_color(colors.muted_text))
                    .hover(|tab| tab.bg(colors.hovered_surface).text_color(colors.text))
                    .on_click(cx.listener(move |view, _, _, cx| view.select_tab(tab, cx)))
                    .child(tab.label())
            }))
            .child(div().flex_1())
            .child(
                div()
                    .px_2()
                    .pb_2()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .text_xs()
                    .text_color(colors.muted_text)
                    .children(
                        (!Self::is_error_state(&self.state))
                            .then(|| div().truncate().child(self.state.status_label())),
                    )
                    .children((self.tab == ResultTab::Data).then(|| {
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .children((!self.staged_cells.is_empty()).then(|| {
                                Button::new(
                                    "review-staged-result-edits-vertical",
                                    format!("Review {} staged", self.staged_cells.len()),
                                )
                                .debug_selector("review-staged-result-edits-vertical")
                                .tone(ButtonTone::Neutral)
                                .on_click(cx.listener(
                                    |_, _, _, cx| cx.emit(ResultsEvent::ReviewStagedEditsRequested),
                                ))
                            }))
                            .children((!self.large_view).then(|| {
                                IconButton::new(
                                    "open-result-data-modal-vertical",
                                    IconName::Maximize,
                                    "Open Data in large view",
                                )
                                .icon_size(13.)
                                .text("Expand")
                                .tooltip("Open Data in large view")
                                .on_click(cx.listener(
                                    |_, _, _, cx| cx.emit(ResultsEvent::OpenDataModalRequested),
                                ))
                            }))
                            .child(
                                IconButton::new(
                                    "copy-result-cell-vertical",
                                    IconName::Copy,
                                    "Copy highlighted fields",
                                )
                                .icon_size(13.)
                                .text("Copy")
                                .tooltip("Copy highlighted fields")
                                .on_click(cx.listener(
                                    |view, _, window, cx| {
                                        view.copy_selected_cell(&CopySelectedCell, window, cx)
                                    },
                                )),
                            )
                            .child(
                                Button::new("copy-result-with-headers-vertical", "Copy + headers")
                                    .tone(ButtonTone::Neutral)
                                    .disabled(self.selected.is_none())
                                    .on_click(cx.listener(|view, _, window, cx| {
                                        view.copy_selected_with_headers(
                                            &CopySelectedWithHeaders,
                                            window,
                                            cx,
                                        )
                                    })),
                            )
                    })),
            )
    }

    fn render_grid(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let colors = cx.theme().colors;
        let Some(data) = self.state.ready() else {
            return div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .p_4()
                .text_center()
                .text_color(colors.muted_text)
                .child(self.state.status_label())
                .into_any_element();
        };
        if self.rendered_columns.is_empty() {
            return div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .p_4()
                .text_center()
                .text_color(colors.muted_text)
                .child(self.state.status_label())
                .into_any_element();
        }
        let visible_columns = self.visible_column_indices();
        if visible_columns.is_empty() {
            return div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .p_4()
                .text_center()
                .text_color(colors.muted_text)
                .child("All fields excluded. Use Inspector to include columns.")
                .into_any_element();
        }
        let visible_widths = visible_columns
            .iter()
            .map(|source| {
                self.column_widths
                    .get(*source)
                    .copied()
                    .unwrap_or(DEFAULT_COLUMN_WIDTH)
            })
            .collect::<Vec<_>>();
        let grid_min_width = px(ROW_NUMBER_WIDTH + visible_widths.iter().sum::<f32>());
        let mut resize_right = ROW_NUMBER_WIDTH;
        let resize_handles = visible_columns
            .iter()
            .copied()
            .enumerate()
            .map(|(display_column, source_column)| {
                let width = visible_widths[display_column];
                resize_right += width;
                div()
                    .id(("resize-result-column", display_column))
                    .debug_selector(move || format!("resize-result-column-{display_column}"))
                    .absolute()
                    .left(px(resize_right - COLUMN_RESIZE_HANDLE_WIDTH))
                    .top_0()
                    .h_full()
                    .w(px(COLUMN_RESIZE_HANDLE_WIDTH))
                    .cursor(CursorStyle::ResizeLeftRight)
                    .block_mouse_except_scroll()
                    .on_drag(
                        ColumnResizeDrag {
                            column: source_column,
                        },
                        |_, _, _, cx| cx.new(|_| gpui::Empty),
                    )
            })
            .collect::<Vec<_>>();
        let header = div()
            .debug_selector(|| "result-header".into())
            .on_drag_move::<ColumnResizeDrag>(cx.listener(Self::resize_column))
            .relative()
            .flex()
            .h(px(HEADER_HEIGHT))
            .flex_none()
            .w_full()
            .min_w(grid_min_width)
            .border_b_1()
            .border_color(colors.subtle_border)
            .bg(colors.toolbar)
            .child(
                div()
                    .id("result-select-all")
                    .role(gpui::Role::Button)
                    .aria_label("Select all result cells")
                    .flex_none()
                    .w(px(ROW_NUMBER_WIDTH))
                    .h_full()
                    .flex()
                    .items_center()
                    .justify_end()
                    .pr_2()
                    .border_r_1()
                    .border_color(colors.subtle_border)
                    .text_xs()
                    .text_color(colors.disabled_text)
                    .when(self.selected == Some(GridSelection::All), |header| {
                        header.bg(colors.selected_surface)
                    })
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|view, _, window, cx| {
                            view.focus_handle.focus(window, cx);
                            view.select_all(cx);
                        }),
                    )
                    .child("#"),
            )
            .children(visible_columns.iter().enumerate().filter_map(
                |(display_column, source_column)| {
                    let source_column = *source_column;
                    let column = self.rendered_columns.get(source_column)?;
                    let width = self
                        .column_widths
                        .get(source_column)
                        .copied()
                        .unwrap_or(DEFAULT_COLUMN_WIDTH);
                    Some(
                        div()
                            .id(("result-column", display_column))
                            .debug_selector(move || format!("result-column-{display_column}"))
                            .relative()
                            .role(gpui::Role::Button)
                            .aria_label(format!("Select or drag column {}", column.name))
                            .flex_none()
                            .w(px(width))
                            .px_2()
                            .flex()
                            .flex_col()
                            .justify_center()
                            .overflow_hidden()
                            .border_r_1()
                            .border_color(colors.subtle_border)
                            .when(
                                self.selected.is_some_and(|selection| {
                                    selection.highlights_column(source_column)
                                }),
                                |header| header.bg(colors.selected_surface).text_color(colors.text),
                            )
                            .on_drag(
                                ColumnDrag {
                                    index: source_column,
                                    name: column.name.clone(),
                                },
                                |drag, _, _, cx| cx.new(|_| drag.clone()),
                            )
                            .drag_over::<ColumnDrag>(move |header, drag, _, cx| {
                                if drag.index == source_column {
                                    header
                                } else {
                                    header
                                        .bg(cx.theme().colors.drop_target_background)
                                        .border_color(cx.theme().colors.drop_target_border)
                                        .border_l_2()
                                }
                            })
                            .on_drop::<ColumnDrag>(cx.listener(
                                move |view, drag: &ColumnDrag, _, cx| {
                                    view.reorder_column(drag.index, source_column, cx)
                                },
                            ))
                            .on_click(cx.listener(move |view, _, window, cx| {
                                view.focus_handle.focus(window, cx);
                                view.select_column(source_column, cx);
                            }))
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .truncate()
                                    .child(column.name.clone()),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(colors.muted_text)
                                    .truncate()
                                    .child(format!(
                                        "{}{}",
                                        column.type_label,
                                        if column.nullable { "?" } else { "" }
                                    )),
                            ),
                    )
                },
            ))
            .children(resize_handles);

        let row_count = data.rows.len();
        let grid_scroll_handle = self.grid_scroll_handle.clone();
        let row_scroll_handle = self.row_scroll_handle.clone();
        let row_columns = visible_columns.clone();
        let row_widths = visible_widths;
        let entity_id = cx.entity().entity_id();
        let list = uniform_list(
            "result-rows",
            row_count,
            cx.processor(move |view, range: Range<usize>, window, cx| {
                let colors = cx.theme().colors;
                if view.state.ready().is_none() {
                    return Vec::new();
                }
                let column_count = row_columns.len();
                let selected = view.selected;
                range
                    .map(|row_index| {
                        let cells = row_columns
                            .iter()
                            .copied()
                            .enumerate()
                            .map(|(display_column, source_column)| {
                                let is_selected = match selected {
                                    Some(GridSelection::Cell { row, column }) => {
                                        row == row_index && column == source_column
                                    }
                                    Some(GridSelection::Range {
                                        anchor_row,
                                        anchor_column,
                                        focus_row,
                                        focus_column,
                                    }) => {
                                        let (rows, columns) = view.range_coordinates(
                                            anchor_row,
                                            anchor_column,
                                            focus_row,
                                            focus_column,
                                        );
                                        rows.contains(&row_index)
                                            && columns.contains(&source_column)
                                    }
                                    Some(GridSelection::Row(row)) => row == row_index,
                                    Some(GridSelection::Column(column)) => column == source_column,
                                    Some(GridSelection::All) => true,
                                    None => false,
                                };
                                let is_editing =
                                    view.editing_cell == Some((row_index, source_column));
                                let is_staged =
                                    view.staged_cells.contains_key(&(row_index, source_column));
                                let rendered = view
                                    .rendered_rows
                                    .get_mut(row_index)
                                    .and_then(|row| row.get_mut(source_column));
                                let (shaped, color, is_number) = match rendered {
                                    Some(cell) => {
                                        let color = Self::cell_color(colors, cell.class);
                                        let is_number = matches!(cell.class, CellClass::Number);
                                        (
                                            Some(Self::shape_cell(cell, color, window).0),
                                            color,
                                            is_number,
                                        )
                                    }
                                    None => (None, colors.muted_text, false),
                                };
                                let inline_edit = view
                                    .inline_cell_edit
                                    .as_ref()
                                    .filter(|edit| {
                                        edit.row == row_index && edit.column == source_column
                                    })
                                    .map(|edit| (edit.input.clone(), edit.error));
                                let is_inline_edit = inline_edit.is_some();
                                let cell = div()
                                    .id(("cell", row_index * column_count + display_column))
                                    .flex_none()
                                    .w(px(row_widths[display_column]))
                                    .h(px(ROW_HEIGHT))
                                    .flex()
                                    .items_center()
                                    .overflow_hidden()
                                    .border_r_1()
                                    .border_color(colors.subtle_border)
                                    .text_color(color)
                                    .when(is_selected, |el| el.bg(colors.selected_surface))
                                    .when(is_staged, |el| {
                                        el.bg(colors.warning_muted).border_color(colors.warning)
                                    })
                                    .when(is_editing && !is_inline_edit, |el| {
                                        el.border_1().border_color(colors.accent)
                                    })
                                    .on_mouse_down(
                                        MouseButton::Right,
                                        cx.listener(
                                            move |view,
                                                  event: &gpui::MouseDownEvent,
                                                  window,
                                                  cx| {
                                                view.open_cell_context_menu(
                                                    row_index,
                                                    source_column,
                                                    event.position,
                                                    window,
                                                    cx,
                                                );
                                                cx.stop_propagation();
                                            },
                                        ),
                                    )
                                    .when(!is_inline_edit, |cell| {
                                        cell.on_mouse_down(
                                            MouseButton::Left,
                                            cx.listener(
                                                move |view,
                                                      event: &gpui::MouseDownEvent,
                                                      window,
                                                      cx| {
                                                    view.focus_handle.focus(window, cx);
                                                    view.select_cell_from_pointer(
                                                        row_index,
                                                        source_column,
                                                        event.modifiers.shift,
                                                        event.click_count,
                                                        cx,
                                                    );
                                                    cx.stop_propagation();
                                                },
                                            ),
                                        )
                                    });
                                if let Some((input, error)) = inline_edit {
                                    let input_focus = input.focus_handle(cx);
                                    cell.border_1()
                                        .border_color(if error {
                                            colors.danger
                                        } else {
                                            colors.accent
                                        })
                                        .on_mouse_down(
                                            MouseButton::Left,
                                            cx.listener(move |_, _, window, cx| {
                                                input_focus.focus(window, cx);
                                                cx.stop_propagation();
                                            }),
                                        )
                                        .on_key_down(cx.listener(
                                            |view, event: &gpui::KeyDownEvent, window, cx| {
                                                match event.keystroke.key.as_str() {
                                                    "enter" => view.submit_inline_cell_edit(cx),
                                                    "escape" => {
                                                        view.cancel_inline_cell_edit(window, cx)
                                                    }
                                                    _ => return,
                                                }
                                                cx.stop_propagation();
                                            },
                                        ))
                                        .child(input)
                                } else {
                                    cell.px_2().children(shaped.map(|line| {
                                        canvas(
                                            |_, _, _| (),
                                            move |bounds, _, window, cx| {
                                                let align = if is_number {
                                                    TextAlign::Right
                                                } else {
                                                    TextAlign::Left
                                                };
                                                let _ = line.paint(
                                                    bounds.origin,
                                                    bounds.size.height,
                                                    align,
                                                    Some(bounds.size.width),
                                                    window,
                                                    cx,
                                                );
                                            },
                                        )
                                        .size_full()
                                    }))
                                }
                            })
                            .collect::<Vec<_>>();
                        let row_number = view.window_start + row_index + 1;
                        div()
                            .debug_selector(move || format!("result-row-{row_index}"))
                            .flex()
                            .w(grid_min_width)
                            .flex_none()
                            .h(px(ROW_HEIGHT))
                            .when(row_index % 2 == 1, |el| el.bg(colors.grid_stripe))
                            .child(
                                div()
                                    .id(("result-row-number", row_index))
                                    .role(gpui::Role::Button)
                                    .aria_label(format!("Select row {row_number}"))
                                    .flex_none()
                                    .w(px(ROW_NUMBER_WIDTH))
                                    .h_full()
                                    .flex()
                                    .items_center()
                                    .justify_end()
                                    .pr_2()
                                    .border_r_1()
                                    .border_color(colors.subtle_border)
                                    .text_xs()
                                    .text_color(colors.disabled_text)
                                    .when(
                                        view.selected.is_some_and(|selection| {
                                            selection.highlights_row(row_index)
                                        }),
                                        |header| {
                                            header
                                                .bg(colors.selected_surface)
                                                .text_color(colors.text)
                                        },
                                    )
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(move |view, _, window, cx| {
                                            view.focus_handle.focus(window, cx);
                                            view.select_row(row_index, cx);
                                        }),
                                    )
                                    .child(row_number.to_string()),
                            )
                            .children(cells)
                    })
                    .collect()
            }),
        )
        .size_full()
        .min_w(grid_min_width)
        // Wheel input is routed below so the nested vertical and horizontal
        // scrollers cannot both consume the same event.
        .overflow_hidden()
        .track_scroll(&self.row_scroll_handle)
        .on_scroll_wheel(move |event, window, cx| {
            let delta = event.delta.pixel_delta(window.line_height());
            let scroll_handle = if event.modifiers.shift {
                &grid_scroll_handle
            } else {
                &row_scroll_handle.0.borrow().base_handle
            };
            let current = scroll_handle.offset();
            let max = scroll_handle.max_offset();
            let next = if event.modifiers.shift {
                let horizontal_delta = if delta.y == px(0.) { delta.x } else { delta.y };
                gpui::point(
                    (current.x + horizontal_delta).clamp(-max.x, px(0.)),
                    current.y,
                )
            } else {
                gpui::point(current.x, (current.y + delta.y).clamp(-max.y, px(0.)))
            };
            if next != current {
                scroll_handle.set_offset(next);
                cx.notify(entity_id);
            }
            // This grid owns both axes. Prevent the enclosing horizontal
            // scroller from also consuming an unmodified vertical wheel.
            cx.stop_propagation();
        });

        div()
            .id("result-hscroll")
            .flex_1()
            .min_h_0()
            .overflow_x_scroll()
            .track_scroll(&self.grid_scroll_handle)
            .child(
                div()
                    .size_full()
                    .min_w(grid_min_width)
                    .flex()
                    .flex_col()
                    .min_h_0()
                    .child(header)
                    // A uniform list needs a definite, clipped viewport. Keep
                    // that viewport as the flex child and let the list fill it.
                    .child(
                        div()
                            .debug_selector(|| "result-row-viewport".into())
                            .relative()
                            .flex_1()
                            .min_h_0()
                            .overflow_hidden()
                            .child(list),
                    ),
            )
            .into_any_element()
    }

    /// Window strip for a result larger than the retained bound. It states the
    /// absolute rows on screen and offers the only forward move a server cursor
    /// supports, while saying plainly that the move is not reversible.
    fn render_window_bar(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let (first, last) = self.window_rows()?;
        if !self.window_held && self.window_start == 0 {
            return None;
        }
        let colors = cx.theme().colors;
        Some(
            div()
                .id("result-window-bar")
                .debug_selector(|| "result-window-bar".into())
                .flex()
                .items_center()
                .gap_2()
                .px_3()
                .py_1()
                .border_t_1()
                .border_color(colors.subtle_border)
                .bg(colors.surface)
                .child(
                    div()
                        .flex_1()
                        .text_xs()
                        .text_color(colors.muted_text)
                        .child(if self.window_held {
                            format!(
                                "Rows {first}–{last}. More rows are waiting; \
                                 loading them discards this window."
                            )
                        } else {
                            format!("Rows {first}–{last}. Earlier rows need a re-run.")
                        }),
                )
                .child(
                    Button::new("result-load-next-window", "Load Next Rows")
                        .tone(ButtonTone::Accent)
                        .disabled(!self.window_held)
                        .on_click(cx.listener(|_, _, _, cx| {
                            cx.emit(ResultsEvent::LoadNextWindowRequested);
                        })),
                ),
        )
    }

    fn render_explain_toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = cx.theme().colors;
        let pending = matches!(self.explain, ExplainState::Pending { .. });
        div()
            .debug_selector(|| "result-explain-toolbar".into())
            .h(cx.theme().metrics.toolbar_height)
            .flex_none()
            .flex()
            .items_center()
            .gap_1()
            .px_2()
            .border_b_1()
            .border_color(colors.subtle_border)
            .bg(colors.panel)
            .child(
                div()
                    .debug_selector(|| "explain-estimated-plan".into())
                    .child(
                        Button::new("explain-estimated-plan", "Estimated plan")
                            .tone(ButtonTone::Accent)
                            .disabled(pending)
                            .on_click(
                                cx.listener(|view, _, _, cx| view.request_explain(false, cx)),
                            ),
                    ),
            )
            .child(
                Button::new("explain-analyzed-plan", "Analyze query")
                    .tone(ButtonTone::Ghost)
                    .start_icon(IconName::Activity)
                    .disabled(pending)
                    .on_click(cx.listener(|view, _, _, cx| view.request_explain(true, cx))),
            )
            .children(pending.then(|| {
                div()
                    .ml_2()
                    .text_xs()
                    .text_color(colors.muted_text)
                    .child("Collecting plan…")
            }))
            .child(div().flex_1())
            .children(matches!(self.explain, ExplainState::Ready(_)).then(|| {
                div()
                    .flex()
                    .gap_1()
                    .child(
                        Button::new("save-plan-capture", "Save capture")
                            .tone(ButtonTone::Accent)
                            .on_click(cx.listener(|_, _, _, cx| {
                                cx.emit(ResultsEvent::CapturePlanRequested)
                            })),
                    )
                    .child(
                        Button::new("open-plan-captures", "Captures")
                            .tone(ButtonTone::Ghost)
                            .on_click(cx.listener(|_, _, _, cx| {
                                cx.emit(ResultsEvent::OpenPlanCapturesRequested)
                            })),
                    )
                    .child(
                        Button::new("copy-raw-plan", "Copy raw")
                            .tone(ButtonTone::Ghost)
                            .start_icon(IconName::Copy)
                            .on_click(cx.listener(|view, _, _, cx| view.copy_raw_plan(cx))),
                    )
            }))
    }

    fn render_explain(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let colors = cx.theme().colors;
        match &self.explain {
            ExplainState::Empty => div()
                .size_full()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .p_4()
                .text_center()
                .gap_2()
                .child(icon(IconName::Activity, colors.accent, 24.))
                .child(
                    div()
                        .text_color(colors.text)
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .child("Inspect query execution"),
                )
                .child(
                    div()
                        .max_w(px(460.))
                        .text_sm()
                        .text_color(colors.muted_text)
                        .child("Estimated plan does not run the query. Analyze query runs it and adds real row counts and timing."),
                )
                .into_any_element(),
            ExplainState::Pending { analyze } => div()
                .size_full()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap_2()
                .children(
                    analyze.then(|| icon(IconName::Activity, colors.accent, 22.)),
                )
                .child(
                    div()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .child(if *analyze {
                            "Running query"
                        } else {
                            "Building estimated plan"
                        }),
                )
                .child(
                    div()
                        .text_sm()
                        .text_color(colors.muted_text)
                        .child(if *analyze {
                            "Runtime depends on the query and database workload."
                        } else {
                            "Waiting for the database optimizer…"
                        }),
                )
                .into_any_element(),
            ExplainState::Failed(message) => div()
                .size_full()
                .p_3()
                .child(ErrorBanner::new(message.clone()))
                .into_any_element(),
            ExplainState::Ready(response) => {
                let engine = match response.engine {
                    sift_protocol::Engine::Postgres => "PostgreSQL",
                    sift_protocol::Engine::SqlServer => "SQL Server",
                };
                let node_count = self.rendered_plan_nodes.len();
                let analyzed = response.analyzed;
                let list = uniform_list(
                    "execution-plan-nodes",
                    node_count,
                    cx.processor(move |view, range: Range<usize>, _, cx| {
                        let colors = cx.theme().colors;
                        range
                            .filter_map(|index| {
                                view.rendered_plan_nodes
                                    .get(index)
                                    .cloned()
                                    .map(|node| (index, node))
                            })
                            .map(|(index, node)| {
                                div()
                                    .id(("execution-plan-node", index))
                                    .h(px(58.))
                                    .w_full()
                                    .pl(px(12. + node.depth.min(24) as f32 * 20.))
                                    .pr_3()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .border_b_1()
                                    .border_color(colors.subtle_border)
                                    .when(index % 2 == 1, |row| row.bg(colors.grid_stripe))
                                    .child(
                                        div()
                                            .w(px(3.))
                                            .h(px(30.))
                                            .rounded(cx.theme().metrics.radius)
                                            .bg(if node.actual.is_some() {
                                                colors.accent
                                            } else {
                                                colors.strong_border
                                            }),
                                    )
                                    .child(
                                        div()
                                            .min_w_0()
                                            .flex_1()
                                            .flex()
                                            .flex_col()
                                            .gap_0p5()
                                            .child(
                                                div()
                                                    .flex()
                                                    .items_center()
                                                    .gap_2()
                                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                                    .child(node.op)
                                                    .children(node.relation.map(Badge::new)),
                                            )
                                            .children((!node.estimated.is_empty()).then(|| {
                                                div()
                                                    .text_xs()
                                                    .text_color(colors.muted_text)
                                                    .child(node.estimated)
                                            })),
                                    )
                                    .children(node.actual.map(|actual| {
                                        div()
                                            .flex_none()
                                            .px_2()
                                            .py_1()
                                            .rounded(cx.theme().metrics.radius)
                                            .bg(colors.accent_muted)
                                            .text_xs()
                                            .text_color(colors.accent_hover)
                                            .child(actual)
                                    }))
                            })
                            .collect()
                    }),
                )
                .size_full()
                .overflow_hidden()
                .track_scroll(&self.plan_scroll_handle);
                div()
                    .size_full()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .min_h(px(42.))
                            .px_3()
                            .flex()
                            .items_center()
                            .gap_2()
                            .bg(colors.toolbar)
                            .border_b_1()
                            .border_color(colors.subtle_border)
                            .child(icon(IconName::Database, colors.muted_text, 14.))
                            .child(div().text_sm().child(engine))
                            .child(Badge::new(if analyzed { "Analyzed" } else { "Estimated" }))
                            .child(div().flex_1())
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(colors.muted_text)
                                    .child(format!("{node_count} plan nodes")),
                            ),
                    )
                    .children(response.warnings.iter().map(|warning| {
                        div()
                            .px_3()
                            .py_1()
                            .bg(colors.warning_muted)
                            .text_xs()
                            .text_color(colors.warning)
                            .child(warning.message.clone())
                    }))
                    .child(div().flex_1().min_h_0().child(list))
                    .into_any_element()
            }
        }
    }

    fn render_message_toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = cx.theme().colors;
        let error_count = self
            .messages
            .iter()
            .filter(|message| message.severity == MessageSeverity::Error)
            .count();
        div()
            .debug_selector(|| "result-message-toolbar".into())
            .h(cx.theme().metrics.toolbar_height)
            .flex_none()
            .flex()
            .items_center()
            .gap_1()
            .px_2()
            .border_b_1()
            .border_color(colors.subtle_border)
            .bg(colors.panel)
            .child(
                Button::new("copy-selected-message", "Copy selected")
                    .tone(ButtonTone::Ghost)
                    .start_icon(IconName::Copy)
                    .disabled(self.selected_message.is_none())
                    .on_click(cx.listener(|view, _, _, cx| view.copy_selected_message(cx))),
            )
            .child(
                Button::new("copy-all-errors", "Copy all errors")
                    .tone(ButtonTone::Ghost)
                    .start_icon(IconName::Copy)
                    .disabled(error_count == 0)
                    .on_click(cx.listener(|view, _, _, cx| view.copy_all_errors(cx))),
            )
            .child(div().flex_1())
            .child(
                div()
                    .text_xs()
                    .text_color(colors.muted_text)
                    .child(format!("{} message(s)", self.messages.len())),
            )
    }

    fn render_messages(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let colors = cx.theme().colors;
        if self.messages.is_empty() {
            return div()
                .size_full()
                .p_3()
                .text_sm()
                .text_color(colors.muted_text)
                .child("No messages.")
                .into_any_element();
        }
        div()
            .id("result-messages-scroll")
            .size_full()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .children(self.messages.iter().enumerate().map(|(index, message)| {
                let selected = self.selected_message == Some(index);
                let (label, color) = match message.severity {
                    MessageSeverity::Info => ("Info", colors.muted_text),
                    MessageSeverity::Warning => ("Warning", colors.warning),
                    MessageSeverity::Error => ("Error", colors.danger),
                };
                div()
                    .id(("result-message", index))
                    .debug_selector(move || format!("result-message-{index}"))
                    .flex_none()
                    .flex()
                    .items_start()
                    .gap_2()
                    .px_3()
                    .py_2()
                    .border_b_1()
                    .border_color(colors.subtle_border)
                    .when(selected, |row| row.bg(colors.selected_surface))
                    .hover(|row| row.bg(colors.hovered_surface))
                    .on_click(cx.listener(move |view, _, _, cx| view.select_message(index, cx)))
                    .child(
                        div()
                            .w(px(54.))
                            .flex_none()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(color)
                            .child(label),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .font_family("monospace")
                            .whitespace_normal()
                            .text_color(colors.text)
                            .child(message.text.clone()),
                    )
                    .child(
                        IconButton::new(
                            ("copy-result-message", index),
                            IconName::Copy,
                            "Copy message",
                        )
                        .square(px(24.))
                        .icon_size(13.)
                        .tooltip("Copy message")
                        .on_click(cx.listener(move |view, _, _, cx| view.copy_message(index, cx))),
                    )
            }))
            .into_any_element()
    }

    fn render_history(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let colors = cx.theme().colors;
        if self.history.loading && self.history.rows.is_empty() {
            return div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .text_color(colors.muted_text)
                .child("Loading query history…")
                .into_any_element();
        }
        if let Some(error) = self.history.error.as_ref() {
            return div()
                .size_full()
                .flex()
                .flex_col()
                .gap_2()
                .items_center()
                .justify_center()
                .p_4()
                .text_color(colors.danger)
                .child(error.clone())
                .child(
                    Button::new("retry-query-history", "Retry")
                        .tone(ButtonTone::Neutral)
                        .on_click(cx.listener(|view, _, _, cx| view.request_history(None, cx))),
                )
                .into_any_element();
        }
        if self.history.loaded && self.history.rows.is_empty() {
            return div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .text_color(colors.muted_text)
                .child("No query history yet.")
                .into_any_element();
        }
        div()
            .id("result-history-scroll")
            .size_full()
            .overflow_y_scroll()
            .children(self.history.rows.iter().enumerate().map(|(index, entry)| {
                let sql = entry.sql_text.clone();
                let redacted = sql.starts_with("sqlfp:");
                let display_sql = if redacted {
                    "Query text not stored".into()
                } else {
                    sql.replace(['\n', '\r'], " ")
                };
                let status = match entry.status {
                    sift_api_types::QueryStatus::Ok => "OK",
                    sift_api_types::QueryStatus::Error => "ERROR",
                    sift_api_types::QueryStatus::Canceled => "CANCELED",
                };
                let detail = match (entry.duration_ms, entry.row_count) {
                    (Some(duration), Some(rows)) => format!("{duration} ms · {rows} row(s)"),
                    (Some(duration), None) => format!("{duration} ms"),
                    (None, Some(rows)) => format!("{rows} row(s)"),
                    (None, None) => "No timing recorded".into(),
                };
                div()
                    .id(("result-history-row", index))
                    .debug_selector(move || format!("result-history-row-{index}"))
                    .min_h(px(46.))
                    .px_3()
                    .py_1()
                    .flex()
                    .items_center()
                    .gap_3()
                    .border_b_1()
                    .border_color(colors.subtle_border)
                    .child(
                        div()
                            .w(px(72.))
                            .flex_none()
                            .text_xs()
                            .text_color(colors.muted_text)
                            .child(entry.started_at.format("%H:%M:%S").to_string()),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(div().truncate().child(display_sql))
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(colors.muted_text)
                                    .child(format!("{status} · {detail}")),
                            ),
                    )
                    .child(
                        Button::new(("rerun-history", index), "Run")
                            .tone(ButtonTone::Ghost)
                            .disabled(redacted)
                            .on_click(cx.listener(move |_, _, _, cx| {
                                cx.emit(ResultsEvent::RerunHistory { sql: sql.clone() })
                            })),
                    )
            }))
            .children(self.history.next_cursor.clone().map(|cursor| {
                div().p_2().flex().justify_center().child(
                    Button::new("load-more-history", "Load More")
                        .tone(ButtonTone::Neutral)
                        .on_click(cx.listener(move |view, _, _, cx| {
                            view.request_history(Some(cursor.clone()), cx)
                        })),
                )
            }))
            .into_any_element()
    }

    fn render_history_toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = cx.theme().colors;
        div()
            .debug_selector(|| "result-history-toolbar".into())
            .h(cx.theme().metrics.toolbar_height)
            .flex_none()
            .flex()
            .items_center()
            .gap_1()
            .px_2()
            .border_b_1()
            .border_color(colors.subtle_border)
            .bg(colors.panel)
            .child(
                div()
                    .debug_selector(|| "refresh-query-history".into())
                    .child(
                        Button::new("refresh-query-history", "Refresh")
                            .tone(ButtonTone::Ghost)
                            .disabled(self.history.loading)
                            .on_click(cx.listener(|view, _, _, cx| view.refresh_history(cx))),
                    ),
            )
            .children(self.history.loading.then(|| {
                div()
                    .ml_2()
                    .text_xs()
                    .text_color(colors.muted_text)
                    .child("Loading history…")
            }))
            .child(div().flex_1())
            .child(div().text_xs().text_color(colors.muted_text).child(format!(
                "{} quer{}",
                self.history.rows.len(),
                if self.history.rows.len() == 1 {
                    "y"
                } else {
                    "ies"
                }
            )))
    }
}

impl Focusable for ResultsView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl gpui::Render for ResultsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if std::mem::take(&mut self.restore_grid_focus) {
            let focus = self.focus_handle.clone();
            window.defer(cx, move |window, cx| focus.focus(window, cx));
        }
        let colors = cx.theme().colors;
        let context_menu_position = self.context_menu_position;
        let body = match self.tab {
            ResultTab::Data => self.render_grid(cx),
            ResultTab::Messages => self.render_messages(cx).into_any_element(),
            ResultTab::Explain => self.render_explain(cx),
            ResultTab::History => self.render_history(cx),
        };

        div()
            .id("sift-results")
            .key_context("SiftResults")
            .track_focus(&self.focus_handle)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|view, _, window, cx| {
                    view.context_menu_position = None;
                    view.focus_handle.focus(window, cx);
                    cx.notify();
                }),
            )
            .on_action(cx.listener(Self::copy_selected_cell))
            .on_action(cx.listener(Self::copy_selected_with_headers))
            .on_action(cx.listener(Self::edit_selected_cell))
            .on_action(cx.listener(Self::toggle_visual_selection))
            .on_action(cx.listener(Self::exit_visual_selection))
            .on_action(cx.listener(Self::paste_selected_cell))
            .on_action(cx.listener(Self::revert_selected_cell))
            .on_action(cx.listener(Self::move_cell_left))
            .on_action(cx.listener(Self::move_cell_right))
            .on_action(cx.listener(Self::move_cell_up))
            .on_action(cx.listener(Self::move_cell_down))
            .on_action(cx.listener(Self::previous_result_tab))
            .on_action(cx.listener(Self::next_result_tab))
            .flex()
            .when(self.placement == ResultPlacement::Bottom, |view| {
                view.flex_col()
            })
            .when(self.placement == ResultPlacement::Right, |view| view.flex())
            .size_full()
            .flex_1()
            .min_h_0()
            .bg(colors.background)
            .text_color(colors.text)
            .when(self.placement == ResultPlacement::Bottom, |view| {
                view.child(self.render_tab_bar(cx))
            })
            .when(self.placement == ResultPlacement::Right, |view| {
                view.child(self.render_vertical_tab_bar(cx))
            })
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .min_h_0()
                    .children(
                        (self.tab == ResultTab::Messages).then(|| self.render_message_toolbar(cx)),
                    )
                    .children(
                        (self.tab == ResultTab::Explain).then(|| self.render_explain_toolbar(cx)),
                    )
                    .children(
                        (self.tab == ResultTab::History).then(|| self.render_history_toolbar(cx)),
                    )
                    .child(div().flex().flex_1().min_w_0().min_h_0().child(body))
                    .children(
                        (self.tab == ResultTab::Data)
                            .then(|| self.render_window_bar(cx))
                            .flatten(),
                    ),
            )
            .children(context_menu_position.map(|position| {
                deferred(
                    anchored()
                        .position(position)
                        .snap_to_window_with_margin(px(8.))
                        .child(
                            div()
                                .debug_selector(|| "result-cell-context-menu".into())
                                .w(px(190.))
                                .p_1()
                                .rounded_sm()
                                .border_1()
                                .border_color(colors.strong_border)
                                .bg(colors.elevated_surface)
                                .shadow_lg()
                                .occlude()
                                .child(
                                    div()
                                        .id("result-cell-see-row-json")
                                        .debug_selector(|| "result-cell-see-row-json".into())
                                        .role(gpui::Role::MenuItem)
                                        .h(px(28.))
                                        .px_2()
                                        .flex()
                                        .items_center()
                                        .rounded_sm()
                                        .hover(|item| item.bg(colors.hovered_surface))
                                        .on_click(cx.listener(|view, _, _, cx| {
                                            view.context_menu_position = None;
                                            cx.emit(ResultsEvent::OpenSelectedRowJsonRequested);
                                            cx.notify();
                                        }))
                                        .child("See row as JSON"),
                                ),
                        ),
                )
                .with_priority(1)
            }))
    }
}

impl gpui::EventEmitter<ResultsEvent> for ResultsView {}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext};
    use sift_protocol::{Code, DriverError, PrimitiveType};

    struct ResultsHost(Entity<ResultsView>);

    impl gpui::Render for ResultsHost {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div().w(px(800.)).h(px(400.)).flex().child(self.0.clone())
        }
    }

    fn execute_response(rows: Vec<Row>, has_more: bool) -> ExecuteResponse {
        ExecuteResponse {
            cursor_id: sift_protocol::CursorId(1),
            columns: vec![column("id", PrimitiveType::Int64, Nullability::NotNullable)],
            schema_digest: "digest".into(),
            rows,
            affected_rows: None,
            warnings: Vec::new(),
            has_more,
        }
    }

    fn column(name: &str, primitive: PrimitiveType, nullable: Nullability) -> ColumnMetadata {
        let mut column = ColumnMetadata::new(name, TypeRef::Primitive(primitive));
        column.nullable = nullable;
        column
    }

    fn history_entry(id: i64, sql: &str) -> QueryHistory {
        QueryHistory {
            id: sift_api_types::QueryHistoryId(id),
            principal_id: sift_api_types::PrincipalId(1),
            room_id: None,
            connection_profile_id: None,
            sql_text: sql.into(),
            started_at: "2026-08-24T10:00:00Z".parse().expect("valid timestamp"),
            duration_ms: Some(12),
            row_count: Some(1),
            status: sift_api_types::QueryStatus::Ok,
            error_code: None,
            error_message: None,
        }
    }

    #[gpui::test]
    fn history_pages_append_and_keep_the_next_cursor(cx: &mut TestAppContext) {
        let view = cx.update(|cx| cx.new(ResultsView::new));
        view.update(cx, |view, cx| view.select_tab(ResultTab::History, cx));
        assert!(view.read_with(cx, |view, _| view.history.loading));

        view.update(cx, |view, cx| {
            view.set_history_page(
                Ok(sift_protocol::CursorPage {
                    items: vec![history_entry(1, "select 1")],
                    next_cursor: Some("next".into()),
                }),
                false,
                cx,
            )
        });
        view.update(cx, |view, cx| {
            view.set_history_page(
                Ok(sift_protocol::CursorPage {
                    items: vec![history_entry(2, "select 2")],
                    next_cursor: None,
                }),
                true,
                cx,
            )
        });

        view.read_with(cx, |view, _| {
            assert_eq!(view.history.rows.len(), 2);
            assert_eq!(view.history.rows[1].sql_text, "select 2");
            assert_eq!(view.history.next_cursor, None);
            assert!(!view.history.loading);
        });
    }

    #[gpui::test]
    fn selected_cell_edit_uses_inline_input_state(cx: &mut TestAppContext) {
        let view = cx.update(|cx| cx.new(ResultsView::new));
        view.update(cx, |view, cx| {
            view.set_state(
                ResultState::from_execute(execute_response(
                    vec![Row::new(vec![Value::Int64(7)])],
                    false,
                )),
                cx,
            );
            view.select_cell(0, 0, cx);
            view.select_cell_from_pointer(0, 0, false, 1, cx);
            view.select_cell_from_pointer(0, 0, false, 1, cx);
            assert_eq!(
                view.selected,
                Some(GridSelection::Cell { row: 0, column: 0 })
            );
            assert!(view.select_cell_from_pointer(0, 0, false, 2, cx));
            assert_eq!(
                view.selected,
                Some(GridSelection::Cell { row: 0, column: 0 })
            );
            let selected = view.selected_cell_edit().unwrap();
            assert!(view.stage_cell_value(
                &selected.original_row,
                &selected.column,
                Value::Int64(9),
                cx,
            ));
            assert_eq!(view.staged_cells.get(&(0, 0)), Some(&Value::Int64(9)));
            assert_eq!(view.rendered_rows[0][0].text, "9");
            assert_eq!(view.selected_cell_edit().unwrap().original, Value::Int64(7));
            view.clear_staged_cells(cx);
            assert!(view.staged_cells.is_empty());
            assert_eq!(view.rendered_rows[0][0].text, "7");
            assert!(view.begin_selected_cell_edit("7".into(), cx).is_some());
            let edit = view.inline_cell_edit.as_ref().expect("inline editor");
            assert_eq!((edit.row, edit.column), (0, 0));
            assert_eq!(edit.input.read(cx).text(), "7");
            assert_eq!(view.editing_cell, Some((0, 0)));
            assert!(!view.select_cell_from_pointer(0, 0, false, 2, cx));
            assert_eq!(
                view.selected,
                Some(GridSelection::Cell { row: 0, column: 0 })
            );
            view.set_inline_cell_edit_pending(true, cx);
            assert!(view.inline_cell_edit.as_ref().unwrap().pending);
            view.set_inline_cell_edit_error(cx);
            assert!(view.inline_cell_edit.as_ref().unwrap().error);
            assert!(!view.inline_cell_edit.as_ref().unwrap().pending);
            view.finish_inline_cell_edit(cx);
            assert!(view.inline_cell_edit.is_none());
            assert_eq!(view.editing_cell, None);
            view.editing_cell = Some((0, 0));
            assert!(
                view.select_cell_from_pointer(0, 0, false, 2, cx),
                "a stale editing outline must not block reopening the inline editor"
            );
        });
    }

    #[gpui::test]
    fn applying_multiple_staged_cells_resolves_row_before_mutation(cx: &mut TestAppContext) {
        let view = cx.update(|cx| cx.new(ResultsView::new));
        view.update(cx, |view, cx| {
            view.set_state(
                ResultState::Ready(ResultData {
                    columns: vec![
                        ResultColumn {
                            name: "id".into(),
                            type_label: "bigint".into(),
                            nullable: false,
                        },
                        ResultColumn {
                            name: "action".into(),
                            type_label: "text".into(),
                            nullable: false,
                        },
                    ],
                    rows: vec![Row::new(vec![Value::Int64(1), Value::Text("open".into())])],
                    ..Default::default()
                }),
                cx,
            );
            let original_row = vec![
                ("id".into(), Value::Int64(1)),
                ("action".into(), Value::Text("open".into())),
            ];
            assert!(view.stage_cell_value(&original_row, "id", Value::Int64(2), cx));
            assert!(view.stage_cell_value(
                &original_row,
                "action",
                Value::Text("closed".into()),
                cx,
            ));

            let saved = [
                (original_row.clone(), "id".to_owned(), Value::Int64(2)),
                (
                    original_row,
                    "action".to_owned(),
                    Value::Text("closed".into()),
                ),
            ];
            assert_eq!(
                view.apply_saved_cell_values(
                    saved
                        .iter()
                        .map(|(row, column, value)| { (row.as_slice(), column.as_str(), value) }),
                    cx,
                ),
                2
            );
            assert!(view.staged_cells.is_empty());
            assert_eq!(
                view.state.ready().unwrap().rows[0].values,
                vec![Value::Int64(2), Value::Text("closed".into())]
            );
            assert_eq!(view.rendered_rows[0][0].text, "2");
            assert_eq!(view.rendered_rows[0][1].text, "closed");
        });
    }

    #[gpui::test]
    fn selected_row_json_preserves_types_and_expands_structured_text(cx: &mut TestAppContext) {
        let view = cx.update(|cx| cx.new(ResultsView::new));
        view.update(cx, |view, cx| {
            view.set_state(
                ResultState::from_execute(ExecuteResponse {
                    cursor_id: sift_protocol::CursorId(1),
                    columns: vec![
                        column("id", PrimitiveType::Int64, Nullability::NotNullable),
                        column("active", PrimitiveType::Bool, Nullability::NotNullable),
                        column("payload", PrimitiveType::Text, Nullability::NotNullable),
                        column("missing", PrimitiveType::Text, Nullability::Nullable),
                    ],
                    schema_digest: "d".into(),
                    rows: vec![Row::new(vec![
                        Value::Int64(7),
                        Value::Bool(true),
                        Value::Text(r#"{"event":"open","tags":["demo"]}"#.into()),
                        Value::Null,
                    ])],
                    affected_rows: None,
                    warnings: Vec::new(),
                    has_more: false,
                }),
                cx,
            );
            view.select_cell(0, 2, cx);
        });

        view.read_with(cx, |view, _| {
            assert_eq!(
                view.selected_row_json(),
                Some(SelectedRowJson {
                    row_index: 0,
                    value: serde_json::json!({
                        "id": 7,
                        "active": true,
                        "payload": {"event": "open", "tags": ["demo"]},
                        "missing": null,
                    }),
                })
            );
        });
    }

    #[gpui::test]
    fn explain_tracks_estimated_and_analyzed_plan_states(cx: &mut TestAppContext) {
        let view = cx.update(|cx| cx.new(ResultsView::new));
        view.update(cx, |view, cx| view.request_explain(false, cx));
        assert!(view.read_with(cx, |view, _| matches!(
            view.explain,
            ExplainState::Pending { analyze: false }
        )));

        let mut root = PlanNode::new("Index Scan");
        root.relation = Some("orders_customer_idx".into());
        root.est_rows = Some(12.0);
        root.est_cost = Some(4.25);
        view.update(cx, |view, cx| {
            view.set_explain_result(
                Ok(Box::new(ExplainResponse {
                    engine: sift_protocol::Engine::Postgres,
                    analyzed: false,
                    root,
                    raw: "{\"Plan\":{}}".into(),
                    warnings: Vec::new(),
                })),
                cx,
            )
        });
        view.read_with(cx, |view, _| match &view.explain {
            ExplainState::Ready(response) => {
                assert_eq!(response.root.op, "Index Scan");
                assert!(!response.analyzed);
                assert_eq!(view.rendered_plan_nodes.len(), 1);
                assert_eq!(view.rendered_plan_nodes[0].op, "Index Scan");
            }
            state => panic!("expected ready plan, got {state:?}"),
        });

        view.update(cx, |view, cx| view.request_explain(true, cx));
        assert!(view.read_with(cx, |view, _| matches!(
            view.explain,
            ExplainState::Pending { analyze: true }
        )));
    }

    #[test]
    fn plan_nodes_flatten_in_display_order_with_depth() {
        let mut root = PlanNode::new("Hash Join");
        let mut scan = PlanNode::new("Seq Scan");
        scan.relation = Some("orders".into());
        scan.children.push(PlanNode::new("Filter"));
        root.children.push(scan);
        root.children.push(PlanNode::new("Index Scan"));

        let rows = flatten_plan(&root);
        assert_eq!(rows.len(), 4);
        assert_eq!(rows[0].op, "Hash Join");
        assert_eq!(rows[0].depth, 0);
        assert_eq!(rows[1].op, "Seq Scan");
        assert_eq!(rows[1].relation.as_deref(), Some("orders"));
        assert_eq!(rows[1].depth, 1);
        assert_eq!(rows[2].op, "Filter");
        assert_eq!(rows[2].depth, 2);
        assert_eq!(rows[3].op, "Index Scan");
        assert_eq!(rows[3].depth, 1);
    }

    #[gpui::test]
    fn a_restored_tab_reports_its_last_run_without_rows(cx: &mut TestAppContext) {
        let view = cx.update(|cx| cx.new(ResultsView::new));
        view.update(cx, |view, cx| {
            view.restore_reference(
                ResultReference {
                    cursor_id: Some(7),
                    row_count: 128,
                    affected_rows: None,
                    has_more: true,
                    completed_at_ms: 1_700_000_000_000,
                },
                cx,
            );
        });

        view.read_with(cx, |view, _| {
            assert!(matches!(view.state(), ResultState::Detached(_)));
            assert!(view.rendered_rows.is_empty());
            assert!(view.state().status_label().contains("128+ row(s)"));
            assert!(view.messages[0].text.contains("re-run"));
        });
    }

    #[gpui::test]
    fn a_live_result_is_never_overwritten_by_a_restored_reference(cx: &mut TestAppContext) {
        let view = cx.update(|cx| cx.new(ResultsView::new));
        view.update(cx, |view, cx| {
            view.set_state(
                ResultState::from_execute(execute_response(
                    vec![Row {
                        values: vec![Value::Int64(1)],
                    }],
                    false,
                )),
                cx,
            );
            view.restore_reference(
                ResultReference {
                    cursor_id: None,
                    row_count: 9,
                    affected_rows: None,
                    has_more: false,
                    completed_at_ms: 1,
                },
                cx,
            );
        });

        view.read_with(cx, |view, _| {
            assert!(matches!(view.state(), ResultState::Ready(_)));
            assert_eq!(view.rendered_rows.len(), 1);
        });
    }

    #[test]
    fn only_a_completed_run_produces_a_reference() {
        let ready = ResultState::from_execute(execute_response(
            vec![
                Row {
                    values: vec![Value::Int64(1)],
                },
                Row {
                    values: vec![Value::Int64(2)],
                },
            ],
            true,
        ));
        let reference = ready.reference(Some(3), 42).expect("ready run references");
        assert_eq!(reference.row_count, 2);
        assert_eq!(reference.cursor_id, Some(3));
        assert!(reference.has_more);
        assert_eq!(reference.completed_at_ms, 42);

        // Nothing that failed, was cancelled, or is still running describes a
        // result the user could return to.
        for state in [
            ResultState::Idle,
            ResultState::Pending,
            ResultState::Cancelled,
            ResultState::TimedOut,
            ResultState::OutcomeUnknown,
            ResultState::Failed("boom".into()),
            ResultState::Streaming(ResultData::default()),
        ] {
            assert!(state.reference(Some(3), 42).is_none());
        }
    }

    #[test]
    fn typed_cells_classify_and_format_values() {
        assert_eq!(
            render_value(&Value::Null),
            CellRender {
                text: "NULL".into(),
                class: CellClass::Null
            }
        );
        assert_eq!(render_value(&Value::Int64(42)).class, CellClass::Number);
        assert_eq!(render_value(&Value::Bool(true)).text, "true");
        assert_eq!(render_value(&Value::Blob(vec![0, 1, 2])).text, "⟨3 bytes⟩");
        assert_eq!(
            render_value(&Value::Text("hi".into())).class,
            CellClass::Text
        );
        let multiline: SharedString = "first\nsecond\rthird".into();
        assert_eq!(single_line_text(&multiline), "first second third");
    }

    #[test]
    fn cell_selection_highlights_its_row_and_column_coordinates() {
        let selection = GridSelection::Cell { row: 2, column: 3 };
        assert!(selection.highlights_row(2));
        assert!(!selection.highlights_row(1));
        assert!(selection.highlights_column(3));
        assert!(!selection.highlights_column(2));

        assert!(GridSelection::Row(4).highlights_row(4));
        assert!(!GridSelection::Row(4).highlights_column(0));
        assert!(GridSelection::Column(5).highlights_column(5));
        assert!(!GridSelection::Column(5).highlights_row(0));
        assert!(GridSelection::All.highlights_row(99));
        assert!(GridSelection::All.highlights_column(99));
    }

    #[test]
    fn ready_state_from_execute_maps_columns_and_rows() {
        let response = ExecuteResponse {
            cursor_id: sift_protocol::CursorId(1),
            columns: vec![
                column("id", PrimitiveType::Int64, Nullability::NotNullable),
                column("name", PrimitiveType::Text, Nullability::Nullable),
            ],
            schema_digest: "d".into(),
            rows: vec![Row::new(vec![Value::Int64(1), Value::Text("a".into())])],
            affected_rows: None,
            warnings: Vec::new(),
            has_more: false,
        };
        let state = ResultState::from_execute(response);
        let ResultState::Ready(data) = &state else {
            panic!("expected ready");
        };
        assert_eq!(data.columns.len(), 2);
        assert_eq!(data.columns[0].type_label, "int64");
        assert!(data.columns[1].nullable);
        assert_eq!(data.rows.len(), 1);
        assert_eq!(state.status_label(), "1 row(s)");
    }

    #[test]
    fn completed_result_status_includes_query_duration() {
        let state = ResultState::Ready(ResultData {
            rows: vec![Row::new(vec![Value::Int64(1)])],
            duration_ms: Some(12),
            ..Default::default()
        });
        assert_eq!(state.status_label(), "1 row(s) · 12 ms");

        let state = ResultState::Ready(ResultData {
            affected_rows: Some(3),
            duration_ms: Some(7),
            ..Default::default()
        });
        assert_eq!(state.status_label(), "3 row(s) affected · 7 ms");
    }

    #[gpui::test]
    fn streamed_result_records_elapsed_query_time(cx: &mut TestAppContext) {
        let view = cx.new(ResultsView::new);
        view.update(cx, |view, cx| {
            view.set_pending(cx);
            view.query_started_at = Some(
                std::time::Instant::now()
                    .checked_sub(std::time::Duration::from_millis(12))
                    .unwrap(),
            );
            view.begin_stream(cx);
            assert_eq!(
                view.apply_stream_page(
                    Page::Rows {
                        rows: vec![Row::new(vec![Value::Int64(1)])],
                    },
                    cx,
                ),
                StreamProgress::Consumed
            );
            assert_eq!(
                view.apply_stream_page(
                    Page::Done {
                        affected_rows: None,
                        warnings: Vec::new(),
                    },
                    cx,
                ),
                StreamProgress::Terminal
            );
            let ResultState::Ready(data) = view.state() else {
                panic!("terminal page should complete the result")
            };
            assert!(data.duration_ms.is_some_and(|duration| duration >= 12));
        });
    }

    #[test]
    fn pages_take_first_result_and_flag_truncation() {
        let pages = vec![
            Page::NextResult {
                columns: vec![column("a", PrimitiveType::Int32, Nullability::Unknown)],
            },
            Page::Rows {
                rows: vec![Row::new(vec![Value::Int32(7)])],
            },
            Page::NextResult {
                columns: vec![column("b", PrimitiveType::Text, Nullability::Unknown)],
            },
        ];
        let ResultState::Ready(data) = ResultState::from_pages(pages) else {
            panic!("expected ready");
        };
        assert_eq!(data.columns.len(), 1);
        assert_eq!(data.columns[0].name, "a");
        assert_eq!(data.rows.len(), 1);
        assert!(data.truncated_extra_results);
    }

    #[test]
    fn pages_error_becomes_failed_state() {
        let pages = vec![Page::Error {
            error: DriverError::new(Code::SyntaxError, "boom"),
        }];
        assert!(matches!(
            ResultState::from_pages(pages),
            ResultState::Failed(message) if message == "boom"
        ));
    }

    fn rows_page(count: usize, from: usize) -> Page {
        Page::Rows {
            rows: (from..from + count)
                .map(|value| Row::new(vec![Value::Int64(value as i64)]))
                .collect(),
        }
    }

    #[gpui::test]
    fn a_full_window_holds_the_next_page_instead_of_dropping_it(cx: &mut TestAppContext) {
        let view = cx.update(|cx| cx.new(ResultsView::new));
        view.update(cx, |view, cx| {
            view.begin_stream(cx);
            assert_eq!(
                view.apply_stream_page(
                    Page::NextResult {
                        columns: vec![column("id", PrimitiveType::Int64, Nullability::NotNullable)],
                    },
                    cx,
                ),
                StreamProgress::Consumed
            );
            for page in 0..2 {
                assert_eq!(
                    view.apply_stream_page(rows_page(4_000, page * 4_000), cx),
                    StreamProgress::Consumed
                );
            }
            assert_eq!(view.window_rows(), Some((1, 8_000)));

            // The third page does not fit, so it is refused rather than
            // silently discarded, and the window says so.
            assert_eq!(
                view.apply_stream_page(rows_page(4_000, 8_000), cx),
                StreamProgress::WindowFull
            );
            assert!(view.window_held());
            assert_eq!(view.window_rows(), Some((1, 8_000)));
            let ResultState::Streaming(data) = view.state() else {
                panic!("expected streaming result")
            };
            assert_eq!(data.rows.len(), 8_000);
            assert!(data.has_more);
        });
    }

    #[gpui::test]
    fn advancing_the_window_renumbers_rows_and_resumes_the_stream(cx: &mut TestAppContext) {
        let view = cx.update(|cx| cx.new(ResultsView::new));
        view.update(cx, |view, cx| {
            view.begin_stream(cx);
            view.apply_stream_page(
                Page::NextResult {
                    columns: vec![column("id", PrimitiveType::Int64, Nullability::NotNullable)],
                },
                cx,
            );
            view.apply_stream_page(rows_page(8_000, 0), cx);
            view.apply_stream_page(rows_page(4_000, 8_000), cx);

            assert!(view.advance_window(cx));
            assert!(!view.window_held());
            assert_eq!(view.window_rows(), None);
            assert!(view.rendered_rows.is_empty());

            // The held page is what resumes the stream, so no row is skipped
            // between the two windows.
            assert_eq!(
                view.apply_stream_page(rows_page(4_000, 8_000), cx),
                StreamProgress::Consumed
            );
            assert_eq!(view.window_rows(), Some((8_001, 12_000)));
            assert_eq!(view.rendered_rows.len(), 4_000);

            assert_eq!(
                view.apply_stream_page(
                    Page::Done {
                        affected_rows: None,
                        warnings: Vec::new(),
                    },
                    cx,
                ),
                StreamProgress::Terminal
            );
            assert!(matches!(view.state(), ResultState::Ready(data) if data.has_more));
            assert!(!view.window_held());
        });
    }

    #[gpui::test]
    fn a_first_page_larger_than_the_budget_is_taken_whole(cx: &mut TestAppContext) {
        // Holding it instead would stall the stream forever, since no amount of
        // advancing would make room. Real driver pages are far smaller than the
        // retained budget; this bounds the pathological case to one page.
        let view = cx.update(|cx| cx.new(ResultsView::new));
        view.update(cx, |view, cx| {
            view.begin_stream(cx);
            view.apply_stream_page(
                Page::NextResult {
                    columns: vec![column("id", PrimitiveType::Int64, Nullability::NotNullable)],
                },
                cx,
            );
            assert_eq!(
                view.apply_stream_page(rows_page(MAX_RETAINED_ROWS + 5, 0), cx),
                StreamProgress::Consumed
            );
            assert!(!view.window_held());
            assert_eq!(view.rendered_rows.len(), MAX_RETAINED_ROWS + 5);
        });
    }

    #[gpui::test]
    fn streamed_terminal_errors_keep_distinct_outcomes(cx: &mut TestAppContext) {
        let view = cx.update(|cx| cx.new(ResultsView::new));
        view.update(cx, |view, cx| {
            view.begin_stream(cx);
            assert_eq!(
                view.apply_stream_page(
                    Page::Error {
                        error: DriverError::new(Code::QueryCanceled, "query was canceled"),
                    },
                    cx,
                ),
                StreamProgress::Terminal
            );
            assert!(matches!(view.state(), ResultState::Cancelled));

            view.begin_stream(cx);
            assert_eq!(
                view.apply_stream_page(
                    Page::Error {
                        error: DriverError::new(Code::QueryTimedOut, "query timed out"),
                    },
                    cx,
                ),
                StreamProgress::Terminal
            );
            assert!(matches!(view.state(), ResultState::TimedOut));
        });
    }

    #[gpui::test]
    fn query_errors_open_messages_and_copy_locally(cx: &mut TestAppContext) {
        let view = cx.update(|cx| cx.new(ResultsView::new));
        view.update(cx, |view, cx| {
            view.set_state(ResultState::Failed("syntax error near FROM".into()), cx);
            assert_eq!(view.active_tab(), ResultTab::Messages);
            view.select_tab(ResultTab::Data, cx);
            assert_eq!(view.active_tab(), ResultTab::Messages);
            assert_eq!(view.messages.len(), 1);
            assert_eq!(view.messages[0].severity, MessageSeverity::Error);

            view.select_message(0, cx);
            view.copy_selected_message(cx);
            assert_eq!(
                cx.read_from_clipboard()
                    .and_then(|item| item.text())
                    .as_deref(),
                Some("syntax error near FROM")
            );

            view.copy_all_errors(cx);
            assert_eq!(
                cx.read_from_clipboard()
                    .and_then(|item| item.text())
                    .as_deref(),
                Some("syntax error near FROM")
            );
        });
    }

    #[gpui::test]
    fn failed_query_renders_message_toolbar_and_copyable_row(cx: &mut TestAppContext) {
        let window = cx
            .update(|cx| {
                cx.open_window(Default::default(), |_window, cx| {
                    let view = cx.new(ResultsView::new);
                    cx.new(|_| ResultsHost(view))
                })
            })
            .unwrap();
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        let host = window.root(&mut cx).unwrap();
        let view = host.read_with(&cx, |host, _| host.0.clone());
        view.update(&mut cx, |view, cx| {
            view.set_state(ResultState::Failed("bad column".into()), cx);
        });
        cx.run_until_parked();

        assert!(cx.debug_bounds("result-message-toolbar").is_some());
        assert!(cx.debug_bounds("result-message-0").is_some());
    }

    #[gpui::test]
    fn explain_and_history_tabs_are_clickable_and_share_toolbar_height(cx: &mut TestAppContext) {
        let window = cx
            .update(|cx| {
                cx.open_window(Default::default(), |_window, cx| {
                    let view = cx.new(ResultsView::new);
                    cx.new(|_| ResultsHost(view))
                })
            })
            .unwrap();
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        let host = window.root(&mut cx).unwrap();
        let view = host.read_with(&cx, |host, _| host.0.clone());

        let explain_tab = cx.debug_bounds("result-tab-explain").expect("Explain tab");
        cx.simulate_click(explain_tab.center(), Modifiers::default());
        cx.run_until_parked();
        assert_eq!(
            view.read_with(&cx, |view, _| view.active_tab()),
            ResultTab::Explain
        );
        let explain_toolbar = cx
            .debug_bounds("result-explain-toolbar")
            .expect("Explain toolbar");

        let estimated = cx
            .debug_bounds("explain-estimated-plan")
            .expect("Estimated plan action");
        cx.simulate_click(estimated.center(), Modifiers::default());
        cx.run_until_parked();
        assert!(view.read_with(&cx, |view, _| matches!(
            view.explain,
            ExplainState::Pending { analyze: false }
        )));

        let history_tab = cx.debug_bounds("result-tab-history").expect("History tab");
        cx.simulate_click(history_tab.center(), Modifiers::default());
        cx.run_until_parked();
        assert_eq!(
            view.read_with(&cx, |view, _| view.active_tab()),
            ResultTab::History
        );
        assert!(view.read_with(&cx, |view, _| view.history.loading));
        let history_toolbar = cx
            .debug_bounds("result-history-toolbar")
            .expect("History toolbar");
        assert_eq!(explain_toolbar.size.height, history_toolbar.size.height);

        view.update(&mut cx, |view, cx| {
            view.set_history_page(
                Ok(sift_protocol::CursorPage {
                    items: vec![history_entry(1, "sqlfp:redacted")],
                    next_cursor: None,
                }),
                false,
                cx,
            )
        });
        cx.run_until_parked();
        assert!(cx.debug_bounds("result-history-row-0").is_some());
        assert!(cx.debug_bounds("refresh-query-history").is_some());
    }

    #[gpui::test]
    fn streamed_messages_reset_per_run_and_append_terminal_warnings(cx: &mut TestAppContext) {
        let view = cx.update(|cx| cx.new(ResultsView::new));
        view.update(cx, |view, cx| {
            view.set_state(ResultState::Failed("old error".into()), cx);
            view.begin_stream(cx);
            assert!(view.messages.is_empty());
            assert_eq!(
                view.apply_stream_page(
                    Page::Done {
                        affected_rows: Some(3),
                        warnings: vec![DriverWarning::new("partial result")],
                    },
                    cx,
                ),
                StreamProgress::Terminal
            );
            assert_eq!(
                view.messages,
                vec![
                    ResultMessage {
                        severity: MessageSeverity::Info,
                        text: "3 row(s) affected".into(),
                    },
                    ResultMessage {
                        severity: MessageSeverity::Warning,
                        text: "partial result".into(),
                    },
                ]
            );
        });
    }

    #[gpui::test]
    fn view_switches_tabs_and_copies_selected_cell(cx: &mut TestAppContext) {
        let window = cx
            .update(|cx| {
                cx.open_window(Default::default(), |_window, cx| {
                    let view = cx.new(ResultsView::new);
                    cx.new(|_| ResultsHost(view))
                })
            })
            .unwrap();
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        let host = window.root(&mut cx).unwrap();
        let view = host.read_with(&cx, |host, _| host.0.clone());

        let response = ExecuteResponse {
            cursor_id: sift_protocol::CursorId(1),
            columns: vec![
                column("name", PrimitiveType::Text, Nullability::Nullable),
                column("rank", PrimitiveType::Int32, Nullability::NotNullable),
            ],
            schema_digest: "d".into(),
            rows: vec![
                Row::new(vec![Value::Text("neo".into()), Value::Int32(1)]),
                Row::new(vec![Value::Text("trinity".into()), Value::Int32(2)]),
            ],
            affected_rows: None,
            warnings: Vec::new(),
            has_more: false,
        };
        view.update(&mut cx, |view, cx| {
            view.set_state(ResultState::from_execute(response), cx);
            assert_eq!(view.active_tab(), ResultTab::Data);
            assert_eq!(view.rendered_rows.len(), 2);
            assert_eq!(view.rendered_rows[0][1].text, "1");
        });
        cx.run_until_parked();
        view.update_in(&mut cx, |view, window, cx| {
            let cell = &mut view.rendered_rows[0][0];
            assert!(cell.shaped.is_some(), "visible cells should shape once");
            let color = ResultsView::cell_color(cx.theme().colors, cell.class);
            let (_, cache_miss) = ResultsView::shape_cell(cell, color, window);
            assert!(!cache_miss, "unchanged cell layout should be reused");
        });
        assert!(
            cx.debug_bounds("result-row-0")
                .is_some_and(|bounds| bounds.size.height > px(0.)),
            "ready result rows should receive a visible layout"
        );
        assert_eq!(
            cx.debug_bounds("result-row-0")
                .map(|bounds| bounds.size.width),
            Some(px(ROW_NUMBER_WIDTH + DEFAULT_COLUMN_WIDTH * 2.0)),
            "row striping should stop after the final visible field"
        );
        assert_eq!(
            cx.debug_bounds("result-header")
                .map(|bounds| bounds.size.height),
            Some(px(HEADER_HEIGHT)),
            "column names and types need a full two-line header"
        );
        let first_column_before = cx.debug_bounds("result-column-0").unwrap().size.width;
        let resize_handle = cx.debug_bounds("resize-result-column-0").unwrap();
        let resize_start = gpui::point(
            resize_handle.left() + resize_handle.size.width / 2.0,
            resize_handle.top() + resize_handle.size.height / 2.0,
        );
        cx.simulate_mouse_down(resize_start, MouseButton::Left, gpui::Modifiers::default());
        cx.simulate_mouse_move(
            gpui::point(resize_start.x + px(8.0), resize_start.y),
            MouseButton::Left,
            gpui::Modifiers::default(),
        );
        cx.simulate_mouse_move(
            gpui::point(resize_start.x + px(72.0), resize_start.y),
            MouseButton::Left,
            gpui::Modifiers::default(),
        );
        cx.simulate_mouse_up(
            gpui::point(resize_start.x + px(72.0), resize_start.y),
            MouseButton::Left,
            gpui::Modifiers::default(),
        );
        cx.run_until_parked();
        let first_column_after = cx.debug_bounds("result-column-0").unwrap().size.width;
        assert!(
            first_column_after >= first_column_before + px(65.0),
            "dragging a column divider must resize that column: {first_column_before:?} -> {first_column_after:?}"
        );
        assert_eq!(
            cx.debug_bounds("result-column-1").unwrap().size.width,
            px(DEFAULT_COLUMN_WIDTH),
            "resizing one column must not resize its neighbor"
        );
        let first_row = cx.debug_bounds("result-row-0").unwrap();
        cx.simulate_mouse_down(
            gpui::point(
                first_row.left() + px(ROW_NUMBER_WIDTH + 12.),
                first_row.top() + px(ROW_HEIGHT / 2.),
            ),
            MouseButton::Left,
            gpui::Modifiers::default(),
        );
        assert_eq!(
            view.read_with(&cx, |view, _| view.selected),
            Some(GridSelection::Cell { row: 0, column: 0 }),
            "selection feedback must occur on press, without waiting for mouse-up"
        );
        cx.simulate_mouse_down(
            gpui::point(
                first_row.left() + px(ROW_NUMBER_WIDTH + 12.),
                first_row.top() + px(ROW_HEIGHT / 2.),
            ),
            MouseButton::Left,
            gpui::Modifiers::default(),
        );
        assert_eq!(
            view.read_with(&cx, |view, _| view.selected),
            Some(GridSelection::Cell { row: 0, column: 0 }),
            "pressing the selected cell again must keep it available for editing"
        );
        view.update(&mut cx, |view, cx| {
            view.select_tab(ResultTab::Messages, cx);
            assert_eq!(view.active_tab(), ResultTab::Messages);
            view.select_relative_tab(1, cx);
            assert_eq!(view.active_tab(), ResultTab::Explain);
            view.select_relative_tab(-1, cx);
            assert_eq!(view.active_tab(), ResultTab::Messages);
            view.focus_data(cx);
            assert_eq!(view.active_tab(), ResultTab::Data);
            assert_eq!(view.selected_text().as_deref(), Some("neo"));
            view.row_scroll_handle
                .scroll_to_item(1, ScrollStrategy::Top);
            view.move_selection(0, 1, cx);
            assert_eq!(view.selected_text().as_deref(), Some("1"));
            assert_eq!(view.row_scroll_handle.logical_scroll_top_index(), 1);
            view.move_selection(1, 0, cx);
            assert_eq!(view.selected_text().as_deref(), Some("2"));
            view.move_selection(0, -1, cx);
            assert_eq!(view.selected_text().as_deref(), Some("trinity"));
            view.select_row(0, cx);
            assert_eq!(view.selected, Some(GridSelection::Row(0)));
            view.select_row(0, cx);
            assert_eq!(view.selected, None);
            view.select_row(0, cx);
            assert_eq!(view.selected_text().as_deref(), Some("neo\t1"));
            view.select_column(1, cx);
            assert_eq!(view.selected, Some(GridSelection::Column(1)));
            view.select_column(1, cx);
            assert_eq!(view.selected, None);
            view.select_column(1, cx);
            assert_eq!(view.selected_text().as_deref(), Some("1\n2"));
            view.select_all(cx);
            assert_eq!(view.selected, Some(GridSelection::All));
            view.select_all(cx);
            assert_eq!(view.selected, None);
            view.select_all(cx);
            assert_eq!(view.selected_text().as_deref(), Some("neo\t1\ntrinity\t2"));
            view.select_cell(0, 0, cx);
            view.extend_cell_selection(1, 1, cx);
            assert_eq!(
                view.selected,
                Some(GridSelection::Range {
                    anchor_row: 0,
                    anchor_column: 0,
                    focus_row: 1,
                    focus_column: 1,
                })
            );
            assert_eq!(view.selected_text().as_deref(), Some("neo\t1\ntrinity\t2"));
            assert_eq!(
                view.selected_text_with_headers().as_deref(),
                Some("name\trank\nneo\t1\ntrinity\t2")
            );
            assert_eq!(view.placement(), ResultPlacement::Bottom);
            view.set_extent(300.0, cx);
            assert_eq!(view.extent(), 300.0);
            view.toggle_placement(cx);
            assert_eq!(view.placement(), ResultPlacement::Right);
            assert_eq!(view.extent(), 420.0);
            view.set_extent(360.0, cx);
            view.toggle_placement(cx);
            assert_eq!(view.extent(), 300.0);
        });
        view.update_in(&mut cx, |view, window, cx| {
            view.set_selection(GridSelection::Cell { row: 0, column: 0 }, cx);
            view.toggle_visual_selection(&ToggleVisualSelection, window, cx);
            view.move_selection(0, 1, cx);
            view.move_selection(1, 0, cx);
            assert_eq!(
                view.selected,
                Some(GridSelection::Range {
                    anchor_row: 0,
                    anchor_column: 0,
                    focus_row: 1,
                    focus_column: 1,
                })
            );
            view.copy_selected_cell(&CopySelectedCell, window, cx);
            assert!(!view.visual_selection);
            assert_eq!(
                cx.read_from_clipboard()
                    .and_then(|item| item.text())
                    .as_deref(),
                Some("neo\t1\ntrinity\t2")
            );
        });
    }

    #[gpui::test]
    fn dragging_headers_reorders_display_selection_and_copy(cx: &mut TestAppContext) {
        let window = cx
            .update(|cx| {
                cx.open_window(Default::default(), |_window, cx| {
                    let view = cx.new(ResultsView::new);
                    cx.new(|_| ResultsHost(view))
                })
            })
            .unwrap();
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        let host = window.root(&mut cx).unwrap();
        let view = host.read_with(&cx, |host, _| host.0.clone());

        view.update(&mut cx, |view, cx| {
            view.set_state(
                ResultState::from_execute(ExecuteResponse {
                    cursor_id: sift_protocol::CursorId(1),
                    columns: vec![
                        column("a", PrimitiveType::Text, Nullability::NotNullable),
                        column("b", PrimitiveType::Text, Nullability::NotNullable),
                        column("c", PrimitiveType::Text, Nullability::NotNullable),
                    ],
                    schema_digest: "d".into(),
                    rows: vec![
                        Row::new(vec![
                            Value::Text("a1".into()),
                            Value::Text("b1".into()),
                            Value::Text("c1".into()),
                        ]),
                        Row::new(vec![
                            Value::Text("a2".into()),
                            Value::Text("b2".into()),
                            Value::Text("c2".into()),
                        ]),
                    ],
                    affected_rows: None,
                    warnings: Vec::new(),
                    has_more: false,
                }),
                cx,
            );
        });
        cx.run_until_parked();

        view.update(&mut cx, |view, cx| view.select_cell(0, 0, cx));
        let source = cx.debug_bounds("result-column-0").unwrap();
        let target = cx.debug_bounds("result-column-2").unwrap();
        let start = source.center();
        cx.simulate_mouse_down(start, MouseButton::Left, gpui::Modifiers::default());
        cx.simulate_mouse_move(
            gpui::point(start.x + px(6.), start.y),
            MouseButton::Left,
            gpui::Modifiers::default(),
        );
        cx.simulate_mouse_move(
            target.center(),
            MouseButton::Left,
            gpui::Modifiers::default(),
        );
        cx.simulate_mouse_up(
            target.center(),
            MouseButton::Left,
            gpui::Modifiers::default(),
        );
        cx.run_until_parked();

        view.update(&mut cx, |view, cx| {
            assert_eq!(
                view.column_order
                    .iter()
                    .map(|source| view.rendered_columns[*source].name.as_ref())
                    .collect::<Vec<_>>(),
                vec!["b", "c", "a"]
            );
            assert_eq!(
                view.selected,
                Some(GridSelection::Cell { row: 0, column: 0 }),
                "selected logical cell follows its dragged column"
            );
            assert_eq!(view.selected_cell(), Some((0, 2)));
            assert_eq!(view.selected_text().as_deref(), Some("a1"));

            view.select_cell(0, 1, cx);
            view.reorder_column(0, 1, cx);
            assert_eq!(
                view.selected,
                Some(GridSelection::Cell { row: 0, column: 1 }),
                "unmoved logical cell follows its shifted display position"
            );
            assert_eq!(view.selected_text().as_deref(), Some("b1"));

            view.reorder_column(0, 2, cx);
            view.select_row(0, cx);
            assert_eq!(view.selected_text().as_deref(), Some("b1\tc1\ta1"));
            view.select_all(cx);
            assert_eq!(
                view.selected_text().as_deref(),
                Some("b1\tc1\ta1\nb2\tc2\ta2"),
                "row and all-cell copy use displayed column order"
            );
        });

        view.update(&mut cx, |view, cx| {
            view.select_column(2, cx);
            view.set_column_included(2, false, cx);
            assert_eq!(view.visible_column_indices(), vec![1, 0]);
            assert_eq!(view.selected, None, "excluding selected field clears it");
            view.select_cell(0, 1, cx);
            view.move_selection(0, 1, cx);
            assert_eq!(view.selected_text().as_deref(), Some("a1"));
            view.select_row(0, cx);
            assert_eq!(
                view.selected_text().as_deref(),
                Some("b1\ta1"),
                "copy omits excluded fields"
            );
            view.set_column_included(2, true, cx);
            assert_eq!(view.visible_column_indices(), vec![1, 2, 0]);
            assert_eq!(
                view.inspector_fields()
                    .into_iter()
                    .map(|field| (field.name.to_string(), field.included))
                    .collect::<Vec<_>>(),
                vec![("b".into(), true), ("c".into(), true), ("a".into(), true)]
            );
        });
    }

    #[gpui::test]
    fn large_grid_virtualizes_cells_and_routes_wheel_axes(cx: &mut TestAppContext) {
        let window = cx
            .update(|cx| {
                cx.open_window(Default::default(), |_window, cx| {
                    let view = cx.new(ResultsView::new);
                    cx.new(|_| ResultsHost(view))
                })
            })
            .unwrap();
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        let host = window.root(&mut cx).unwrap();
        let view = host.read_with(&cx, |host, _| host.0.clone());
        let columns = (0..8)
            .map(|index| {
                column(
                    &format!("column_{index}"),
                    PrimitiveType::Int64,
                    Nullability::NotNullable,
                )
            })
            .collect();
        let rows = (0..100)
            .map(|row| {
                Row::new(
                    (0..8)
                        .map(|column| Value::Int64((row * 8 + column) as i64))
                        .collect(),
                )
            })
            .collect();
        view.update(&mut cx, |view, cx| {
            view.set_state(
                ResultState::from_execute(ExecuteResponse {
                    cursor_id: sift_protocol::CursorId(1),
                    columns,
                    schema_digest: "d".into(),
                    rows,
                    affected_rows: None,
                    warnings: Vec::new(),
                    has_more: false,
                }),
                cx,
            );
        });
        cx.run_until_parked();

        let shaped_count = |view: &ResultsView| {
            view.rendered_rows
                .iter()
                .flatten()
                .filter(|cell| cell.shaped.is_some())
                .count()
        };
        let initially_shaped = view.read_with(&cx, |view, _| shaped_count(view));
        assert!(initially_shaped > 0);
        assert!(
            initially_shaped < 100 * 8,
            "offscreen rows must remain unshaped"
        );

        view.update(&mut cx, |view, cx| {
            view.select_cell(0, 0, cx);
            for _ in 0..7 {
                view.move_selection(0, 1, cx);
            }
        });
        cx.run_until_parked();
        view.read_with(&cx, |view, _| {
            assert!(
                view.grid_scroll_handle.offset().x < px(0.),
                "moving the selected cell sideways should reveal its column"
            );
            view.grid_scroll_handle
                .set_offset(gpui::point(px(0.), px(0.)));
        });

        view.update(&mut cx, |view, cx| view.select_cell(0, 1, cx));
        cx.run_until_parked();
        assert_eq!(
            view.read_with(&cx, |view, _| shaped_count(view)),
            initially_shaped,
            "selection repaint must reuse visible shaped lines"
        );

        let viewport = cx.debug_bounds("result-row-viewport").unwrap();
        cx.simulate_event(gpui::ScrollWheelEvent {
            position: viewport.center(),
            delta: gpui::ScrollDelta::Pixels(gpui::point(px(0.), px(-120.))),
            ..Default::default()
        });
        cx.run_until_parked();
        let vertical_offset = view.read_with(&cx, |view, _| {
            assert_eq!(view.grid_scroll_handle.offset().x, px(0.));
            view.row_scroll_handle.0.borrow().base_handle.offset().y
        });
        assert!(vertical_offset < px(0.));

        cx.simulate_event(gpui::ScrollWheelEvent {
            position: viewport.center(),
            delta: gpui::ScrollDelta::Pixels(gpui::point(px(0.), px(-120.))),
            modifiers: gpui::Modifiers {
                shift: true,
                ..Default::default()
            },
            ..Default::default()
        });
        cx.run_until_parked();
        view.read_with(&cx, |view, _| {
            assert_eq!(
                view.row_scroll_handle.0.borrow().base_handle.offset().y,
                vertical_offset,
                "shift-scroll must not move rows"
            );
            assert!(
                view.grid_scroll_handle.offset().x < px(0.),
                "shift-scroll should move columns"
            );
        });
    }

    #[test]
    fn execution_errors_map_to_distinct_states() {
        assert!(matches!(
            ResultState::from_execution_error(true, "connection lost"),
            ResultState::OutcomeUnknown
        ));
        assert!(matches!(
            ResultState::from_execution_error(false, "query was canceled"),
            ResultState::Cancelled
        ));
        assert!(matches!(
            ResultState::from_execution_error(false, "statement timed out"),
            ResultState::TimedOut
        ));
        assert!(matches!(
            ResultState::from_execution_error(false, "syntax error at or near \"slect\""),
            ResultState::Failed(_)
        ));
    }

    #[test]
    fn distinct_states_have_distinct_labels() {
        assert_eq!(ResultState::Cancelled.status_label(), "Cancelled");
        assert_eq!(ResultState::TimedOut.status_label(), "Timed out");
        assert_eq!(
            ResultState::OutcomeUnknown.status_label(),
            "Outcome unknown"
        );
        assert_eq!(
            ResultState::Unavailable("No connection".into()).status_label(),
            "No connection"
        );
    }
}
