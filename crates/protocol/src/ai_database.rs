//! Typed database drafts and explicit human review. No provider-facing apply tool.
use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    AiProposal, CatalogGraph, CatalogRevision, ConnectionId, EditPlan, EditSet, MigrationOptions,
    MigrationPlan, MigrationRun, SchemaChangeRisk, SessionId,
};

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum AiDatabaseDraft {
    RowEditSet {
        edit_set: EditSet,
        expected_catalog_revision: CatalogRevision,
    },
    MigrationDraft {
        desired_catalog: CatalogGraph,
        expected_catalog_revision: CatalogRevision,
        #[serde(default)]
        options: MigrationOptions,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StageAiDatabaseProposalRequest {
    pub client_request_id: Uuid,
    pub lease_token: Uuid,
    pub draft: AiDatabaseDraft,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AiDatabaseProposalDetail {
    pub proposal: AiProposal,
    pub draft: AiDatabaseDraft,
    pub source_digest: String,
    pub database_identity: String,
    pub publication_id: Option<Uuid>,
    pub apply_state: Option<AiDatabaseApplyState>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AiDatabaseApplyState {
    Applying,
    Applied,
    Failed,
    OutcomeUnknown,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReviewAiDatabaseProposalRequest {
    pub client_request_id: Uuid,
    pub session: SessionId,
    pub connection: ConnectionId,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AiDatabasePreview {
    RowEditSet { plan: EditPlan },
    MigrationDraft { plan: MigrationPlan },
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AiDatabaseProposalReview {
    pub id: Uuid,
    pub proposal_id: Uuid,
    pub reviewer_id: i64,
    pub session: SessionId,
    pub connection: ConnectionId,
    pub source_digest: String,
    pub content_sha256: String,
    pub review_digest: String,
    pub database_label: String,
    pub production: bool,
    pub preview: AiDatabasePreview,
    pub expires_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ApplyAiDatabaseProposalRequest {
    pub client_request_id: Uuid,
    pub review_id: Uuid,
    pub review_digest: String,
    /// Required for a source explicitly tagged production; must match its label.
    pub production_confirmation: Option<String>,
    #[serde(default)]
    pub acknowledgements: Vec<SchemaChangeRisk>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AiDatabaseApplyReceipt {
    pub proposal_id: Uuid,
    pub state: AiDatabaseApplyState,
    pub row_result: Option<crate::ApplyEditsResult>,
    pub migration_result: Option<MigrationRun>,
    /// Sanitized application outcome, never a raw driver/model error.
    pub message: String,
}
