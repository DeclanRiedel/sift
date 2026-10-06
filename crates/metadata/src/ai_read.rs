//! Narrow AI reads of existing historical resources; no new content persistence.
use super::{
    ConnectionProfileId, MetadataError, MetadataStore, PrincipalId, QueryHistory, Result, RoomId,
    TenantId,
};
use rusqlite::params;
use sift_protocol::{PlanCapture, PlanCaptureId, PlanCaptureSummary};

impl MetadataStore {
    pub fn ai_query_history(
        &self,
        tenant: TenantId,
        profile: ConnectionProfileId,
        actor: PrincipalId,
        room: Option<RoomId>,
    ) -> Result<Vec<QueryHistory>> {
        let selected = self.get_connection_profile_for_principal(profile, actor)?;
        if selected.tenant_id != tenant {
            return Err(MetadataError::TenantMismatch(profile, tenant));
        }
        if let Some(room) = room {
            if self.get_room(room)?.tenant_id != tenant
                || !self.room_access_is_active(room, actor)?
            {
                return Err(MetadataError::AiInvalid(
                    "AI room history is unavailable".into(),
                ));
            }
        }
        let conn = self.conn()?;
        super::ensure_principal_tenant_member_locked(&conn, tenant, actor)?;
        let mut statement=conn.prepare("SELECT id, principal_id, connection_profile_id, sql_text, started_at,
                duration_ms, row_count, status, error_code, error_message, room_id, variable_descriptors_json
            FROM query_history WHERE connection_profile_id=?1
              AND ((?2 IS NULL AND principal_id=?3) OR (?2 IS NOT NULL AND room_id=?2))
            ORDER BY id DESC LIMIT 21")?;
        let history = super::rows(statement.query_map(
            params![profile.0, room.map(|room| room.0), actor.0],
            super::query_history_from_row,
        )?);
        history
    }

    pub fn ai_plan_capture(
        &self,
        tenant: TenantId,
        profile: ConnectionProfileId,
        actor: PrincipalId,
        id: PlanCaptureId,
    ) -> Result<PlanCapture> {
        let selected = self.get_connection_profile_for_principal(profile, actor)?;
        if selected.tenant_id != tenant {
            return Err(MetadataError::AiInvalid(
                "Saved plan is unavailable in this AI scope".into(),
            ));
        }
        let capture = self.get_plan_capture(tenant, id)?;
        if capture.creator_principal_id != actor.0 || capture.connection_profile_id != profile.0 {
            return Err(MetadataError::AiInvalid(
                "Saved plan is unavailable in this AI scope".into(),
            ));
        }
        Ok(capture)
    }

    pub fn ai_plan_captures(
        &self,
        tenant: TenantId,
        profile: ConnectionProfileId,
        actor: PrincipalId,
    ) -> Result<Vec<PlanCaptureSummary>> {
        self.list_plan_captures_scoped(tenant, None, None, 21, Some((profile, actor)))
    }
}
