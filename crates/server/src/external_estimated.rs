//! Bounded foreign plan evidence, kept estimated and explicitly unattested.
use super::*;
use sift_extension_protocol::DriverEstimatedPlan as Raw;
fn invalid() -> DriverError {
    DriverError::new(
        Code::UnsupportedResultShape,
        "External estimated-plan shape, mode or bounds are invalid",
    )
}
pub(crate) fn external_estimated_response(
    engine: Engine,
    plan: Raw,
    max_bytes: usize,
) -> Result<ExplainResponse, DriverError> {
    if serde_json::to_vec(&plan).map_err(|_| invalid())?.len() > max_bytes {
        return Err(invalid());
    }
    let (root, raw) = match (engine, plan) {
        (Engine::Postgres, Raw::PostgresJson { json }) => {
            json_bounds(&json, 0, &mut 0)?;
            let [entry] = json.as_array().ok_or_else(invalid)?.as_slice() else {
                return Err(invalid());
            };
            pg_shape(entry.get("Plan").ok_or_else(invalid)?, 0, &mut 0)?;
            (
                parse_pg_plan(&json)?,
                serde_json::to_string_pretty(&json).map_err(|_| invalid())?,
            )
        }
        (Engine::SqlServer, Raw::SqlServerXml { xml }) => {
            let doc = roxmltree::Document::parse_with_options(
                &xml,
                roxmltree::ParsingOptions {
                    nodes_limit: 100_000,
                    ..Default::default()
                },
            )
            .map_err(|_| invalid())?;
            if doc.root_element().tag_name().name() != "ShowPlanXML" {
                return Err(invalid());
            }
            for node in doc.descendants().filter(|node| node.is_element()) {
                if node.ancestors().take(66).count() > 65
                    || matches!(
                        node.tag_name().name(),
                        "RunTimeInformation"
                            | "RunTimeCountersPerThread"
                            | "QueryTimeStats"
                            | "WaitStats"
                            | "TimeStatistics"
                    )
                    || node
                        .attributes()
                        .any(|attr| attr.name().starts_with("Actual"))
                    || (node.tag_name().name() == "RelOp"
                        && (node.attribute("PhysicalOp").map_or(true, str::is_empty)
                            || ["EstimateRows", "EstimatedTotalSubtreeCost"]
                                .into_iter()
                                .any(|key| {
                                    node.attribute(key).is_some_and(|value| {
                                        value
                                            .parse::<f64>()
                                            .map_or(true, |n| !n.is_finite() || n < 0.0)
                                    })
                                })))
                {
                    return Err(invalid());
                }
            }
            let roots = doc
                .descendants()
                .filter(|node| {
                    is_tag(*node, "RelOp")
                        && !node
                            .ancestors()
                            .skip(1)
                            .any(|parent| is_tag(parent, "RelOp"))
                })
                .collect::<Vec<_>>();
            if roots.is_empty() {
                return Err(invalid());
            }
            let root = if roots.len() == 1 {
                parse_mssql_plan(&xml)?
            } else {
                let mut root = PlanNode::new("SQL Server estimated plans");
                root.children = roots.into_iter().map(mssql_node).collect();
                root
            };
            (root, xml)
        }
        (Engine::Sqlite, Raw::SqliteQueryPlan { rows }) => {
            if rows.is_empty() || rows.len() > 10_000 {
                return Err(invalid());
            }
            let response = sqlite_plan(
                rows.into_iter()
                    .map(|row| {
                        sift_protocol::Row::new(vec![
                            Value::Int64(row.id),
                            Value::Int64(row.parent),
                            Value::Int64(row.aux),
                            Value::Text(row.detail),
                        ])
                    })
                    .collect(),
            )
            .map_err(|_| invalid())?;
            (response.root, response.raw)
        }
        _ => return Err(invalid()),
    };
    tree_bounds(&root, 0, &mut 0)?;
    if raw.len() > max_bytes {
        return Err(invalid());
    }
    Ok(ExplainResponse {engine,analyzed:false,root,raw,warnings:vec![sift_protocol::DriverWarning::new("External provider-supplied estimated plan. Estimated-only behavior, restored session state and provenance are provider claims, not host attestations or a provider quality certification; no analyzed/Profile capture was requested.")]})
}
fn json_bounds(
    value: &serde_json::Value,
    depth: usize,
    count: &mut usize,
) -> Result<(), DriverError> {
    *count += 1;
    if depth > 64 || *count > 100_000 {
        return Err(invalid());
    }
    match value {
        serde_json::Value::Array(values) => {
            for value in values {
                json_bounds(value, depth + 1, count)?;
            }
        }
        serde_json::Value::Object(values) => {
            for (key, value) in values {
                if key.starts_with("Actual ")
                    || key.starts_with("Rows Removed")
                    || matches!(
                        key.as_str(),
                        "Execution Time"
                            | "Triggers"
                            | "Shared Hit Blocks"
                            | "Shared Read Blocks"
                            | "Local Hit Blocks"
                            | "Local Read Blocks"
                    )
                {
                    return Err(invalid());
                }
                json_bounds(value, depth + 1, count)?;
            }
        }
        _ => {}
    }
    Ok(())
}
fn pg_shape(value: &serde_json::Value, depth: usize, count: &mut usize) -> Result<(), DriverError> {
    *count += 1;
    if depth > 64
        || *count > 10_000
        || !value
            .get("Node Type")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|name| !name.is_empty())
    {
        return Err(invalid());
    }
    for key in ["Plan Rows", "Total Cost"] {
        if value
            .get(key)
            .is_some_and(|value| value.as_f64().map_or(true, |n| !n.is_finite() || n < 0.0))
        {
            return Err(invalid());
        }
    }
    if let Some(plans) = value.get("Plans") {
        for plan in plans.as_array().ok_or_else(invalid)? {
            pg_shape(plan, depth + 1, count)?;
        }
    }
    Ok(())
}
fn tree_bounds(node: &PlanNode, depth: usize, count: &mut usize) -> Result<(), DriverError> {
    *count += 1;
    if depth > 64
        || *count > 10_000
        || node.actual_rows.is_some()
        || node.actual_ms.is_some()
        || node.est_rows.is_some_and(|n| !n.is_finite() || n < 0.0)
        || node.est_cost.is_some_and(|n| !n.is_finite() || n < 0.0)
    {
        return Err(invalid());
    }
    for node in &node.children {
        tree_bounds(node, depth + 1, count)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sift_extension_protocol::DriverSqlitePlanRow;
    fn pg() -> Raw {
        Raw::PostgresJson {
            json: serde_json::json!([{"Plan":{"Node Type":"Result","Plan Rows":1,"Total Cost":0.1}}]),
        }
    }
    #[test]
    fn external_plan_limits_modes_shapes_and_estimates_are_distinct() {
        assert!(
            !external_estimated_response(Engine::Postgres, pg(), 4096)
                .unwrap()
                .analyzed
        );
        assert!(external_estimated_response(Engine::SqlServer, pg(), 4096).is_err());
        assert!(external_estimated_response(Engine::Postgres, pg(), 4).is_err());
        for json in [
            serde_json::json!([]),
            serde_json::json!([{"Plan":{}}]),
            serde_json::json!([{"Plan":{"Node Type":"Result","Actual Rows":0}}]),
            serde_json::json!([{"Plan":{"Node Type":"Result","Plan Rows":-1}}]),
            serde_json::json!([{"Plan":{"Node Type":"Result","Total Cost":"unknown"}}]),
        ] {
            assert!(external_estimated_response(
                Engine::Postgres,
                Raw::PostgresJson { json },
                4096
            )
            .is_err());
        }
        for xml in ["<ShowPlanXML><RelOp PhysicalOp='Scan' EstimateRows='unknown'/></ShowPlanXML>","<ShowPlanXML/>","<ShowPlanXML><RelOp PhysicalOp='Scan' EstimateRows='NaN'/></ShowPlanXML>","<ShowPlanXML><RelOp PhysicalOp='Scan'><RunTimeInformation/></RelOp></ShowPlanXML>","<!DOCTYPE x [<!ENTITY a 'private'>]><ShowPlanXML><RelOp PhysicalOp='&a;'/></ShowPlanXML>"] {
            assert!(external_estimated_response(Engine::SqlServer,Raw::SqlServerXml {xml:xml.into()},4096).is_err());
        }
        assert!(external_estimated_response(
            Engine::Sqlite,
            Raw::SqliteQueryPlan {
                rows: vec![DriverSqlitePlanRow {
                    id: 1,
                    parent: 1,
                    aux: 0,
                    detail: "cycle".into()
                }]
            },
            4096
        )
        .is_err());
        let mut json = serde_json::json!({"Node Type":"Result"});
        for _ in 0..35 {
            json = serde_json::json!({"Node Type":"Result","Plans":[json]});
        }
        assert!(external_estimated_response(
            Engine::Postgres,
            Raw::PostgresJson {
                json: serde_json::json!([{"Plan":json}])
            },
            16_384
        )
        .is_err());
        let multi = external_estimated_response(
            Engine::SqlServer,
            Raw::SqlServerXml {
                xml: "<ShowPlanXML><RelOp PhysicalOp='A'/><RelOp PhysicalOp='B'/></ShowPlanXML>"
                    .into(),
            },
            4096,
        )
        .unwrap();
        assert_eq!(multi.root.children.len(), 2);
    }
}
