//! Central Phase F authorization evaluator (ADR-020).

use sift_protocol::{ConnectionPolicy, OperationKind, TenantRole};

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
        ManagePrincipal | ManageGithubAllowlist | ManagePrincipalKey | ManageTenantLimits
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

    if operation == ApplyDocumentOperation
        && matches!(scope.room_role, Some(AuthorizationRoomRole::Viewer))
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

pub const fn is_connection_operation(operation: OperationKind) -> bool {
    use OperationKind::*;
    matches!(
        operation,
        OpenConnection
            | CloseConnection
            | RefreshSchema
            | GenerateDdl
            | ExecuteQuery
            | ExportQuery
            | Complete
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
