//! Query results surface: a `ResultSet` projection of the server's execution
//! outcome and a `ResultsView` that renders it as query-owned Data / Messages /
//! Explain / History tabs. The model maps real `sift_protocol` result types and
//! is GPUI-free so cell formatting and state transitions are unit-testable. The
//! grid virtualizes rows so paint cost tracks the viewport, not cardinality.

use std::ops::Range;

use gpui::{
    actions, canvas, div, prelude::*, px, uniform_list, App, ClipboardItem, Context, Div,
    FocusHandle, Focusable, IntoElement, MouseButton, Pixels, ScrollStrategy, ShapedLine,
    SharedString, Stateful, TextAlign, TextRun, UniformListScrollHandle, Window,
};
use sift_protocol::{
    ColumnMetadata, DriverWarning, ExecuteResponse, Nullability, Page, Row, TypeRef, Value,
};
use sift_ui::{ActiveTheme, Badge, Clickable, IconButton, IconName, ThemeColors};

const MIN_COLUMN_WIDTH: f32 = 144.0;
pub(crate) const ROW_NUMBER_WIDTH: f32 = 46.0;
pub(crate) const ROW_HEIGHT: f32 = 24.0;
const HEADER_HEIGHT: f32 = 40.0;
/// Hard UI retention bound. WebSocket ACK backpressure limits pages in flight;
/// this separately prevents an arbitrarily large completed query from growing
/// desktop memory without bound.
pub const MAX_RETAINED_ROWS: usize = 10_000;

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

/// A result column's presentation projection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResultColumn {
    pub name: String,
    pub type_label: String,
    pub nullable: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GridSelection {
    Cell { row: usize, column: usize },
    Row(usize),
    Column(usize),
    All,
}

impl GridSelection {
    fn highlights_row(self, row: usize) -> bool {
        matches!(
            self,
            Self::Cell { row: selected, .. } | Self::Row(selected) if selected == row
        ) || self == Self::All
    }

    fn highlights_column(self, column: usize) -> bool {
        matches!(
            self,
            Self::Cell {
                column: selected,
                ..
            } | Self::Column(selected) if selected == column
        ) || self == Self::All
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
    pub fn status_label(&self) -> String {
        match self {
            ResultState::Idle => "Ready".into(),
            ResultState::Pending => "Running…".into(),
            ResultState::Streaming(data) => format!("{}+ row(s) · Running…", data.rows.len()),
            ResultState::Ready(data) => match (data.rows.len(), data.affected_rows) {
                (0, Some(affected)) => format!("{affected} row(s) affected"),
                (rows, _) => {
                    let more = if data.has_more { "+" } else { "" };
                    format!("{rows}{more} row(s)")
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
        MoveCellLeft,
        MoveCellRight,
        MoveCellUp,
        MoveCellDown
    ]
);

/// The query-owned results surface.
pub struct ResultsView {
    focus_handle: FocusHandle,
    state: ResultState,
    /// Display-ready values are built once per result update, not repeatedly
    /// for every visible row during scroll and selection paints.
    rendered_columns: Vec<CachedColumnRender>,
    rendered_rows: Vec<Vec<CachedCellRender>>,
    tab: ResultTab,
    selected: Option<GridSelection>,
    row_scroll_handle: UniformListScrollHandle,
    grid_scroll_handle: gpui::ScrollHandle,
    placement: ResultPlacement,
    bottom_height: f32,
    right_width: f32,
    stream_result_seen: bool,
}

impl ResultsView {
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self {
            focus_handle: cx.focus_handle(),
            state: ResultState::Idle,
            rendered_columns: Vec::new(),
            rendered_rows: Vec::new(),
            tab: ResultTab::Data,
            selected: None,
            row_scroll_handle: UniformListScrollHandle::new(),
            grid_scroll_handle: gpui::ScrollHandle::new(),
            placement: ResultPlacement::Bottom,
            bottom_height: 240.0,
            right_width: 420.0,
            stream_result_seen: false,
        }
    }

    pub fn state(&self) -> &ResultState {
        &self.state
    }

    pub fn active_tab(&self) -> ResultTab {
        self.tab
    }

    #[cfg(test)]
    pub(crate) fn selected_cell(&self) -> Option<(usize, usize)> {
        match self.selected {
            Some(GridSelection::Cell { row, column }) => Some((row, column)),
            _ => None,
        }
    }

    pub(crate) fn placement(&self) -> ResultPlacement {
        self.placement
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

    /// Adopt a new outcome, resetting selection and focusing the Data tab.
    pub fn set_state(&mut self, state: ResultState, cx: &mut Context<Self>) {
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
        self.state = state;
        self.stream_result_seen = false;
        self.selected = None;
        self.tab = ResultTab::Data;
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

    /// Consume one server page. Returns true when the page ends the stream.
    pub fn apply_stream_page(&mut self, page: Page, cx: &mut Context<Self>) -> bool {
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
                }
                cx.notify();
                false
            }
            Page::Rows { rows } => {
                let ResultState::Streaming(data) = &mut self.state else {
                    unreachable!("stream initialized above")
                };
                if !data.truncated_extra_results {
                    let available = MAX_RETAINED_ROWS.saturating_sub(data.rows.len());
                    let dropped = rows.len().saturating_sub(available);
                    for row in rows.into_iter().take(available) {
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
                    data.has_more |= dropped > 0;
                }
                cx.notify();
                false
            }
            Page::Error { error } => {
                let state = match error.code {
                    sift_protocol::Code::QueryCanceled => ResultState::Cancelled,
                    sift_protocol::Code::QueryTimedOut => ResultState::TimedOut,
                    _ => ResultState::Failed(error.message),
                };
                self.set_state(state, cx);
                true
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
                data.warnings = warnings;
                self.state = ResultState::Ready(data);
                self.stream_result_seen = false;
                cx.notify();
                true
            }
        }
    }

    pub fn set_unavailable(&mut self, reason: impl Into<String>, cx: &mut Context<Self>) {
        self.set_state(ResultState::Unavailable(reason.into()), cx);
    }

    fn select_tab(&mut self, tab: ResultTab, cx: &mut Context<Self>) {
        self.tab = tab;
        cx.notify();
    }

    fn select_cell(&mut self, row: usize, column: usize, cx: &mut Context<Self>) {
        self.set_selection(GridSelection::Cell { row, column }, cx);
    }

    fn select_row(&mut self, row: usize, cx: &mut Context<Self>) {
        self.set_selection(GridSelection::Row(row), cx);
    }

    fn select_column(&mut self, column: usize, cx: &mut Context<Self>) {
        self.set_selection(GridSelection::Column(column), cx);
    }

    fn select_all(&mut self, cx: &mut Context<Self>) {
        self.set_selection(GridSelection::All, cx);
    }

    fn set_selection(&mut self, selection: GridSelection, cx: &mut Context<Self>) {
        if self.selected != Some(selection) {
            self.selected = Some(selection);
            cx.notify();
        }
    }

    fn move_selection(&mut self, row_delta: isize, column_delta: isize, cx: &mut Context<Self>) {
        let Some(data) = self.state.ready() else {
            return;
        };
        if data.rows.is_empty() || data.columns.is_empty() {
            return;
        }
        let (row, column) = match self.selected {
            Some(GridSelection::Cell { row, column }) => (row, column),
            Some(GridSelection::Row(row)) => (row, 0),
            Some(GridSelection::Column(column)) => (0, column),
            Some(GridSelection::All) | None => (0, 0),
        };
        let previous_row = row;
        let row = row
            .saturating_add_signed(row_delta)
            .min(data.rows.len() - 1);
        let column = column
            .saturating_add_signed(column_delta)
            .min(data.columns.len() - 1);
        self.set_selection(GridSelection::Cell { row, column }, cx);
        // Most repeated arrow events stay inside the viewport. Do not make the
        // uniform list resolve a deferred scroll request and relayout its rows
        // until selection actually crosses a visible edge.
        if row != previous_row && self.row_needs_reveal(row) {
            self.row_scroll_handle
                .scroll_to_item(row, ScrollStrategy::Nearest);
        }
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

    fn copy_selected_cell(&mut self, _: &CopySelectedCell, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = self.selected_text() {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
    }

    fn selected_text(&self) -> Option<String> {
        let row_text = |row: &[CachedCellRender]| {
            row.iter()
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
            GridSelection::Row(row) => self.rendered_rows.get(row).map(|row| row_text(row)),
            GridSelection::Column(column) => {
                let values = self
                    .rendered_rows
                    .iter()
                    .filter_map(|row| row.get(column))
                    .map(|cell| cell.text.to_string())
                    .collect::<Vec<_>>();
                (!values.is_empty()).then(|| values.join("\n"))
            }
            GridSelection::All => Some(
                self.rendered_rows
                    .iter()
                    .map(|row| row_text(row))
                    .collect::<Vec<_>>()
                    .join("\n"),
            ),
        }
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
                    .max_w(px(260.))
                    .min_w_0()
                    .overflow_hidden()
                    .items_center()
                    .gap_2()
                    .px_2()
                    .text_xs()
                    .text_color(colors.muted_text)
                    .child(Badge::new(self.state.status_label()))
                    .child(
                        IconButton::new(
                            "copy-result-cell",
                            IconName::Copy,
                            "Copy highlighted fields",
                        )
                        .square(px(24.))
                        .icon_size(13.)
                        .tooltip("Copy highlighted fields")
                        .on_click(cx.listener(|view, _, window, cx| {
                            view.copy_selected_cell(&CopySelectedCell, window, cx)
                        })),
                    ),
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
                    .child(div().truncate().child(self.state.status_label()))
                    .child(
                        IconButton::new(
                            "copy-result-cell-vertical",
                            IconName::Copy,
                            "Copy highlighted fields",
                        )
                        .icon_size(13.)
                        .text("Copy")
                        .tooltip("Copy highlighted fields")
                        .on_click(cx.listener(|view, _, window, cx| {
                            view.copy_selected_cell(&CopySelectedCell, window, cx)
                        })),
                    ),
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
        let grid_min_width =
            px(ROW_NUMBER_WIDTH + MIN_COLUMN_WIDTH * self.rendered_columns.len() as f32);
        let header =
            div()
                .debug_selector(|| "result-header".into())
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
                .children(self.rendered_columns.iter().enumerate().map(
                    |(column_index, column)| {
                        div()
                            .id(("result-column", column_index))
                            .role(gpui::Role::Button)
                            .aria_label(format!("Select column {}", column.name))
                            .flex_1()
                            .min_w(px(MIN_COLUMN_WIDTH))
                            .px_2()
                            .flex()
                            .flex_col()
                            .justify_center()
                            .overflow_hidden()
                            .border_r_1()
                            .border_color(colors.subtle_border)
                            .when(
                                self.selected.is_some_and(|selection| {
                                    selection.highlights_column(column_index)
                                }),
                                |header| header.bg(colors.selected_surface).text_color(colors.text),
                            )
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |view, _, window, cx| {
                                    view.focus_handle.focus(window, cx);
                                    view.select_column(column_index, cx);
                                }),
                            )
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
                            )
                    },
                ));

        let row_count = data.rows.len();
        let grid_scroll_handle = self.grid_scroll_handle.clone();
        let row_scroll_handle = self.row_scroll_handle.clone();
        let entity_id = cx.entity().entity_id();
        let list = uniform_list(
            "result-rows",
            row_count,
            cx.processor(move |view, range: Range<usize>, window, cx| {
                let colors = cx.theme().colors;
                if view.state.ready().is_none() {
                    return Vec::new();
                }
                let column_count = view.rendered_columns.len();
                let selected = view.selected;
                range
                    .map(|row_index| {
                        let cells = (0..column_count)
                            .map(|column_index| {
                                let rendered = view
                                    .rendered_rows
                                    .get_mut(row_index)
                                    .and_then(|row| row.get_mut(column_index));
                                let is_selected = match selected {
                                    Some(GridSelection::Cell { row, column }) => {
                                        row == row_index && column == column_index
                                    }
                                    Some(GridSelection::Row(row)) => row == row_index,
                                    Some(GridSelection::Column(column)) => column == column_index,
                                    Some(GridSelection::All) => true,
                                    None => false,
                                };
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
                                div()
                                    .id(("cell", row_index * column_count + column_index))
                                    .flex_1()
                                    .min_w(px(MIN_COLUMN_WIDTH))
                                    .h(px(ROW_HEIGHT))
                                    .px_2()
                                    .flex()
                                    .items_center()
                                    .overflow_hidden()
                                    .border_r_1()
                                    .border_color(colors.subtle_border)
                                    .text_color(color)
                                    .when(is_selected, |el| el.bg(colors.selected_surface))
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(move |view, _, window, cx| {
                                            view.focus_handle.focus(window, cx);
                                            view.select_cell(row_index, column_index, cx)
                                        }),
                                    )
                                    .children(shaped.map(|line| {
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
                            })
                            .collect::<Vec<_>>();
                        div()
                            .debug_selector(move || format!("result-row-{row_index}"))
                            .flex()
                            .w_full()
                            .min_w(grid_min_width)
                            .h(px(ROW_HEIGHT))
                            .when(row_index % 2 == 1, |el| el.bg(colors.grid_stripe))
                            .child(
                                div()
                                    .id(("result-row-number", row_index))
                                    .role(gpui::Role::Button)
                                    .aria_label(format!("Select row {}", row_index + 1))
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
                                    .child((row_index + 1).to_string()),
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

    fn render_messages(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = cx.theme().colors;
        let body = div().p_3().flex().flex_col().gap_1().text_sm();
        match &self.state {
            ResultState::Streaming(data) | ResultState::Ready(data) => {
                let mut rows: Vec<gpui::AnyElement> = Vec::new();
                if let Some(affected) = data.affected_rows {
                    rows.push(
                        div()
                            .child(format!("{affected} row(s) affected"))
                            .into_any_element(),
                    );
                }
                if data.truncated_extra_results {
                    rows.push(
                        div()
                            .text_color(colors.warning)
                            .child("Additional result sets were truncated to the first.")
                            .into_any_element(),
                    );
                }
                for warning in &data.warnings {
                    rows.push(
                        div()
                            .text_color(colors.warning)
                            .child(warning.message.clone())
                            .into_any_element(),
                    );
                }
                if rows.is_empty() {
                    rows.push(
                        div()
                            .text_color(colors.muted_text)
                            .child("No messages.")
                            .into_any_element(),
                    );
                }
                body.children(rows)
            }
            ResultState::Failed(message) => {
                body.child(div().text_color(colors.danger).child(message.clone()))
            }
            other => body.child(
                div()
                    .text_color(colors.muted_text)
                    .child(other.status_label()),
            ),
        }
    }
}

impl Focusable for ResultsView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl gpui::Render for ResultsView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = cx.theme().colors;
        let body = match self.tab {
            ResultTab::Data => self.render_grid(cx),
            ResultTab::Messages => self.render_messages(cx).into_any_element(),
            ResultTab::Explain => div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .p_4()
                .text_center()
                .text_color(colors.muted_text)
                .child("Run with EXPLAIN to see a plan here.")
                .into_any_element(),
            ResultTab::History => div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .p_4()
                .text_center()
                .text_color(colors.muted_text)
                .child("Query history appears here.")
                .into_any_element(),
        };

        div()
            .id("sift-results")
            .key_context("SiftResults")
            .track_focus(&self.focus_handle)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|view, _, window, cx| view.focus_handle.focus(window, cx)),
            )
            .on_action(cx.listener(Self::copy_selected_cell))
            .on_action(cx.listener(Self::move_cell_left))
            .on_action(cx.listener(Self::move_cell_right))
            .on_action(cx.listener(Self::move_cell_up))
            .on_action(cx.listener(Self::move_cell_down))
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
            .child(div().flex().flex_1().min_w_0().min_h_0().child(body))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{Entity, TestAppContext, VisualTestContext};
    use sift_protocol::{Code, DriverError, PrimitiveType};

    struct ResultsHost(Entity<ResultsView>);

    impl gpui::Render for ResultsHost {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div().w(px(800.)).h(px(400.)).flex().child(self.0.clone())
        }
    }

    fn column(name: &str, primitive: PrimitiveType, nullable: Nullability) -> ColumnMetadata {
        let mut column = ColumnMetadata::new(name, TypeRef::Primitive(primitive));
        column.nullable = nullable;
        column
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

    #[gpui::test]
    fn streamed_pages_append_incrementally_and_respect_retention_bound(cx: &mut TestAppContext) {
        let view = cx.update(|cx| cx.new(ResultsView::new));
        view.update(cx, |view, cx| {
            view.begin_stream(cx);
            assert!(!view.apply_stream_page(
                Page::NextResult {
                    columns: vec![column("id", PrimitiveType::Int64, Nullability::NotNullable,)],
                },
                cx,
            ));
            let rows = (0..MAX_RETAINED_ROWS + 5)
                .map(|value| Row::new(vec![Value::Int64(value as i64)]))
                .collect();
            assert!(!view.apply_stream_page(Page::Rows { rows }, cx));
            let ResultState::Streaming(data) = view.state() else {
                panic!("expected streaming result")
            };
            assert_eq!(data.rows.len(), MAX_RETAINED_ROWS);
            assert!(data.has_more);
            assert_eq!(view.rendered_rows.len(), MAX_RETAINED_ROWS);

            assert!(view.apply_stream_page(
                Page::Done {
                    affected_rows: None,
                    warnings: Vec::new(),
                },
                cx,
            ));
            assert!(matches!(view.state(), ResultState::Ready(data) if data.has_more));
        });
    }

    #[gpui::test]
    fn streamed_terminal_errors_keep_distinct_outcomes(cx: &mut TestAppContext) {
        let view = cx.update(|cx| cx.new(ResultsView::new));
        view.update(cx, |view, cx| {
            view.begin_stream(cx);
            assert!(view.apply_stream_page(
                Page::Error {
                    error: DriverError::new(Code::QueryCanceled, "query was canceled"),
                },
                cx,
            ));
            assert!(matches!(view.state(), ResultState::Cancelled));

            view.begin_stream(cx);
            assert!(view.apply_stream_page(
                Page::Error {
                    error: DriverError::new(Code::QueryTimedOut, "query timed out"),
                },
                cx,
            ));
            assert!(matches!(view.state(), ResultState::TimedOut));
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
            cx.debug_bounds("result-header")
                .map(|bounds| bounds.size.height),
            Some(px(HEADER_HEIGHT)),
            "column names and types need a full two-line header"
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
        view.update(&mut cx, |view, cx| {
            view.select_tab(ResultTab::Messages, cx);
            assert_eq!(view.active_tab(), ResultTab::Messages);
            view.select_cell(0, 0, cx);
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
            assert_eq!(view.selected_text().as_deref(), Some("neo\t1"));
            view.select_column(1, cx);
            assert_eq!(view.selected, Some(GridSelection::Column(1)));
            assert_eq!(view.selected_text().as_deref(), Some("1\n2"));
            view.select_all(cx);
            assert_eq!(view.selected, Some(GridSelection::All));
            assert_eq!(view.selected_text().as_deref(), Some("neo\t1\ntrinity\t2"));
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
