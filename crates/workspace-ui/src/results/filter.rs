//! Prepared local grid predicates. Allocate during preparation, not per row.

use super::{CachedCellRender, CellClass, ResultsView};
use sift_protocol::{ResultFilterLogic, ResultFilterOperator};
use std::cmp::Ordering;

struct ColumnFilter {
    column: usize,
    operator: ResultFilterOperator,
    value: String,
    number: Option<f64>,
}

impl ColumnFilter {
    fn matches(&self, cell: Option<&CachedCellRender>) -> bool {
        let operator = self.operator;
        let Some(cell) = cell else {
            return operator == ResultFilterOperator::IsNull;
        };
        if operator == ResultFilterOperator::IsNull {
            return cell.class == CellClass::Null;
        }
        if operator == ResultFilterOperator::IsNotNull {
            return cell.class != CellClass::Null;
        }
        if cell.class == CellClass::Null {
            return false;
        }
        let value = self.value.as_str();
        // Text predicates need neither numeric parsing nor a comparison.
        match operator {
            ResultFilterOperator::Contains => return cell.filter_text.contains(value),
            ResultFilterOperator::NotContains => return !cell.filter_text.contains(value),
            ResultFilterOperator::StartsWith => return cell.filter_text.starts_with(value),
            ResultFilterOperator::EndsWith => return cell.filter_text.ends_with(value),
            _ => {}
        }
        let ordering = if cell.class == CellClass::Number {
            match (cell.text.parse::<f64>(), self.number) {
                (Ok(left), Some(right)) => left.partial_cmp(&right).unwrap_or(Ordering::Equal),
                _ => cell.filter_text.as_str().cmp(value),
            }
        } else {
            cell.filter_text.as_str().cmp(value)
        };
        match operator {
            ResultFilterOperator::Equals => ordering == Ordering::Equal,
            ResultFilterOperator::NotEquals => ordering != Ordering::Equal,
            ResultFilterOperator::GreaterThan => ordering == Ordering::Greater,
            ResultFilterOperator::GreaterThanOrEqual => ordering != Ordering::Less,
            ResultFilterOperator::LessThan => ordering == Ordering::Less,
            ResultFilterOperator::LessThanOrEqual => ordering != Ordering::Greater,
            _ => unreachable!("text and null predicates handled above"),
        }
    }
}

pub(super) struct PreparedFilters {
    logic: ResultFilterLogic,
    groups: Vec<(ResultFilterLogic, Vec<ColumnFilter>)>,
}

impl PreparedFilters {
    pub(super) fn new(view: &ResultsView) -> Self {
        let mut groups = view
            .filter_group_logics
            .iter()
            .map(|logic| (*logic, Vec::new()))
            .collect::<Vec<_>>();
        for (column, value) in view.column_filters.iter().enumerate() {
            if !view.filter_is_active(column) {
                continue;
            }
            let group = view.column_filter_groups.get(column).copied().unwrap_or(0);
            let Some((_, filters)) = groups.get_mut(group) else {
                continue;
            };
            let value = value.trim().to_lowercase();
            filters.push(ColumnFilter {
                column,
                operator: view
                    .column_filter_operators
                    .get(column)
                    .copied()
                    .unwrap_or_default(),
                number: value.parse().ok(),
                value,
            });
        }
        groups.retain(|(_, filters)| !filters.is_empty());
        Self {
            logic: view.filter_logic,
            groups,
        }
    }

    pub(super) fn matches(&self, cells: &[CachedCellRender]) -> bool {
        if self.groups.is_empty() {
            return true;
        }
        let mut groups = self.groups.iter().map(|(logic, filters)| {
            let mut matches = filters
                .iter()
                .map(|filter| filter.matches(cells.get(filter.column)));
            match logic {
                ResultFilterLogic::All => matches.all(|matched| matched),
                ResultFilterLogic::Any => matches.any(|matched| matched),
            }
        });
        match self.logic {
            ResultFilterLogic::All => groups.all(|matched| matched),
            ResultFilterLogic::Any => groups.any(|matched| matched),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::results::PreparedCellRender;

    #[test]
    fn predicates_preserve_null_numeric_and_text_semantics() {
        let cell = |text: &str, class| -> CachedCellRender {
            PreparedCellRender {
                text: text.to_owned().into(),
                paint_text: text.to_owned().into(),
                filter_text: text.to_lowercase(),
                class,
            }
            .into()
        };
        let number = cell("20", CellClass::Number);
        let text = cell("Éclair", CellClass::Text);
        let null = cell("NULL", CellClass::Null);
        for (operator, value, candidate, expected) in [
            (ResultFilterOperator::GreaterThan, "3", &number, true),
            (ResultFilterOperator::LessThan, "3", &number, false),
            (ResultFilterOperator::Equals, "20.0", &number, true),
            (ResultFilterOperator::Contains, "2", &number, true),
            (ResultFilterOperator::StartsWith, "é", &text, true),
            (ResultFilterOperator::EndsWith, "air", &text, true),
            (ResultFilterOperator::NotContains, "cake", &text, true),
            (ResultFilterOperator::NotEquals, "x", &null, false),
            (ResultFilterOperator::IsNull, "", &null, true),
            (ResultFilterOperator::IsNotNull, "", &null, false),
        ] {
            let filter = ColumnFilter {
                column: 0,
                operator,
                value: value.into(),
                number: value.parse().ok(),
            };
            assert_eq!(filter.matches(Some(candidate)), expected, "{operator:?}");
            assert_eq!(
                filter.matches(None),
                operator == ResultFilterOperator::IsNull
            );
        }
    }
}
