//! Short-lived, initiator-bound exact-body previews; durable bodies live in turns.
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use sift_metadata::PrincipalId;
use sift_protocol::{AiAttachmentPreview, ToolContext};
use uuid::Uuid;

use crate::error::{ApiError, ApiResult};

const TTL: Duration = Duration::from_secs(600);
const MAX_PREVIEWS: usize = 128;
const MAX_PER_ACTOR: usize = 16;
const MAX_BYTES: usize = 8 * 1024 * 1024;

#[derive(Clone)]
pub(crate) struct PreviewEntry {
    pub actor: PrincipalId,
    pub chat_id: Uuid,
    pub target: ToolContext,
    pub source_target: ToolContext,
    pub publication_id: Option<Uuid>,
    pub preview: AiAttachmentPreview,
    created: Instant,
    bytes: usize,
}
#[derive(Clone, Default)]
pub(crate) struct PreviewRegistry {
    entries: Arc<Mutex<HashMap<Uuid, PreviewEntry>>>,
}
impl PreviewRegistry {
    pub fn insert(
        &self,
        actor: PrincipalId,
        chat_id: Uuid,
        target: ToolContext,
        source_target: ToolContext,
        publication_id: Option<Uuid>,
        preview: AiAttachmentPreview,
    ) -> ApiResult<()> {
        let bytes = serde_json::to_vec(&preview)
            .map_err(|_| ApiError::Internal("Cannot encode AI attachment preview".into()))?
            .len();
        if bytes > 96 * 1024 {
            return Err(ApiError::BadRequest(
                "Attachment preview exceeds the byte limit".into(),
            ));
        }
        let mut entries = self.entries.lock().unwrap();
        entries.retain(|_, entry| {
            entry.created.elapsed() < TTL && entry.preview.expires_at > chrono::Utc::now()
        });
        while entries.len() >= MAX_PREVIEWS
            || entries
                .values()
                .map(|entry| entry.bytes)
                .sum::<usize>()
                .saturating_add(bytes)
                > MAX_BYTES
            || entries
                .values()
                .filter(|entry| entry.actor == actor)
                .count()
                >= MAX_PER_ACTOR
        {
            let actor_full = entries
                .values()
                .filter(|entry| entry.actor == actor)
                .count()
                >= MAX_PER_ACTOR;
            let oldest = entries
                .iter()
                .filter(|(_, entry)| !actor_full || entry.actor == actor)
                .min_by_key(|(_, entry)| entry.created)
                .map(|(id, _)| *id)
                .ok_or_else(|| ApiError::BadRequest("Attachment preview quota reached".into()))?;
            entries.remove(&oldest);
        }
        entries.insert(
            preview.id,
            PreviewEntry {
                actor,
                chat_id,
                target,
                source_target,
                publication_id,
                preview,
                created: Instant::now(),
                bytes,
            },
        );
        Ok(())
    }
    pub fn get(&self, actor: PrincipalId, chat_id: Uuid, id: Uuid) -> ApiResult<PreviewEntry> {
        let mut entries = self.entries.lock().unwrap();
        entries.retain(|_, entry| {
            entry.created.elapsed() < TTL && entry.preview.expires_at > chrono::Utc::now()
        });
        entries
            .get(&id)
            .filter(|entry| entry.actor == actor && entry.chat_id == chat_id)
            .cloned()
            .ok_or_else(|| {
                ApiError::BadRequest(
                    "Attachment preview expired or is unavailable. Review the source again.".into(),
                )
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sift_protocol::{AiAttachmentSource, AiContextAttachment, AiVisibility};

    fn preview() -> AiAttachmentPreview {
        AiAttachmentPreview {
            id: Uuid::new_v4(),
            attachment: AiContextAttachment {
                source: AiAttachmentSource::QueryHistory { history_id: 1 },
                label: "Historical statement".into(),
                content: serde_json::json!({"sql":"SELECT 1"}),
                sha256: "fixture".into(),
                truncated: false,
                origin_visibility: AiVisibility::Private,
                published_by: None,
            },
            visibility: AiVisibility::Private,
            requires_publication_ack: false,
            expires_at: chrono::Utc::now() + chrono::Duration::seconds(600),
        }
    }

    #[test]
    fn exact_previews_are_actor_chat_bound_expiring_and_quota_limited() {
        let registry = PreviewRegistry::default();
        let actor = PrincipalId(1);
        let chat = Uuid::new_v4();
        let target = ToolContext {
            tenant_id: Some(1),
            room_id: None,
            profile_id: Some(1),
            connection_id: Some("execution-connection".into()),
            document_id: None,
        };
        let first = preview();
        registry
            .insert(
                actor,
                chat,
                target.clone(),
                target.clone(),
                None,
                first.clone(),
            )
            .unwrap();
        assert_eq!(registry.get(actor, chat, first.id).unwrap().preview, first);
        assert!(registry.get(PrincipalId(2), chat, first.id).is_err());
        assert!(registry.get(actor, Uuid::new_v4(), first.id).is_err());
        for _ in 0..MAX_PER_ACTOR {
            registry
                .insert(actor, chat, target.clone(), target.clone(), None, preview())
                .unwrap();
        }
        assert_eq!(registry.entries.lock().unwrap().len(), MAX_PER_ACTOR);
        assert!(registry.get(actor, chat, first.id).is_err());
        let last = preview();
        registry
            .insert(actor, chat, target.clone(), target, None, last.clone())
            .unwrap();
        registry
            .entries
            .lock()
            .unwrap()
            .get_mut(&last.id)
            .unwrap()
            .created = Instant::now() - TTL;
        assert!(registry.get(actor, chat, last.id).is_err());
        let mut oversized = preview();
        oversized.attachment.content = serde_json::json!("x".repeat(96 * 1024));
        assert!(registry
            .insert(
                actor,
                chat,
                ToolContext {
                    tenant_id: None,
                    room_id: None,
                    profile_id: None,
                    connection_id: None,
                    document_id: None
                },
                ToolContext {
                    tenant_id: None,
                    room_id: None,
                    profile_id: None,
                    connection_id: None,
                    document_id: None
                },
                None,
                oversized
            )
            .is_err());
    }
}
