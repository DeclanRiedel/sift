//! Draft-based inline filters. Inputs own their text; typing never scans the grid.
use super::*;

const OPERATORS: [ResultFilterOperator; 12] = [
    ResultFilterOperator::Equals,
    ResultFilterOperator::NotEquals,
    ResultFilterOperator::Contains,
    ResultFilterOperator::NotContains,
    ResultFilterOperator::StartsWith,
    ResultFilterOperator::EndsWith,
    ResultFilterOperator::GreaterThan,
    ResultFilterOperator::GreaterThanOrEqual,
    ResultFilterOperator::LessThan,
    ResultFilterOperator::LessThanOrEqual,
    ResultFilterOperator::IsNull,
    ResultFilterOperator::IsNotNull,
];

#[derive(Debug, Clone, Default)]
pub(super) struct FilterSpec {
    pub logic: ResultFilterLogic,
    pub groups: Vec<FilterGroup>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{TestAppContext, VisualTestContext};

    #[gpui::test]
    fn drafts_apply_multiple_conditions_cancel_and_guard_staged_edits(cx: &mut TestAppContext) {
        let window = cx
            .update(|cx| cx.open_window(Default::default(), |_, cx| cx.new(ResultsView::new)))
            .unwrap();
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        let view = window.root(&mut cx).unwrap();
        view.update(&mut cx, |view, cx| {
            view.set_state(
                ResultState::Ready(ResultData {
                    columns: vec![ResultColumn {
                        name: "amount".into(),
                        type_label: "int64".into(),
                        nullable: false,
                    }],
                    rows: (1..=5)
                        .map(|value| Row::new(vec![Value::Int64(value)]))
                        .collect(),
                    ..ResultData::default()
                }),
                cx,
            )
        });
        view.update_in(&mut cx, |view, window, cx| {
            view.open_filter_builder(0, window, cx);
            let original_projection = view.display_rows.clone();
            let first = ResultsView::new_filter_condition(0, cx);
            let second = ResultsView::new_filter_condition(0, cx);
            let draft = view.filter_draft.as_mut().unwrap();
            draft.groups[0].conditions.push(first);
            let first = &mut draft.groups[0].conditions[0];
            first.condition.operator = ResultFilterOperator::GreaterThan;
            first.input.update(cx, |input, cx| input.set_text("1", cx));
            draft.groups[0].conditions.push(second);
            let second = &mut draft.groups[0].conditions[1];
            second.condition.operator = ResultFilterOperator::LessThan;
            second.input.update(cx, |input, cx| input.set_text("4", cx));
            assert_eq!(
                *view.display_rows,
                [0, 1, 2, 3, 4],
                "typing only changes the draft"
            );
            assert!(Arc::ptr_eq(&original_projection, &view.display_rows));
            view.apply_filter_builder(window, cx);
            assert_eq!(*view.display_rows, [1, 2]);
            assert!(view.filter_draft.is_none());
            view.open_filter_builder(0, window, cx);
            view.filter_draft.as_mut().unwrap().groups[0].conditions[0]
                .input
                .update(cx, |input, cx| input.set_text("bad number", cx));
            view.apply_filter_builder(window, cx);
            assert!(view.filter_draft.as_ref().unwrap().error.is_some());
            assert_eq!(*view.display_rows, [1, 2]);
            view.close_grid_transform(window, cx);
            view.open_filter_builder(0, window, cx);
            assert_eq!(
                view.filter_draft.as_ref().unwrap().groups[0].conditions[0]
                    .input
                    .read(cx)
                    .text(),
                "1"
            );
            view.filter_draft.as_mut().unwrap().groups[0].conditions[1]
                .condition
                .enabled = false;
            view.set_staged_row_deletions(1, cx);
            view.apply_filter_builder(window, cx);
            assert_eq!(*view.display_rows, [1, 2]);
            view.set_staged_row_deletions(0, cx);
            view.apply_filter_builder(window, cx);
            assert_eq!(*view.display_rows, [1, 2, 3, 4]);
            let stored = view.take_active_result_set().unwrap();
            view.restore_result_set(stored);
            assert!(view.filter_draft.is_none());
            assert_eq!(
                view.applied_filter.as_ref().unwrap().groups[0]
                    .conditions
                    .len(),
                2
            );
        });
        view.update_in(&mut cx, |view, window, cx| {
            view.open_filter_builder(0, window, cx)
        });
        let before = cx
            .debug_bounds("result-grid-transform-editor")
            .expect("filter builder");
        cx.simulate_keystrokes("c");
        assert_eq!(
            cx.debug_bounds("result-grid-transform-editor"),
            Some(before),
            "opening a choice must not resize the builder"
        );
        let field = cx.debug_bounds("filter-column-0").expect("column selector");
        let menu = cx
            .debug_bounds("filter-choice-menu")
            .expect("anchored choices");
        assert_eq!(menu.left(), field.left());
        assert!(menu.top() >= field.bottom());
        assert!(menu.size.width < before.size.width);
        assert!(view.read_with(&cx, |view, _| view
            .filter_draft
            .as_ref()
            .unwrap()
            .picker
            .is_some()));
        cx.simulate_keystrokes("escape");
        assert!(view.read_with(&cx, |view, _| view
            .filter_draft
            .as_ref()
            .unwrap()
            .picker
            .is_none()));
        cx.simulate_keystrokes("escape");
        assert!(view.read_with(&cx, |view, _| view.filter_draft.is_none()));
    }

    #[gpui::test]
    fn builder_preserves_empty_strings_and_nulls_and_combines_groups(cx: &mut TestAppContext) {
        let view = cx.new(ResultsView::new);
        view.update(cx, |view, cx| {
            view.set_state(
                ResultState::Ready(ResultData {
                    columns: vec![ResultColumn {
                        name: "name".into(),
                        type_label: "text".into(),
                        nullable: true,
                    }],
                    rows: vec![
                        Row::new(vec![Value::Text("".into())]),
                        Row::new(vec![Value::Null]),
                        Row::new(vec![Value::Text("other".into())]),
                    ],
                    ..ResultData::default()
                }),
                cx,
            );
            let condition = FilterCondition {
                column: 0,
                operator: ResultFilterOperator::Equals,
                value: "".into(),
                enabled: true,
            };
            view.applied_filter = Some(FilterSpec {
                logic: ResultFilterLogic::Any,
                groups: vec![
                    FilterGroup {
                        logic: ResultFilterLogic::All,
                        conditions: vec![condition.clone()],
                    },
                    FilterGroup {
                        logic: ResultFilterLogic::All,
                        conditions: vec![FilterCondition {
                            operator: ResultFilterOperator::IsNull,
                            ..condition
                        }],
                    },
                ],
            });
            view.rebuild_display_rows(cx);
            assert_eq!(*view.display_rows, [0, 1]);
            view.applied_filter.as_mut().unwrap().logic = ResultFilterLogic::All;
            view.rebuild_display_rows(cx);
            assert!(view.display_rows.is_empty());
        });
    }

    #[gpui::test]
    fn builder_sort_and_text_view_keep_changes_draft_until_apply(cx: &mut TestAppContext) {
        let window = cx
            .update(|cx| cx.open_window(Default::default(), |_, cx| cx.new(ResultsView::new)))
            .unwrap();
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        let view = window.root(&mut cx).unwrap();
        view.update(&mut cx, |view, cx| {
            view.set_state(
                ResultState::Ready(ResultData {
                    columns: vec![ResultColumn {
                        name: "score".into(),
                        type_label: "int64".into(),
                        nullable: false,
                    }],
                    rows: vec![
                        Row::new(vec![Value::Int64(2)]),
                        Row::new(vec![Value::Int64(1)]),
                    ],
                    ..ResultData::default()
                }),
                cx,
            );
        });
        view.update_in(&mut cx, |view, window, cx| {
            view.open_filter_builder(0, window, cx);
            let draft = view.filter_draft.as_mut().unwrap();
            draft.groups[0].conditions.clear();
            assert!(view.sorts.is_empty());
        });
        let add_sort = cx.debug_bounds("filter-add-sort").expect("add sort");
        cx.simulate_click(add_sort.center(), gpui::Modifiers::default());
        assert_eq!(
            view.read_with(&cx, |view, _| view
                .filter_draft
                .as_ref()
                .unwrap()
                .sorts
                .clone()),
            vec![(0, SortDirection::Ascending)]
        );
        let text = cx.debug_bounds("filter-view-text").expect("Text toggle");
        cx.simulate_click(text.center(), gpui::Modifiers::default());
        assert!(view.read_with(&cx, |view, _| view.filter_draft.as_ref().unwrap().text_view));
        view.update(&mut cx, |view, cx| {
            view.set_filter_sql_preview(Ok("SELECT * FROM scores ORDER BY score ASC".into()), cx);
        });
        assert!(view.read_with(&cx, |view, cx| view
            .filter_draft
            .as_ref()
            .unwrap()
            .sql_editor
            .as_ref()
            .unwrap()
            .read(cx)
            .document()
            .text()
            .contains("ORDER BY score ASC")));
        let builder = cx
            .debug_bounds("filter-view-builder")
            .expect("Builder toggle");
        cx.simulate_click(builder.center(), gpui::Modifiers::default());
        assert!(!view.read_with(&cx, |view, _| view.filter_draft.as_ref().unwrap().text_view));
        view.update_in(&mut cx, |view, window, cx| {
            view.apply_filter_builder(window, cx)
        });
        assert_eq!(
            view.read_with(&cx, |view, _| view.sorts.clone()),
            vec![(0, SortDirection::Ascending)]
        );
        assert_eq!(
            *view.read_with(&cx, |view, _| view.display_rows.clone()),
            [1, 0]
        );
    }
}

#[derive(Debug, Clone)]
pub(super) struct FilterGroup {
    pub logic: ResultFilterLogic,
    pub conditions: Vec<FilterCondition>,
}

#[derive(Debug, Clone)]
pub(super) struct FilterCondition {
    pub column: usize,
    pub operator: ResultFilterOperator,
    pub value: String,
    pub enabled: bool,
}

struct DraftCondition {
    condition: FilterCondition,
    input: Entity<TextInput>,
}

struct DraftGroup {
    logic: ResultFilterLogic,
    conditions: Vec<DraftCondition>,
}

pub(super) struct FilterDraft {
    focus: FocusHandle,
    selected: usize,
    picker_selected: usize,
    logic: ResultFilterLogic,
    groups: Vec<DraftGroup>,
    database: bool,
    text_view: bool,
    sorts: Vec<(usize, SortDirection)>,
    sql_editor: Option<Entity<QueryEditor>>,
    generated_sql: Option<String>,
    find_open: bool,
    error: Option<String>,
    picker: Option<(usize, usize, bool)>, // group, condition, column (otherwise operator); usize::MAX group = sort
    search: Entity<TextInput>,
    _search_subscription: Subscription,
}

fn logic_label(logic: ResultFilterLogic) -> &'static str {
    match logic {
        ResultFilterLogic::All => "All",
        ResultFilterLogic::Any => "Any",
    }
}

fn toggle(logic: &mut ResultFilterLogic) {
    *logic = match logic {
        ResultFilterLogic::All => ResultFilterLogic::Any,
        ResultFilterLogic::Any => ResultFilterLogic::All,
    };
}

impl FilterDraft {
    fn spec(&self, cx: &App) -> FilterSpec {
        FilterSpec {
            logic: self.logic,
            groups: self
                .groups
                .iter()
                .map(|group| FilterGroup {
                    logic: group.logic,
                    conditions: group
                        .conditions
                        .iter()
                        .map(|row| FilterCondition {
                            value: row.input.read(cx).text().to_owned(),
                            ..row.condition.clone()
                        })
                        .collect(),
                })
                .collect(),
        }
    }
}

impl ResultsView {
    fn render_filter_choice_field(
        &self,
        group: usize,
        row: usize,
        column: bool,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let draft = self.filter_draft.as_ref().unwrap();
        let sort_field = group == usize::MAX;
        let colors = cx.theme().colors;
        let label = if sort_field {
            self.rendered_columns[draft.sorts[row].0].name.clone()
        } else {
            let condition = &draft.groups[group].conditions[row].condition;
            if column {
                self.rendered_columns[condition.column].name.clone()
            } else {
                filter_operator_label(condition.operator).into()
            }
        };
        let active = draft.picker == Some((group, row, column));
        let id = if sort_field {
            4096 + row
        } else {
            group * 64 + row
        };
        let choices = if active {
            self.filter_choices(cx)
        } else {
            Vec::new()
        };
        div()
            .relative()
            .w(px(if column { 190. } else { 76. }))
            .flex_none()
            .child(
                div()
                    .id((
                        if column {
                            "filter-column"
                        } else {
                            "filter-operator"
                        },
                        id,
                    ))
                    .debug_selector(move || {
                        format!("filter-{}-{id}", if column { "column" } else { "operator" })
                    })
                    .role(gpui::Role::Button)
                    .aria_label(if column {
                        "Choose column"
                    } else {
                        "Choose operator"
                    })
                    .h(px(28.))
                    .px_2()
                    .flex()
                    .items_center()
                    .gap_2()
                    .rounded_sm()
                    .border_1()
                    .border_color(if active || (!column && !sort_field) {
                        colors.accent
                    } else {
                        colors.subtle_border
                    })
                    .bg(if !column && !sort_field {
                        colors.active_surface
                    } else {
                        colors.surface
                    })
                    .hover(|field| field.bg(colors.hovered_surface))
                    .on_click(cx.listener(move |view, _, window, cx| {
                        cx.stop_propagation();
                        view.filter_picker(group, row, column, window, cx);
                    }))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_sm()
                            .text_color(if !column && !sort_field {
                                colors.accent
                            } else {
                                colors.text
                            })
                            .child(label),
                    )
                    .child(icon(IconName::ChevronDown, colors.muted_text, 12.)),
            )
            .children(active.then(|| {
                div().absolute().top_full().left_0().child(
                    deferred(
                        anchored().anchor(gpui::Anchor::TopLeft).child(
                            div()
                                .id("filter-choice-menu")
                                .debug_selector(|| "filter-choice-menu".into())
                                .w(px(264.))
                                .p_1()
                                .mt_1()
                                .rounded_sm()
                                .border_1()
                                .border_color(colors.strong_border)
                                .bg(gpui::Hsla {
                                    a: 1.,
                                    ..colors.elevated_surface
                                })
                                .shadow_sm()
                                .occlude()
                                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                                .on_mouse_down_out(cx.listener(|view, _, window, cx| {
                                    if let Some(draft) = &mut view.filter_draft {
                                        draft.picker = None;
                                        draft.focus.focus(window, cx);
                                        cx.notify();
                                    }
                                }))
                                .child(
                                    div()
                                        .px_1()
                                        .pb_1()
                                        .border_b_1()
                                        .border_color(colors.subtle_border)
                                        .child(draft.search.clone()),
                                )
                                .child(
                                    div()
                                        .id("filter-picker-options")
                                        .max_h(px(196.))
                                        .overflow_y_scroll()
                                        .when(choices.is_empty(), |list| {
                                            list.child(
                                                div()
                                                    .p_2()
                                                    .text_sm()
                                                    .text_color(colors.muted_text)
                                                    .child("No matching choices"),
                                            )
                                        })
                                        .children(choices.into_iter().enumerate().map(
                                            |(position, (index, label))| {
                                                div()
                                                    .id(("filter-choice", index))
                                                    .h(px(28.))
                                                    .px_2()
                                                    .flex()
                                                    .items_center()
                                                    .rounded_sm()
                                                    .text_sm()
                                                    .when(
                                                        position == draft.picker_selected,
                                                        |item| item.bg(colors.active_surface),
                                                    )
                                                    .hover(|item| item.bg(colors.hovered_surface))
                                                    .on_click(cx.listener(
                                                        move |view, _, window, cx| {
                                                            cx.stop_propagation();
                                                            view.choose_filter_option(index, cx);
                                                            view.filter_draft
                                                                .as_ref()
                                                                .unwrap()
                                                                .focus
                                                                .focus(window, cx);
                                                        },
                                                    ))
                                                    .child(div().min_w_0().truncate().child(label))
                                            },
                                        )),
                                ),
                        ),
                    )
                    .with_priority(3),
                )
            }))
            .into_any_element()
    }

    fn filter_choices(&self, cx: &App) -> Vec<(usize, String)> {
        let Some(draft) = &self.filter_draft else {
            return Vec::new();
        };
        let Some((_, _, column)) = draft.picker else {
            return Vec::new();
        };
        let query = draft.search.read(cx).text().to_lowercase();
        let choices = if column {
            self.rendered_columns
                .iter()
                .enumerate()
                .map(|(index, col)| (index, col.name.to_string()))
                .collect::<Vec<_>>()
        } else {
            OPERATORS
                .iter()
                .enumerate()
                .map(|(index, operator)| (index, filter_operator_label(*operator).to_owned()))
                .collect()
        };
        choices
            .into_iter()
            .filter(|(_, label)| label.to_lowercase().contains(&query))
            .take(100)
            .collect()
    }

    fn choose_filter_option(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(draft) = &mut self.filter_draft else {
            return;
        };
        let Some((group, row, column)) = draft.picker.take() else {
            return;
        };
        if group == usize::MAX {
            if let Some(other) = draft
                .sorts
                .iter()
                .position(|(column, _)| *column == index && *column != draft.sorts[row].0)
            {
                draft.sorts[other].0 = draft.sorts[row].0;
            }
            draft.sorts[row].0 = index;
        } else {
            let condition = &mut draft.groups[group].conditions[row].condition;
            if column {
                condition.column = index;
            } else {
                condition.operator = OPERATORS[index];
            }
        }
        cx.notify();
    }

    pub(super) fn render_filter_summary(&self, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        let spec = self.applied_filter.as_ref()?;
        let count = spec
            .groups
            .iter()
            .flat_map(|group| &group.conditions)
            .filter(|condition| condition.enabled)
            .count();
        if count == 0 {
            return None;
        }
        let column = spec
            .groups
            .iter()
            .flat_map(|group| &group.conditions)
            .next()?
            .column;
        let join = |logic| match logic {
            ResultFilterLogic::All => " AND ",
            ResultFilterLogic::Any => " OR ",
        };
        let summary = spec
            .groups
            .iter()
            .filter_map(|group| {
                let conditions = group
                    .conditions
                    .iter()
                    .filter(|condition| condition.enabled)
                    .map(|condition| {
                        let value = if condition.operator.requires_value() {
                            format!(
                                " {:?}",
                                condition.value.chars().take(80).collect::<String>()
                            )
                        } else {
                            String::new()
                        };
                        format!(
                            "{} {}{value}",
                            self.rendered_columns[condition.column].name,
                            filter_operator_label(condition.operator)
                        )
                    })
                    .collect::<Vec<_>>();
                (!conditions.is_empty())
                    .then(|| format!("({})", conditions.join(join(group.logic))))
            })
            .collect::<Vec<_>>()
            .join(join(spec.logic));
        Some(
            div()
                .flex()
                .flex_wrap()
                .items_center()
                .gap_2()
                .px_2()
                .py_1()
                .bg(cx.theme().colors.toolbar)
                .child(div().flex_1().min_w_0().truncate().text_xs().child(summary))
                .child(div().text_xs().child(format!(
                    "Loaded rows · {count} conditions · {} / {} matched",
                    self.display_rows.len(),
                    self.rendered_rows.len()
                )))
                .child(
                    Button::new("edit-result-filter", "Edit")
                        .tone(ButtonTone::Ghost)
                        .on_click(cx.listener(move |view, _, window, cx| {
                            view.open_filter_builder(column, window, cx)
                        })),
                )
                .child(
                    Button::new("clear-result-filter", "Clear")
                        .tone(ButtonTone::Ghost)
                        .disabled(self.has_staged_changes())
                        .on_click(cx.listener(|view, _, _, cx| {
                            view.applied_filter = Some(FilterSpec::default());
                            view.rebuild_display_rows(cx);
                        })),
                )
                .into_any_element(),
        )
    }

    fn new_filter_condition(column: usize, cx: &mut Context<Self>) -> DraftCondition {
        DraftCondition {
            condition: FilterCondition {
                column,
                operator: ResultFilterOperator::Equals,
                value: String::new(),
                enabled: true,
            },
            input: cx.new(|cx| TextInput::new("", "Value", cx).aria_label("Filter value")),
        }
    }

    pub(super) fn open_filter_builder(
        &mut self,
        column: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.filter_draft.is_none() {
            let spec = self.applied_filter.clone().unwrap_or_else(|| FilterSpec {
                logic: self.filter_logic,
                groups: self
                    .filter_group_logics
                    .iter()
                    .enumerate()
                    .map(|(group, logic)| FilterGroup {
                        logic: *logic,
                        conditions: self
                            .column_filters
                            .iter()
                            .enumerate()
                            .filter(|(index, _)| {
                                self.filter_is_active(*index)
                                    && self.column_filter_groups[*index] == group
                            })
                            .map(|(index, value)| FilterCondition {
                                column: index,
                                operator: self.column_filter_operators[index],
                                value: value.clone(),
                                enabled: true,
                            })
                            .collect(),
                    })
                    .collect(),
            });
            let search = cx.new(|cx| {
                TextInput::new("", "Search choices…", cx).aria_label("Search filter choices")
            });
            let subscription = cx.subscribe_in(&search, window, |view, _, event, window, cx| {
                if *event == TextInputEvent::Submitted {
                    let selected = view
                        .filter_draft
                        .as_ref()
                        .map_or(0, |draft| draft.picker_selected);
                    if let Some((index, _)) = view.filter_choices(cx).get(selected).cloned() {
                        view.choose_filter_option(index, cx);
                        if let Some(draft) = &view.filter_draft {
                            draft.focus.focus(window, cx);
                        }
                    }
                } else if let Some(draft) = &mut view.filter_draft {
                    draft.picker_selected = 0;
                }
                cx.notify();
            });
            self.filter_draft = Some(FilterDraft {
                focus: cx.focus_handle(),
                selected: 0,
                picker_selected: 0,
                logic: spec.logic,
                database: false,
                text_view: false,
                sorts: self.sorts.clone(),
                sql_editor: None,
                generated_sql: None,
                find_open: false,
                error: None,
                picker: None,
                search,
                _search_subscription: subscription,
                groups: spec
                    .groups
                    .into_iter()
                    .map(|group| DraftGroup {
                        logic: group.logic,
                        conditions: group
                            .conditions
                            .into_iter()
                            .map(|condition| {
                                let input = cx.new(|cx| {
                                    TextInput::new(condition.value.clone(), "Value", cx)
                                        .aria_label("Filter value")
                                });
                                DraftCondition { condition, input }
                            })
                            .collect(),
                    })
                    .collect(),
            });
        }
        let draft = self.filter_draft.as_mut().unwrap();
        if draft.groups.is_empty() {
            draft.groups.push(DraftGroup {
                logic: ResultFilterLogic::All,
                conditions: Vec::new(),
            });
        }
        self.grid_transform_column = Some(column);
        self.grid_transform_tab = GridTransformTab::Filter;
        draft.focus.focus(window, cx);
        cx.notify();
    }

    fn filter_picker(
        &mut self,
        group: usize,
        row: usize,
        column: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(draft) = &mut self.filter_draft {
            draft.picker = Some((group, row, column));
            draft.picker_selected = 0;
            draft.search.update(cx, |input, cx| input.set_text("", cx));
            draft.search.focus_handle(cx).focus(window, cx);
            cx.notify();
        }
    }

    fn draft_result_transform(&self, cx: &App) -> sift_protocol::ResultTransform {
        let draft = self.filter_draft.as_ref().unwrap();
        let spec = draft.spec(cx);
        sift_protocol::ResultTransform {
            logic: spec.logic,
            groups: spec
                .groups
                .iter()
                .map(|group| sift_protocol::ResultFilterGroup {
                    logic: group.logic,
                    filters: group
                        .conditions
                        .iter()
                        .filter(|row| row.enabled)
                        .map(|row| sift_protocol::ResultFilter {
                            column: self.rendered_columns[row.column].name.to_string(),
                            operator: row.operator,
                            value: row.operator.requires_value().then(|| row.value.clone()),
                        })
                        .collect(),
                })
                .filter(|group| !group.filters.is_empty())
                .collect(),
            sorts: draft
                .sorts
                .iter()
                .map(|(column, direction)| sift_protocol::ResultSort {
                    column: self.rendered_columns[*column].name.to_string(),
                    direction: match direction {
                        SortDirection::Ascending => sift_protocol::ResultSortDirection::Ascending,
                        SortDirection::Descending => sift_protocol::ResultSortDirection::Descending,
                    },
                })
                .collect(),
        }
    }

    pub(crate) fn set_filter_sql_preview(
        &mut self,
        preview: Result<String, String>,
        cx: &mut Context<Self>,
    ) {
        let Some(draft) = self.filter_draft.as_mut() else {
            return;
        };
        match preview {
            Ok(sql) => {
                let sql = format!("{};\n", sql.trim_end().trim_end_matches(';').trim_end());
                if let Some(editor) = &draft.sql_editor {
                    editor.update(cx, |editor, cx| editor.replace_text_from_owner(&sql, cx));
                } else {
                    draft.sql_editor = Some(
                        cx.new(|cx| QueryEditor::new(QueryDocument::with_random_peer(&sql), cx)),
                    );
                }
                draft.generated_sql = Some(sql);
                draft.error = None;
            }
            Err(error) => draft.error = Some(error),
        }
        cx.notify();
    }

    fn apply_filter_builder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(draft) = &self.filter_draft else {
            return;
        };
        let spec = draft.spec(cx);
        let sorts = draft.sorts.clone();
        // Validate comparisons using observed scalar kinds, never display labels.
        // Null-only/empty columns remain unknown and are validated by the server.
        for condition in spec
            .groups
            .iter()
            .flat_map(|group| &group.conditions)
            .filter(|row| row.enabled)
        {
            let scalar_comparison = matches!(
                condition.operator,
                ResultFilterOperator::Equals
                    | ResultFilterOperator::NotEquals
                    | ResultFilterOperator::GreaterThan
                    | ResultFilterOperator::GreaterThanOrEqual
                    | ResultFilterOperator::LessThan
                    | ResultFilterOperator::LessThanOrEqual
            );
            let numeric = self
                .rendered_rows
                .iter()
                .filter_map(|row| row.get(condition.column))
                .find(|cell| cell.class != CellClass::Null)
                .is_some_and(|cell| cell.class == CellClass::Number);
            if scalar_comparison
                && numeric
                && !condition.value.parse::<f64>().is_ok_and(f64::is_finite)
            {
                self.filter_draft.as_mut().unwrap().error = Some(format!(
                    "{} requires a numeric value.",
                    self.rendered_columns[condition.column].name
                ));
                cx.notify();
                return;
            }
        }
        if self.has_staged_changes() {
            self.filter_draft.as_mut().unwrap().error =
                Some("Apply or discard staged data edits before filtering.".into());
            cx.notify();
            return;
        }
        if draft.database {
            let transform = self.draft_result_transform(cx);
            // The owning pane supplies query/connection context and audits execution.
            // Keep the draft available if execution fails; no optimistic local filtering.
            cx.emit(ResultsEvent::ApplyTransformRequested { transform });
            return;
        }
        self.applied_filter = Some(spec);
        self.sorts = sorts;
        self.rebuild_display_rows(cx);
        self.close_grid_transform(window, cx);
    }

    pub(super) fn render_filter_builder(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let draft = self.filter_draft.as_ref().unwrap();
        let colors = cx.theme().colors;
        let groups = draft
            .groups
            .iter()
            .enumerate()
            .map(|(group_index, group)| {
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .px_2()
                    .py_1()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .children((group.conditions.len() > 1).then(|| {
                                Button::new(
                                    ("filter-group-logic", group_index),
                                    format!("Match {}", logic_label(group.logic)),
                                )
                                .tone(ButtonTone::Ghost)
                                .on_click(cx.listener(
                                    move |view, _, _, cx| {
                                        toggle(
                                            &mut view.filter_draft.as_mut().unwrap().groups
                                                [group_index]
                                                .logic,
                                        );
                                        cx.notify();
                                    },
                                ))
                            }))
                            .child(
                                Button::new(("filter-add-condition", group_index), "+")
                                    .tone(ButtonTone::Ghost)
                                    .disabled(group.conditions.len() >= 64)
                                    .on_click(cx.listener(move |view, _, window, cx| {
                                        let row = Self::new_filter_condition(
                                            view.grid_transform_column.unwrap_or(0),
                                            cx,
                                        );
                                        row.input.focus_handle(cx).focus(window, cx);
                                        let draft = view.filter_draft.as_mut().unwrap();
                                        draft.picker = None;
                                        draft.groups[group_index].conditions.push(row);
                                        cx.notify();
                                    })),
                            )
                            .children((draft.groups.len() > 1).then(|| {
                                Button::new(("filter-remove-group", group_index), "Remove group")
                                    .tone(ButtonTone::Ghost)
                                    .on_click(cx.listener(move |view, _, _, cx| {
                                        let draft = view.filter_draft.as_mut().unwrap();
                                        draft.picker = None;
                                        draft.groups.remove(group_index);
                                        cx.notify();
                                    }))
                            })),
                    )
                    .when(group.conditions.is_empty(), |group| {
                        group.child(
                            div()
                                .px_2()
                                .py_1()
                                .text_sm()
                                .text_color(colors.muted_text)
                                .child("Click + to add filter criteria"),
                        )
                    })
                    .children(group.conditions.iter().enumerate().map(|(row_index, row)| {
                        let id = group_index * 64 + row_index;
                        let selected = draft.selected
                            == draft
                                .groups
                                .iter()
                                .take(group_index)
                                .map(|group| group.conditions.len())
                                .sum::<usize>()
                                + row_index;
                        let condition = &row.condition;
                        div()
                            .border_l_2()
                            .border_color(if selected {
                                colors.accent
                            } else {
                                colors.subtle_border
                            })
                            .pl_2()
                            .flex()
                            .flex_wrap()
                            .items_center()
                            .gap_1()
                            .child(
                                Button::new(
                                    ("filter-enabled", id),
                                    if condition.enabled { "✓" } else { "—" },
                                )
                                .tone(ButtonTone::Ghost)
                                .on_click(cx.listener(
                                    move |view, _, _, cx| {
                                        let row = &mut view.filter_draft.as_mut().unwrap().groups
                                            [group_index]
                                            .conditions[row_index];
                                        row.condition.enabled = !row.condition.enabled;
                                        cx.notify();
                                    },
                                )),
                            )
                            .child(self.render_filter_choice_field(
                                group_index,
                                row_index,
                                true,
                                cx,
                            ))
                            .child(self.render_filter_choice_field(
                                group_index,
                                row_index,
                                false,
                                cx,
                            ))
                            .when(condition.operator.requires_value(), |row_element| {
                                row_element.child(
                                    div()
                                        .w(px(220.))
                                        .h(px(28.))
                                        .px_2()
                                        .rounded_sm()
                                        .border_1()
                                        .border_color(colors.subtle_border)
                                        .bg(colors.surface)
                                        .child(row.input.clone()),
                                )
                            })
                            .child(
                                Button::new(("filter-remove-condition", id), "×")
                                    .tone(ButtonTone::Ghost)
                                    .on_click(cx.listener(move |view, _, _, cx| {
                                        let draft = view.filter_draft.as_mut().unwrap();
                                        draft.picker = None;
                                        draft.groups[group_index].conditions.remove(row_index);
                                        cx.notify();
                                    })),
                            )
                    }))
            })
            .collect::<Vec<_>>();
        let sort_rows = draft
            .sorts
            .iter()
            .enumerate()
            .map(|(index, (_, direction))| {
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .px_2()
                    .py_1()
                    .child(self.render_filter_choice_field(usize::MAX, index, true, cx))
                    .child(
                        Button::new(
                            ("filter-sort-direction", index),
                            if *direction == SortDirection::Ascending {
                                "↑ ASC"
                            } else {
                                "↓ DESC"
                            },
                        )
                        .tone(ButtonTone::Accent)
                        .on_click(cx.listener(move |view, _, _, cx| {
                            let direction = &mut view.filter_draft.as_mut().unwrap().sorts[index].1;
                            *direction = if *direction == SortDirection::Ascending {
                                SortDirection::Descending
                            } else {
                                SortDirection::Ascending
                            };
                            cx.notify();
                        })),
                    )
                    .child(
                        Button::new(("filter-remove-sort", index), "×")
                            .tone(ButtonTone::Ghost)
                            .on_click(cx.listener(move |view, _, _, cx| {
                                let draft = view.filter_draft.as_mut().unwrap();
                                draft.picker = None;
                                draft.sorts.remove(index);
                                cx.notify();
                            })),
                    )
            })
            .collect::<Vec<_>>();
        div()
            .id("result-grid-transform-editor")
            .track_focus(&draft.focus)
            .debug_selector(|| "result-grid-transform-editor".into())
            .flex_none()
            .flex()
            .flex_col()
            .border_b_1()
            .border_color(colors.subtle_border)
            .bg(colors.elevated_surface)
            .on_key_down(cx.listener(|view, event: &gpui::KeyDownEvent, window, cx| {
                let key = event.keystroke.key.as_str();
                if view
                    .filter_draft
                    .as_ref()
                    .is_some_and(|draft| draft.picker.is_some())
                    && matches!(key, "up" | "down")
                {
                    let count = view.filter_choices(cx).len();
                    let draft = view.filter_draft.as_mut().unwrap();
                    draft.picker_selected = draft
                        .picker_selected
                        .saturating_add_signed(if key == "up" { -1 } else { 1 })
                        .min(count.saturating_sub(1));
                    cx.notify();
                    cx.stop_propagation();
                    return;
                }
                if view
                    .filter_draft
                    .as_ref()
                    .is_some_and(|draft| !draft.text_view && draft.focus.is_focused(window))
                {
                    let draft = view.filter_draft.as_mut().unwrap();
                    let rows = draft
                        .groups
                        .iter()
                        .enumerate()
                        .flat_map(|(g, group)| (0..group.conditions.len()).map(move |r| (g, r)))
                        .collect::<Vec<_>>();
                    if matches!(key, "j" | "k" | "down" | "up") {
                        draft.selected = draft
                            .selected
                            .saturating_add_signed(if matches!(key, "k" | "up") { -1 } else { 1 })
                            .min(rows.len().saturating_sub(1));
                        cx.notify();
                        cx.stop_propagation();
                        return;
                    }
                    if let Some(&(group, row)) = rows.get(draft.selected) {
                        if key == "enter" {
                            draft.groups[group].conditions[row]
                                .input
                                .focus_handle(cx)
                                .focus(window, cx);
                            cx.stop_propagation();
                            return;
                        }
                        if matches!(key, "c" | "o") {
                            view.filter_picker(group, row, key == "c", window, cx);
                            cx.stop_propagation();
                            return;
                        }
                    }
                }
                if event.keystroke.key == "escape" {
                    if let Some(draft) = &mut view.filter_draft {
                        if draft.picker.take().is_some() || !draft.focus.is_focused(window) {
                            draft.focus.focus(window, cx);
                            cx.notify();
                        } else {
                            view.close_grid_transform(window, cx);
                        }
                    }
                    cx.stop_propagation();
                }
            }))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_2()
                    .py_1()
                    .child("Filter & Sort")
                    .child(
                        Button::new(
                            "filter-scope",
                            if draft.database {
                                "Database query"
                            } else {
                                "Loaded rows"
                            },
                        )
                        .tone(ButtonTone::Ghost)
                        .on_click(cx.listener(|view, _, _, cx| {
                            let draft = view.filter_draft.as_mut().unwrap();
                            draft.database = !draft.database;
                            cx.notify();
                        })),
                    )
                    .child(div().flex_1())
                    .children((!draft.text_view).then(|| {
                        Button::new("filter-see-full-sql", "See full SQL")
                            .debug_selector("filter-see-full-sql")
                            .tone(ButtonTone::Ghost)
                            .on_click(cx.listener(|view, _, _, cx| {
                                cx.emit(ResultsEvent::OpenTransformSqlRequested {
                                    transform: view.draft_result_transform(cx),
                                });
                            }))
                    }))
                    .child(
                        div()
                            .id("filter-view-toggle")
                            .role(gpui::Role::TabList)
                            .flex()
                            .items_center()
                            .rounded_sm()
                            .border_1()
                            .border_color(colors.subtle_border)
                            .bg(colors.toolbar)
                            .child(
                                div()
                                    .id("filter-view-builder")
                                    .debug_selector(|| "filter-view-builder".into())
                                    .role(gpui::Role::Tab)
                                    .aria_label("Builder")
                                    .h(px(25.))
                                    .px_2()
                                    .flex()
                                    .items_center()
                                    .rounded_sm()
                                    .bg(if draft.text_view {
                                        colors.toolbar
                                    } else {
                                        colors.active_surface
                                    })
                                    .text_color(if draft.text_view {
                                        colors.muted_text
                                    } else {
                                        colors.accent
                                    })
                                    .on_click(cx.listener(|view, _, window, cx| {
                                        let draft = view.filter_draft.as_mut().unwrap();
                                        draft.text_view = false;
                                        draft.focus.focus(window, cx);
                                        cx.notify();
                                    }))
                                    .child("Builder"),
                            )
                            .child(
                                div()
                                    .id("filter-view-text")
                                    .debug_selector(|| "filter-view-text".into())
                                    .role(gpui::Role::Tab)
                                    .aria_label("Text")
                                    .h(px(25.))
                                    .px_2()
                                    .flex()
                                    .items_center()
                                    .rounded_sm()
                                    .bg(if draft.text_view {
                                        colors.active_surface
                                    } else {
                                        colors.toolbar
                                    })
                                    .text_color(if draft.text_view {
                                        colors.accent
                                    } else {
                                        colors.muted_text
                                    })
                                    .on_click(cx.listener(|view, _, window, cx| {
                                        let regenerate =
                                            view.filter_draft.as_ref().is_some_and(|draft| {
                                                draft.sql_editor.as_ref().is_none_or(|editor| {
                                                    draft.generated_sql.as_deref()
                                                        == Some(editor.read(cx).document().text())
                                                })
                                            });
                                        let draft = view.filter_draft.as_mut().unwrap();
                                        draft.text_view = true;
                                        draft.picker = None;
                                        if let Some(editor) = &draft.sql_editor {
                                            editor.focus_handle(cx).focus(window, cx);
                                        } else {
                                            draft.focus.focus(window, cx);
                                        }
                                        if regenerate {
                                            cx.emit(ResultsEvent::PreviewTransformSqlRequested {
                                                transform: view.draft_result_transform(cx),
                                            });
                                        }
                                        cx.notify();
                                    }))
                                    .child("Text"),
                            ),
                    ),
            )
            .children((!draft.text_view).then(|| {
                div()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .id("filter-condition-scroll")
                            .min_h(px(96.))
                            .max_h(px(180.))
                            .overflow_y_scroll()
                            .children(groups),
                    )
                    .child(
                        div()
                            .border_t_1()
                            .border_color(colors.subtle_border)
                            .px_2()
                            .py_1()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child("Sort By")
                            .child(
                                Button::new("filter-add-sort", "+")
                                    .debug_selector("filter-add-sort")
                                    .tone(ButtonTone::Ghost)
                                    .disabled(draft.sorts.len() >= self.rendered_columns.len())
                                    .on_click(cx.listener(|view, _, _, cx| {
                                        let draft = view.filter_draft.as_mut().unwrap();
                                        let column =
                                            (0..view.rendered_columns.len()).find(|index| {
                                                !draft.sorts.iter().any(|(used, _)| used == index)
                                            });
                                        if let Some(column) = column {
                                            draft.sorts.push((column, SortDirection::Ascending));
                                            cx.notify();
                                        }
                                    })),
                            )
                            .when(draft.sorts.is_empty(), |row| {
                                row.child(
                                    div()
                                        .text_sm()
                                        .text_color(colors.muted_text)
                                        .child("Click + to add sort criteria"),
                                )
                            }),
                    )
                    .children(sort_rows)
            }))
            .children(draft.text_view.then(|| {
                div()
                    .h(px(190.))
                    .w_full()
                    .children(draft.sql_editor.iter().cloned())
                    .when(draft.sql_editor.is_none(), |panel| {
                        panel
                            .p_2()
                            .text_sm()
                            .text_color(colors.muted_text)
                            .child("Preparing SQL…")
                    })
            }))
            .children((draft.find_open && !draft.text_view).then(|| {
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap_2()
                    .px_2()
                    .py_1()
                    .child(div().w(px(220.)).child(self.grid_search_input.clone()))
                    .child(
                        Button::new("filter-find-next", "Next match")
                            .tone(ButtonTone::Ghost)
                            .on_click(cx.listener(|view, _, _, cx| {
                                view.select_next_search_match(cx);
                            })),
                    )
            }))
            .children(
                draft
                    .error
                    .as_ref()
                    .map(|error| div().px_2().text_color(colors.danger).child(error.clone())),
            )
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap_2()
                    .px_2()
                    .py_1()
                    .children((!draft.text_view && draft.groups.len() > 1).then(|| {
                        Button::new(
                            "filter-root-logic",
                            format!("Match {} groups", logic_label(draft.logic)),
                        )
                        .tone(ButtonTone::Ghost)
                        .on_click(cx.listener(|view, _, _, cx| {
                            toggle(&mut view.filter_draft.as_mut().unwrap().logic);
                            cx.notify();
                        }))
                    }))
                    .children((!draft.text_view).then(|| {
                        Button::new("filter-add-group", "+ Group")
                            .tone(ButtonTone::Ghost)
                            .disabled(draft.groups.len() >= 16)
                            .on_click(cx.listener(|view, _, _, cx| {
                                let row = Self::new_filter_condition(
                                    view.grid_transform_column.unwrap_or(0),
                                    cx,
                                );
                                view.filter_draft.as_mut().unwrap().groups.push(DraftGroup {
                                    logic: ResultFilterLogic::All,
                                    conditions: vec![row],
                                });
                                cx.notify();
                            }))
                    }))
                    .children((!draft.text_view).then(|| {
                        Button::new("filter-clear", "Clear")
                            .tone(ButtonTone::Ghost)
                            .on_click(cx.listener(|view, _, _, cx| {
                                let draft = view.filter_draft.as_mut().unwrap();
                                draft.groups.clear();
                                draft.sorts.clear();
                                draft.picker = None;
                                cx.notify();
                            }))
                    }))
                    .child(div().flex_1())
                    .children((!draft.text_view).then(|| {
                        Button::new("filter-find", "Find")
                            .tone(ButtonTone::Ghost)
                            .on_click(cx.listener(|view, _, window, cx| {
                                let draft = view.filter_draft.as_mut().unwrap();
                                draft.find_open = !draft.find_open;
                                if draft.find_open {
                                    view.grid_search_input.focus_handle(cx).focus(window, cx);
                                } else {
                                    draft.focus.focus(window, cx);
                                }
                                cx.notify();
                            }))
                    }))
                    .child(
                        Button::new("close-result-grid-transform-editor", "Cancel")
                            .debug_selector("close-result-grid-transform-editor")
                            .tone(ButtonTone::Ghost)
                            .on_click(cx.listener(|view, _, window, cx| {
                                view.close_grid_transform(window, cx)
                            })),
                    )
                    .child(
                        Button::new(
                            "filter-apply",
                            if draft.text_view {
                                "Open SQL in editor"
                            } else if draft.database {
                                "Run query"
                            } else {
                                "Apply Filter & Sort"
                            },
                        )
                        .debug_selector("filter-apply")
                        .tone(ButtonTone::Accent)
                        .on_click(cx.listener(|view, _, window, cx| {
                            if view
                                .filter_draft
                                .as_ref()
                                .is_some_and(|draft| draft.text_view)
                            {
                                if let Some(editor) =
                                    &view.filter_draft.as_ref().unwrap().sql_editor
                                {
                                    cx.emit(ResultsEvent::OpenSqlTextRequested {
                                        sql: editor.read(cx).document().text().to_owned(),
                                    });
                                }
                            } else {
                                view.apply_filter_builder(window, cx)
                            }
                        })),
                    ),
            )
            .into_any_element()
    }
}
