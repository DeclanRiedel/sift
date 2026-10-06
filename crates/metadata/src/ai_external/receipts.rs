//! Remote reads share the canonical run budget and preserve encrypted results.
use super::publication::ExternalSourceAuthorization;
use super::*;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum AiExternalInvocationKind {
    Read,
    Inventory,
}
impl AiExternalInvocationKind {
    pub fn action(self) -> &'static str {
        match self {
            Self::Read => "external_read",
            Self::Inventory => "external_inventory",
        }
    }
    fn approves(self, source: &AiExternalSource, alias: &str) -> bool {
        source.definition.tools.iter().any(|tool| match self {
            Self::Read => tool.alias == alias && tool.policy == AiExternalToolPolicy::Read,
            Self::Inventory => {
                (alias.is_empty() || tool.alias == alias)
                    && tool.policy != AiExternalToolPolicy::Unavailable
            }
        })
    }
}

#[derive(Clone)]
pub struct AiExternalInvocation {
    pub kind: AiExternalInvocationKind,
    pub run_id: Uuid,
    pub actor: PrincipalId,
    pub lease: Uuid,
    pub call_id: Uuid,
    pub proof: AiExternalSourceProof,
    pub alias: String,
}

impl MetadataStore {
    pub async fn ai_external_run_source(
        &self,
        run_id: Uuid,
        actor: PrincipalId,
        lease: Uuid,
        source_id: Uuid,
    ) -> Result<(AiExternalSourceProof, AiExternalSource)> {
        let run = self.ai_tool_run(run_id, actor, lease).await?;
        let chat = self.get_ai_chat(run.chat_id, actor).await?;
        let proof = run
            .context
            .external_sources
            .iter()
            .find(|source| source.source_id == source_id)
            .cloned()
            .ok_or_else(|| invalid("Select and review this source before starting the turn"))?;
        let tenant = TenantId(chat.tenant_id);
        let mut source = if chat.visibility == sift_protocol::AiVisibility::RoomPublic {
            let (mut source, aliases) = self
                .authorize_ai_external_room_source(
                    tenant,
                    crate::RoomId(chat.room_id.ok_or(MetadataError::AiAccessDenied)?),
                    actor,
                    &proof,
                )
                .await?;
            source
                .definition
                .tools
                .retain(|tool| aliases.contains(&tool.alias));
            source
        } else {
            self.authorize_ai_external_private_source(tenant, actor, &proof)
                .await?
        };
        source
            .definition
            .tools
            .retain(|tool| tool.policy != AiExternalToolPolicy::Unavailable);
        Ok((proof, source))
    }

    pub async fn reserve_ai_external_call(
        &self,
        request: AiExternalInvocation,
        max_calls: u32,
        arguments_sha256: String,
    ) -> Result<()> {
        let AiExternalInvocation {
            kind,
            run_id,
            actor,
            lease,
            call_id,
            proof,
            alias,
        } = request;
        let (current, source) = self
            .ai_external_run_source(run_id, actor, lease, proof.source_id)
            .await?;
        if current != proof
            || !kind.approves(&source, &alias)
            || arguments_sha256.len() != 64
            || !arguments_sha256
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(MetadataError::AiAccessDenied);
        }
        let run = self.ai_tool_run(run_id, actor, lease).await?;
        let chat = self.get_ai_chat(run.chat_id, actor).await?;
        let mut context = run.context;
        context.external_sources = vec![proof.clone()];
        let guard = self
            .authorize_ai_external_turn_sources(&chat, actor, &context)
            .await?
            .pop()
            .ok_or(MetadataError::AiAccessDenied)?;
        self.reserve_ai_tool_descriptor(crate::ai_run::AiToolReservation {
            run_id,actor,lease_token:lease,call_id,max_calls,
            descriptor:serde_json::json!({"tool":kind.action(),"source":proof,"alias":alias,"arguments_sha256":arguments_sha256}),
            source:Some(guard),
        }).await
    }

    /// Caller cannot append this event through the provider API. It is accepted
    /// only for a matching server reservation and fresh source authority.
    pub async fn complete_ai_external_call(
        &self,
        request: AiExternalInvocation,
        result: serde_json::Value,
    ) -> Result<()> {
        let AiExternalInvocation {
            kind,
            run_id,
            actor,
            lease,
            call_id,
            proof,
            alias,
        } = request;
        let (current, source) = self
            .ai_external_run_source(run_id, actor, lease, proof.source_id)
            .await?;
        if current != proof || !kind.approves(&source, &alias) {
            return Err(MetadataError::AiAccessDenied);
        }
        let run = self.ai_tool_run(run_id, actor, lease).await?;
        let chat = self.get_ai_chat(run.chat_id, actor).await?;
        let tenant = TenantId(chat.tenant_id);
        let mut context = run.context.clone();
        context.external_sources = vec![proof.clone()];
        let mut guards = self
            .authorize_ai_external_turn_sources(&chat, actor, &context)
            .await?;
        let guard: ExternalSourceAuthorization =
            guards.pop().ok_or(MetadataError::AiAccessDenied)?;
        let store = self.clone();
        let reservation_handle:String = sqlite_blocking(move || {
            let conn = store.conn()?;
            crate::ai_run::ai_run_authorized_conn(&conn, run_id, actor, lease)?;
            conn.query_row("SELECT content_handle FROM ai_run_event WHERE run_id=?1 AND tool_call_id=?2 AND kind='tool_requested'", params![run_id.to_string(),call_id.to_string()], |row|row.get(0)).optional()?.ok_or_else(||invalid("External call was not reserved"))
        }).await?;
        let reservation: serde_json::Value = serde_json::from_slice(
            &self
                .ai_content
                .get(tenant.0, &reservation_handle)
                .await?
                .ok_or_else(|| invalid("External reservation is unavailable"))?,
        )?;
        if reservation.get("tool").and_then(serde_json::Value::as_str) != Some(kind.action())
            || reservation.get("source") != Some(&serde_json::to_value(&proof)?)
            || reservation.get("alias").and_then(serde_json::Value::as_str) != Some(alias.as_str())
        {
            return Err(invalid("External read does not match its reservation"));
        }
        let bytes =
            serde_json::to_vec(&serde_json::json!({"source":proof,"alias":alias,"result":result}))?;
        if bytes.len() > 16 * 1024 {
            return Err(invalid("External result exceeds its durable receipt limit"));
        }
        let handle = self.ai_content.put(tenant.0, &bytes).await?;
        let written = handle.clone();
        let store = self.clone();
        let outcome = sqlite_blocking(move || {
            let mut conn = store.conn()?; let tx = conn.transaction()?;
            let (_, status) = crate::ai_run::ai_run_authorized_conn(&tx, run_id, actor, lease)?;
            if status!="running" { return Err(invalid("AI run is no longer active")); }
            guard.require(&tx, tenant, actor)?;
            let terminal: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM ai_run_event WHERE run_id=?1 AND tool_call_id=?2 AND kind IN ('tool_completed','tool_denied'))",
                params![run_id.to_string(),call_id.to_string()], |row|row.get(0))?;
            if terminal { return Err(invalid("External call is already settled")); }
            let sequence:u64 = tx.query_row("SELECT next_sequence FROM ai_run WHERE id=?1",[run_id.to_string()], |row|row.get(0))?;
            tx.execute("INSERT INTO ai_run_event(run_id,sequence,kind,at,tool_call_id,content_handle) VALUES(?1,?2,'tool_completed',?3,?4,?5)",
                params![run_id.to_string(), sequence, Utc::now().to_rfc3339(), call_id.to_string(),written])?;
            tx.execute("UPDATE ai_run SET next_sequence=next_sequence+1 WHERE id=?1",[run_id.to_string()])?;
            tx.commit()?; Ok(())
        }).await;
        if outcome.is_err() {
            self.queue_external_content_cleanup(tenant, &handle)?;
        }
        outcome
    }
}
