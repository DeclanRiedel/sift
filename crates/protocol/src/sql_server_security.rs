use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SqlServerSecurityState {
    Available,
    PermissionRequired,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SqlServerSecuritySection<T> {
    pub state: SqlServerSecurityState,
    pub items: Vec<T>,
    pub truncated: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SqlServerSecurityReport {
    pub database: String,
    pub logins: SqlServerSecuritySection<SqlServerLogin>,
    pub principals: SqlServerSecuritySection<SqlServerPrincipal>,
    pub memberships: SqlServerSecuritySection<SqlServerRoleMembership>,
    pub schemas: SqlServerSecuritySection<SqlServerSchemaOwner>,
    /// Explicit schema grants and denies only. Fixed and inherited rights are omitted.
    pub schema_permissions: SqlServerSecuritySection<SqlServerSchemaPermission>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SqlServerLogin {
    pub name: String,
    pub kind: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SqlServerPrincipal {
    pub name: String,
    pub kind: String,
    pub authentication: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SqlServerRoleMembership {
    pub role: String,
    pub member: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SqlServerSchemaOwner {
    pub schema: String,
    pub owner: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SqlServerSchemaPermission {
    pub schema: String,
    pub grantee: String,
    pub permission: String,
    pub state: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SqlServerSecurityAction {
    CreateDatabaseRole { name: String },
    AddRoleMember { role: String, member: String },
    DropRoleMember { role: String, member: String },
    GrantSchemaSelect { schema: String, grantee: String },
    RevokeSchemaSelect { schema: String, grantee: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SqlServerSecurityPreview {
    pub action: SqlServerSecurityAction,
    pub sql: String,
    pub precondition: String,
    pub warning: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ApplySqlServerSecurityRequest {
    pub action: SqlServerSecurityAction,
    pub precondition: String,
    pub production_confirmed: bool,
}
