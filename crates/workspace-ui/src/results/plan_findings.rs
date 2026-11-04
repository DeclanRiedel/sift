//! Conservative observations from captured plans, never tuning prescriptions.
use sift_protocol::ExplainResponse;

/// Bound work and output even for externally supplied saved plans.
pub(super) fn findings(plan: &ExplainResponse) -> Vec<String> {
    let mut findings = Vec::new();
    let mut pending = vec![&plan.root];
    let mut visited = 0;
    while let Some(node) = pending.pop() {
        visited += 1;
        if visited > 4096 || findings.len() >= 32 {
            findings.push("Plan observations truncated.".into());
            break;
        }
        let op = node.op.to_ascii_lowercase();
        if op.contains("scan") || op.contains("sort") {
            findings.push(format!(
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
            ));
        }
        // PostgreSQL's native disk evidence. Do not infer spills from sort
        // presence, estimated costs, or missing counters.
        if plan.analyzed && plan.engine == sift_protocol::Engine::Postgres {
            let disk_sort =
                node.extra.get("Sort Space Type").and_then(|v| v.as_str()) == Some("Disk");
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
                findings.push(format!(
                    "{}: native disk/temp evidence (disk sort: {disk_sort}, temp blocks written: {}, hash batches: {}). Counters may include descendants; do not sum nodes.",
                    node.op,
                    node.extra.get("Temp Written Blocks").map_or_else(|| "unavailable".into(), ToString::to_string),
                    node.extra.get("Hash Batches").map_or_else(|| "unavailable".into(), ToString::to_string),
                ));
            }
        }
        pending.extend(
            node.children
                .iter()
                .rev()
                .take(4096 - pending.len().min(4096)),
        );
    }
    findings
}

#[cfg(test)]
mod tests {
    use super::*;
    use sift_protocol::{Engine, PlanNode};

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
