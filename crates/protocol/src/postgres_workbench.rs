use serde::{Deserialize, Serialize};

/// Role-visible PostgreSQL catalog entries. Both lists are bounded by the
/// server; absent extension versions mean the extension is not installed.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PostgresExtension {
    pub name: String,
    pub installed_version: Option<String>,
    pub default_version: Option<String>,
    pub schema: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PostgresPartition {
    pub parent_schema: String,
    pub parent: String,
    pub child_schema: String,
    pub child: String,
    pub bound: Option<String>,
}

/// A bounded rendering of one policy; expressions are display-only.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PostgresPolicy {
    pub schema: String,
    pub table: String,
    pub name: String,
    pub command: String,
    pub permissive: bool,
    pub roles: String,
    pub roles_truncated: bool,
    pub using_expression: Option<String>,
    pub using_truncated: bool,
    pub check_expression: Option<String>,
    pub check_truncated: bool,
    pub row_security_enabled: bool,
    pub row_security_forced: bool,
}

/// One explicit PostgreSQL 16+ role membership, separate from effective privileges.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PostgresRoleMembership {
    pub role: String,
    pub member: String,
    pub grantor: String,
    pub admin_option: bool,
    pub inherit_option: bool,
    pub set_option: bool,
}

/// Public attributes only; no password or password hash is accepted or returned.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PostgresRoleDetails {
    pub inherit: bool,
    pub create_database: bool,
    pub replication: bool,
    pub bypass_rls: bool,
    pub connection_limit: i32,
    pub valid_until: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub defaults: Vec<crate::PostgresRoleDefault>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum PostgresRoleValidity {
    Infinite,
    Until { at: chrono::DateTime<chrono::Utc> },
}
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct PostgresRoleAttributePatch {
    pub login: Option<bool>,
    pub inherit: Option<bool>,
    pub create_database: Option<bool>,
    pub create_role: Option<bool>,
    pub superuser: Option<bool>,
    pub replication: Option<bool>,
    pub bypass_rls: Option<bool>,
    pub connection_limit: Option<i32>,
    pub valid_until: Option<PostgresRoleValidity>,
}

/// Public role attributes only; PostgreSQL password hashes are never selected.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PostgresRole {
    pub name: String,
    pub can_login: bool,
    pub can_create_role: bool,
    pub superuser: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<PostgresRoleDetails>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PostgresOwnedObject {
    pub kind: PostgresOwnedObjectKind,
    pub name: String,
    pub owner: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PostgresOwnedObjectKind {
    Database,
    Schema,
}

/// One explicit schema ACL entry, separate from inherited effective access.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PostgresSchemaGrant {
    pub schema: String,
    pub grantee: String,
    pub privilege: String,
    pub grantable: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PostgresSchemaPrivilege {
    Usage,
    Create,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PostgresObjectPageQuery {
    #[serde(default)]
    pub offset: u32,
    #[serde(default)]
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PostgresObjectPage<T> {
    pub items: Vec<T>,
    pub next_offset: Option<u32>,
}

/// Typed PostgreSQL workbench actions. Names are identifiers, never SQL fragments.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PostgresObjectAction {
    ChangeDefaultPrivilege {
        creator: String,
        schema: Option<String>,
        object_kind: crate::PostgresDefaultPrivilegeKind,
        grantee: crate::PostgresPrivilegeGrantee,
        privilege: crate::PostgresObjectPrivilege,
        change: crate::PostgresPrivilegeChange,
    },
    ChangeObjectPrivilege {
        target: crate::PostgresPrivilegeTarget,
        grantee: crate::PostgresPrivilegeGrantee,
        privilege: crate::PostgresObjectPrivilege,
        change: crate::PostgresPrivilegeChange,
    },
    CreateRole {
        name: String,
    },
    CreateRoleWithAttributes {
        name: String,
        attributes: PostgresRoleAttributePatch,
    },
    RenameRole {
        name: String,
        new_name: String,
    },
    AlterRoleAttributes {
        name: String,
        attributes: PostgresRoleAttributePatch,
    },
    DropRole {
        name: String,
    },
    SetRoleDefault {
        name: String,
        scope: crate::PostgresRoleDefaultScope,
        setting: crate::PostgresRoleSetting,
        /// Integer KiB, milliseconds or workers, or on/off according to setting.
        value: String,
    },
    ResetRoleDefault {
        name: String,
        scope: crate::PostgresRoleDefaultScope,
        setting: crate::PostgresRoleSetting,
    },
    GrantRoleMembership {
        role: String,
        member: String,
        grantor: String,
        admin_option: bool,
        inherit_option: bool,
        set_option: bool,
    },
    RevokeRoleMembership {
        role: String,
        member: String,
        grantor: String,
    },
    GrantSchemaPrivilege {
        schema: String,
        grantee: String,
        privilege: PostgresSchemaPrivilege,
    },
    RevokeSchemaPrivilege {
        schema: String,
        grantee: String,
        privilege: PostgresSchemaPrivilege,
    },
    ChangeOwner {
        object_kind: PostgresOwnedObjectKind,
        name: String,
        new_owner: String,
    },
    InstallExtension {
        name: String,
    },
    DropExtension {
        name: String,
    },
    UpdateExtension {
        name: String,
        version: String,
    },
    DetachPartition {
        parent_schema: String,
        parent: String,
        child_schema: String,
        child: String,
    },
    AttachPartition {
        parent_schema: String,
        parent: String,
        child_schema: String,
        child: String,
        bound: PostgresPartitionBound,
    },
    CreatePolicy {
        schema: String,
        table: String,
        name: String,
        definition: PostgresPolicyDefinition,
    },
    AlterPolicy {
        schema: String,
        table: String,
        name: String,
        #[serde(default)]
        roles: Option<Vec<PostgresPolicyRole>>,
        #[serde(default)]
        using_expression: Option<String>,
        #[serde(default)]
        check_expression: Option<String>,
    },
    DropPolicy {
        schema: String,
        table: String,
        name: String,
    },
    SetRowSecurity {
        schema: String,
        table: String,
        setting: PostgresRowSecuritySetting,
    },
    RenamePolicy {
        schema: String,
        table: String,
        name: String,
        new_name: String,
    },
}

/// Bounds are data, never caller-supplied SQL fragments.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PostgresPartitionBound {
    Default,
    Range {
        from: Vec<PostgresPartitionValue>,
        to: Vec<PostgresPartitionValue>,
    },
    List {
        values: Vec<PostgresPartitionValue>,
    },
    Hash {
        modulus: u32,
        remainder: u32,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum PostgresPartitionValue {
    Literal(String),
    Null,
    MinValue,
    MaxValue,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PostgresObjectImpact {
    pub objects: Vec<String>,
    pub truncated: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PostgresObjectPreview {
    pub action: PostgresObjectAction,
    pub sql: String,
    pub precondition: String,
    pub warning: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub impact: Option<PostgresObjectImpact>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ApplyPostgresObjectRequest {
    pub action: PostgresObjectAction,
    pub precondition: String,
    pub confirmed: bool,
    #[serde(default)]
    pub production_confirmed: bool,
}

/// Native command/mode are immutable under ALTER POLICY.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PostgresPolicyCommand {
    All,
    Select,
    Insert,
    Update,
    Delete,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(
    tag = "kind",
    content = "name",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum PostgresPolicyRole {
    Public,
    Named(String),
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PostgresPolicyDefinition {
    pub command: PostgresPolicyCommand,
    pub permissive: bool,
    pub roles: Vec<PostgresPolicyRole>,
    #[serde(default)]
    pub using_expression: Option<String>,
    #[serde(default)]
    pub check_expression: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PostgresRowSecuritySetting {
    Enable,
    Disable,
    Force,
    NoForce,
}
