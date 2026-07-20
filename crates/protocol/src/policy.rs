//! Public authorization policy, rate-limit, and tenant-usage contracts.
//!
//! These are wire types only. Evaluation, persistence, and accounting live in
//! the server and metadata crates.

use serde::{Deserialize, Serialize};

use crate::OperationKind;

#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum TenantRole {
    Owner,
    Admin,
    #[default]
    Member,
    Viewer,
}

impl TenantRole {
    pub const fn rank(self) -> u8 {
        match self {
            Self::Viewer => 0,
            Self::Member => 1,
            Self::Admin => 2,
            Self::Owner => 3,
        }
    }

    pub const fn satisfies(self, minimum: Self) -> bool {
        self.rank() >= minimum.rank()
    }
}

/// An exact engine-normalized schema selector. `catalog = None` selects the
/// active catalog only; it is not a wildcard.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SchemaSelector {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub catalog: Option<String>,
    pub schema: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ConnectionPolicy {
    #[serde(default)]
    pub minimum_tenant_role: TenantRole,
    #[serde(default)]
    pub read_only: bool,
    /// `None` is unrestricted by an allowlist; `Some([])` permits nothing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowed_ops: Option<Vec<OperationKind>>,
    /// Always takes precedence over `allowed_ops`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blocked_ops: Vec<OperationKind>,
    /// `None` is unrestricted; `Some([])` permits no schema.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowed_schemas: Option<Vec<SchemaSelector>>,
    /// Monotonically increases when the durable profile policy changes.
    #[serde(default)]
    pub revision: u64,
}

impl Default for ConnectionPolicy {
    fn default() -> Self {
        Self {
            minimum_tenant_role: TenantRole::Member,
            read_only: false,
            allowed_ops: None,
            blocked_ops: Vec::new(),
            allowed_schemas: None,
            revision: 0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct UpdateConnectionPolicyRequest {
    /// Optimistic concurrency guard. Omit only when replacing an unversioned
    /// legacy/default policy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_revision: Option<u64>,
    pub minimum_tenant_role: TenantRole,
    pub read_only: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowed_ops: Option<Vec<OperationKind>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blocked_ops: Vec<OperationKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowed_schemas: Option<Vec<SchemaSelector>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RateLimitClass {
    Control,
    Interactive,
    Query,
    HeavyTransfer,
    StreamBytes,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TenantResource {
    ConnectionProfiles,
    Sessions,
    Connections,
    ConcurrentQueries,
    Cursors,
    RetainedResultBytes,
}

/// Effective tenant ceilings. `None` means unlimited and `Some(0)` denies new
/// admission for that resource.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TenantResourceLimits {
    pub connection_profiles: Option<u64>,
    pub sessions: Option<u64>,
    pub connections: Option<u64>,
    pub concurrent_queries: Option<u64>,
    pub cursors: Option<u64>,
    pub retained_result_bytes: Option<u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TenantResourceUsage {
    pub connection_profiles: u64,
    pub sessions: u64,
    pub connections: u64,
    pub concurrent_queries: u64,
    pub cursors: u64,
    pub retained_result_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TenantUsageSnapshot {
    pub tenant_id: i64,
    pub limits: TenantResourceLimits,
    pub usage: TenantResourceUsage,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct UpdateTenantLimitsRequest {
    pub limits: TenantResourceLimits,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn role_order_is_conservative() {
        assert!(TenantRole::Owner.satisfies(TenantRole::Admin));
        assert!(TenantRole::Admin.satisfies(TenantRole::Member));
        assert!(!TenantRole::Viewer.satisfies(TenantRole::Member));
    }

    #[test]
    fn policy_preserves_none_versus_empty() {
        let unrestricted = serde_json::to_value(ConnectionPolicy::default()).unwrap();
        assert!(unrestricted.get("allowed_ops").is_none());

        let policy = ConnectionPolicy {
            allowed_ops: Some(Vec::new()),
            allowed_schemas: Some(Vec::new()),
            ..ConnectionPolicy::default()
        };
        let encoded = serde_json::to_value(policy).unwrap();
        assert_eq!(encoded["allowed_ops"], serde_json::json!([]));
        assert_eq!(encoded["allowed_schemas"], serde_json::json!([]));
    }
}
