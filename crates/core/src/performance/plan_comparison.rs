//! Private descriptive plan comparison. Correspondences are heuristic, not node identity.
use serde::Serialize;
use sift_protocol::{Engine, PlanNode, SavedBenchmarkRun};
use std::collections::{BTreeMap, HashSet};

const ALIGNMENT_CELLS: usize = 1_000_000;
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Node {
    pub op: String,
    pub relation: Option<String>,
    pub estimated_rows: Option<f64>,
    pub estimated_cost: Option<f64>,
    pub actual_rows: Option<f64>,
    pub actual_ms: Option<f64>,
    pub attributes: BTreeMap<String, serde_json::Value>,
    pub children: usize,
}
impl From<&PlanNode> for Node {
    fn from(n: &PlanNode) -> Self {
        Self {
            op: n.op.clone(),
            relation: n.relation.clone(),
            estimated_rows: n.est_rows,
            estimated_cost: n.est_cost,
            actual_rows: n.actual_rows,
            actual_ms: n.actual_ms,
            attributes: n.extra.clone(),
            children: n.children.len(),
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Change {
    Added,
    Removed,
    Changed,
    Unchanged,
}
#[derive(Debug, Clone, Serialize)]
pub struct Row {
    pub a_path: Option<Vec<usize>>,
    pub b_path: Option<Vec<usize>>,
    pub a: Option<Node>,
    pub b: Option<Node>,
    pub change: Change,
    /// This row's children use positional comparison because keys are ambiguous
    /// or the global alignment budget was exhausted.
    pub positional_children: bool,
}
#[derive(Debug, Clone, Serialize)]
pub struct Capture {
    pub run_id: String,
    pub captured_at: String,
    pub sift_elapsed_ns: u64,
    pub native_planning_ms: Option<f64>,
    pub native_execution_ms: Option<f64>,
}
#[derive(Debug, Clone, Serialize)]
pub struct PlanComparison {
    pub engine: Engine,
    pub analyzed: bool,
    pub a: Capture,
    pub b: Capture,
    pub warnings: Vec<String>,
    pub rows: Vec<Row>,
    pub added: usize,
    pub removed: usize,
    pub changed: usize,
    pub unchanged: usize,
    pub positional_groups: usize,
}
fn key(node: &PlanNode) -> (&str, Option<&str>) {
    (&node.op, node.relation.as_deref())
}
type Pair = (Option<usize>, Option<usize>);
fn gap(out: &mut Vec<Pair>, a: std::ops::Range<usize>, b: std::ops::Range<usize>) {
    let count = a.len().max(b.len());
    for i in 0..count {
        out.push((
            (i < a.len()).then_some(a.start + i),
            (i < b.len()).then_some(b.start + i),
        ));
    }
}
fn align(a: &[PlanNode], b: &[PlanNode], budget: &mut usize) -> (Vec<Pair>, bool) {
    let mut out = Vec::new();
    if a.is_empty() || b.is_empty() {
        gap(&mut out, 0..a.len(), 0..b.len());
        return (out, false);
    }
    let unique = |nodes: &[PlanNode]| {
        let mut keys = HashSet::new();
        nodes.iter().all(|n| keys.insert(key(n)))
    };
    let cells = (a.len() + 1) * (b.len() + 1);
    if cells > *budget || !unique(a) || !unique(b) {
        gap(&mut out, 0..a.len(), 0..b.len());
        return (out, true);
    }
    *budget -= cells;
    let width = b.len() + 1;
    let mut table = vec![0u16; cells];
    for i in (0..a.len()).rev() {
        for j in (0..b.len()).rev() {
            table[i * width + j] = if key(&a[i]) == key(&b[j]) {
                1 + table[(i + 1) * width + j + 1]
            } else {
                table[(i + 1) * width + j].max(table[i * width + j + 1])
            };
        }
    }
    let (mut i, mut j, mut a_start, mut b_start) = (0, 0, 0, 0);
    while i < a.len() && j < b.len() {
        if key(&a[i]) == key(&b[j]) {
            gap(&mut out, a_start..i, b_start..j);
            out.push((Some(i), Some(j)));
            i += 1;
            j += 1;
            a_start = i;
            b_start = j;
        } else if table[(i + 1) * width + j] >= table[i * width + j + 1] {
            i += 1;
        } else {
            j += 1;
        }
    }
    gap(&mut out, a_start..a.len(), b_start..b.len());
    (out, false)
}
fn walk(
    a: Option<(&PlanNode, Vec<usize>)>,
    b: Option<(&PlanNode, Vec<usize>)>,
    budget: &mut usize,
    rows: &mut Vec<Row>,
) {
    let ac = a.as_ref().map_or(&[][..], |(n, _)| n.children.as_slice());
    let bc = b.as_ref().map_or(&[][..], |(n, _)| n.children.as_slice());
    let (children, positional_children) = align(ac, bc, budget);
    let an = a.as_ref().map(|(n, _)| Node::from(*n));
    let bn = b.as_ref().map(|(n, _)| Node::from(*n));
    let change = match (&an, &bn) {
        (None, Some(_)) => Change::Added,
        (Some(_), None) => Change::Removed,
        _ if an == bn => Change::Unchanged,
        _ => Change::Changed,
    };
    rows.push(Row {
        a_path: a.as_ref().map(|(_, p)| p.clone()),
        b_path: b.as_ref().map(|(_, p)| p.clone()),
        a: an,
        b: bn,
        change,
        positional_children,
    });
    for (ai, bi) in children {
        let left = ai.map(|i| {
            let mut path = a.as_ref().unwrap().1.clone();
            path.push(i);
            (&ac[i], path)
        });
        let right = bi.map(|i| {
            let mut path = b.as_ref().unwrap().1.clone();
            path.push(i);
            (&bc[i], path)
        });
        walk(left, right, budget, rows);
    }
}
pub fn compare(
    a: &SavedBenchmarkRun,
    b: &SavedBenchmarkRun,
) -> Result<PlanComparison, &'static str> {
    let ap = a.profile.as_ref().ok_or("Run A has no attached Profile")?;
    let bp = b.profile.as_ref().ok_or("Run B has no attached Profile")?;
    ap.validate_for(&a.report)?;
    bp.validate_for(&b.report)?;
    if ap.response.plan.engine != bp.response.plan.engine
        || ap.response.plan.analyzed != bp.response.plan.analyzed
    {
        return Err("Plan comparison needs the same engine and estimated/analyzed state");
    }
    let capture = |p: &sift_protocol::BenchmarkProfileAttachment| Capture {
        run_id: p.response.run_id.to_string(),
        captured_at: p.captured_at.to_rfc3339(),
        sift_elapsed_ns: p.response.server_elapsed_ns,
        native_planning_ms: p.response.planning_ms,
        native_execution_ms: p.response.execution_ms,
    };
    let mut warnings = super::compare_benchmarks(&a.report, &b.report)
        .warnings
        .into_iter()
        .map(|warning| format!("Benchmark context: {warning}"))
        .collect::<Vec<_>>();
    warnings.push("Private caller-associated Profile captures. Alignment is heuristic within ordered parents, not semantic node identity; moves can appear as removal/addition. Data/cache equivalence and controlled regression are unproven.".into());
    warnings.push("Native counters are compared as reported, without loop/rebind normalization. Missing is unavailable. Node costs/times are never summed; benchmark samples remain separate. Attributes and identifiers may be sensitive.".into());
    if ap.sql != bp.sql {
        warnings.push("The captured SQL differs between A and B.".into());
    }
    if ap.response.environment.is_none() || bp.response.environment.is_none() {
        warnings.push("Profile environment observations are unavailable for A or B.".into());
    }
    if serde_json::to_value(&ap.response.environment).ok()
        != serde_json::to_value(&bp.response.environment).ok()
    {
        warnings.push("Profile environment observations differ between A and B.".into());
    }
    let mut rows = Vec::new();
    let mut budget = ALIGNMENT_CELLS;
    walk(
        Some((&ap.response.plan.root, vec![])),
        Some((&bp.response.plan.root, vec![])),
        &mut budget,
        &mut rows,
    );
    let mut comparison = PlanComparison {
        engine: ap.response.plan.engine,
        analyzed: ap.response.plan.analyzed,
        a: capture(ap),
        b: capture(bp),
        warnings,
        rows,
        added: 0,
        removed: 0,
        changed: 0,
        unchanged: 0,
        positional_groups: 0,
    };
    for row in &comparison.rows {
        match row.change {
            Change::Added => comparison.added += 1,
            Change::Removed => comparison.removed += 1,
            Change::Changed => comparison.changed += 1,
            Change::Unchanged => comparison.unchanged += 1,
        }
        comparison.positional_groups += usize::from(row.positional_children);
    }
    Ok(comparison)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn saved(root: PlanNode) -> SavedBenchmarkRun {
        serde_json::from_value(serde_json::json!({"id":uuid::Uuid::new_v4(),"saved_at":chrono::Utc::now(),"name":"private fixture","report":{"version":2,"run_id":uuid::Uuid::new_v4(),"engine":"postgres","sql":"SELECT 1","captured_at":chrono::Utc::now(),"parameter_count":0,"warmups":0,"requested_iterations":1,"query_timeout_ms":100,"total_budget_ms":1000,"delay_ms":0,"samples":[],"completed":false,"warnings":[]},"profile":{"captured_at":chrono::Utc::now(),"sql":"SELECT 1","parameter_count":0,"response":{"run_id":uuid::Uuid::new_v4(),"plan":{"engine":"postgres","analyzed":true,"raw":"private raw plan sentinel","root":root},"server_elapsed_ns":100}}})).unwrap()
    }
    fn parent(children: Vec<PlanNode>) -> PlanNode {
        let mut root = PlanNode::new("Parent");
        root.children = children;
        root
    }
    #[test]
    fn saved_plans_track_insertions_replacements_moves_counters_and_attributes() {
        let a = saved(parent(vec![PlanNode::new("Scan")]));
        let same = compare(&a, &a).unwrap();
        assert_eq!(same.unchanged, 2);
        assert_eq!(same.changed, 0);
        assert_eq!(same.a.native_execution_ms, None);
        assert!(!serde_json::to_string(&same)
            .unwrap()
            .contains("private raw plan sentinel"));
        let b = saved(parent(vec![PlanNode::new("Sort"), PlanNode::new("Scan")]));
        let diff = compare(&a, &b).unwrap();
        assert_eq!(diff.added, 1);
        assert_eq!(diff.changed, 1);
        let scan = diff
            .rows
            .iter()
            .find(|r| r.a.as_ref().is_some_and(|n| n.op == "Scan"))
            .unwrap();
        assert_eq!(scan.a_path, Some(vec![0]));
        assert_eq!(scan.b_path, Some(vec![1]));
        assert_eq!(scan.change, Change::Unchanged);
        let removed = compare(&b, &a).unwrap();
        assert_eq!(removed.removed, 1);
        let replacement = compare(&a, &saved(parent(vec![PlanNode::new("Index Scan")]))).unwrap();
        assert_eq!(replacement.rows[1].change, Change::Changed);
        assert_eq!(replacement.rows[1].a_path, Some(vec![0]));
        let a = saved(parent(vec![PlanNode::new("Left"), PlanNode::new("Right")]));
        let moved = compare(
            &a,
            &saved(parent(vec![PlanNode::new("Right"), PlanNode::new("Left")])),
        )
        .unwrap();
        assert_eq!((moved.added, moved.removed), (1, 1));
        let mut b = a.clone();
        let root = &mut b.profile.as_mut().unwrap().response.plan.root;
        root.actual_rows = Some(0.0);
        root.actual_ms = Some(0.0);
        root.extra
            .insert("Filter".into(), serde_json::json!("private-attribute"));
        let diff = compare(&a, &b).unwrap();
        assert_eq!(diff.changed, 1);
        assert_eq!(diff.rows[0].a.as_ref().unwrap().actual_rows, None);
        assert_eq!(diff.rows[0].b.as_ref().unwrap().actual_rows, Some(0.0));
        assert!(serde_json::to_string(&diff)
            .unwrap()
            .contains("private-attribute"));
        let mut b = a.clone();
        b.profile = None;
        assert!(compare(&a, &b).is_err());
        let mut b = a.clone();
        b.report.engine = Engine::Sqlite;
        let profile = b.profile.as_mut().unwrap();
        profile.response.plan.engine = Engine::Sqlite;
        profile.response.plan.analyzed = false;
        assert!(compare(&a, &b).is_err());
    }
    #[test]
    fn ambiguous_keys_and_global_alignment_budget_fall_back_without_losing_nodes() {
        let a = saved(parent(vec![PlanNode::new("Scan"), PlanNode::new("Scan")]));
        let diff = compare(&a, &a).unwrap();
        assert_eq!(diff.positional_groups, 1);
        assert_eq!(diff.rows.len(), 3);
        assert!(diff.rows[0].positional_children);
        let children = |prefix: &str| {
            (0..800)
                .map(|i| PlanNode::new(format!("{prefix}{i}")))
                .collect::<Vec<_>>()
        };
        let mut left = parent(children("L"));
        left.op = "Left".into();
        let mut right = parent(children("R"));
        right.op = "Right".into();
        let a = saved(parent(vec![left, right]));
        let diff = compare(&a, &a).unwrap();
        assert_eq!(diff.rows.len(), 1603);
        assert_eq!(diff.unchanged, 1603);
        assert_eq!(diff.positional_groups, 1);
        assert!(
            diff.rows
                .iter()
                .find(|r| r.a_path == Some(vec![1]))
                .unwrap()
                .positional_children
        );
        let wide = saved(parent(
            (0..1001).map(|i| PlanNode::new(format!("N{i}"))).collect(),
        ));
        let diff = compare(&wide, &wide).unwrap();
        assert_eq!(diff.positional_groups, 1);
        assert_eq!(diff.rows.len(), 1002);
    }
}
