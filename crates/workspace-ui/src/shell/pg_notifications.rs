use super::*;

const MAX_ROWS: usize = 200;
const MAX_BYTES: usize = 1024 * 1024;
const MAX_PAYLOAD_BYTES: usize = 16 * 1024;

#[derive(Debug, Clone)]
pub(super) struct PgNotificationRow {
    pub(super) channel: String,
    pub(super) payload: String,
    pub(super) received_at_ms: u64,
}

pub(super) struct PgNotificationState {
    pub(super) channel_input: Entity<TextInput>,
    pub(super) channel: String,
    pub(super) instance_id: Option<String>,
    pub(super) profile_id: Option<i64>,
    pub(super) session: Option<sift_protocol::SessionId>,
    pub(super) connection: Option<sift_protocol::ConnectionId>,
    pub(super) generation: u64,
    pub(super) pending: bool,
    pub(super) listening: bool,
    pub(super) error: Option<String>,
    pub(super) rows: Vec<Arc<PgNotificationRow>>,
    pub(super) bytes: usize,
    pub(super) dropped: u64,
    pub(super) selected: usize,
    pub(super) scroll: UniformListScrollHandle,
}

impl PgNotificationState {
    pub(super) fn push(&mut self, channel: String, payload: String, received_at_ms: u64) {
        let follow_newest = self.rows.is_empty() || self.selected == self.rows.len() - 1;
        let payload = if payload.len() > MAX_PAYLOAD_BYTES {
            let mut end = MAX_PAYLOAD_BYTES;
            while !payload.is_char_boundary(end) {
                end -= 1;
            }
            self.dropped = self.dropped.saturating_add(1);
            format!("{}\n… payload truncated …", &payload[..end])
        } else {
            payload
        };
        let size = channel.len().saturating_add(payload.len());
        while !self.rows.is_empty()
            && (self.rows.len() >= MAX_ROWS || self.bytes + size > MAX_BYTES)
        {
            let old = self.rows.remove(0);
            self.bytes -= old.channel.len() + old.payload.len();
            self.selected = self.selected.saturating_sub(1);
            self.dropped = self.dropped.saturating_add(1);
        }
        self.bytes += size;
        self.rows.push(Arc::new(PgNotificationRow {
            channel,
            payload,
            received_at_ms,
        }));
        if follow_newest {
            self.selected = self.rows.len() - 1;
            self.scroll
                .scroll_to_item(self.selected, ScrollStrategy::Nearest);
        }
    }
}

impl WorkspaceShell {
    pub(super) fn pg_listener_target_changed(&mut self, cx: &mut Context<Self>) {
        if self.pg_notifications.pending || self.pg_notifications.listening {
            self.stop_pg_notifications(cx);
        }
        self.pg_notifications.instance_id = None;
        self.pg_notifications.profile_id = None;
        self.pg_notifications.session = None;
        self.pg_notifications.connection = None;
        self.pg_notifications.rows.clear();
        self.pg_notifications.bytes = 0;
        self.pg_notifications.dropped = 0;
        self.pg_notifications.selected = 0;
        self.pg_notifications.error =
            Some("Database connection changed; start a new listener".into());
        cx.notify();
    }

    pub(super) fn on_pg_listener_event(&mut self, event: ExecutorEvent, cx: &mut Context<Self>) {
        match event {
            ExecutorEvent::PgListenerStarted {
                instance_id,
                profile_id,
                session,
                connection,
                generation,
                result,
            } => {
                if !self.pg_listener_matches(&instance_id, profile_id, generation) {
                    return;
                }
                self.pg_notifications.pending = false;
                match result {
                    Ok(()) => {
                        self.pg_notifications.session = Some(session);
                        self.pg_notifications.connection = Some(connection);
                        self.pg_notifications.listening = true;
                        self.pg_notifications.error = None;
                    }
                    Err(message) => {
                        self.pg_notifications.listening = false;
                        self.pg_notifications.error = Some(message);
                    }
                }
            }
            ExecutorEvent::PgListenerMessage {
                instance_id,
                profile_id,
                session,
                connection,
                generation,
                channel,
                payload,
                received_at_ms,
                queued_counter,
            } => {
                queued_counter.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
                if !self.pg_listener_matches(&instance_id, profile_id, generation)
                    || self.pg_notifications.session != Some(session)
                    || self.pg_notifications.connection != Some(connection)
                    || !self.pg_notifications.listening
                {
                    return;
                }
                self.pg_notifications.push(channel, payload, received_at_ms);
            }
            ExecutorEvent::PgListenerFailed {
                instance_id,
                profile_id,
                session,
                connection,
                generation,
                message,
            } => {
                if !self.pg_listener_matches(&instance_id, profile_id, generation)
                    || self.pg_notifications.session != Some(session)
                    || self.pg_notifications.connection != Some(connection)
                {
                    return;
                }
                self.pg_notifications.listening = false;
                self.pg_notifications.error = Some(message);
            }
            _ => unreachable!("only PostgreSQL listener events are routed here"),
        }
        cx.notify();
    }

    fn pg_listener_matches(
        &self,
        instance_id: &Option<String>,
        profile_id: i64,
        generation: u64,
    ) -> bool {
        self.modal == Some(Modal::PgNotifications)
            && self.pg_notifications.instance_id == *instance_id
            && self.selected_instance_id == *instance_id
            && self.pg_notifications.profile_id == Some(profile_id)
            && matches!(self.connection_status, ConnectionStatus::Connected { profile_id: active, .. } if active == profile_id)
            && self.pg_notifications.generation == generation
    }

    pub(super) fn open_pg_notifications(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.require_operation(
            sift_protocol::OperationKind::Listen,
            "Listen to PostgreSQL",
            cx,
        ) {
            return;
        }
        if !matches!(self.connection_status, ConnectionStatus::Connected { .. }) {
            self.show_error_toast("Connect to PostgreSQL first".into(), cx);
            return;
        }
        self.modal = Some(Modal::PgNotifications);
        self.pg_notifications
            .channel_input
            .focus_handle(cx)
            .focus(window, cx);
        cx.notify();
    }

    pub(super) fn start_pg_notifications(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.pg_notifications.pending {
            return;
        }
        let channel = self
            .pg_notifications
            .channel_input
            .read(cx)
            .text()
            .trim()
            .to_owned();
        if !valid_pg_channel(&channel) {
            self.pg_notifications.error = Some("Channel must start with a letter or underscore and contain only ASCII letters, digits, or underscores".into());
            cx.notify();
            return;
        }
        if !self.require_operation(
            sift_protocol::OperationKind::Listen,
            "Listen to PostgreSQL",
            cx,
        ) {
            return;
        }
        let ConnectionStatus::Connected { profile_id, .. } = self.connection_status else {
            self.pg_notifications.error = Some("Database connection is unavailable".into());
            cx.notify();
            return;
        };
        let Some(sender) = &self.executor_sender else {
            self.pg_notifications.error = Some("Listener executor is unavailable".into());
            cx.notify();
            return;
        };
        let generation = self.pg_notifications.generation.wrapping_add(1);
        let instance_id = self.selected_instance_id.clone();
        if sender
            .send(ExecutorCommand::StartPgListener {
                instance_id: instance_id.clone(),
                profile_id,
                channel: channel.clone(),
                generation,
            })
            .is_err()
        {
            self.pg_notifications.error = Some("Listener request was not queued".into());
            cx.notify();
            return;
        }
        self.pg_notifications.generation = generation;
        self.pg_notifications.instance_id = instance_id;
        self.pg_notifications.profile_id = Some(profile_id);
        self.pg_notifications.session = None;
        self.pg_notifications.connection = None;
        self.pg_notifications.channel = channel;
        self.pg_notifications.pending = true;
        self.pg_notifications.listening = false;
        self.pg_notifications.error = None;
        self.pg_notifications.rows.clear();
        self.pg_notifications.bytes = 0;
        self.pg_notifications.dropped = 0;
        self.pg_notifications.selected = 0;
        self.focus_handle.focus(window, cx);
        cx.notify();
    }

    pub(super) fn stop_pg_notifications(&mut self, cx: &mut Context<Self>) {
        self.pg_notifications.generation = self.pg_notifications.generation.wrapping_add(1);
        self.pg_notifications.pending = false;
        self.pg_notifications.listening = false;
        self.pg_notifications.session = None;
        self.pg_notifications.connection = None;
        if let Some(sender) = &self.executor_sender {
            let _ = sender.send(ExecutorCommand::StopPgListener);
        }
        cx.notify();
    }

    pub(super) fn handle_pg_notification_key(
        &mut self,
        event: &gpui::KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if event.keystroke.modifiers.modified()
            || self
                .pg_notifications
                .channel_input
                .focus_handle(cx)
                .is_focused(window)
        {
            return;
        }
        let count = self.pg_notifications.rows.len();
        let next = match event.keystroke.key.as_str() {
            "j" | "down" => self.pg_notifications.selected.saturating_add(1),
            "k" | "up" => self.pg_notifications.selected.saturating_sub(1),
            "g" => 0,
            "G" => count.saturating_sub(1),
            "y" => {
                if let Some(row) = self
                    .pg_notifications
                    .rows
                    .get(self.pg_notifications.selected)
                {
                    cx.write_to_clipboard(gpui::ClipboardItem::new_string(row.payload.clone()));
                }
                cx.stop_propagation();
                return;
            }
            _ => return,
        };
        self.pg_notifications.selected = next.min(count.saturating_sub(1));
        self.pg_notifications
            .scroll
            .scroll_to_item(self.pg_notifications.selected, ScrollStrategy::Nearest);
        cx.stop_propagation();
        cx.notify();
    }
}

pub(super) fn valid_pg_channel(channel: &str) -> bool {
    let mut chars = channel.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
        && channel.len() <= 63
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

#[cfg(test)]
mod tests {
    use super::valid_pg_channel;

    #[test]
    fn channel_identifiers_are_ascii_and_fit_postgres_limit() {
        assert!(valid_pg_channel("_job_events_9"));
        assert!(valid_pg_channel(&"a".repeat(63)));
        for invalid in ["", "9jobs", "job-events", " jobs", "jób", &"a".repeat(64)] {
            assert!(!valid_pg_channel(invalid));
        }
    }
}
