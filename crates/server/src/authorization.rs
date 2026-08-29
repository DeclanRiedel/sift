//! Central authorization evaluator (ADR-020).

use sift_protocol::{ConnectionPolicy, OperationClassification, OperationKind, TenantRole};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthorizationRoomRole {
    Owner,
    Editor,
    Viewer,
}

#[derive(Debug, Clone, Default)]
pub struct AuthorizationScope {
    pub authenticated: bool,
    pub trusted_local: bool,
    pub instance_admin: bool,
    pub tenant_role: Option<TenantRole>,
    pub room_role: Option<AuthorizationRoomRole>,
    pub connection_policy: Option<ConnectionPolicy>,
}

impl AuthorizationScope {
    pub fn trusted_local() -> Self {
        Self {
            authenticated: true,
            trusted_local: true,
            ..Self::default()
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthorizationDenial {
    AuthenticationRequired,
    InstanceAdminRequired,
    TenantAdminRequired,
    TenantMemberRequired,
    TenantRoleTooLow,
    RoomOwnerRequired,
    RoomEditorRequired,
    OperationNotAllowed,
    OperationBlocked,
}

impl AuthorizationDenial {
    pub const fn public_reason(self) -> &'static str {
        match self {
            Self::AuthenticationRequired => "authentication required",
            Self::InstanceAdminRequired => "instance administrator context required",
            Self::TenantAdminRequired => "tenant administrator context required",
            Self::TenantMemberRequired => "tenant membership required",
            Self::TenantRoleTooLow => "tenant role cannot use this connection profile",
            Self::RoomOwnerRequired => "room owner context required",
            Self::RoomEditorRequired => "room editor context required",
            Self::OperationNotAllowed => "operation is not allowed by connection policy",
            Self::OperationBlocked => "operation is blocked by connection policy",
        }
    }
}

pub fn authorize(
    scope: &AuthorizationScope,
    operation: OperationKind,
) -> Result<(), AuthorizationDenial> {
    if !scope.authenticated {
        return Err(AuthorizationDenial::AuthenticationRequired);
    }

    use OperationKind::*;
    if matches!(
        operation,
        ManagePrincipal
            | ManageGithubAllowlist
            | ManagePrincipalKey
            | ManageTenantLimits
            | ManageInstanceConfiguration
            | BackupState
            | RestoreState
    ) && !scope.instance_admin
    {
        return Err(AuthorizationDenial::InstanceAdminRequired);
    }
    if matches!(operation, ManageTenantInvitation | ManageConnectionPolicy)
        && !matches!(
            scope.tenant_role,
            Some(TenantRole::Owner | TenantRole::Admin)
        )
    {
        return Err(AuthorizationDenial::TenantAdminRequired);
    }

    if matches!(operation, BindWorkspaceProjection | ManageSchedule)
        && !matches!(scope.room_role, Some(AuthorizationRoomRole::Owner))
        && !scope.trusted_local
    {
        return Err(AuthorizationDenial::RoomOwnerRequired);
    }

    if matches!(
        operation,
        ApplyDocumentUpdate
            | ManageWorkspace
            | RestoreWorkspace
            | BindWorkspaceProjection
            | ManageWorkspaceProjection
            | WriteVcs
            | ManageDdlSource
            | ManageRunConfiguration
            | ExecuteRun
            | ManageSchedule
            | ManageTransferRecipe
            | ExecuteTransferRecipe
    ) && matches!(scope.room_role, Some(AuthorizationRoomRole::Viewer))
    {
        return Err(AuthorizationDenial::RoomEditorRequired);
    }

    if !is_connection_operation(operation) {
        return Ok(());
    }

    if let Some(role) = scope.tenant_role {
        if role == TenantRole::Viewer {
            return Err(AuthorizationDenial::TenantMemberRequired);
        }
    } else if scope.connection_policy.is_some() && !scope.trusted_local {
        return Err(AuthorizationDenial::TenantMemberRequired);
    }

    if matches!(scope.room_role, Some(AuthorizationRoomRole::Viewer)) {
        return Err(AuthorizationDenial::RoomEditorRequired);
    }

    if let Some(policy) = &scope.connection_policy {
        let role = scope
            .tenant_role
            .or(scope.trusted_local.then_some(TenantRole::Owner))
            .ok_or(AuthorizationDenial::TenantMemberRequired)?;
        if !role.satisfies(policy.minimum_tenant_role) {
            return Err(AuthorizationDenial::TenantRoleTooLow);
        }
        if policy.blocked_ops.contains(&operation) {
            return Err(AuthorizationDenial::OperationBlocked);
        }
        if policy
            .allowed_ops
            .as_ref()
            .is_some_and(|allowed| !allowed.contains(&operation))
        {
            return Err(AuthorizationDenial::OperationNotAllowed);
        }
    }
    Ok(())
}

pub fn authorize_extension(
    scope: &AuthorizationScope,
    classification: OperationClassification,
) -> Result<(), AuthorizationDenial> {
    if classification == OperationClassification::Administrative {
        if !scope.authenticated {
            return Err(AuthorizationDenial::AuthenticationRequired);
        }
        return scope
            .instance_admin
            .then_some(())
            .ok_or(AuthorizationDenial::InstanceAdminRequired);
    }
    let operation = match classification {
        OperationClassification::Read => OperationKind::RefreshSchema,
        OperationClassification::ExecuteRead => OperationKind::ExecuteQuery,
        OperationClassification::Write | OperationClassification::Destructive => {
            OperationKind::ApplyEdits
        }
        OperationClassification::Administrative => unreachable!(),
    };
    authorize(scope, operation)
}

pub const fn is_connection_operation(operation: OperationKind) -> bool {
    use OperationKind::*;
    matches!(
        operation,
        OpenConnection
            | CloseConnection
            | PingConnection
            | RefreshSchema
            | ReadCatalogGraph
            | ProjectCatalogDiagram
            | CreateCatalogSnapshot
            | CompareCatalogSchemas
            | PreviewMigration
            | ApplyMigration
            | CancelMigration
            | GetMigrationRun
            | StartComparison
            | PrepareComparisonPatch
            | CaptureSemanticPlan
            | GenerateDdl
            | ExecuteQuery
            | ExportQuery
            | Complete
            | OpenSemanticDocument
            | UpdateSemanticDocument
            | CloseSemanticDocument
            | SelectStatement
            | DiagnoseSql
            | FormatSql
            | SqlQuickFix
            | FindSqlUsages
            | PrepareSqlRefactor
            | Listen
            | CancelQuery
            | PreviewEdits
            | ApplyEdits
            | SearchSchema
            | SearchData
            | Explain
            | ListProcesses
            | KillProcess
            | ImportCsv
            | BulkInsert
            | BeginTransaction
            | ListTransactions
            | PreviewTransaction
            | CommitTransaction
            | RollbackTransaction
            | Savepoint
            | RollbackToSavepoint
            | ReleaseSavepoint
            | ExecuteRun
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn member_scope(policy: ConnectionPolicy) -> AuthorizationScope {
        AuthorizationScope {
            authenticated: true,
            tenant_role: Some(TenantRole::Member),
            connection_policy: Some(policy),
            ..AuthorizationScope::default()
        }
    }

    #[test]
    fn blocklist_wins_over_allowlist_and_admin_role() {
        let policy = ConnectionPolicy {
            minimum_tenant_role: TenantRole::Member,
            allowed_ops: Some(vec![OperationKind::ExecuteQuery]),
            blocked_ops: vec![OperationKind::ExecuteQuery],
            ..ConnectionPolicy::default()
        };
        let mut scope = member_scope(policy);
        scope.tenant_role = Some(TenantRole::Owner);
        assert_eq!(
            authorize(&scope, OperationKind::ExecuteQuery),
            Err(AuthorizationDenial::OperationBlocked)
        );
    }

    #[test]
    fn extension_classification_cannot_weaken_core_policy() {
        let mut scope = member_scope(ConnectionPolicy::default());
        assert!(authorize_extension(&scope, OperationClassification::Read).is_ok());
        scope.room_role = Some(AuthorizationRoomRole::Viewer);
        assert_eq!(
            authorize_extension(&scope, OperationClassification::Read),
            Err(AuthorizationDenial::RoomEditorRequired)
        );
        assert_eq!(
            authorize_extension(&scope, OperationClassification::Write),
            Err(AuthorizationDenial::RoomEditorRequired)
        );
        assert_eq!(
            authorize_extension(&scope, OperationClassification::Administrative),
            Err(AuthorizationDenial::InstanceAdminRequired)
        );
    }

    #[test]
    fn tenant_and_room_viewers_cannot_execute() {
        let mut scope = member_scope(ConnectionPolicy::default());
        scope.tenant_role = Some(TenantRole::Viewer);
        assert_eq!(
            authorize(&scope, OperationKind::ExecuteQuery),
            Err(AuthorizationDenial::TenantMemberRequired)
        );
        scope.tenant_role = Some(TenantRole::Member);
        scope.room_role = Some(AuthorizationRoomRole::Viewer);
        assert_eq!(
            authorize(&scope, OperationKind::ExecuteQuery),
            Err(AuthorizationDenial::RoomEditorRequired)
        );
    }

    #[test]
    fn workspace_mutations_follow_room_roles() {
        let mut scope = AuthorizationScope {
            authenticated: true,
            tenant_role: Some(TenantRole::Member),
            room_role: Some(AuthorizationRoomRole::Viewer),
            ..AuthorizationScope::default()
        };
        assert!(authorize(&scope, OperationKind::ReadWorkspace).is_ok());
        assert_eq!(
            authorize(&scope, OperationKind::ManageWorkspace),
            Err(AuthorizationDenial::RoomEditorRequired)
        );

        scope.room_role = Some(AuthorizationRoomRole::Editor);
        assert!(authorize(&scope, OperationKind::ManageWorkspace).is_ok());
        assert!(authorize(&scope, OperationKind::ManageWorkspaceProjection).is_ok());
        assert_eq!(
            authorize(&scope, OperationKind::BindWorkspaceProjection),
            Err(AuthorizationDenial::RoomOwnerRequired)
        );

        scope.room_role = Some(AuthorizationRoomRole::Owner);
        assert!(authorize(&scope, OperationKind::BindWorkspaceProjection).is_ok());
        assert!(authorize(&scope, OperationKind::ManageWorkspaceProjection).is_ok());
        assert!(authorize(&scope, OperationKind::ManageSchedule).is_ok());
    }

    #[test]
    fn vcs_role_matrix_allows_reads_but_requires_editor_for_mutations() {
        let mut scope = AuthorizationScope {
            authenticated: true,
            tenant_role: Some(TenantRole::Member),
            room_role: Some(AuthorizationRoomRole::Viewer),
            ..AuthorizationScope::default()
        };
        assert!(authorize(&scope, OperationKind::ReadVcs).is_ok());
        assert_eq!(
            authorize(&scope, OperationKind::WriteVcs),
            Err(AuthorizationDenial::RoomEditorRequired)
        );

        for role in [AuthorizationRoomRole::Editor, AuthorizationRoomRole::Owner] {
            scope.room_role = Some(role);
            assert!(authorize(&scope, OperationKind::ReadVcs).is_ok());
            assert!(authorize(&scope, OperationKind::WriteVcs).is_ok());
        }
    }

    #[test]
    fn tenant_role_and_profile_minimum_matrix_is_deny_wins() {
        for (role, default_allowed, admin_policy_allowed) in [
            (TenantRole::Viewer, false, false),
            (TenantRole::Member, true, false),
            (TenantRole::Admin, true, true),
            (TenantRole::Owner, true, true),
        ] {
            let mut default_scope = member_scope(ConnectionPolicy::default());
            default_scope.tenant_role = Some(role);
            assert_eq!(
                authorize(&default_scope, OperationKind::ExecuteQuery).is_ok(),
                default_allowed,
                "default policy for {role:?}"
            );

            let mut admin_scope = member_scope(ConnectionPolicy {
                minimum_tenant_role: TenantRole::Admin,
                ..ConnectionPolicy::default()
            });
            admin_scope.tenant_role = Some(role);
            assert_eq!(
                authorize(&admin_scope, OperationKind::ExecuteQuery).is_ok(),
                admin_policy_allowed,
                "admin-minimum policy for {role:?}"
            );
        }
    }

    #[test]
    fn administration_uses_the_correct_authority() {
        let scope = AuthorizationScope {
            authenticated: true,
            tenant_role: Some(TenantRole::Admin),
            ..AuthorizationScope::default()
        };
        assert!(authorize(&scope, OperationKind::ManageConnectionPolicy).is_ok());
        assert_eq!(
            authorize(&scope, OperationKind::ManageTenantLimits),
            Err(AuthorizationDenial::InstanceAdminRequired)
        );
    }

    #[test]
    fn every_operation_kind_has_a_total_decision() {
        let scope = AuthorizationScope::trusted_local();
        for operation in OperationKind::ALL {
            let _ = authorize(&scope, operation);
        }
    }
}
