//! Conservative observations from captured plans, never tuning prescriptions.
use sift_protocol::ExplainResponse;

/// Compare reported native row counters without assuming identical loop semantics.
pub(super) fn row_delta(node: &sift_protocol::PlanNode) -> Option<String> {
    let estimated = node.est_rows?;
    let actual = node.actual_rows?;
    if !estimated.is_finite() || !actual.is_finite() || estimated < 0.0 || actual < 0.0 {
        return None;
    }
    let delta = actual - estimated;
    let ratio = if estimated > 0.0 {
        let ratio = actual / estimated;
        if ratio.is_finite() {
            format!("{ratio:.2}×")
        } else {
            "unavailable".into()
        }
    } else {
        "unavailable (zero estimate)".into()
    };
    Some(format!(
        "reported row delta {delta:+.2}; actual/estimate {ratio}"
    ))
}

#[derive(Debug, Clone)]
pub(super) struct Finding {
    pub node_index: Option<usize>,
    pub text: String,
}

/// Bound work and output even for externally supplied saved plans.
pub(super) fn findings(plan: &ExplainResponse) -> Vec<Finding> {
    let mut findings = Vec::new();
    let mut pending = vec![std::slice::from_ref(&plan.root).iter()];
    let mut visited = 0;
    let mut truncated = false;
    while let Some(children) = pending.last_mut() {
        let Some(node) = children.next() else {
            pending.pop();
            continue;
        };
        let node_index = visited;
        visited += 1;
        if visited > 4096 || findings.len() >= 32 {
            truncated = true;
            break;
        }
        let op = node.op.to_ascii_lowercase();
        if op.contains("scan") || op.contains("sort") {
            findings.push(Finding {
                node_index: Some(node_index),
                text: format!(
                    "{}{}: {} operator; presence alone does not establish a bottleneck.",
                    node.op,
                    node.relation
                        .as_ref()
                        .map(|r| format!(" on {r}"))
                        .unwrap_or_default(),
                    if plan.analyzed {
                        "instrumented"
                    } else {
                        "estimated"
                    },
                ),
            });
        }
        // PostgreSQL's native disk evidence. Do not infer spills from sort
        // presence, estimated costs, or missing counters.
        if plan.analyzed && plan.engine == sift_protocol::Engine::Postgres {
            let sort_space = node.extra.get("Sort Space Type").and_then(|v| v.as_str());
            let disk_sort = sort_space == Some("Disk");
            let temp_written = node
                .extra
                .get("Temp Written Blocks")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let batches = node
                .extra
                .get("Hash Batches")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            if disk_sort || temp_written > 0 || batches > 1 {
                findings.push(Finding { node_index: Some(node_index), text: format!(
                    "{}: native disk/temp evidence (sort space: {}, temp blocks written: {}, hash batches: {}). Counters may include descendants; do not sum nodes.",
                    node.op,
                    sort_space.unwrap_or("unavailable"),
                    node.extra.get("Temp Written Blocks").map_or_else(|| "unavailable".into(), ToString::to_string),
                    node.extra.get("Hash Batches").map_or_else(|| "unavailable".into(), ToString::to_string),
                ) });
            }
        }
        if plan.analyzed && plan.engine == sift_protocol::Engine::SqlServer {
            for key in ["SpillToTempDb", "SortSpillDetails", "HashSpillDetails"] {
                if let Some(evidence) = node.extra.get(key).filter(|value| value.is_object()) {
                    findings.push(Finding { node_index: Some(node_index), text: format!(
                        "{}: native {key} warning {evidence}. Operator-local evidence; missing counters unavailable.",
                        node.op,
                    ) });
                }
            }
        }
        if !node.children.is_empty() {
            pending.push(node.children.iter());
        }
    }
    if truncated || findings.len() > 32 {
        findings.truncate(31);
        findings.push(Finding {
            node_index: None,
            text: "Plan observations truncated.".into(),
        });
    }
    findings
}

#[cfg(test)]
mod tests {
    use super::*;
    use sift_protocol::{Engine, PlanNode};

    #[test]
    fn repeated_operator_findings_retain_exact_preorder_identity() {
        let mut plan = ExplainResponse {
            engine: Engine::Postgres,
            analyzed: true,
            root: PlanNode::new("Append"),
            raw: String::new(),
            warnings: vec![],
        };
        let mut first = PlanNode::new("Sort");
        first.extra.insert("Sort Space Type".into(), "Disk".into());
        first.children.push(PlanNode::new("Result"));
        plan.root.children = vec![first, PlanNode::new("Sort")];
        let observations = findings(&plan);
        assert_eq!(
            observations
                .iter()
                .map(|f| f.node_index)
                .collect::<Vec<_>>(),
            vec![Some(1), Some(1), Some(3)]
        );
        let mut flat = Vec::new();
        fn walk<'a>(node: &'a PlanNode, flat: &mut Vec<&'a PlanNode>) {
            flat.push(node);
            for child in &node.children {
                walk(child, flat);
            }
        }
        walk(&plan.root, &mut flat);
        for finding in &observations {
            assert_eq!(flat[finding.node_index.unwrap()].op, "Sort");
        }
        plan.root.children = (0..5000).map(|_| PlanNode::new("Result")).collect();
        plan.root.children[4095] = PlanNode::new("Sort");
        let bounded = findings(&plan);
        assert_eq!(bounded.len(), 1);
        assert!(bounded[0].node_index.is_none());
        assert_eq!(bounded[0].text, "Plan observations truncated.");
        plan.root = PlanNode::new("Sort");
        for _ in 0..5000 {
            let mut parent = PlanNode::new("Result");
            parent.children.push(plan.root);
            plan.root = parent;
        }
        let deep = findings(&plan);
        assert_eq!(deep.len(), 1);
        assert!(deep[0].node_index.is_none());
        // Tear down iteratively too: recursive PlanNode drop is outside traversal.
        while let Some(child) = plan.root.children.pop() {
            plan.root = child;
        }
    }

    #[test]
    fn sqlserver_spill_warnings_require_measured_operator_evidence() {
        let mut plan = ExplainResponse {
            engine: Engine::SqlServer,
            analyzed: true,
            root: PlanNode::new("Hash Match"),
            raw: String::new(),
            warnings: vec![],
        };
        assert!(findings(&plan).is_empty());
        plan.root
            .extra
            .insert("SpillToTempDb".into(), serde_json::json!({}));
        assert_eq!(findings(&plan).len(), 1);
        plan.analyzed = false;
        assert!(findings(&plan).is_empty());
    }

    #[test]
    fn wide_plans_have_bounded_observations_and_explicit_truncation() {
        let mut plan = ExplainResponse {
            engine: Engine::Postgres,
            analyzed: true,
            root: PlanNode::new("Append"),
            raw: String::new(),
            warnings: vec![],
        };
        plan.root.children = (0..5000).map(|_| PlanNode::new("Sort")).collect();
        let observations = findings(&plan);
        assert_eq!(observations.len(), 32);
        assert_eq!(
            observations.last().unwrap().text,
            "Plan observations truncated."
        );
    }

    #[test]
    fn row_deltas_preserve_zero_and_reject_missing_or_invalid_evidence() {
        let mut node = PlanNode::new("Scan");
        node.est_rows = Some(0.0);
        assert!(row_delta(&node).is_none());
        node.actual_rows = Some(4.0);
        assert!(row_delta(&node)
            .unwrap()
            .contains("unavailable (zero estimate)"));
        node.est_rows = Some(2.0);
        assert!(row_delta(&node).unwrap().contains("2.00×"));
        node.actual_rows = Some(0.0);
        assert!(row_delta(&node).unwrap().contains("-2.00"));
        node.actual_rows = Some(f64::NAN);
        assert!(row_delta(&node).is_none());
        node.actual_rows = Some(-1.0);
        assert!(row_delta(&node).is_none());
    }

    #[test]
    fn disk_findings_require_native_measured_evidence() {
        let mut plan = ExplainResponse {
            engine: Engine::Postgres,
            analyzed: false,
            root: PlanNode::new("Sort"),
            raw: String::new(),
            warnings: vec![],
        };
        plan.root
            .extra
            .insert("Sort Space Type".into(), "Disk".into());
        assert_eq!(findings(&plan).len(), 1);
        plan.analyzed = true;
        assert_eq!(findings(&plan).len(), 2);
        plan.root.extra.clear();
        assert_eq!(findings(&plan).len(), 1);
        plan.root.extra.insert("Hash Batches".into(), 2.into());
        assert_eq!(findings(&plan).len(), 2);
        plan.engine = Engine::SqlServer;
        assert_eq!(findings(&plan).len(), 1);
    }
}
