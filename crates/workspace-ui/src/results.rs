//! Query results surface: a `ResultSet` projection of the server's execution
//! outcome and a `ResultsView` that renders it as query-owned Data / Messages /
//! Explain / History tabs. The model maps real `sift_protocol` result types and
//! is GPUI-free so cell formatting and state transitions are unit-testable. The
//! grid virtualizes rows so paint cost tracks the viewport, not cardinality.

use std::ops::Range;

use gpui::{
    actions, div, prelude::*, px, uniform_list, App, ClipboardItem, Context, FocusHandle,
    Focusable, IntoElement, Window,
};
use sift_protocol::{
    ColumnMetadata, DriverWarning, ExecuteResponse, Nullability, Page, Row, TypeRef, Value,
};
use sift_ui::{icon, IconName, Theme};

const MIN_COLUMN_WIDTH: f32 = 144.0;
const ROW_NUMBER_WIDTH: f32 = 46.0;
const ROW_HEIGHT: f32 = 24.0;

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

/// A result column's presentation projection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResultColumn {
    pub name: String,
    pub type_label: String,
    pub nullable: bool,
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
            ResultState::Ready(data) => Some(data),
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

actions!(sift_results, [CopySelectedCell]);

/// The query-owned results surface.
pub struct ResultsView {
    focus_handle: FocusHandle,
    theme: Theme,
    state: ResultState,
    tab: ResultTab,
    selected: Option<(usize, usize)>,
    placement: ResultPlacement,
    bottom_height: f32,
    right_width: f32,
}

impl ResultsView {
    pub fn new(theme: Theme, cx: &mut Context<Self>) -> Self {
        Self {
            focus_handle: cx.focus_handle(),
            theme,
            state: ResultState::Idle,
            tab: ResultTab::Data,
            selected: None,
            placement: ResultPlacement::Bottom,
            bottom_height: 240.0,
            right_width: 420.0,
        }
    }

    pub fn state(&self) -> &ResultState {
        &self.state
    }

    pub fn active_tab(&self) -> ResultTab {
        self.tab
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

    pub(crate) fn toggle_placement(&mut self, cx: &mut Context<Self>) {
        self.placement = match self.placement {
            ResultPlacement::Bottom => ResultPlacement::Right,
            ResultPlacement::Right => ResultPlacement::Bottom,
        };
        cx.notify();
    }

    pub fn set_theme(&mut self, theme: Theme, cx: &mut Context<Self>) {
        self.theme = theme;
        cx.notify();
    }

    /// Adopt a new outcome, resetting selection and focusing the Data tab.
    pub fn set_state(&mut self, state: ResultState, cx: &mut Context<Self>) {
        self.state = state;
        self.selected = None;
        self.tab = ResultTab::Data;
        cx.notify();
    }

    pub fn set_pending(&mut self, cx: &mut Context<Self>) {
        self.set_state(ResultState::Pending, cx);
    }

    pub fn set_unavailable(&mut self, reason: impl Into<String>, cx: &mut Context<Self>) {
        self.set_state(ResultState::Unavailable(reason.into()), cx);
    }

    fn select_tab(&mut self, tab: ResultTab, cx: &mut Context<Self>) {
        self.tab = tab;
        cx.notify();
    }

    fn select_cell(&mut self, row: usize, column: usize, cx: &mut Context<Self>) {
        self.selected = Some((row, column));
        cx.notify();
    }

    fn copy_selected_cell(&mut self, _: &CopySelectedCell, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = self.selected_cell_text() {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
    }

    fn selected_cell_text(&self) -> Option<String> {
        let (row, column) = self.selected?;
        let data = self.state.ready()?;
        let value = data.rows.get(row)?.values.get(column)?;
        Some(render_value(value).text)
    }

    fn cell_color(&self, class: CellClass) -> gpui::Hsla {
        let colors = self.theme.colors;
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

    fn render_tab_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = self.theme.colors;
        div()
            .h(px(30.))
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
                                let selected = tab == self.tab;
                                div()
                                    .id(("result-tab", tab as usize))
                                    .flex_none()
                                    .flex()
                                    .items_center()
                                    .h_full()
                                    .px_3()
                                    .relative()
                                    .text_sm()
                                    .when(selected, |el| el.text_color(colors.text))
                                    .when(selected, |el| {
                                        el.child(
                                            div()
                                                .absolute()
                                                .left_2()
                                                .right_2()
                                                .bottom_0()
                                                .h(px(1.))
                                                .bg(colors.accent),
                                        )
                                    })
                                    .when(!selected, |el| el.text_color(colors.muted_text))
                                    .hover(|el| el.text_color(colors.text))
                                    .on_click(
                                        cx.listener(move |view, _, _, cx| view.select_tab(tab, cx)),
                                    )
                                    .child(tab.label())
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
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .px_1()
                            .rounded(px(3.))
                            .bg(colors.hovered_surface)
                            .child(self.state.status_label()),
                    )
                    .child(
                        div()
                            .id("copy-result-cell")
                            .flex_none()
                            .role(gpui::Role::Button)
                            .aria_label("Copy selected result cell")
                            .size(px(24.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(4.))
                            .hover(|button| button.bg(colors.hovered_surface))
                            .on_click(cx.listener(|view, _, window, cx| {
                                view.copy_selected_cell(&CopySelectedCell, window, cx)
                            }))
                            .child(icon(IconName::Copy, colors.muted_text, 13.)),
                    ),
            )
    }

    fn render_vertical_tab_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = self.theme.colors;
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
                    .h(px(30.))
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
                        div()
                            .id("copy-result-cell-vertical")
                            .role(gpui::Role::Button)
                            .aria_label("Copy selected result cell")
                            .h(px(24.))
                            .px_1()
                            .flex()
                            .items_center()
                            .gap_1()
                            .rounded(px(4.))
                            .hover(|button| button.bg(colors.hovered_surface))
                            .on_click(cx.listener(|view, _, window, cx| {
                                view.copy_selected_cell(&CopySelectedCell, window, cx)
                            }))
                            .child(icon(IconName::Copy, colors.muted_text, 13.))
                            .child("Copy"),
                    ),
            )
    }

    fn render_grid(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let colors = self.theme.colors;
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
        if data.columns.is_empty() {
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
        let grid_min_width = px(ROW_NUMBER_WIDTH + MIN_COLUMN_WIDTH * data.columns.len() as f32);
        let header = div()
            .debug_selector(|| "result-header".into())
            .flex()
            .h(px(ROW_HEIGHT + 4.0))
            .w_full()
            .min_w(grid_min_width)
            .border_b_1()
            .border_color(colors.subtle_border)
            .bg(colors.toolbar)
            .child(
                div()
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
                    .child("#"),
            )
            .children(data.columns.iter().map(|column| {
                div()
                    .flex_1()
                    .min_w(px(MIN_COLUMN_WIDTH))
                    .px_2()
                    .flex()
                    .flex_col()
                    .justify_center()
                    .overflow_hidden()
                    .border_r_1()
                    .border_color(colors.subtle_border)
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
            }));

        let row_count = data.rows.len();
        let list = uniform_list(
            "result-rows",
            row_count,
            cx.processor(move |view, range: Range<usize>, _, cx| {
                let colors = view.theme.colors;
                let Some(data) = view.state.ready() else {
                    return Vec::new();
                };
                let column_count = data.columns.len();
                range
                    .map(|row_index| {
                        let selected_row = view.selected.map(|(r, _)| r);
                        let cells = (0..column_count)
                            .map(|column_index| {
                                let rendered = data
                                    .rows
                                    .get(row_index)
                                    .and_then(|row| row.values.get(column_index))
                                    .map(render_value);
                                let is_selected = view.selected == Some((row_index, column_index));
                                let (text, color) = match &rendered {
                                    Some(cell) => (cell.text.clone(), view.cell_color(cell.class)),
                                    None => (String::new(), colors.muted_text),
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
                                    .when(
                                        rendered.as_ref().is_some_and(|cell| {
                                            matches!(cell.class, CellClass::Number)
                                        }),
                                        |el| el.justify_end(),
                                    )
                                    .when(is_selected, |el| {
                                        el.bg(colors.active_surface)
                                            .border_1()
                                            .border_color(colors.accent)
                                    })
                                    .on_click(cx.listener(move |view, _, _, cx| {
                                        view.select_cell(row_index, column_index, cx)
                                    }))
                                    .child(div().truncate().child(text))
                            })
                            .collect::<Vec<_>>();
                        div()
                            .debug_selector(move || format!("result-row-{row_index}"))
                            .flex()
                            .w_full()
                            .min_w(grid_min_width)
                            .h(px(ROW_HEIGHT))
                            .when(row_index % 2 == 1, |el| el.bg(colors.grid_stripe))
                            .when(selected_row == Some(row_index), |el| {
                                el.bg(colors.selected_surface)
                            })
                            .hover(|el| el.bg(colors.hovered_surface))
                            .child(
                                div()
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
                                    .child((row_index + 1).to_string()),
                            )
                            .children(cells)
                    })
                    .collect()
            }),
        )
        .size_full()
        .min_w(grid_min_width);

        div()
            .id("result-hscroll")
            .flex_1()
            .min_h_0()
            .overflow_x_scroll()
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

    fn render_messages(&self) -> impl IntoElement {
        let colors = self.theme.colors;
        let body = div().p_3().flex().flex_col().gap_1().text_sm();
        match &self.state {
            ResultState::Ready(data) => {
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
        let colors = self.theme.colors;
        let body = match self.tab {
            ResultTab::Data => self.render_grid(cx),
            ResultTab::Messages => self.render_messages().into_any_element(),
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
            .on_action(cx.listener(Self::copy_selected_cell))
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
    fn view_switches_tabs_and_copies_selected_cell(cx: &mut TestAppContext) {
        let window = cx
            .update(|cx| {
                cx.open_window(Default::default(), |_window, cx| {
                    let view = cx.new(|cx| ResultsView::new(Theme::dark(), cx));
                    cx.new(|_| ResultsHost(view))
                })
            })
            .unwrap();
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        let host = window.root(&mut cx).unwrap();
        let view = host.read_with(&cx, |host, _| host.0.clone());

        let response = ExecuteResponse {
            cursor_id: sift_protocol::CursorId(1),
            columns: vec![column("name", PrimitiveType::Text, Nullability::Nullable)],
            schema_digest: "d".into(),
            rows: vec![Row::new(vec![Value::Text("neo".into())])],
            affected_rows: None,
            warnings: Vec::new(),
            has_more: false,
        };
        view.update(&mut cx, |view, cx| {
            view.set_state(ResultState::from_execute(response), cx);
            assert_eq!(view.active_tab(), ResultTab::Data);
        });
        cx.run_until_parked();
        assert!(
            cx.debug_bounds("result-row-0")
                .is_some_and(|bounds| bounds.size.height > px(0.)),
            "ready result rows should receive a visible layout"
        );
        view.update(&mut cx, |view, cx| {
            view.select_tab(ResultTab::Messages, cx);
            assert_eq!(view.active_tab(), ResultTab::Messages);
            view.select_cell(0, 0, cx);
            assert_eq!(view.selected_cell_text().as_deref(), Some("neo"));
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
