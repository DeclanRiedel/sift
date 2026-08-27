//! Conservative complete-result filter/sort rewriting before cursor creation.

use sift_protocol::{
    Engine, ResultFilter, ResultFilterLogic, ResultFilterOperator, ResultSortDirection,
    ResultTransform,
};

pub(crate) fn apply(
    engine: Engine,
    sql: &str,
    transform: &ResultTransform,
) -> Result<String, String> {
    if transform.groups.is_empty() && transform.sorts.is_empty() {
        return Ok(sql.to_owned());
    }
    if transform.groups.len() > 16 {
        return Err("result transform supports at most 16 filter groups".into());
    }
    let filter_count = transform
        .groups
        .iter()
        .map(|group| group.filters.len())
        .sum::<usize>();
    if filter_count > 64 {
        return Err("result transform supports at most 64 filters".into());
    }
    if transform.sorts.len() > 8 {
        return Err("result transform supports at most 8 sort columns".into());
    }

    let base = sql.trim().strip_suffix(';').unwrap_or(sql.trim()).trim();
    if base.contains(';') {
        return Err("result transforms require one SELECT statement".into());
    }
    let lower = base.to_ascii_lowercase();
    if !(lower.starts_with("select ") || lower.starts_with("with ")) {
        return Err("result transforms can only wrap SELECT queries".into());
    }

    let mut output = format!("SELECT * FROM ({base}) AS sift_result");
    let groups = transform
        .groups
        .iter()
        .filter(|group| !group.filters.is_empty())
        .map(|group| {
            let predicates = group
                .filters
                .iter()
                .map(|filter| predicate(engine, filter))
                .collect::<Result<Vec<_>, _>>()?;
            Ok(format!(
                "({})",
                predicates.join(match group.logic {
                    ResultFilterLogic::All => " AND ",
                    ResultFilterLogic::Any => " OR ",
                })
            ))
        })
        .collect::<Result<Vec<_>, String>>()?;
    if !groups.is_empty() {
        output.push_str(" WHERE ");
        output.push_str(&groups.join(match transform.logic {
            ResultFilterLogic::All => " AND ",
            ResultFilterLogic::Any => " OR ",
        }));
    }

    if !transform.sorts.is_empty() {
        output.push_str(" ORDER BY ");
        for (index, sort) in transform.sorts.iter().enumerate() {
            validate_column(&sort.column)?;
            if index > 0 {
                output.push_str(", ");
            }
            output.push_str(&crate::ddl::quote_ident(&sort.column, engine));
            output.push_str(match sort.direction {
                ResultSortDirection::Ascending => " ASC",
                ResultSortDirection::Descending => " DESC",
            });
        }
    }
    Ok(output)
}

fn predicate(engine: Engine, filter: &ResultFilter) -> Result<String, String> {
    validate_column(&filter.column)?;
    let column = crate::ddl::quote_ident(&filter.column, engine);
    if !filter.operator.requires_value() {
        return Ok(match filter.operator {
            ResultFilterOperator::IsNull => format!("{column} IS NULL"),
            ResultFilterOperator::IsNotNull => format!("{column} IS NOT NULL"),
            _ => unreachable!("value-free operators handled above"),
        });
    }
    let value = filter
        .value
        .as_deref()
        .ok_or_else(|| "result filter operator requires a value".to_owned())?;
    if value.len() > 4_096 {
        return Err("result filter value exceeds 4096 bytes".into());
    }
    let literal = sql_string(value, engine);
    let text_column = match engine {
        Engine::Postgres => format!("LOWER(CAST({column} AS text))"),
        Engine::SqlServer => format!("LOWER(CAST({column} AS nvarchar(max)))"),
    };
    let text_literal = format!("LOWER({literal})");
    Ok(match filter.operator {
        ResultFilterOperator::Contains | ResultFilterOperator::NotContains => {
            let expression = match engine {
                Engine::Postgres => format!("POSITION({text_literal} IN {text_column}) > 0"),
                Engine::SqlServer => format!("CHARINDEX({text_literal}, {text_column}) > 0"),
            };
            if filter.operator == ResultFilterOperator::NotContains {
                format!("NOT ({expression})")
            } else {
                expression
            }
        }
        ResultFilterOperator::StartsWith => match engine {
            Engine::Postgres => format!("POSITION({text_literal} IN {text_column}) = 1"),
            Engine::SqlServer => format!("CHARINDEX({text_literal}, {text_column}) = 1"),
        },
        ResultFilterOperator::EndsWith => match engine {
            Engine::Postgres => {
                format!("RIGHT({text_column}, LENGTH({text_literal})) = {text_literal}")
            }
            Engine::SqlServer => {
                format!("RIGHT({text_column}, LEN({text_literal})) = {text_literal}")
            }
        },
        ResultFilterOperator::Equals => format!("{column} = {literal}"),
        ResultFilterOperator::NotEquals => format!("{column} <> {literal}"),
        ResultFilterOperator::GreaterThan => format!("{column} > {literal}"),
        ResultFilterOperator::GreaterThanOrEqual => format!("{column} >= {literal}"),
        ResultFilterOperator::LessThan => format!("{column} < {literal}"),
        ResultFilterOperator::LessThanOrEqual => format!("{column} <= {literal}"),
        ResultFilterOperator::IsNull | ResultFilterOperator::IsNotNull => unreachable!(),
    })
}

fn validate_column(column: &str) -> Result<(), String> {
    if column.trim().is_empty() || column.len() > 256 || column.contains('\0') {
        Err("result transform contains an invalid column name".to_owned())
    } else {
        Ok(())
    }
}

fn sql_string(value: &str, engine: Engine) -> String {
    let escaped = value.replace('\'', "''");
    match engine {
        Engine::Postgres => format!("'{escaped}'"),
        Engine::SqlServer => format!("N'{escaped}'"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sift_protocol::{ResultFilterGroup, ResultSort};

    fn filter(column: &str, operator: ResultFilterOperator, value: Option<&str>) -> ResultFilter {
        ResultFilter {
            column: column.into(),
            operator,
            value: value.map(str::to_owned),
        }
    }

    #[test]
    fn postgres_transform_quotes_groups_and_multiple_sorts() {
        let sql = apply(
            Engine::Postgres,
            "select * from events;",
            &ResultTransform {
                logic: ResultFilterLogic::All,
                groups: vec![ResultFilterGroup {
                    logic: ResultFilterLogic::Any,
                    filters: vec![
                        filter("odd\"name", ResultFilterOperator::Contains, Some("O'Brien")),
                        filter("deleted_at", ResultFilterOperator::IsNull, None),
                    ],
                }],
                sorts: vec![
                    ResultSort {
                        column: "created_at".into(),
                        direction: ResultSortDirection::Descending,
                    },
                    ResultSort {
                        column: "id".into(),
                        direction: ResultSortDirection::Ascending,
                    },
                ],
            },
        )
        .unwrap();
        assert_eq!(
            sql,
            "SELECT * FROM (select * from events) AS sift_result WHERE (POSITION(LOWER('O''Brien') IN LOWER(CAST(\"odd\"\"name\" AS text))) > 0 OR \"deleted_at\" IS NULL) ORDER BY \"created_at\" DESC, \"id\" ASC"
        );
    }

    #[test]
    fn transforms_support_typed_comparisons_and_reject_bad_inputs() {
        let transform = ResultTransform {
            logic: ResultFilterLogic::All,
            groups: vec![ResultFilterGroup {
                logic: ResultFilterLogic::All,
                filters: vec![filter(
                    "amount",
                    ResultFilterOperator::GreaterThanOrEqual,
                    Some("10"),
                )],
            }],
            sorts: Vec::new(),
        };
        assert_eq!(
            apply(Engine::SqlServer, "select amount from events", &transform).unwrap(),
            "SELECT * FROM (select amount from events) AS sift_result WHERE ([amount] >= N'10')"
        );
        assert!(apply(Engine::SqlServer, "delete from events", &transform).is_err());
        assert!(apply(Engine::SqlServer, "select 1; select 2", &transform).is_err());
    }
}
