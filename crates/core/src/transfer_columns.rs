//! Pure ordered tabular import selection; no file, database or UI access.
use sift_protocol::{Engine, TransferDirection};
use std::collections::{BTreeMap, BTreeSet, HashSet};
#[derive(Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Projection {
    pub source: String,
    pub target: String,
}
#[derive(Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ComputedProjection {
    pub target: String,
    pub expression: crate::transfer_expressions::Expression,
}
#[derive(Clone)]
pub enum Selection {
    Rename(BTreeMap<String, String>),
    Project(Vec<Projection>),
    Expressions(Vec<ComputedProjection>),
}
impl Selection {
    pub fn computed_text_columns(&self) -> BTreeSet<String> {
        match self {
            Self::Expressions(columns) => columns
                .iter()
                .filter(|column| {
                    !matches!(
                        column.expression,
                        crate::transfer_expressions::Expression::Column { .. }
                    )
                })
                .map(|column| column.target.clone())
                .collect(),
            _ => BTreeSet::new(),
        }
    }
    pub fn is_projection(&self) -> bool {
        matches!(self, Self::Project(_) | Self::Expressions(_))
    }
}
fn invalid(message: &'static str) -> &'static str {
    message
}
fn target_valid(target: &str) -> bool {
    !target.is_empty() && target.trim() == target && !target.contains('\0') && target.len() <= 128
}

pub fn validate_options(
    direction: TransferDirection,
    format: &str,
    version: &str,
    options: &serde_json::Value,
) -> Result<(), &'static str> {
    sql_server_mode(direction, format, options)?;
    parse(direction, format, version, options).map(|_| ())
}
pub fn sql_server_mode(
    direction: TransferDirection,
    format: &str,
    options: &serde_json::Value,
) -> Result<Option<sift_protocol::SqlServerColumnMode>, &'static str> {
    let Some(value) = options.get("sql_server_mode") else {
        return Ok(None);
    };
    if direction != TransferDirection::Import || !matches!(format, "csv" | "xlsx" | "parquet") {
        return Err("SQL Server column modes require a CSV, XLSX or Parquet upload import");
    }
    if options.get("durable_resume").is_some() {
        return Err("SQL Server column modes cannot use durable resume");
    }
    serde_json::from_value(value.clone())
        .map(Some)
        .map_err(|_| "sql_server_mode requires only identity and computed modes")
}
pub fn null_token(
    direction: TransferDirection,
    format: &str,
    version: &str,
    options: &serde_json::Value,
) -> Result<String, &'static str> {
    let Some(value) = options.get("null_value") else {
        return Ok("NULL".into());
    };
    if direction != TransferDirection::Import || !matches!(format, "csv" | "xlsx") || version != "3"
    {
        return Err("null_value requires a version-3 CSV or XLSX upload import");
    }
    value
        .as_str()
        .filter(|value| !value.is_empty() && value.len() <= 128 && !value.contains('\0'))
        .map(str::to_owned)
        .ok_or("null_value requires a nonempty marker of at most 128 bytes")
}
pub fn parse(
    direction: TransferDirection,
    format: &str,
    version: &str,
    options: &serde_json::Value,
) -> Result<Option<Selection>, &'static str> {
    null_token(direction, format, version, options)?;
    if matches!(format, "csv" | "xlsx" | "parquet")
        && !(version == "1"
            || (matches!(version, "2" | "3") && direction == TransferDirection::Import))
    {
        return Err(invalid("unsupported bundled import recipe format version"));
    }
    let mappings = options.get("column_mappings");
    let projection = options.get("column_projection");
    let expressions = options.get("column_expressions");
    if mappings.is_none() && projection.is_none() && expressions.is_none() {
        return Ok(None);
    }
    if direction != TransferDirection::Import || !matches!(format, "csv" | "xlsx" | "parquet") {
        return Err(invalid(
            "column selection requires a CSV, XLSX or Parquet upload import",
        ));
    }
    if format != "csv" && !matches!(version, "2" | "3") {
        return Err("XLSX/Parquet column selection requires format_version 2 or 3");
    }
    if [
        mappings.is_some(),
        projection.is_some(),
        expressions.is_some(),
    ]
    .into_iter()
    .filter(|present| *present)
    .count()
        > 1
    {
        return Err(invalid(
            "column_mappings, column_projection and column_expressions cannot be combined",
        ));
    }
    if let Some(expressions) = expressions {
        if version != "3" {
            return Err("column_expressions requires import format_version 3");
        }
        let expressions = expressions
            .as_array()
            .ok_or("column_expressions must be an ordered array")?;
        if expressions.is_empty() || expressions.len() > 256 {
            return Err("column_expressions requires 1–256 outputs");
        }
        let expressions: Vec<ComputedProjection> =
            serde_json::from_value(serde_json::Value::Array(expressions.clone()))
                .map_err(|_| "expression outputs require target and a supported expression")?;
        let mut targets = HashSet::new();
        let mut nodes = 0;
        for column in &expressions {
            if !target_valid(&column.target) || !targets.insert(column.target.as_str()) {
                return Err("expression targets must be valid and distinct");
            }
            nodes += column.expression.validate()?;
            if nodes > 4096 {
                return Err("too many transfer expression nodes");
            }
        }
        return Ok(Some(Selection::Expressions(expressions)));
    }
    if let Some(projection) = projection {
        if !matches!(version, "2" | "3") {
            return Err(invalid(
                "column_projection requires import format_version 2 or 3",
            ));
        }
        let projection = projection
            .as_array()
            .ok_or_else(|| invalid("column_projection must be an ordered array"))?;
        if projection.is_empty() || projection.len() > 256 {
            return Err(invalid("column_projection requires 1–256 output columns"));
        }
        let projection: Vec<Projection> =
            serde_json::from_value(serde_json::Value::Array(projection.clone())).map_err(|_| {
                invalid("each projected column requires only source and target strings")
            })?;
        let mut targets = HashSet::new();
        for column in &projection {
            if column.source.len() > 4096 || !target_valid(&column.target) {
                return Err(invalid("projected source or target column name is invalid"));
            }
            if !targets.insert(column.target.as_str()) {
                return Err(invalid("mapped target columns must be distinct"));
            }
        }
        return Ok(Some(Selection::Project(projection)));
    }
    let mappings = mappings
        .unwrap()
        .as_object()
        .ok_or_else(|| invalid("column_mappings must be an object of source and target names"))?;
    if mappings.len() > 256 {
        return Err(invalid("too many column mappings"));
    }
    let mut parsed = BTreeMap::new();
    let mut targets = HashSet::new();
    for (source, target) in mappings {
        if source.len() > 4096 {
            return Err("mapped source column name is too long");
        }
        let target = target
            .as_str()
            .filter(|target| target_valid(target))
            .ok_or_else(|| invalid("target column name is invalid"))?;
        if !targets.insert(target) {
            return Err(invalid("mapped target columns must be distinct"));
        }
        parsed.insert(source.clone(), target.to_string());
    }
    Ok(Some(Selection::Rename(parsed)))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Column {
    pub index: usize,
    pub target: String,
    pub expression: Option<crate::transfer_expressions::Resolved>,
}
impl Column {
    pub fn references(&self, indices: &mut std::collections::BTreeSet<usize>) {
        match &self.expression {
            Some(expression) => expression.references(indices),
            None => {
                indices.insert(self.index);
            }
        }
    }
    pub fn evaluate(
        &self,
        row: &[sift_protocol::Value],
        budget: &mut crate::transfer_expressions::Budget,
    ) -> Result<sift_protocol::Value, &'static str> {
        match &self.expression {
            Some(expression) => expression.evaluate(row, budget),
            None => row
                .get(self.index)
                .cloned()
                .ok_or("inconsistent expression source row"),
        }
    }
}
impl Selection {
    pub fn resolve(&self, headers: &[String]) -> Result<Vec<Column>, &'static str> {
        let columns = match self {
            Self::Expressions(expressions) => {
                if headers.len() > 2000 {
                    return Err("expression source exceeds 2000 columns");
                }
                expressions
                    .iter()
                    .map(|column| {
                        let expression = column.expression.resolve(headers)?;
                        Ok(Column {
                            index: expression.direct_column().unwrap_or(0),
                            target: column.target.clone(),
                            expression: Some(expression),
                        })
                    })
                    .collect::<Result<Vec<_>, &'static str>>()?
            }
            Self::Project(projection) => {
                let mut columns = Vec::with_capacity(projection.len());
                for column in projection {
                    let mut matches = headers
                        .iter()
                        .enumerate()
                        .filter(|(_, header)| **header == column.source);
                    let index = matches
                        .next()
                        .ok_or("column projection references an unknown source")?
                        .0;
                    if matches.next().is_some() {
                        return Err("column projection source header is ambiguous");
                    }
                    columns.push(Column {
                        index,
                        target: column.target.clone(),
                        expression: None,
                    });
                }
                columns
            }
            Self::Rename(mappings) => {
                if mappings
                    .keys()
                    .any(|source| !headers.iter().any(|header| header == source))
                {
                    return Err("column mapping references an unknown source");
                }
                headers
                    .iter()
                    .enumerate()
                    .map(|(index, source)| Column {
                        index,
                        expression: None,
                        target: mappings
                            .get(source)
                            .cloned()
                            .unwrap_or_else(|| source.clone()),
                    })
                    .collect()
            }
        };
        let mut targets = HashSet::new();
        for column in &columns {
            if !target_valid(&column.target) {
                return Err("target column name is invalid");
            }
            if !targets.insert(&column.target) {
                return Err("mapped target columns must be distinct");
            }
        }
        Ok(columns)
    }
}
/// SQLite folds ASCII identifier case even for quoted column names. Other
/// providers keep their native destination/collation checks.
pub fn validate_provider_targets<'a>(
    engine: Engine,
    names: impl IntoIterator<Item = &'a str>,
) -> Result<(), &'static str> {
    if engine == Engine::Sqlite {
        let mut unique = HashSet::new();
        for name in names {
            if !unique.insert(name.to_ascii_lowercase()) {
                return Err("SQLite target column names must be distinct ignoring ASCII case");
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn text_expression_recipes_are_versioned_bounded_and_share_null_policy() {
        let options = serde_json::json!({"column_expressions":[{"target":"label","expression":{"op":"upper","input":{"op":"column","source":"raw"}}}]});
        for format in ["csv", "xlsx", "parquet"] {
            assert!(parse(TransferDirection::Import, format, "3", &options)
                .unwrap()
                .unwrap()
                .is_projection());
            for version in ["1", "2"] {
                assert!(parse(TransferDirection::Import, format, version, &options).is_err());
            }
            assert!(parse(TransferDirection::Export, format, "3", &options).is_err());
        }
        for options in [
            serde_json::json!({"column_expressions":[]}),
            serde_json::json!({"column_expressions":[{"target":"x","expression":{"op":"null"}}],"column_projection":[]}),
            serde_json::json!({"column_expressions":[{"target":"x","expression":{"op":"sql","text":"private"}}]}),
            serde_json::json!({"null_value":""}),
        ] {
            assert!(parse(TransferDirection::Import, "csv", "3", &options).is_err());
        }
        let marker = serde_json::json!({"null_value":"[nil]"});
        assert_eq!(
            null_token(TransferDirection::Import, "csv", "3", &marker).unwrap(),
            "[nil]"
        );
        for format in ["csv", "xlsx"] {
            assert!(parse(TransferDirection::Import, format, "2", &marker).is_err());
        }
        assert!(parse(TransferDirection::Import, "parquet", "3", &marker).is_err());
    }

    #[test]
    fn ordered_selection_is_shared_and_version_fenced() {
        let options = serde_json::json!({"column_projection":[{"source":"label","target":"name"},{"source":"id","target":"copy"},{"source":"id","target":"id"}]});
        for format in ["csv", "xlsx", "parquet"] {
            let selection = parse(TransferDirection::Import, format, "2", &options)
                .unwrap()
                .unwrap();
            let columns = selection
                .resolve(&["private".into(), "id".into(), "label".into()])
                .unwrap();
            assert_eq!(
                columns,
                vec![
                    Column {
                        index: 2,
                        expression: None,
                        target: "name".into()
                    },
                    Column {
                        index: 1,
                        expression: None,
                        target: "copy".into()
                    },
                    Column {
                        index: 1,
                        expression: None,
                        target: "id".into()
                    }
                ]
            );
            assert!(parse(TransferDirection::Import, format, "1", &options).is_err());
            assert!(parse(TransferDirection::Export, format, "2", &options).is_err());
            assert!(parse(
                TransferDirection::Import,
                format,
                "4",
                &serde_json::json!({})
            )
            .is_err());
        }
        assert!(parse(TransferDirection::Import, "jsonl", "2", &options).is_err());
        let old = parse(
            TransferDirection::Import,
            "csv",
            "1",
            &serde_json::json!({"column_mappings":{"id":"renamed"}}),
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            old.resolve(&["private".into(), "id".into()]).unwrap()[1].target,
            "renamed"
        );
        for options in [
            serde_json::json!({"column_projection":[]}),
            serde_json::json!({"column_projection":[{"source":"x","target":"a","expression":"x"}]}),
            serde_json::json!({"column_projection":[{"source":"x","target":"a"},{"source":"y","target":"a"}]}),
            serde_json::json!({"column_projection":[{"source":"x","target":"a"}],"column_mappings":{}}),
        ] {
            assert!(parse(TransferDirection::Import, "parquet", "2", &options).is_err());
        }
    }
    #[test]
    fn resolution_refuses_ambiguous_sources_and_native_sqlite_aliases() {
        let selection = parse(
            TransferDirection::Import,
            "parquet",
            "2",
            &serde_json::json!({"column_projection":[{"source":"id","target":"renamed"}]}),
        )
        .unwrap()
        .unwrap();
        assert!(selection.resolve(&["id".into(), "id".into()]).is_err());
        assert!(selection.resolve(&["ID".into()]).is_err());
        assert!(selection
            .resolve(&["private".into(), "private".into(), "id".into()])
            .is_ok());
        assert!(validate_provider_targets(Engine::Sqlite, ["id", "ID"]).is_err());
        assert!(validate_provider_targets(Engine::Postgres, ["id", "ID"]).is_ok());
    }
}
