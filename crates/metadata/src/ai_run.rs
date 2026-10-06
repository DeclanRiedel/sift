//! Durable AI turns. The desktop owns the provider process; metadata owns the trace.

use chrono::Utc;
use rusqlite::{params, OptionalExtension};
use sha2::{Digest, Sha256};
use sift_protocol::{
    AiEventKind, AiRun, AiRunDetail, AiRunEvent, AiRunLease, AiRunStatus, AiTurnContext,
    StartAiTurnRequest,
};
use uuid::Uuid;

use super::{sqlite_blocking, MetadataError, MetadataStore, PrincipalId, Result};

const MAX_PROMPT_BYTES: usize = 64 * 1024;
const MAX_CONTEXT_BYTES: usize = 1024 * 1024;
const MAX_EVENT_BYTES: usize = 16 * 1024;
const MAX_EVENTS_PER_RUN: u64 = 1_024;

/// Apply requested exclusions before persistence or provider delivery, and
/// reject oversized or invalid advisory observations. Attachments keep their
/// independent reviewed-source authorization and are never altered here.
pub fn normalize_ai_context(context: &mut AiTurnContext) -> Result<()> {
    let mut source_ids = std::collections::HashSet::new();
    if context.external_sources.len() > 4
        || context.external_sources.iter().any(|source| {
            source.source_id.is_nil()
                || source.credential_identity.is_nil()
                || source.source_revision == 0
                || source.config_sha256.len() != 64
                || !source
                    .config_sha256
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit())
                || source.label.is_empty()
                || source.label.len() > 120
                || source.label.chars().any(char::is_control)
                || source.room_grant_id.is_some_and(|id| id.is_nil())
                || !source_ids.insert(source.source_id)
        })
    {
        return Err(MetadataError::AiInvalid(
            "External source selection is invalid or exceeds its limit".into(),
        ));
    }
    if !context.inclusion.sql {
        context.sql = None;
    }
    if !context.inclusion.selection {
        if let Some(sql) = &mut context.sql {
            sql.selected_start = None;
            sql.selected_end = None;
        }
    }
    if !context.inclusion.errors {
        context.current_error = None;
    }
    if !context.inclusion.environment {
        context.database = None;
        context.dialect = None;
        context.environment_label = None;
    }
    if let Some(sql) = &context.sql {
        match (sql.selected_start, sql.selected_end) {
            (None, None) => {}
            (Some(start), Some(end))
                if start <= end
                    && (end as usize) <= sql.text.len()
                    && sql.text.is_char_boundary(start as usize)
                    && sql.text.is_char_boundary(end as usize) => {}
            _ => {
                return Err(MetadataError::AiInvalid(
                    "AI selection does not match the SQL snapshot".into(),
                ))
            }
        }
    }
    if let Some(workspace) = &mut context.workspace {
        if context.sql.is_none() {
            workspace.document_revision = None;
            workspace.current_statement = None;
        }
        if !context.inclusion.diagnostics {
            workspace.diagnostics.clear();
            workspace.diagnostics_omitted = 0;
            workspace.diagnostics_stale = false;
            workspace.diagnostics_truncated = false;
        }
        if !context.inclusion.connection_state {
            workspace.editor_read_only = None;
            workspace.connection_read_only = None;
            workspace.transaction = None;
        }
        if workspace.diagnostics.len() > 32
            || workspace.diagnostics.iter().any(|diagnostic| {
                diagnostic.code.len() > 128
                    || diagnostic.message.len() > 2048
                    || diagnostic.range.start > diagnostic.range.end
            })
        {
            return Err(MetadataError::AiInvalid(
                "AI diagnostics exceed the context bounds".into(),
            ));
        }
        if let Some(sql) = &context.sql {
            let valid = |range: sift_protocol::TextRange| {
                range.start <= range.end
                    && range.end as usize <= sql.text.len()
                    && sql.text.is_char_boundary(range.start as usize)
                    && sql.text.is_char_boundary(range.end as usize)
            };
            if workspace
                .current_statement
                .is_some_and(|range| !valid(range))
                || workspace
                    .diagnostics
                    .iter()
                    .any(|diagnostic| !valid(diagnostic.range))
            {
                return Err(MetadataError::AiInvalid(
                    "AI source offsets do not match the SQL snapshot".into(),
                ));
            }
        }
    }
    Ok(())
}

pub struct AiAuthorizedToolRun {
    pub chat_id: Uuid,
    pub context: AiTurnContext,
    pub mode: sift_protocol::AiMode,
    pub started_at: chrono::DateTime<Utc>,
    pub visibility: sift_protocol::AiVisibility,
}

impl MetadataStore {
    /// Recover a run abandoned by a crashed or permanently disconnected
    /// desktop after the instance's maximum run duration.
    pub async fn expire_ai_runs_for_chat(
        &self,
        chat_id: Uuid,
        actor: PrincipalId,
        max_age_secs: u32,
    ) -> Result<()> {
        let store = self.clone();
        sqlite_blocking(move || {
            let mut conn = store.conn()?;
            let tx = conn.transaction()?;
            super::ai::require_chat_access(&tx, chat_id, actor)?;
            let cutoff = (Utc::now() - chrono::Duration::seconds(i64::from(max_age_secs)))
                .to_rfc3339();
            let mut statement = tx.prepare(
                "SELECT id,next_sequence FROM ai_run WHERE chat_id=?1 AND status='running' AND julianday(started_at)<julianday(?2)",
            )?;
            let expired = statement.query_map(params![chat_id.to_string(),cutoff],
                |row| Ok((row.get::<_,String>(0)?,row.get::<_,u64>(1)?)))?
                .collect::<std::result::Result<Vec<_>,_>>()?;
            drop(statement);
            let now = Utc::now().to_rfc3339();
            for (run_id, sequence) in expired {
                tx.execute("UPDATE ai_run SET status='interrupted',ended_at=?2,next_sequence=next_sequence+1 WHERE id=?1",
                    params![run_id,now])?;
                tx.execute("INSERT INTO ai_run_event(run_id,sequence,kind,at) VALUES(?1,?2,'stopped',?3)",
                    params![run_id,sequence,now])?;
            }
            tx.commit()?;
            Ok(())
        }).await
    }

    pub async fn ai_tool_run(
        &self,
        run_id: Uuid,
        actor: PrincipalId,
        lease_token: Uuid,
    ) -> Result<AiAuthorizedToolRun> {
        let store = self.clone();
        let (tenant, chat_id, context_handle, mode, started_at, visibility) =
            sqlite_blocking(move || {
                let conn = store.conn()?;
                let (tenant, status) = ai_run_authorized_conn(&conn, run_id, actor, lease_token)?;
                if status != "running" {
                    return Err(MetadataError::AiInvalid("AI run is no longer active".into()));
                }
                let (chat_id, context_handle, mode, started_at, visibility):
                    (String, String, String, String, String) = conn.query_row(
                    "SELECT r.chat_id,r.context_handle,r.mode,r.started_at,c.visibility FROM ai_run r JOIN ai_chat c ON c.id=r.chat_id WHERE r.id=?1",
                    [run_id.to_string()],
                    |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?)),
                )?;
                Ok((tenant, chat_id, context_handle, mode, started_at, visibility))
            })
            .await?;
        let bytes = self
            .ai_content
            .get(tenant, &context_handle)
            .await?
            .ok_or_else(|| MetadataError::AiContent("AI run context is unavailable".into()))?;
        Ok(AiAuthorizedToolRun {
            chat_id: parse_uuid(&chat_id)?,
            context: serde_json::from_slice(&bytes)?,
            mode: parse_mode(&mode)?,
            started_at: super::parse_time_sql(started_at)?,
            visibility: match visibility.as_str() {
                "private" => sift_protocol::AiVisibility::Private,
                "room_public" => sift_protocol::AiVisibility::RoomPublic,
                _ => return Err(MetadataError::AiContent("invalid AI visibility".into())),
            },
        })
    }

    /// Reserve a single tool call before dispatch. A reused ID is rejected,
    /// so a transport retry cannot accidentally execute a SELECT twice.
    pub async fn reserve_ai_tool_call(
        &self,
        run_id: Uuid,
        actor: PrincipalId,
        lease_token: Uuid,
        call_id: Uuid,
        max_calls: u32,
        tool: sift_protocol::AiToolKind,
    ) -> Result<()> {
        let store = self.clone();
        let (tenant, _) =
            sqlite_blocking(move || store.ai_run_authorized(run_id, actor, lease_token)).await?;
        let content = serde_json::to_vec(&serde_json::json!({"tool": tool}))?;
        let handle = self.ai_content.put(tenant, &content).await?;
        let stored_handle = handle.clone();
        let store = self.clone();
        let result = sqlite_blocking(move || {
            let mut conn = store.conn()?;
            let tx = conn.transaction()?;
            let (_, status) = ai_run_authorized_conn(&tx, run_id, actor, lease_token)?;
            if status != "running" {
                return Err(MetadataError::AiInvalid("AI run is no longer active".into()));
            }
            ensure_ai_tool_budget_conn(&tx, run_id, max_calls)?;
            let existing: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM ai_run_event WHERE run_id=?1 AND tool_call_id=?2)",
                params![run_id.to_string(), call_id.to_string()], |row| row.get(0),
            )?;
            if existing {
                return Err(MetadataError::AiInvalid("AI tool call ID was already used".into()));
            }
            let sequence: u64 = tx.query_row("SELECT next_sequence FROM ai_run WHERE id=?1",
                [run_id.to_string()], |row| row.get(0))?;
            if sequence > MAX_EVENTS_PER_RUN {
                return Err(MetadataError::AiInvalid("AI run event limit reached".into()));
            }
            tx.execute("INSERT INTO ai_run_event(run_id,sequence,kind,at,tool_call_id,content_handle) VALUES(?1,?2,'tool_requested',?3,?4,?5)",
                params![run_id.to_string(),sequence,Utc::now().to_rfc3339(),call_id.to_string(),stored_handle])?;
            tx.execute("UPDATE ai_run SET next_sequence=next_sequence+1 WHERE id=?1",
                [run_id.to_string()])?;
            tx.commit()?;
            Ok(())
        }).await;
        if result.is_err() {
            self.ai_content.delete(tenant, &handle).await?;
        }
        result
    }

    pub async fn finish_ai_tool_call(
        &self,
        run_id: Uuid,
        actor: PrincipalId,
        lease_token: Uuid,
        call_id: Uuid,
        succeeded: bool,
    ) -> Result<()> {
        let store = self.clone();
        sqlite_blocking(move || {
            let mut conn = store.conn()?;
            let tx = conn.transaction()?;
            // Receipt settlement is a trusted server cleanup path. Revocation
            // must deny delivery without leaving an accepted call unfinished.
            let (chat_id, status) = ai_run_lease_conn(&tx, run_id, actor, lease_token)?;
            let succeeded = succeeded && status == "running"
                && super::ai::require_chat_access(&tx, chat_id, actor).is_ok();
            let requested: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM ai_run_event WHERE run_id=?1 AND tool_call_id=?2 AND kind='tool_requested')",
                params![run_id.to_string(),call_id.to_string()], |row| row.get(0),
            )?;
            if !requested { return Err(MetadataError::AiInvalid("AI tool call was not reserved".into())); }
            let existing: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM ai_run_event WHERE run_id=?1 AND tool_call_id=?2 AND kind IN ('tool_completed','tool_denied'))",
                params![run_id.to_string(),call_id.to_string()], |row| row.get(0),
            )?;
            if existing { return Ok(()); }
            let sequence: u64 = tx.query_row("SELECT next_sequence FROM ai_run WHERE id=?1",
                [run_id.to_string()], |row| row.get(0))?;
            let kind = if succeeded { "tool_completed" } else { "tool_denied" };
            tx.execute("INSERT INTO ai_run_event(run_id,sequence,kind,at,tool_call_id) VALUES(?1,?2,?3,?4,?5)",
                params![run_id.to_string(),sequence,kind,Utc::now().to_rfc3339(),call_id.to_string()])?;
            tx.execute("UPDATE ai_run SET next_sequence=next_sequence+1 WHERE id=?1",
                [run_id.to_string()])?;
            tx.commit()?;
            Ok(())
        }).await
    }

    pub async fn start_ai_run(
        &self,
        chat_id: Uuid,
        actor: PrincipalId,
        mut request: StartAiTurnRequest,
    ) -> Result<AiRunLease> {
        normalize_ai_context(&mut request.context)?;
        if request.prompt.trim().is_empty() || request.prompt.len() > MAX_PROMPT_BYTES {
            return Err(MetadataError::AiInvalid(
                "AI prompt is empty or too large".into(),
            ));
        }
        if request.desktop_id.is_nil()
            || request
                .model
                .as_ref()
                .is_some_and(|model| model.is_empty() || model.len() > 128)
        {
            return Err(MetadataError::AiInvalid(
                "AI desktop or model identifier is invalid".into(),
            ));
        }
        let chat = self.get_ai_chat(chat_id, actor).await?;
        if chat.visibility == sift_protocol::AiVisibility::RoomPublic {
            request.context.workspace = None;
            request.context.current_error = None;
        }
        if request.context.target.tenant_id != Some(chat.tenant_id)
            || (chat.room_id.is_some() && request.context.target.room_id != chat.room_id)
        {
            return Err(MetadataError::AiAccessDenied);
        }
        // The HTTP boundary materializes and compares the committed document
        // text before entering here. Independently bind its identity to this
        // room so internal callers cannot publish a different room's resource.
        if let (sift_protocol::AiVisibility::RoomPublic, Some(sql)) =
            (chat.visibility, request.context.sql.as_ref())
        {
            let document_id = sql.room_document_id.ok_or_else(|| {
                MetadataError::AiInvalid("public SQL requires a room document".into())
            })?;
            let document = self
                .get_document_for_principal(super::DocumentId(document_id), actor, false)
                .map_err(|_| {
                    MetadataError::AiInvalid("public SQL document is unavailable".into())
                })?;
            if Some(document.room_id.0) != chat.room_id
                || request.context.target.document_id.as_deref()
                    != Some(document_id.to_string().as_str())
                || sql.document_revision.is_none()
            {
                return Err(MetadataError::AiInvalid(
                    "public SQL document does not match this chat".into(),
                ));
            }
        }
        self.validate_ai_attachments(&chat, actor, &request.context)
            .await?;
        let source_authorizations = self
            .authorize_ai_external_turn_sources(&chat, actor, &request.context)
            .await?;
        let context = serde_json::to_vec(&request.context)?;
        if context.len() > MAX_CONTEXT_BYTES {
            return Err(MetadataError::AiInvalid("AI context is too large".into()));
        }
        let request_digest = format!("{:x}", Sha256::digest(serde_json::to_vec(&request)?));
        if let Some(lease) = self
            .prior_ai_run_lease(chat_id, actor, request.client_request_id, &request_digest)
            .await?
        {
            return Ok(lease);
        }
        let prompt_handle = self
            .ai_content
            .put(chat.tenant_id, request.prompt.as_bytes())
            .await?;
        let context_handle = match self.ai_content.put(chat.tenant_id, &context).await {
            Ok(handle) => handle,
            Err(error) => {
                self.ai_content
                    .delete(chat.tenant_id, &prompt_handle)
                    .await?;
                return Err(error);
            }
        };
        let id = Uuid::new_v4();
        let response_model = request.model.clone();
        let lease_token = Uuid::new_v4();
        let lease_digest = format!("{:x}", Sha256::digest(lease_token.as_bytes()));
        let lease_handle = match self
            .ai_content
            .put(chat.tenant_id, lease_token.as_bytes())
            .await
        {
            Ok(handle) => handle,
            Err(error) => {
                self.ai_content
                    .delete(chat.tenant_id, &prompt_handle)
                    .await?;
                self.ai_content
                    .delete(chat.tenant_id, &context_handle)
                    .await?;
                return Err(error);
            }
        };
        let now = Utc::now();
        let store = self.clone();
        let prompt_for_db = prompt_handle.clone();
        let context_for_db = context_handle.clone();
        let lease_for_db = lease_handle.clone();
        let request_digest_for_retry = request_digest.clone();
        let result = sqlite_blocking(move || {
            let mut conn = store.conn()?;
            let tx = conn.transaction()?;
            super::ai::require_chat_access(&tx, chat_id, actor)?;
            for authorization in &source_authorizations {
                authorization.require(&tx, super::TenantId(chat.tenant_id), actor)?;
            }
            let active: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM ai_run WHERE chat_id=?1 AND status='running')",
                [chat_id.to_string()], |row| row.get(0),
            )?;
            if active {
                return Err(MetadataError::AiInvalid("AI chat already has an active run".into()));
            }
            let previous: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM ai_run WHERE chat_id=?1 AND client_request_id=?2)",
                params![chat_id.to_string(), request.client_request_id.to_string()],
                |row| row.get(0),
            )?;
            if previous {
                return Err(MetadataError::AiInvalid("AI turn request was already used".into()));
            }
            tx.execute(
                "INSERT INTO ai_run(id,chat_id,client_request_id,request_sha256,initiator_principal_id,desktop_id,lease_digest,provider,model,mode,status,prompt_handle,context_handle,lease_handle,next_sequence,started_at)
                 VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,'running',?11,?12,?13,2,?14)",
                params![id.to_string(),chat_id.to_string(),request.client_request_id.to_string(),request_digest,actor.0,request.desktop_id.to_string(),lease_digest,
                    provider_text(request.provider),request.model.clone(),mode_text(request.mode),prompt_for_db,context_for_db,lease_for_db,now.to_rfc3339()],
            )?;
            tx.execute(
                "INSERT INTO ai_run_event(run_id,sequence,kind,at) VALUES(?1,1,'started',?2)",
                params![id.to_string(), now.to_rfc3339()],
            )?;
            tx.execute("UPDATE ai_chat SET updated_at=?2,revision=revision+1 WHERE id=?1",
                params![chat_id.to_string(), now.to_rfc3339()])?;
            tx.commit()?;
            Ok(())
        }).await;
        if let Err(error) = result {
            self.ai_content
                .delete(chat.tenant_id, &prompt_handle)
                .await?;
            self.ai_content
                .delete(chat.tenant_id, &context_handle)
                .await?;
            self.ai_content
                .delete(chat.tenant_id, &lease_handle)
                .await?;
            if let Some(lease) = self
                .prior_ai_run_lease(
                    chat_id,
                    actor,
                    request.client_request_id,
                    &request_digest_for_retry,
                )
                .await?
            {
                return Ok(lease);
            }
            return Err(error);
        }
        Ok(AiRunLease {
            run: AiRun {
                id,
                chat_id,
                desktop_id: request.desktop_id,
                initiator_principal_id: actor.0,
                provider: request.provider,
                model: response_model,
                mode: request.mode,
                status: AiRunStatus::Running,
                next_sequence: 2,
                started_at: now,
                ended_at: None,
            },
            lease_token,
        })
    }

    async fn validate_ai_attachments(
        &self,
        chat: &sift_protocol::AiChat,
        actor: PrincipalId,
        context: &AiTurnContext,
    ) -> Result<()> {
        use sift_protocol::{AiAttachmentSource, AiVisibility};
        if context.attachments.len() > 4 {
            return Err(MetadataError::AiInvalid("Too many AI attachments".into()));
        }
        let tenant = super::TenantId(chat.tenant_id);
        for attachment in &context.attachments {
            let digest = format!(
                "sha256:{:x}",
                Sha256::digest(serde_json::to_vec(
                    &serde_json::json!({"source":attachment.source,"content":attachment.content})
                )?)
            );
            if digest != attachment.sha256
                || attachment.label.len() > 512
                || serde_json::to_vec(&attachment.content)?.len() > 64 * 1024
            {
                return Err(MetadataError::AiInvalid(
                    "AI attachment body is invalid".into(),
                ));
            }
            let mut origin = AiVisibility::Private;
            match attachment.source {
                AiAttachmentSource::QueryRows { .. } => {
                    if chat.visibility == AiVisibility::RoomPublic {
                        return Err(MetadataError::AiInvalid(
                            "Private query rows cannot be published".into(),
                        ));
                    }
                }
                AiAttachmentSource::RoomRows { room_id, .. } => {
                    let room = super::RoomId(room_id);
                    if chat.room_id != Some(room_id)
                        || self.get_room(room)?.tenant_id != tenant
                        || !self.room_access_is_active(room, actor)?
                    {
                        return Err(MetadataError::AiAccessDenied);
                    }
                    origin = AiVisibility::RoomPublic;
                }
                AiAttachmentSource::QueryHistory { history_id } => {
                    let shared = chat
                        .room_id
                        .map(super::RoomId)
                        .map(|room| {
                            self.ai_room_query_history_entry(
                                tenant,
                                room,
                                actor,
                                super::QueryHistoryId(history_id),
                            )
                        })
                        .transpose()?
                        .flatten();
                    if shared.is_some() {
                        origin = AiVisibility::RoomPublic;
                    } else {
                        let profile = super::ConnectionProfileId(
                            context.target.profile_id.ok_or_else(|| {
                                MetadataError::AiInvalid(
                                    "Attachment requires its source profile".into(),
                                )
                            })?,
                        );
                        let entry = self.ai_query_history_entry(
                            tenant,
                            profile,
                            actor,
                            super::QueryHistoryId(history_id),
                            chat.room_id.map(super::RoomId),
                        )?;
                        if chat.visibility == AiVisibility::RoomPublic && entry.room_id.is_some() {
                            return Err(MetadataError::AiAccessDenied);
                        }
                    }
                }
                AiAttachmentSource::PlanCapture { capture_id } => {
                    let profile =
                        super::ConnectionProfileId(context.target.profile_id.ok_or_else(|| {
                            MetadataError::AiInvalid(
                                "Attachment requires its source profile".into(),
                            )
                        })?);
                    self.ai_plan_capture(tenant, profile, actor, capture_id)?;
                }
            }
            if attachment.origin_visibility != origin {
                return Err(MetadataError::AiInvalid(
                    "Attachment sharing label does not match its source".into(),
                ));
            }
            if chat.visibility == AiVisibility::RoomPublic && origin == AiVisibility::Private {
                let _grant = self
                    .ai_room_publication(
                        super::RoomId(chat.room_id.ok_or(MetadataError::AiAccessDenied)?),
                        actor,
                    )
                    .await?
                    .filter(|grant| {
                        Some(grant.id) == context.publication_id
                            && grant.allow_rows
                            && Some(grant.source.profile_id) == context.target.profile_id
                    })
                    .ok_or(MetadataError::AiAccessDenied)?;
                if attachment.published_by != Some(actor.0) {
                    return Err(MetadataError::AiAccessDenied);
                }
            } else if attachment.published_by.is_some() {
                return Err(MetadataError::AiInvalid(
                    "Unexpected attachment publication attribution".into(),
                ));
            }
        }
        Ok(())
    }

    async fn prior_ai_run_lease(
        &self,
        chat_id: Uuid,
        actor: PrincipalId,
        client_request_id: Uuid,
        request_digest: &str,
    ) -> Result<Option<AiRunLease>> {
        let store = self.clone();
        let prior = sqlite_blocking(move || {
            let conn = store.conn()?;
            let tenant = super::ai::require_chat_access(&conn, chat_id, actor)?;
            let row = conn.query_row(
                "SELECT id,initiator_principal_id,request_sha256,lease_handle,provider,model,mode,status,next_sequence,started_at,ended_at,desktop_id FROM ai_run WHERE chat_id=?1 AND client_request_id=?2",
                params![chat_id.to_string(), client_request_id.to_string()],
                |row| Ok((row.get::<_,String>(0)?,row.get::<_,i64>(1)?,row.get::<_,String>(2)?,
                    row.get::<_,String>(3)?,row.get::<_,String>(4)?,row.get::<_,Option<String>>(5)?,
                    row.get::<_,String>(6)?,row.get::<_,String>(7)?,row.get::<_,u64>(8)?,
                    row.get::<_,String>(9)?,row.get::<_,Option<String>>(10)?,row.get::<_,String>(11)?)),
            ).optional()?;
            Ok(row.map(|row| (tenant,row)))
        }).await?;
        let Some((tenant, row)) = prior else {
            return Ok(None);
        };
        if row.1 != actor.0 || row.2 != request_digest {
            return Err(MetadataError::AiInvalid(
                "AI turn request ID was reused with different content or initiator".into(),
            ));
        }
        let bytes = self
            .ai_content
            .get(tenant, &row.3)
            .await?
            .ok_or_else(|| MetadataError::AiContent("AI run lease is unavailable".into()))?;
        let lease_token = Uuid::from_slice(&bytes)
            .map_err(|_| MetadataError::AiContent("AI run lease is invalid".into()))?;
        let run = AiRun {
            id: parse_uuid(&row.0)?,
            chat_id,
            desktop_id: parse_uuid(&row.11)?,
            initiator_principal_id: row.1,
            provider: parse_provider(&row.4)?,
            model: row.5,
            mode: parse_mode(&row.6)?,
            status: parse_status(&row.7)?,
            next_sequence: row.8,
            started_at: super::parse_time_sql(row.9)?,
            ended_at: row.10.map(super::parse_time_sql).transpose()?,
        };
        Ok(Some(AiRunLease { run, lease_token }))
    }

    pub async fn list_ai_runs(
        &self,
        chat_id: Uuid,
        viewer: PrincipalId,
    ) -> Result<Vec<AiRunDetail>> {
        self.list_recent_ai_runs(chat_id, viewer, 200).await
    }

    pub async fn list_recent_ai_runs(
        &self,
        chat_id: Uuid,
        viewer: PrincipalId,
        limit: usize,
    ) -> Result<Vec<AiRunDetail>> {
        self.read_ai_runs(chat_id, viewer, limit, None).await
    }

    pub async fn ai_run_detail(
        &self,
        chat_id: Uuid,
        run_id: Uuid,
        viewer: PrincipalId,
    ) -> Result<AiRunDetail> {
        self.read_ai_runs(chat_id, viewer, 1, Some(run_id))
            .await?
            .pop()
            .ok_or(MetadataError::AiNotFound)
    }

    async fn read_ai_runs(
        &self,
        chat_id: Uuid,
        viewer: PrincipalId,
        limit: usize,
        run_id: Option<Uuid>,
    ) -> Result<Vec<AiRunDetail>> {
        if !(1..=200).contains(&limit) {
            return Err(MetadataError::AiInvalid(
                "AI history limit must be between 1 and 200".into(),
            ));
        }
        let store = self.clone();
        let records = sqlite_blocking(move || {
            let conn = store.conn()?;
            let tenant = super::ai::require_chat_access(&conn, chat_id, viewer)?;
            let mut statement = conn.prepare(
                "SELECT id,initiator_principal_id,provider,model,mode,status,next_sequence,started_at,ended_at,prompt_handle,context_handle,desktop_id
                 FROM (SELECT * FROM ai_run WHERE chat_id=?1 AND (?3 IS NULL OR id=?3) ORDER BY started_at DESC,id DESC LIMIT ?2)
                 ORDER BY started_at,id",
            )?;
            let records = statement.query_map(params![chat_id.to_string(), limit, run_id.map(|id| id.to_string())], |row| {
                Ok((row.get::<_, String>(0)?,row.get::<_, i64>(1)?,row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,row.get::<_, String>(4)?,row.get::<_, String>(5)?,
                    row.get::<_, u64>(6)?,row.get::<_, String>(7)?,row.get::<_, Option<String>>(8)?,
                    row.get::<_, String>(9)?,row.get::<_, String>(10)?,row.get::<_,String>(11)?))
            })?.collect::<std::result::Result<Vec<_>,_>>()?;
            Ok((tenant,records))
        }).await?;
        let mut details = Vec::new();
        for record in records.1 {
            let prompt = self
                .ai_content
                .get(records.0, &record.9)
                .await?
                .ok_or_else(|| MetadataError::AiContent("AI prompt is unavailable".into()))?;
            let context = self
                .ai_content
                .get(records.0, &record.10)
                .await?
                .ok_or_else(|| MetadataError::AiContent("AI context is unavailable".into()))?;
            let prompt = String::from_utf8(prompt)
                .map_err(|_| MetadataError::AiContent("AI prompt is invalid UTF-8".into()))?;
            let context: AiTurnContext = serde_json::from_slice(&context)?;
            details.push(AiRunDetail {
                run: AiRun {
                    id: parse_uuid(&record.0)?,
                    chat_id,
                    desktop_id: parse_uuid(&record.11)?,
                    initiator_principal_id: record.1,
                    provider: parse_provider(&record.2)?,
                    model: record.3,
                    mode: parse_mode(&record.4)?,
                    status: parse_status(&record.5)?,
                    next_sequence: record.6,
                    started_at: super::parse_time_sql(record.7)?,
                    ended_at: record.8.map(super::parse_time_sql).transpose()?,
                },
                prompt,
                context,
            });
        }
        let store = self.clone();
        sqlite_blocking(move || {
            let conn = store.conn()?;
            super::ai::require_chat_access(&conn, chat_id, viewer)?;
            Ok(())
        })
        .await?;
        Ok(details)
    }

    pub async fn append_ai_provider_event(
        &self,
        run_id: Uuid,
        actor: PrincipalId,
        lease_token: Uuid,
        client_event_id: Uuid,
        kind: AiEventKind,
        content: serde_json::Value,
    ) -> Result<AiRunEvent> {
        if !matches!(
            kind,
            AiEventKind::MessageDelta
                | AiEventKind::MessageCompleted
                | AiEventKind::ProgressSummary
        ) {
            return Err(MetadataError::AiInvalid(
                "desktop cannot author canonical tool or lifecycle events".into(),
            ));
        }
        if !matches!(&content, serde_json::Value::Object(fields)
            if fields.len() == 1 && fields.get("text").is_some_and(serde_json::Value::is_string))
        {
            return Err(MetadataError::AiInvalid(
                "AI provider events require only a text field".into(),
            ));
        }
        let bytes = serde_json::to_vec(&content)?;
        if bytes.len() > MAX_EVENT_BYTES {
            return Err(MetadataError::AiInvalid("AI event is too large".into()));
        }
        let store = self.clone();
        let (tenant, _) =
            sqlite_blocking(move || store.ai_run_authorized(run_id, actor, lease_token)).await?;
        let handle = self.ai_content.put(tenant, &bytes).await?;
        let stored_handle = handle.clone();
        let now = Utc::now();
        let store = self.clone();
        let result = sqlite_blocking(move || {
            let mut conn = store.conn()?;
            let tx = conn.transaction()?;
            let (_, status) = ai_run_authorized_conn(&tx, run_id, actor, lease_token)?;
            if status != "running" { return Err(MetadataError::AiInvalid("AI run is no longer active".into())); }
            let prior: Option<u64> = tx.query_row(
                "SELECT sequence FROM ai_run_event WHERE run_id=?1 AND client_event_id=?2",
                params![run_id.to_string(),client_event_id.to_string()], |row| row.get(0),
            ).optional()?;
            if let Some(sequence) = prior { return Ok((sequence,false)); }
            let sequence: u64 = tx.query_row("SELECT next_sequence FROM ai_run WHERE id=?1",
                [run_id.to_string()], |row| row.get(0))?;
            if sequence > MAX_EVENTS_PER_RUN { return Err(MetadataError::AiInvalid("AI run event limit reached".into())); }
            tx.execute("INSERT INTO ai_run_event(run_id,sequence,client_event_id,kind,at,content_handle) VALUES(?1,?2,?3,?4,?5,?6)",
                params![run_id.to_string(),sequence,client_event_id.to_string(),event_text(kind),now.to_rfc3339(),stored_handle])?;
            tx.execute("UPDATE ai_run SET next_sequence=next_sequence+1 WHERE id=?1",[run_id.to_string()])?;
            tx.commit()?;
            Ok((sequence,true))
        }).await;
        let (sequence, inserted) = match result {
            Ok(value) => value,
            Err(error) => {
                self.ai_content.delete(tenant, &handle).await?;
                return Err(error);
            }
        };
        if !inserted {
            self.ai_content.delete(tenant, &handle).await?;
            let previous = self
                .list_ai_run_events(run_id, actor, sequence.saturating_sub(1))
                .await?
                .into_iter()
                .next()
                .ok_or_else(|| {
                    MetadataError::AiContent("deduplicated AI event is unavailable".into())
                })?;
            if previous.kind != kind || previous.content.as_ref() != Some(&content) {
                return Err(MetadataError::AiInvalid(
                    "AI event ID was reused with different content".into(),
                ));
            }
            return Ok(previous);
        }
        Ok(AiRunEvent {
            run_id,
            sequence,
            kind,
            at: now,
            content: Some(content),
            tool_call_id: None,
            proposal_id: None,
        })
    }

    pub async fn list_ai_run_events(
        &self,
        run_id: Uuid,
        viewer: PrincipalId,
        after: u64,
    ) -> Result<Vec<AiRunEvent>> {
        let store = self.clone();
        let (tenant, records) = sqlite_blocking(move || {
            let conn = store.conn()?;
            let tenant = ai_run_viewer_tenant(&conn,run_id,viewer)?;
            let mut statement = conn.prepare("SELECT sequence,kind,at,content_handle,tool_call_id,proposal_id FROM ai_run_event WHERE run_id=?1 AND sequence>?2 ORDER BY sequence LIMIT 500")?;
            let records = statement.query_map(params![run_id.to_string(),after], |row| {
                Ok((row.get::<_,u64>(0)?,row.get::<_,String>(1)?,row.get::<_,String>(2)?,
                    row.get::<_,Option<String>>(3)?,row.get::<_,Option<String>>(4)?,row.get::<_,Option<String>>(5)?))
            })?.collect::<std::result::Result<Vec<_>,_>>()?;
            Ok((tenant,records))
        }).await?;
        let mut events = Vec::new();
        for record in records {
            let content = if let Some(handle) = record.3 {
                let bytes =
                    self.ai_content.get(tenant, &handle).await?.ok_or_else(|| {
                        MetadataError::AiContent("AI event is unavailable".into())
                    })?;
                Some(serde_json::from_slice(&bytes)?)
            } else {
                None
            };
            events.push(AiRunEvent {
                run_id,
                sequence: record.0,
                kind: parse_event(&record.1)?,
                at: super::parse_time_sql(record.2)?,
                content,
                tool_call_id: record.4.map(|v| parse_uuid(&v)).transpose()?,
                proposal_id: record.5.map(|v| parse_uuid(&v)).transpose()?,
            });
        }
        let store = self.clone();
        sqlite_blocking(move || {
            let conn = store.conn()?;
            ai_run_viewer_tenant(&conn, run_id, viewer)?;
            Ok(())
        })
        .await?;
        Ok(events)
    }

    pub async fn finish_ai_run(
        &self,
        run_id: Uuid,
        actor: PrincipalId,
        lease_token: Uuid,
        status: AiRunStatus,
    ) -> Result<()> {
        if !matches!(
            status,
            AiRunStatus::Completed
                | AiRunStatus::Failed
                | AiRunStatus::Canceled
                | AiRunStatus::Interrupted
        ) {
            return Err(MetadataError::AiInvalid(
                "terminal AI status required".into(),
            ));
        }
        let store = self.clone();
        sqlite_blocking(move || {
            let mut conn = store.conn()?;
            let tx = conn.transaction()?;
            // A revoked initiator may still close their own leased run without
            // reading chat content or declaring a successful completion.
            let current = if status == AiRunStatus::Completed {
                ai_run_authorized_conn(&tx,run_id,actor,lease_token)?.1
            } else {
                ai_run_lease_conn(&tx,run_id,actor,lease_token)?.1
            };
            if current == status_text(status) { return Ok(()); }
            if current != "running" { return Err(MetadataError::AiInvalid("AI run already ended".into())); }
            let now = Utc::now().to_rfc3339();
            tx.execute("UPDATE ai_run SET status=?2,ended_at=?3,next_sequence=next_sequence+1 WHERE id=?1",
                params![run_id.to_string(),status_text(status),now])?;
            tx.execute("INSERT INTO ai_run_event(run_id,sequence,kind,at) SELECT id,next_sequence-1,'stopped',?2 FROM ai_run WHERE id=?1",
                params![run_id.to_string(),now])?;
            tx.commit()?;
            Ok(())
        }).await
    }

    fn ai_run_authorized(
        &self,
        run_id: Uuid,
        actor: PrincipalId,
        token: Uuid,
    ) -> Result<(i64, String)> {
        let conn = self.conn()?;
        ai_run_authorized_conn(&conn, run_id, actor, token)
    }
}

fn ai_run_viewer_tenant(
    conn: &rusqlite::Connection,
    run_id: Uuid,
    viewer: PrincipalId,
) -> Result<i64> {
    let chat_id: String = conn
        .query_row(
            "SELECT chat_id FROM ai_run WHERE id=?1",
            [run_id.to_string()],
            |row| row.get(0),
        )
        .optional()?
        .ok_or(MetadataError::AiNotFound)?;
    super::ai::require_chat_access(conn, parse_uuid(&chat_id)?, viewer)
}

pub(super) fn ensure_ai_tool_budget_conn(
    conn: &rusqlite::Connection,
    run_id: Uuid,
    max_calls: u32,
) -> Result<()> {
    if !(1..=100).contains(&max_calls) {
        return Err(MetadataError::AiInvalid(
            "invalid AI tool-call policy".into(),
        ));
    }
    let count: u32 = conn.query_row(
        "SELECT COUNT(*) FROM ai_run_event WHERE run_id=?1 AND kind IN ('tool_requested','proposal_created')",
        [run_id.to_string()], |row| row.get(0),
    )?;
    if count >= max_calls {
        return Err(MetadataError::AiInvalid(
            "AI tool call limit reached".into(),
        ));
    }
    Ok(())
}

pub(super) fn ai_run_authorized_conn(
    conn: &rusqlite::Connection,
    run_id: Uuid,
    actor: PrincipalId,
    token: Uuid,
) -> Result<(i64, String)> {
    let (chat_id, status) = ai_run_lease_conn(conn, run_id, actor, token)?;
    let tenant = super::ai::require_chat_access(conn, chat_id, actor)?;
    Ok((tenant, status))
}

fn ai_run_lease_conn(
    conn: &rusqlite::Connection,
    run_id: Uuid,
    actor: PrincipalId,
    token: Uuid,
) -> Result<(Uuid, String)> {
    let digest = format!("{:x}", Sha256::digest(token.as_bytes()));
    let (chat_id, initiator, stored, status): (String, i64, String, String) = conn
        .query_row(
            "SELECT chat_id,initiator_principal_id,lease_digest,status FROM ai_run WHERE id=?1",
            [run_id.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()?
        .ok_or(MetadataError::AiNotFound)?;
    if initiator != actor.0 || stored != digest {
        return Err(MetadataError::AiAccessDenied);
    }
    Ok((parse_uuid(&chat_id)?, status))
}

fn parse_uuid(raw: &str) -> Result<Uuid> {
    Uuid::parse_str(raw).map_err(|_| MetadataError::AiContent("invalid AI identifier".into()))
}
fn provider_text(v: sift_protocol::AiProvider) -> &'static str {
    match v {
        sift_protocol::AiProvider::Codex => "codex",
        sift_protocol::AiProvider::ClaudeCode => "claude_code",
        sift_protocol::AiProvider::OpenCode => "open_code",
    }
}
fn parse_provider(v: &str) -> Result<sift_protocol::AiProvider> {
    match v {
        "codex" => Ok(sift_protocol::AiProvider::Codex),
        "claude_code" => Ok(sift_protocol::AiProvider::ClaudeCode),
        "open_code" => Ok(sift_protocol::AiProvider::OpenCode),
        _ => Err(MetadataError::AiContent("invalid AI provider".into())),
    }
}
fn mode_text(v: sift_protocol::AiMode) -> &'static str {
    match v {
        sift_protocol::AiMode::Read => "read",
        sift_protocol::AiMode::Propose => "propose",
    }
}
fn parse_mode(v: &str) -> Result<sift_protocol::AiMode> {
    match v {
        "read" => Ok(sift_protocol::AiMode::Read),
        "propose" => Ok(sift_protocol::AiMode::Propose),
        _ => Err(MetadataError::AiContent("invalid AI mode".into())),
    }
}
fn status_text(v: AiRunStatus) -> &'static str {
    match v {
        AiRunStatus::Running => "running",
        AiRunStatus::Completed => "completed",
        AiRunStatus::Failed => "failed",
        AiRunStatus::Canceled => "canceled",
        AiRunStatus::Interrupted => "interrupted",
    }
}
fn parse_status(v: &str) -> Result<AiRunStatus> {
    match v {
        "running" => Ok(AiRunStatus::Running),
        "completed" => Ok(AiRunStatus::Completed),
        "failed" => Ok(AiRunStatus::Failed),
        "canceled" => Ok(AiRunStatus::Canceled),
        "interrupted" => Ok(AiRunStatus::Interrupted),
        _ => Err(MetadataError::AiContent("invalid AI run status".into())),
    }
}
fn event_text(v: AiEventKind) -> &'static str {
    match v {
        AiEventKind::Started => "started",
        AiEventKind::MessageDelta => "message_delta",
        AiEventKind::MessageCompleted => "message_completed",
        AiEventKind::ProgressSummary => "progress_summary",
        AiEventKind::ToolRequested => "tool_requested",
        AiEventKind::ToolCompleted => "tool_completed",
        AiEventKind::ToolDenied => "tool_denied",
        AiEventKind::ProposalCreated => "proposal_created",
        AiEventKind::ProposalApplied => "proposal_applied",
        AiEventKind::ProposalDiscarded => "proposal_discarded",
        AiEventKind::Stopped => "stopped",
    }
}
fn parse_event(v: &str) -> Result<AiEventKind> {
    match v {
        "started" => Ok(AiEventKind::Started),
        "message_delta" => Ok(AiEventKind::MessageDelta),
        "message_completed" => Ok(AiEventKind::MessageCompleted),
        "progress_summary" => Ok(AiEventKind::ProgressSummary),
        "tool_requested" => Ok(AiEventKind::ToolRequested),
        "tool_completed" => Ok(AiEventKind::ToolCompleted),
        "tool_denied" => Ok(AiEventKind::ToolDenied),
        "proposal_created" => Ok(AiEventKind::ProposalCreated),
        "proposal_applied" => Ok(AiEventKind::ProposalApplied),
        "proposal_discarded" => Ok(AiEventKind::ProposalDiscarded),
        "stopped" => Ok(AiEventKind::Stopped),
        _ => Err(MetadataError::AiContent("invalid AI event kind".into())),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::{MemorySecretStore, TenantId};
    use sift_protocol::{AiMode, AiProvider, AiVisibility, ToolContext};

    fn request(tenant: i64) -> StartAiTurnRequest {
        StartAiTurnRequest {
            attachment_previews: Vec::new(),
            client_request_id: Uuid::new_v4(),
            desktop_id: Uuid::new_v4(),
            prompt: "Explain this query".into(),
            provider: AiProvider::Codex,
            model: None,
            mode: AiMode::Read,
            context: AiTurnContext {
                external_sources: Vec::new(),
                inclusion: Default::default(),
                workspace: None,
                attachments: Vec::new(),
                target: ToolContext {
                    tenant_id: Some(tenant),
                    room_id: None,
                    profile_id: None,
                    connection_id: None,
                    document_id: None,
                },
                editor_item_id: None,
                database: None,
                dialect: Some("postgres".into()),
                environment_label: None,
                sql: None,
                current_error: None,
                staged_change_count: 0,
                publication_id: None,
            },
        }
    }

    #[test]
    fn context_choices_exclude_automatic_fields_and_validate_source_offsets() {
        use sift_protocol::{
            AiContextDiagnostic, AiContextInclusion, AiSqlContext, AiWorkspaceContext,
            DiagnosticSeverity, TextRange,
        };
        let mut context = request(1).context;
        let text = "SELECT '💡'";
        let start = text.find('💡').unwrap() as u32;
        context.sql = Some(AiSqlContext {
            text: text.into(),
            room_document_id: None,
            document_revision: Some(1),
            selected_start: Some(start + 1),
            selected_end: Some(start + 4),
        });
        assert!(normalize_ai_context(&mut context).is_err());
        context.sql.as_mut().unwrap().selected_start = Some(start);
        context.workspace = Some(AiWorkspaceContext {
            document_revision: Some(1),
            current_statement: Some(TextRange {
                start: 0,
                end: text.len() as u32,
            }),
            editor_read_only: Some(true),
            diagnostics: vec![
                AiContextDiagnostic {
                    severity: DiagnosticSeverity::Error,
                    code: "fixture".into(),
                    message: "diagnostic".into(),
                    range: TextRange {
                        start,
                        end: start + 4
                    },
                };
                33
            ],
            ..Default::default()
        });
        assert!(normalize_ai_context(&mut context).is_err());
        context.workspace.as_mut().unwrap().diagnostics.truncate(32);
        normalize_ai_context(&mut context).unwrap();
        context.current_error = Some("private diagnostic".into());
        context.environment_label = Some("production".into());
        context.inclusion = AiContextInclusion {
            sql: false,
            selection: false,
            errors: false,
            diagnostics: false,
            environment: false,
            connection_state: false,
        };
        normalize_ai_context(&mut context).unwrap();
        assert!(context.sql.is_none());
        assert!(context.current_error.is_none());
        assert!(context.dialect.is_none());
        assert!(context.environment_label.is_none());
        let workspace = context.workspace.unwrap();
        assert!(workspace.current_statement.is_none());
        assert!(workspace.document_revision.is_none());
        assert!(workspace.editor_read_only.is_none());
        assert!(workspace.diagnostics.is_empty());
        let mut legacy = serde_json::to_value(request(1).context).unwrap();
        legacy.as_object_mut().unwrap().remove("inclusion");
        legacy.as_object_mut().unwrap().remove("workspace");
        let restored: AiTurnContext = serde_json::from_value(legacy).unwrap();
        assert_eq!(restored.inclusion, AiContextInclusion::default());
        assert!(restored.workspace.is_none());
    }

    #[tokio::test]
    async fn reads_and_proposals_share_one_quota_with_idempotent_staging() {
        let store = MetadataStore::open_in_memory(Arc::new(MemorySecretStore::new())).unwrap();
        store.bootstrap_local("owner").unwrap();
        let owner = PrincipalId(1);
        let chat = store
            .create_ai_chat(
                TenantId(1),
                None,
                owner,
                AiVisibility::Private,
                "AI chat".into(),
            )
            .await
            .unwrap();
        let mut turn = request(1);
        turn.mode = AiMode::Propose;
        let target = turn.context.target.clone();
        let lease = store.start_ai_run(chat.id, owner, turn).await.unwrap();
        let draft = sift_protocol::StageAiQueryProposalRequest {
            client_request_id: Uuid::new_v4(),
            lease_token: lease.lease_token,
            target,
            base_revision: 1,
            proposed_sql: "SELECT 1".into(),
        };
        let staged = store
            .stage_ai_query_proposal_with_limit(lease.run.id, owner, draft.clone(), 2)
            .await
            .unwrap();
        let replay = store
            .stage_ai_query_proposal_with_limit(lease.run.id, owner, draft.clone(), 2)
            .await
            .unwrap();
        assert_eq!(staged.proposal.id, replay.proposal.id);
        store
            .reserve_ai_tool_call(
                lease.run.id,
                owner,
                lease.lease_token,
                Uuid::new_v4(),
                2,
                sift_protocol::AiToolKind::Schema,
            )
            .await
            .unwrap();
        assert!(store
            .reserve_ai_tool_call(
                lease.run.id,
                owner,
                lease.lease_token,
                Uuid::new_v4(),
                2,
                sift_protocol::AiToolKind::Schema
            )
            .await
            .is_err());
        let mut next = draft.clone();
        next.client_request_id = Uuid::new_v4();
        assert!(store
            .stage_ai_query_proposal_with_limit(lease.run.id, owner, next, 2)
            .await
            .is_err());
        assert_eq!(
            store
                .stage_ai_query_proposal_with_limit(lease.run.id, owner, draft, 2)
                .await
                .unwrap()
                .proposal
                .id,
            staged.proposal.id
        );
        assert_eq!(
            store
                .list_ai_run_events(lease.run.id, owner, 0)
                .await
                .unwrap()
                .iter()
                .filter(|event| matches!(
                    event.kind,
                    AiEventKind::ToolRequested | AiEventKind::ProposalCreated
                ))
                .count(),
            2
        );
    }

    #[tokio::test]
    async fn reserved_tool_receipts_settle_as_denied_after_membership_revocation() {
        let store = MetadataStore::open_in_memory(Arc::new(MemorySecretStore::new())).unwrap();
        store.bootstrap_local("owner").unwrap();
        let owner = PrincipalId(1);
        let chat = store
            .create_ai_chat(
                TenantId(1),
                None,
                owner,
                AiVisibility::Private,
                "AI chat".into(),
            )
            .await
            .unwrap();
        let lease = store
            .start_ai_run(chat.id, owner, request(1))
            .await
            .unwrap();
        let call = Uuid::new_v4();
        store
            .reserve_ai_tool_call(
                lease.run.id,
                owner,
                lease.lease_token,
                call,
                20,
                sift_protocol::AiToolKind::Schema,
            )
            .await
            .unwrap();
        store
            .conn()
            .unwrap()
            .execute(
                "DELETE FROM membership WHERE tenant_id=1 AND principal_id=1",
                [],
            )
            .unwrap();
        assert!(store
            .ai_tool_run(lease.run.id, owner, lease.lease_token)
            .await
            .is_err());
        assert!(store
            .finish_ai_tool_call(lease.run.id, owner, Uuid::new_v4(), call, true)
            .await
            .is_err());
        assert!(store
            .finish_ai_tool_call(lease.run.id, PrincipalId(99), lease.lease_token, call, true)
            .await
            .is_err());
        store
            .finish_ai_tool_call(lease.run.id, owner, lease.lease_token, call, true)
            .await
            .unwrap();
        store
            .finish_ai_tool_call(lease.run.id, owner, lease.lease_token, call, true)
            .await
            .unwrap();
        let kinds = store.conn().unwrap().prepare("SELECT kind FROM ai_run_event WHERE run_id=?1 AND tool_call_id=?2 ORDER BY sequence").unwrap()
            .query_map(params![lease.run.id.to_string(), call.to_string()], |row| row.get::<_, String>(0)).unwrap()
            .collect::<std::result::Result<Vec<_>,_>>().unwrap();
        assert_eq!(kinds, ["tool_requested", "tool_denied"]);
        assert!(store
            .finish_ai_run(
                lease.run.id,
                owner,
                lease.lease_token,
                AiRunStatus::Completed
            )
            .await
            .is_err());
        assert!(store
            .finish_ai_run(
                lease.run.id,
                PrincipalId(99),
                lease.lease_token,
                AiRunStatus::Canceled
            )
            .await
            .is_err());
        assert!(store
            .finish_ai_run(lease.run.id, owner, Uuid::new_v4(), AiRunStatus::Canceled)
            .await
            .is_err());
        store
            .finish_ai_run(
                lease.run.id,
                owner,
                lease.lease_token,
                AiRunStatus::Canceled,
            )
            .await
            .unwrap();
        store
            .finish_ai_run(
                lease.run.id,
                owner,
                lease.lease_token,
                AiRunStatus::Canceled,
            )
            .await
            .unwrap();
        let terminal: (String, i64) = store.conn().unwrap().query_row(
            "SELECT status,(SELECT COUNT(*) FROM ai_run_event WHERE run_id=?1 AND kind='stopped') FROM ai_run WHERE id=?1",
            [lease.run.id.to_string()], |row| Ok((row.get(0)?, row.get(1)?))).unwrap();
        assert_eq!(terminal, ("canceled".into(), 1));

        assert!(store
            .finish_ai_tool_call(
                lease.run.id,
                owner,
                lease.lease_token,
                Uuid::new_v4(),
                false
            )
            .await
            .is_err());
    }

    #[tokio::test]
    async fn run_trace_is_encrypted_ordered_and_lease_bound() {
        let store = MetadataStore::open_in_memory(Arc::new(MemorySecretStore::new())).unwrap();
        store.bootstrap_local("owner").unwrap();
        let owner = PrincipalId(1);
        let chat = store
            .create_ai_chat(
                TenantId(1),
                None,
                owner,
                AiVisibility::Private,
                "AI chat".into(),
            )
            .await
            .unwrap();
        let turn = request(1);
        let lease = store
            .start_ai_run(chat.id, owner, turn.clone())
            .await
            .unwrap();
        let retry_lease = store
            .start_ai_run(chat.id, owner, turn.clone())
            .await
            .unwrap();
        assert_eq!(retry_lease.run.id, lease.run.id);
        assert_eq!(retry_lease.lease_token, lease.lease_token);
        let mut changed = turn;
        changed.prompt = "Different prompt".into();
        assert!(matches!(
            store.start_ai_run(chat.id, owner, changed).await,
            Err(MetadataError::AiInvalid(_))
        ));
        let event_id = Uuid::new_v4();
        let content = serde_json::json!({"text":"private answer"});
        let first = store
            .append_ai_provider_event(
                lease.run.id,
                owner,
                lease.lease_token,
                event_id,
                AiEventKind::MessageCompleted,
                content.clone(),
            )
            .await
            .unwrap();
        let retry = store
            .append_ai_provider_event(
                lease.run.id,
                owner,
                lease.lease_token,
                event_id,
                AiEventKind::MessageCompleted,
                content,
            )
            .await
            .unwrap();
        assert_eq!(first.sequence, retry.sequence);
        assert!(matches!(
            store
                .append_ai_provider_event(
                    lease.run.id,
                    owner,
                    lease.lease_token,
                    event_id,
                    AiEventKind::MessageCompleted,
                    serde_json::json!({"text":"changed"}),
                )
                .await,
            Err(MetadataError::AiInvalid(_))
        ));
        assert_eq!(
            store
                .list_ai_run_events(lease.run.id, owner, 0)
                .await
                .unwrap()
                .len(),
            2
        );
        assert!(matches!(
            store
                .append_ai_provider_event(
                    lease.run.id,
                    owner,
                    Uuid::new_v4(),
                    Uuid::new_v4(),
                    AiEventKind::MessageDelta,
                    serde_json::json!({"text":"x"})
                )
                .await,
            Err(MetadataError::AiAccessDenied)
        ));
        assert!(matches!(
            store
                .append_ai_provider_event(
                    lease.run.id,
                    owner,
                    lease.lease_token,
                    Uuid::new_v4(),
                    AiEventKind::ToolCompleted,
                    serde_json::json!({"text":"forged"})
                )
                .await,
            Err(MetadataError::AiInvalid(_))
        ));
        let (prompt_handle, event_handle): (String,String) = store.conn().unwrap().query_row(
            "SELECT r.prompt_handle,e.content_handle FROM ai_run r JOIN ai_run_event e ON e.run_id=r.id WHERE r.id=?1 AND e.sequence=2",
            [lease.run.id.to_string()], |row| Ok((row.get(0)?,row.get(1)?))).unwrap();
        assert_ne!(prompt_handle, "Explain this query");
        assert_ne!(event_handle, "private answer");
        store
            .finish_ai_run(
                lease.run.id,
                owner,
                lease.lease_token,
                AiRunStatus::Completed,
            )
            .await
            .unwrap();
        assert_eq!(
            store.list_ai_runs(chat.id, owner).await.unwrap()[0]
                .run
                .status,
            AiRunStatus::Completed
        );
        assert_eq!(
            store
                .list_ai_run_events(lease.run.id, owner, 0)
                .await
                .unwrap()
                .len(),
            3
        );
    }

    #[tokio::test]
    async fn encrypted_turn_and_event_reads_recheck_access_after_decryption() {
        struct PausedSecrets {
            inner: MemorySecretStore,
            pause: std::sync::atomic::AtomicBool,
            reached: tokio::sync::Notify,
            resume: tokio::sync::Notify,
        }
        #[async_trait::async_trait]
        impl super::super::SecretStore for PausedSecrets {
            async fn put(&self, namespace: &str, handle: &str, secret: &[u8]) -> Result<()> {
                self.inner.put(namespace, handle, secret).await
            }
            async fn get(&self, namespace: &str, handle: &str) -> Result<Option<Vec<u8>>> {
                if self.pause.swap(false, std::sync::atomic::Ordering::SeqCst) {
                    self.reached.notify_one();
                    self.resume.notified().await;
                }
                self.inner.get(namespace, handle).await
            }
            async fn delete(&self, namespace: &str, handle: &str) -> Result<()> {
                self.inner.delete(namespace, handle).await
            }
        }
        for read_events in [false, true] {
            let secrets = Arc::new(PausedSecrets {
                inner: MemorySecretStore::new(),
                pause: false.into(),
                reached: Default::default(),
                resume: Default::default(),
            });
            let store = MetadataStore::open_in_memory(secrets.clone()).unwrap();
            store.bootstrap_local("owner").unwrap();
            let owner = PrincipalId(1);
            let chat = store
                .create_ai_chat(
                    TenantId(1),
                    None,
                    owner,
                    AiVisibility::Private,
                    "Private".into(),
                )
                .await
                .unwrap();
            let lease = store
                .start_ai_run(chat.id, owner, request(1))
                .await
                .unwrap();
            store
                .append_ai_provider_event(
                    lease.run.id,
                    owner,
                    lease.lease_token,
                    Uuid::new_v4(),
                    AiEventKind::MessageCompleted,
                    serde_json::json!({"text":"Private response"}),
                )
                .await
                .unwrap();
            secrets
                .pause
                .store(true, std::sync::atomic::Ordering::SeqCst);
            let reader = store.clone();
            let read = tokio::spawn(async move {
                if read_events {
                    reader
                        .list_ai_run_events(lease.run.id, owner, 0)
                        .await
                        .map(|_| ())
                } else {
                    reader
                        .ai_run_detail(chat.id, lease.run.id, owner)
                        .await
                        .map(|_| ())
                }
            });
            tokio::time::timeout(
                std::time::Duration::from_secs(2),
                secrets.reached.notified(),
            )
            .await
            .unwrap();
            store
                .conn()
                .unwrap()
                .execute(
                    "DELETE FROM membership WHERE tenant_id=1 AND principal_id=1",
                    [],
                )
                .unwrap();
            secrets.resume.notify_one();
            let result = tokio::time::timeout(std::time::Duration::from_secs(2), read)
                .await
                .unwrap()
                .unwrap();
            assert!(matches!(result, Err(MetadataError::AiNotFound)));
        }
    }

    #[tokio::test]
    async fn recent_turns_and_exact_turn_reads_are_bounded_and_chat_authorized() {
        let store = MetadataStore::open_in_memory(Arc::new(MemorySecretStore::new())).unwrap();
        store.bootstrap_local("owner").unwrap();
        let owner = PrincipalId(1);
        let chat = store
            .create_ai_chat(
                TenantId(1),
                None,
                owner,
                AiVisibility::Private,
                "History".into(),
            )
            .await
            .unwrap();
        let other = store
            .create_ai_chat(
                TenantId(1),
                None,
                owner,
                AiVisibility::Private,
                "Other".into(),
            )
            .await
            .unwrap();
        let mut ids = Vec::new();
        for index in 0..3 {
            let mut turn = request(1);
            turn.prompt = format!("Question {index}");
            let lease = store.start_ai_run(chat.id, owner, turn).await.unwrap();
            store
                .finish_ai_run(
                    lease.run.id,
                    owner,
                    lease.lease_token,
                    AiRunStatus::Completed,
                )
                .await
                .unwrap();
            ids.push(lease.run.id);
        }
        let all = store.list_ai_runs(chat.id, owner).await.unwrap();
        let recent = store.list_recent_ai_runs(chat.id, owner, 2).await.unwrap();
        assert_eq!(recent, all[1..]);
        let exact = store.ai_run_detail(chat.id, ids[0], owner).await.unwrap();
        assert_eq!(exact.prompt, "Question 0");
        assert!(matches!(
            store.ai_run_detail(other.id, ids[0], owner).await,
            Err(MetadataError::AiNotFound)
        ));
        assert!(store
            .ai_run_detail(chat.id, ids[0], PrincipalId(99))
            .await
            .is_err());
        assert!(store.list_recent_ai_runs(chat.id, owner, 0).await.is_err());
        assert!(store
            .list_recent_ai_runs(chat.id, owner, 201)
            .await
            .is_err());
    }

    #[tokio::test]
    async fn public_run_rejects_unverified_sql() {
        let store = MetadataStore::open_in_memory(Arc::new(MemorySecretStore::new())).unwrap();
        store.bootstrap_local("owner").unwrap();
        let owner = PrincipalId(1);
        let room = store
            .create_room(
                TenantId(1),
                owner,
                crate::NewRoom {
                    name: "shared".into(),
                    kind: crate::RoomKind::Shared,
                },
            )
            .unwrap()
            .id;
        let chat = store
            .create_ai_chat(
                TenantId(1),
                Some(room),
                owner,
                AiVisibility::RoomPublic,
                "shared AI".into(),
            )
            .await
            .unwrap();
        let mut turn = request(1);
        turn.context.target.room_id = Some(room.0);
        turn.context.sql = Some(sift_protocol::AiSqlContext {
            text: "select * from private_table".into(),
            room_document_id: Some(1),
            document_revision: None,
            selected_start: None,
            selected_end: None,
        });
        assert!(matches!(
            store.start_ai_run(chat.id, owner, turn).await,
            Err(MetadataError::AiInvalid(_))
        ));
    }
}
