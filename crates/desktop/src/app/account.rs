use super::*;
use sift_workspace_ui::{AccountAction, AccountReply, AccountScope};

pub(super) async fn account_action(
    server: DesktopServer,
    scope: &AccountScope,
    action: AccountAction,
) -> Result<AccountReply, String> {
    if server.instance().id != scope.instance_id {
        return Err("The selected server changed; reopen Account".into());
    }
    let client = server.client().await?;
    if matches!(action, AccountAction::LoadMethods) {
        return client
            .auth_methods()
            .await
            .map(AccountReply::Methods)
            .map_err(|e| e.to_string());
    }
    let identity = client.whoami().await.map_err(|e| e.to_string())?;
    if scope.principal_id != Some(identity.principal.id) {
        return Err("The signed-in account changed; reopen Account".into());
    }
    match action {
        AccountAction::LoadMethods => unreachable!(),
        AccountAction::ListInvitations { tenant_id } => {
            let invitations = client
                .tenant_invitations(TenantId(tenant_id))
                .await
                .map_err(|e| e.to_string())?;
            Ok(AccountReply::Invitations {
                tenant_id,
                invitations,
            })
        }
        AccountAction::CreateInvitation { tenant_id, request } => {
            let invitation = client
                .create_tenant_invitation(TenantId(tenant_id), request)
                .await
                .map_err(|e| e.to_string())?;
            Ok(AccountReply::Issued {
                tenant_id,
                invitation,
            })
        }
        AccountAction::RevokeInvitation {
            tenant_id,
            invitation_id,
        } => {
            client
                .revoke_tenant_invitation(TenantId(tenant_id), invitation_id)
                .await
                .map_err(|e| e.to_string())?;
            let invitations = client
                .tenant_invitations(TenantId(tenant_id))
                .await
                .map_err(|e| e.to_string())?;
            Ok(AccountReply::Invitations {
                tenant_id,
                invitations,
            })
        }
        AccountAction::AcceptInvitation { request } => {
            client
                .accept_tenant_invitation(request)
                .await
                .map_err(|e| e.to_string())?;
            client
                .whoami()
                .await
                .map(AccountReply::Accepted)
                .map_err(|e| e.to_string())
        }
    }
}
