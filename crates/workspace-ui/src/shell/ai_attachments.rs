//! Reviewed attachment flow. Source coordinates travel to the server; cell
//! values and historical records are never uploaded as authoritative context.
use super::*;

#[derive(Default)]
pub(super) struct AiAttachmentState {
    pub(super) pending: Option<uuid::Uuid>,
    pub(super) review: Option<AiAttachmentReview>,
    pub(super) accepted: Vec<AiAttachmentReview>,
    pub(super) room_list_pending: Option<uuid::Uuid>,
    pub(super) room_results: Vec<sift_protocol::RoomQueryResult>,
}

pub(super) struct AiAttachmentReview {
    pub(super) preview: sift_protocol::AiAttachmentPreview,
    pub(super) target: sift_protocol::ToolContext,
    pub(super) body: SharedString,
    pub(super) publish_ack: bool,
}

impl WorkspaceShell {
    pub(super) fn preview_ai_selected_rows(&mut self, cx: &mut Context<Self>) {
        self.ai.context_origin = WorkspaceSurface::Results;
        let result: Result<_, String> = (|| {
            let (item, view) = self
                .focused_pane_results_item(cx)
                .ok_or("Select cells in a query result first")?;
            let source = self
                .result_ai_sources
                .get(&item)
                .ok_or("This result has no retained query source")?;
            let selection = view.read(cx).ai_selection()?;
            let result_id = source.result_id.ok_or(
                "This server did not retain an attachment source; run the query again when ready",
            )?;
            let mut context = self.ai_context_snapshot(cx)?;
            let original = source
                .target
                .as_ref()
                .ok_or("This result has no managed connection profile")?;
            if original.instance_id != self.selected_instance_id.as_deref().unwrap_or("local")
                || Some(original.tenant_id) != self.selected_tenant_id()
            {
                return Err("Return to the result's instance and tenant before attaching".into());
            }
            if self
                .ai
                .chat
                .as_ref()
                .map(|chat| chat.visibility)
                .or_else(|| {
                    self.ai
                        .policy
                        .as_ref()
                        .map(|policy| policy.new_chat_visibility)
                })
                == Some(sift_protocol::AiVisibility::RoomPublic)
            {
                return Err("Private grid rows cannot enter a room chat. Attach an existing shared room result instead.".into());
            }
            context.target.profile_id = Some(original.profile_id);
            context.target.connection_id = source
                .session
                .zip(source.connection)
                .map(|(session, connection)| format!("{}:{}", session.0, connection.0));
            context.target.document_id = None;
            Ok(sift_protocol::PreviewAiAttachmentRequest {
                target: context.target,
                source: sift_protocol::AiAttachmentSource::QueryRows {
                    result_id,
                    result_set: selection.result_set,
                    schema_digest: selection.schema_digest,
                    row_ordinals: selection.row_ordinals,
                    column_indices: selection.column_indices,
                },
            })
        })();
        match result {
            Ok(request) => self.request_ai_attachment(request, cx),
            Err(error) => {
                self.ai.error = Some(error);
                cx.notify();
            }
        }
    }

    pub(super) fn preview_ai_selected_history(&mut self, cx: &mut Context<Self>) {
        let result: Result<_, String> = (|| {
            let instance = self.selected_instance_id.as_deref().unwrap_or("local");
            if self.query_history.instance.as_deref() != Some(instance) {
                return Err("Load query history from this instance before attaching".into());
            }
            let entries = self.filtered_query_history(cx);
            let entry = entries
                .get(self.query_history.selected)
                .ok_or("Select a query history entry first")?;
            let mut target = self.ai_context_snapshot(cx)?.target;
            target.document_id = None;
            let shared =
                target.room_id.is_some() && entry.room_id.map(|room| room.0) == target.room_id;
            if !shared && entry.connection_profile_id.map(|profile| profile.0) != target.profile_id
            {
                return Err(
                    "Select this history entry's connection profile before reviewing it".into(),
                );
            }
            Ok(sift_protocol::PreviewAiAttachmentRequest {
                target,
                source: sift_protocol::AiAttachmentSource::QueryHistory {
                    history_id: entry.id.0,
                },
            })
        })();
        match result {
            Ok(request) => self.request_ai_attachment(request, cx),
            Err(error) => {
                self.ai.error = Some(error);
                cx.notify();
            }
        }
    }

    pub(super) fn preview_ai_selected_plan(&mut self, cx: &mut Context<Self>) {
        let result: Result<_, String> = (|| {
            if self.selected_plan_captures.len() != 1 {
                return Err("Select one saved execution plan before attaching".into());
            }
            let id = self.selected_plan_captures[0];
            let capture = self
                .plan_captures
                .iter()
                .find(|capture| capture.id == id)
                .ok_or("Refresh the saved plan list before attaching")?;
            let mut target = self.ai_context_snapshot(cx)?.target;
            target.document_id = None;
            if target.profile_id != Some(capture.connection_profile_id) {
                return Err(
                    "Select the saved plan's connection profile before reviewing it".into(),
                );
            }
            Ok(sift_protocol::PreviewAiAttachmentRequest {
                target,
                source: sift_protocol::AiAttachmentSource::PlanCapture { capture_id: id },
            })
        })();
        match result {
            Ok(request) => self.request_ai_attachment(request, cx),
            Err(error) => {
                self.ai.error = Some(error);
                cx.notify();
            }
        }
    }

    pub(super) fn load_ai_room_results(&mut self, cx: &mut Context<Self>) {
        let room = self
            .ai
            .chat
            .as_ref()
            .and_then(|chat| chat.room_id)
            .or_else(|| self.ai_publication_room(cx));
        let Some(room_id) = room else {
            self.ai.error =
                Some("Open a room chat or room document to browse shared results".into());
            cx.notify();
            return;
        };
        let Some(sender) = &self.executor_sender else {
            return;
        };
        let nonce = uuid::Uuid::new_v4();
        if sender
            .send(ExecutorCommand::ListAiRoomResults {
                instance_id: self
                    .selected_instance_id
                    .clone()
                    .unwrap_or_else(|| "local".into()),
                room_id,
                nonce,
            })
            .is_ok()
        {
            self.ai.attachments.room_list_pending = Some(nonce);
            self.ai.error = None;
        }
        cx.notify();
    }

    pub(super) fn preview_ai_room_cells(
        &mut self,
        result: sift_protocol::RoomQueryResult,
        result_set: usize,
        cx: &mut Context<Self>,
    ) {
        let request: Result<_, String> = (|| {
            let mut target = self.ai_context_snapshot(cx)?.target;
            if target.room_id != Some(result.room_id) {
                return Err("Return to the shared result's room before attaching".into());
            }
            target.document_id = None;
            if self
                .ai
                .chat
                .as_ref()
                .map(|chat| chat.visibility)
                .or_else(|| {
                    self.ai
                        .policy
                        .as_ref()
                        .map(|policy| policy.new_chat_visibility)
                })
                == Some(sift_protocol::AiVisibility::RoomPublic)
            {
                target.profile_id = None;
                target.connection_id = None;
            }
            let row_ordinals =
                attachment_positions(self.ai.room_rows_input.read(cx).text(), 100, 50_000)?;
            let column_indices =
                attachment_positions(self.ai.room_columns_input.read(cx).text(), 64, 512)?
                    .into_iter()
                    .map(|index| index as u32)
                    .collect();
            let schema_digest = result
                .schema_digests
                .get(result_set)
                .ok_or("This shared result has no retained schema")?
                .clone();
            Ok(sift_protocol::PreviewAiAttachmentRequest {
                target,
                source: sift_protocol::AiAttachmentSource::RoomRows {
                    room_id: result.room_id,
                    result_id: result.result_id,
                    result_set: result_set as u32,
                    schema_digest,
                    row_ordinals,
                    column_indices,
                },
            })
        })();
        match request {
            Ok(request) => self.request_ai_attachment(request, cx),
            Err(error) => {
                self.ai.error = Some(error);
                cx.notify();
            }
        }
    }

    pub(super) fn request_ai_attachment(
        &mut self,
        request: sift_protocol::PreviewAiAttachmentRequest,
        cx: &mut Context<Self>,
    ) {
        if self.ai.pending || self.ai.attachments.pending.is_some() {
            return;
        }
        if self.ai.attachments.accepted.len() >= 4 {
            self.ai.error =
                Some("Remove an attachment before adding another; each turn supports four".into());
            cx.notify();
            return;
        }
        let Some(visibility) = self
            .ai
            .chat
            .as_ref()
            .map(|chat| chat.visibility)
            .or_else(|| {
                self.ai
                    .policy
                    .as_ref()
                    .map(|policy| policy.new_chat_visibility)
            })
        else {
            self.ai.error = Some("Wait for chat visibility to load before attaching".into());
            cx.notify();
            return;
        };
        let Some(tenant_id) = self.selected_tenant_id() else {
            return;
        };
        let Some(sender) = &self.executor_sender else {
            return;
        };
        let nonce = uuid::Uuid::new_v4();
        if sender
            .send(ExecutorCommand::PreviewAiAttachment {
                instance_id: self
                    .selected_instance_id
                    .clone()
                    .unwrap_or_else(|| "local".into()),
                tenant_id,
                chat_id: self.ai.chat.as_ref().map(|chat| chat.id),
                visibility,
                nonce,
                request: request.clone(),
            })
            .is_ok()
        {
            self.ai.attachments.pending = Some(nonce);
            self.ai.attachments.review = None;
            self.ai.error = None;
            self.ai.activity = Some("Preparing exact attachment preview…".into());
        }
        cx.notify();
    }

    pub(super) fn cancel_ai_attachment(&mut self, cx: &mut Context<Self>) {
        self.ai.attachments.pending = None;
        self.ai.attachments.review = None;
        self.ai.activity = None;
        self.ai.error = None;
        cx.notify();
    }

    pub(super) fn confirm_ai_attachment_publication(&mut self, cx: &mut Context<Self>) {
        if let Some(review) = &mut self.ai.attachments.review {
            if review.preview.requires_publication_ack {
                review.publish_ack = !review.publish_ack;
                self.ai.error = None;
            }
        }
        cx.notify();
    }

    pub(super) fn accept_ai_attachment(&mut self, cx: &mut Context<Self>) {
        let Some(review) = self.ai.attachments.review.as_ref() else {
            return;
        };
        if review.preview.expires_at <= chrono::Utc::now() {
            self.ai.error = Some("This preview expired; review the source again".into());
            cx.notify();
            return;
        }
        if review.preview.requires_publication_ack && !review.publish_ack {
            self.ai.error = Some(
                "Confirm publication to current and future room members before attaching".into(),
            );
            cx.notify();
            return;
        }
        if self.ai.attachments.accepted.len() >= 4 {
            return;
        }
        if let Some(review) = self.ai.attachments.review.take() {
            if self.ai.input.read(cx).text().trim().is_empty() {
                let prompt = match review.preview.attachment.source {
                    sift_protocol::AiAttachmentSource::QueryHistory { .. } =>
                        "Explain this query history entry and any failure. Suggest a safe next step.",
                    sift_protocol::AiAttachmentSource::PlanCapture { .. } =>
                        "Explain this saved execution plan and its main performance risks.",
                    sift_protocol::AiAttachmentSource::QueryRows { .. }
                    | sift_protocol::AiAttachmentSource::RoomRows { .. } =>
                        "Explain the selected result cells and any patterns or anomalies.",
                };
                self.ai
                    .input
                    .update(cx, |input, cx| input.set_text(prompt, cx));
            }
            self.ai.attachments.accepted.push(review);
            self.ai.error = None;
        }
        cx.notify();
    }

    pub(super) fn render_ai_attachments(&self, cx: &mut Context<Self>) -> AnyElement {
        let colors = cx.theme().colors;
        let mut view = div().flex().flex_col().gap_1().text_xs().child(
            Button::new("ai-attach-selected-rows", "Review selected result rows")
                .tone(ButtonTone::Ghost)
                .disabled(self.ai.pending || self.ai.attachments.pending.is_some())
                .on_click(cx.listener(|shell, _, _, cx| shell.preview_ai_selected_rows(cx))),
        );
        view = view.child(
            div()
                .flex()
                .flex_wrap()
                .gap_1()
                .child(
                    Button::new(
                        "ai-attach-selected-history",
                        "Review selected history/error",
                    )
                    .tone(ButtonTone::Ghost)
                    .disabled(self.ai.pending || self.ai.attachments.pending.is_some())
                    .on_click(cx.listener(|shell, _, _, cx| shell.preview_ai_selected_history(cx))),
                )
                .child(
                    Button::new("ai-attach-selected-plan", "Review selected saved plan")
                        .tone(ButtonTone::Ghost)
                        .disabled(self.ai.pending || self.ai.attachments.pending.is_some())
                        .on_click(
                            cx.listener(|shell, _, _, cx| shell.preview_ai_selected_plan(cx)),
                        ),
                ),
        );
        view = view.child(
            Button::new(
                "ai-browse-shared-results",
                "Browse existing shared room results",
            )
            .tone(ButtonTone::Ghost)
            .disabled(self.ai.pending || self.ai.attachments.room_list_pending.is_some())
            .on_click(cx.listener(|shell, _, _, cx| shell.load_ai_room_results(cx))),
        );
        if !self.ai.attachments.room_results.is_empty() {
            let mut sources = div()
                .id("ai-shared-result-picker")
                .max_h(px(180.))
                .overflow_y_scroll()
                .flex()
                .flex_col()
                .gap_1()
                .child("Shared result cells · rows and columns start at 1; commas and ranges (1-5)")
                .child(self.ai.room_rows_input.clone())
                .child(self.ai.room_columns_input.clone());
            for result in self.ai.attachments.room_results.iter().rev().take(16) {
                for set in 0..result.schema_digests.len().min(8) {
                    let source = result.clone();
                    sources = sources.child(
                        Button::new(
                            format!("ai-review-room-result-{}-{set}", result.result_id),
                            format!(
                                "Review {} · user {} · {:?} · set {}",
                                result.created_at.format("%H:%M:%S"),
                                result.actor_principal_id,
                                result.status,
                                set + 1
                            ),
                        )
                        .tone(ButtonTone::Ghost)
                        .disabled(self.ai.pending || self.ai.attachments.pending.is_some())
                        .on_click(cx.listener(move |shell, _, _, cx| {
                            shell.preview_ai_room_cells(source.clone(), set, cx)
                        })),
                    );
                }
            }
            view = view.child(sources);
        }
        if self.ai.attachments.pending.is_some() {
            view = view.child(
                Button::new("ai-cancel-attachment-preview", "Cancel preview")
                    .tone(ButtonTone::Ghost)
                    .on_click(cx.listener(|shell, _, _, cx| {
                        shell.ai.attachments.pending = None;
                        shell.ai.activity = None;
                        cx.notify();
                    })),
            );
        }
        for review in &self.ai.attachments.accepted {
            let id = review.preview.id;
            let expired = review.preview.expires_at <= chrono::Utc::now();
            view = view.child(
                div()
                    .flex()
                    .gap_1()
                    .items_center()
                    .child(format!(
                        "{}{} · {}",
                        review.preview.attachment.label,
                        if review.preview.attachment.truncated {
                            " · excerpt"
                        } else {
                            ""
                        },
                        if expired {
                            "expired — review again"
                        } else {
                            "reviewed"
                        }
                    ))
                    .child(
                        Button::new(format!("ai-remove-attachment-{id}"), "Remove")
                            .tone(ButtonTone::Ghost)
                            .disabled(self.ai.pending)
                            .on_click(cx.listener(move |shell, _, _, cx| {
                                shell
                                    .ai
                                    .attachments
                                    .accepted
                                    .retain(|review| review.preview.id != id);
                                cx.notify();
                            })),
                    ),
            );
        }
        if let Some(review) = &self.ai.attachments.review {
            view = view.child(
                div()
                    .border_1()
                    .border_color(colors.subtle_border)
                    .p_2()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(review.preview.attachment.label.clone())
                    .child(
                        if review.preview.visibility == sift_protocol::AiVisibility::RoomPublic {
                            "Visible to current and future room members"
                        } else {
                            "Private to you"
                        },
                    )
                    .when(review.preview.attachment.truncated, |view| {
                        view.child("Bounded excerpt: some source content is omitted")
                    })
                    .child(
                        div()
                            .id("ai-exact-attachment-preview")
                            .max_h(px(240.))
                            .overflow_y_scroll()
                            .whitespace_normal()
                            .child(review.body.clone()),
                    )
                    .when(review.preview.requires_publication_ack, |view| {
                        view.child(
                            Button::new(
                                "ai-attachment-publish-ack",
                                if review.publish_ack {
                                    "Publication confirmed ✓"
                                } else {
                                    "Share this exact content with this room"
                                },
                            )
                            .tone(if review.publish_ack {
                                ButtonTone::Accent
                            } else {
                                ButtonTone::Ghost
                            })
                            .on_click(cx.listener(
                                |shell, _, _, cx| {
                                    shell.confirm_ai_attachment_publication(cx);
                                },
                            )),
                        )
                    })
                    .child(
                        div()
                            .flex()
                            .gap_1()
                            .child(
                                Button::new("ai-accept-attachment", "Attach reviewed content")
                                    .disabled(
                                        review.preview.requires_publication_ack
                                            && !review.publish_ack,
                                    )
                                    .on_click(cx.listener(|shell, _, _, cx| {
                                        shell.accept_ai_attachment(cx)
                                    })),
                            )
                            .child(
                                Button::new("ai-cancel-attachment", "Cancel")
                                    .tone(ButtonTone::Ghost)
                                    .on_click(cx.listener(|shell, _, _, cx| {
                                        shell.cancel_ai_attachment(cx);
                                    })),
                            ),
                    ),
            );
        }
        view.into_any_element()
    }
}

/// Parse bounded source coordinates without expanding an unbounded range.
fn attachment_positions(text: &str, limit: usize, ceiling: u64) -> Result<Vec<u64>, String> {
    let mut indices = Vec::new();
    for part in text.split(',') {
        let part = part.trim();
        let (start, end) = if let Some((start, end)) = part.split_once('-') {
            (start.trim().parse::<u64>(), end.trim().parse::<u64>())
        } else {
            (part.parse::<u64>(), part.parse::<u64>())
        };
        let (start, end) = (
            start.map_err(|_| "Use row or column numbers such as 1,3,5-9".to_owned())?,
            end.map_err(|_| "Use row or column numbers such as 1,3,5-9".to_owned())?,
        );
        if start == 0
            || end < start
            || end > ceiling
            || end - start >= limit as u64
            || indices.len() + (end - start + 1) as usize > limit
        {
            return Err(format!(
                "Select at most {limit} rows or columns numbered 1 to {ceiling}"
            ));
        }
        for index in start..=end {
            if indices.contains(&(index - 1)) {
                return Err("Each row or column may appear only once".into());
            }
            indices.push(index - 1);
        }
    }
    if indices.is_empty() {
        return Err("Select at least one row or column".into());
    }
    Ok(indices)
}

#[cfg(test)]
mod tests {
    use super::attachment_positions;
    #[test]
    fn source_indices_preserve_order_and_bound_range_expansion() {
        assert_eq!(
            attachment_positions("4,1-3", 4, 50_000).unwrap(),
            vec![3, 0, 1, 2]
        );
        for text in ["1-99999999999", "1,1", "4-2", "", "50001", "0"] {
            assert!(attachment_positions(text, 100, 50_000).is_err());
        }
        assert!(attachment_positions("1-65", 64, 512).is_err());
    }
}
