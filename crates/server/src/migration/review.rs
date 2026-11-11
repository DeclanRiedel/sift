//! Native renderer evidence; callers cannot substitute a model-selected change list.
use super::*;

#[derive(Clone)]
pub(crate) struct MigrationReviewEvidence {
    plan_sha256: String,
    changes: Vec<SchemaChange>,
}

impl MigrationReviewEvidence {
    pub(crate) fn matches_plan(&self, plan: &MigrationPlan) -> bool {
        sift_core::ai_delegation::canonical_digest(plan)
            .ok()
            .as_ref()
            == Some(&self.plan_sha256)
    }

    pub(crate) fn changes_for(&self, plan: &MigrationPlan) -> Option<Vec<SchemaChange>> {
        self.matches_plan(plan).then(|| self.changes.clone())
    }
}

pub(crate) fn render_reviewed_plan(
    engine: Engine,
    diff: &SchemaDiff,
    from: &CatalogGraph,
    to: &CatalogGraph,
    selected: &[SchemaChangeId],
    expected_live_revision: sift_protocol::CatalogRevision,
    options: &MigrationOptions,
) -> Result<(MigrationPlan, MigrationReviewEvidence), MigrationRenderError> {
    let plan = render_plan(
        engine,
        diff,
        from,
        to,
        selected,
        expected_live_revision,
        options,
    )?;
    let ids = plan
        .groups
        .iter()
        .flat_map(|g| &g.statements)
        .flat_map(|s| &s.change_ids)
        .collect::<BTreeSet<_>>();
    let changes = diff
        .changes
        .iter()
        .filter(|c| ids.contains(&c.id))
        .cloned()
        .collect::<Vec<_>>();
    if ids.is_empty() || ids.len() != changes.len() {
        return Err(MigrationRenderError::UnknownChange);
    }
    let evidence = MigrationReviewEvidence {
        plan_sha256: sift_core::ai_delegation::canonical_digest(&plan)
            .map_err(|_| MigrationRenderError::Serialization)?,
        changes,
    };
    Ok((plan, evidence))
}
