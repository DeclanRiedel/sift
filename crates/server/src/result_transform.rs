//! Conservative complete-result filter/sort rewriting before cursor creation.

use sift_protocol::{Engine, ResultSortDirection, ResultTransform};

pub(crate) fn apply(
    engine: Engine,
    sql: &str,
    transform: &ResultTransform,
) -> Result<String, String> {
    if transform.filters.is_empty() && transform.sort.is_none() {
        return Ok(sql.to_owned());
    }
    if transform.filters.len() > 32 {
        return Err("result transform supports at most 32 filters".into());
    }
    let base = sql.trim().strip_suffix(';').unwrap_or(sql.trim()).trim();
    if base.contains(';') {
        return Err("result transforms require one SELECT statement".into());
    }
    let lower = base.to_ascii_lowercase();
    if !(lower.starts_with("select ") || lower.starts_with("with ")) {
        return Err("result transforms can only wrap SELECT queries".into());
    }

    let validate_column = |column: &str| {
        if column.trim().is_empty() || column.len() > 256 || column.contains('\0') {
            Err("result transform contains an invalid column name".to_owned())
        } else {
            Ok(())
        }
    };
    let mut output = format!("SELECT * FROM ({base}) AS sift_result");
    if !transform.filters.is_empty() {
        output.push_str(" WHERE ");
        for (index, filter) in transform.filters.iter().enumerate() {
            validate_column(&filter.column)?;
            if filter.contains.len() > 4_096 {
                return Err("result filter text exceeds 4096 bytes".into());
            }
            if index > 0 {
                output.push_str(" AND ");
            }
            let column = crate::ddl::quote_ident(&filter.column, engine);
            let literal = sql_string(&filter.contains, engine);
            match engine {
                Engine::Postgres => output.push_str(&format!(
                    "POSITION(LOWER({literal}) IN LOWER(CAST({column} AS text))) > 0"
                )),
                Engine::SqlServer => output.push_str(&format!(
                    "CHARINDEX(LOWER({literal}), LOWER(CAST({column} AS nvarchar(max)))) > 0"
                )),
            }
        }
    }
    if let Some(sort) = &transform.sort {
        validate_column(&sort.column)?;
        output.push_str(" ORDER BY ");
        output.push_str(&crate::ddl::quote_ident(&sort.column, engine));
        output.push_str(match sort.direction {
            ResultSortDirection::Ascending => " ASC",
            ResultSortDirection::Descending => " DESC",
        });
    }
    Ok(output)
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
    use sift_protocol::{ResultFilter, ResultSort};

    #[test]
    fn postgres_transform_is_quoted_and_wraps_the_complete_query() {
        let sql = apply(
            Engine::Postgres,
            "select * from events;",
            &ResultTransform {
                filters: vec![ResultFilter {
                    column: "odd\"name".into(),
                    contains: "O'Brien".into(),
                }],
                sort: Some(ResultSort {
                    column: "created_at".into(),
                    direction: ResultSortDirection::Descending,
                }),
            },
        )
        .unwrap();
        assert_eq!(
            sql,
            "SELECT * FROM (select * from events) AS sift_result WHERE POSITION(LOWER('O''Brien') IN LOWER(CAST(\"odd\"\"name\" AS text))) > 0 ORDER BY \"created_at\" DESC"
        );
    }

    #[test]
    fn transforms_reject_mutations_and_statement_batches() {
        let transform = ResultTransform {
            filters: Vec::new(),
            sort: Some(ResultSort {
                column: "id".into(),
                direction: ResultSortDirection::Ascending,
            }),
        };
        assert!(apply(Engine::SqlServer, "delete from events", &transform).is_err());
        assert!(apply(Engine::SqlServer, "select 1; select 2", &transform).is_err());
    }
}
