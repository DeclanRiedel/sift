//! Shared bounds for isolated provider input and output.
use sift_protocol::AiTurnContext;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt};

#[derive(Clone)]
pub(crate) struct AiEventSender {
    events: tokio::sync::mpsc::UnboundedSender<sift_workspace_ui::ExecutorEvent>,
    scope: Option<sift_workspace_ui::AiViewScope>,
}

impl AiEventSender {
    pub(crate) fn new(
        events: tokio::sync::mpsc::UnboundedSender<sift_workspace_ui::ExecutorEvent>,
        scope: Option<sift_workspace_ui::AiViewScope>,
    ) -> Self {
        Self { events, scope }
    }

    pub(crate) fn scope(&self) -> Option<&sift_workspace_ui::AiViewScope> {
        self.scope.as_ref()
    }

    pub(crate) fn send(&self, event: sift_workspace_ui::ExecutorEvent) -> Result<(), ()> {
        let event = match &self.scope {
            Some(scope) => sift_workspace_ui::ExecutorEvent::AiScoped {
                scope: scope.clone(),
                event: Box::new(event),
            },
            None => event,
        };
        self.events.send(event).map_err(|_| ())
    }
}

#[cfg(test)]
impl From<tokio::sync::mpsc::UnboundedSender<sift_workspace_ui::ExecutorEvent>> for AiEventSender {
    fn from(events: tokio::sync::mpsc::UnboundedSender<sift_workspace_ui::ExecutorEvent>) -> Self {
        Self::new(events, None)
    }
}

pub(crate) const MAX_FRAME_BYTES: usize = 1024 * 1024;
pub(crate) const MAX_ANSWER_BYTES: usize = 64 * 1024;
const MAX_INPUT_BYTES: usize = 1024 * 1024;
const MAX_HISTORY_BYTES: usize = 128 * 1024;
const MAX_HISTORY_TURNS: usize = 6;

#[derive(Default)]
pub(crate) struct History {
    pub(crate) turns: Vec<(String, String)>,
    pub(crate) earlier_omitted: bool,
}

pub(crate) struct Input {
    pub(crate) text: String,
    pub(crate) omitted_turns: usize,
}

pub(crate) fn input(
    context: &AiTurnContext,
    prompt: &str,
    history: &[(String, String)],
    earlier_omitted: bool,
) -> Result<Input, String> {
    let context = serde_json::to_string(context).map_err(|_| "Cannot encode Sift context")?;
    let current_size = context
        .len()
        .saturating_add(prompt.len())
        .saturating_add(128);
    if current_size > MAX_INPUT_BYTES {
        return Err("Current context and prompt exceed the Sift input limit; remove an attachment or exclude context".into());
    }
    let history_budget = MAX_HISTORY_BYTES.min(MAX_INPUT_BYTES - current_size);
    let mut retained = Vec::new();
    let mut history_size = 0usize;
    for turn in history.iter().rev().take(MAX_HISTORY_TURNS) {
        let size = turn.0.len().saturating_add(turn.1.len()).saturating_add(24);
        if size > history_budget.saturating_sub(history_size) {
            break;
        }
        history_size += size;
        retained.push(turn);
    }
    let omitted_turns = history.len() - retained.len();
    let mut text = String::with_capacity(current_size + history_size);
    if omitted_turns > 0 || earlier_omitted {
        text.push_str("Earlier chat turns were omitted to fit Sift's context limits.\n\n");
    }
    for (question, answer) in retained.into_iter().rev() {
        text.push_str("User: ");
        text.push_str(question);
        text.push_str("\nAssistant: ");
        text.push_str(answer);
        text.push_str("\n\n");
    }
    text.push_str("Current Sift context (fixed for this turn):\n");
    text.push_str(&context);
    text.push_str("\n\nUser: ");
    text.push_str(prompt);
    debug_assert!(text.len() <= MAX_INPUT_BYTES);
    Ok(Input {
        text,
        omitted_turns,
    })
}

/// Reject an oversized frame before a newline-free stream can grow allocation.
pub(crate) async fn frame<R: AsyncBufRead + Unpin>(
    reader: &mut R,
) -> Result<Option<String>, String> {
    let mut bytes = Vec::new();
    let size = reader
        .take((MAX_FRAME_BYTES + 1) as u64)
        .read_until(b'\n', &mut bytes)
        .await
        .map_err(|_| "Provider output failed")?;
    if size == 0 {
        return Ok(None);
    }
    if size > MAX_FRAME_BYTES {
        return Err("Provider event exceeded the Sift limit".into());
    }
    String::from_utf8(bytes)
        .map(Some)
        .map_err(|_| "Provider sent invalid UTF-8".into())
}

pub(crate) async fn prepare(
    client: &sift_client_sdk::Client,
    lease: &sift_protocol::AiRunLease,
    context: &AiTurnContext,
    prompt: &str,
    history: &History,
    events: &AiEventSender,
) -> Result<String, String> {
    let input = input(context, prompt, &history.turns, history.earlier_omitted)?;
    if input.omitted_turns > 0 || history.earlier_omitted {
        let notice = "Earlier chat turns were omitted to fit Sift's context limits. The current context and reviewed attachments are included in full.";
        crate::ai_tools::append_text(
            client,
            lease,
            sift_protocol::AiEventKind::ProgressSummary,
            notice,
        )
        .await?;
        let _ = events.send(sift_workspace_ui::ExecutorEvent::AiMessage {
            kind: sift_protocol::AiEventKind::ProgressSummary,
            text: notice.into(),
        });
    }
    Ok(input.text)
}

/// Remaining time never refreshes when provider progress arrives.
pub(crate) fn remaining(
    started_at: chrono::DateTime<chrono::Utc>,
    max_run_secs: u32,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<std::time::Duration, String> {
    if !(1..=3600).contains(&max_run_secs) {
        return Err("Invalid Sift run time policy".into());
    }
    let elapsed = (now - started_at).max(chrono::Duration::zero());
    let remaining = chrono::Duration::seconds(i64::from(max_run_secs)) - elapsed;
    if remaining <= chrono::Duration::zero() {
        return Err("AI run time limit reached".into());
    }
    remaining
        .to_std()
        .map_err(|_| "Invalid Sift run deadline".into())
}

pub(crate) struct InvocationIds {
    entries: std::collections::HashMap<String, ([u8; 32], uuid::Uuid)>,
    max_calls: usize,
}

impl InvocationIds {
    pub(crate) fn new(max_calls: usize) -> Self {
        Self {
            entries: Default::default(),
            max_calls,
        }
    }

    pub(crate) fn get(
        &mut self,
        id: &serde_json::Value,
        name: &str,
        arguments: &serde_json::Value,
    ) -> Result<uuid::Uuid, String> {
        use sha2::{Digest, Sha256};
        if !(id.is_string() || id.is_number()) {
            return Err("Provider tool request has no valid ID".into());
        }
        let key = serde_json::to_string(id).map_err(|_| "Invalid provider tool ID")?;
        if key.len() > 512 {
            return Err("Provider tool ID exceeded the Sift limit".into());
        }
        let body = serde_json::to_vec(&(name, arguments))
            .map_err(|_| "Invalid provider tool arguments")?;
        if body.len() > MAX_FRAME_BYTES {
            return Err("Provider tool arguments exceeded the Sift limit".into());
        }
        let digest: [u8; 32] = Sha256::digest(body).into();
        if let Some((prior, invocation)) = self.entries.get(&key) {
            if prior != &digest {
                return Err("Provider reused a tool request ID with different arguments".into());
            }
            return Ok(*invocation);
        }
        if self.entries.len() >= self.max_calls {
            return Err("Sift tool-call budget reached".into());
        }
        let invocation = uuid::Uuid::new_v4();
        self.entries.insert(key, (digest, invocation));
        Ok(invocation)
    }
}

#[derive(Default)]
pub(crate) struct AnswerBudget {
    streamed: usize,
    completed: usize,
}

impl AnswerBudget {
    pub(crate) fn delta(&mut self, text: &str) -> Result<(), String> {
        let size = self.streamed.saturating_add(text.len());
        if size > MAX_ANSWER_BYTES {
            return Err("Provider reply exceeded the Sift text limit".into());
        }
        self.streamed = size;
        Ok(())
    }

    /// Completed messages mirror streamed text, so account the two forms
    /// separately while bounding the sum across non-streamed model steps.
    pub(crate) fn message(&mut self, text: &str) -> Result<(), String> {
        let size = self.completed.saturating_add(text.len());
        if size > MAX_ANSWER_BYTES {
            return Err("Provider reply exceeded the Sift text limit".into());
        }
        self.completed = size;
        Ok(())
    }

    pub(crate) fn completed(text: &str) -> Result<(), String> {
        if text.len() > MAX_ANSWER_BYTES {
            return Err("Provider reply exceeded the Sift text limit".into());
        }
        Ok(())
    }
}

/// Keep live UI updates responsive while persisting bounded batches rather
/// than consuming one durable event for every native token fragment.
#[derive(Default)]
pub(crate) struct TextPublisher {
    pending: String,
    budget: AnswerBudget,
}

impl TextPublisher {
    pub(crate) async fn delta(
        &mut self,
        client: &sift_client_sdk::Client,
        lease: &sift_protocol::AiRunLease,
        events: &AiEventSender,
        text: &str,
    ) -> Result<(), String> {
        if text.is_empty() {
            return Ok(());
        }
        self.budget.delta(text)?;
        self.pending.push_str(text);
        let _ = events.send(sift_workspace_ui::ExecutorEvent::AiTextDelta(text.into()));
        if self.pending.len() >= 1024 {
            self.flush(client, lease).await?;
        }
        Ok(())
    }

    pub(crate) fn message(&mut self, text: &str) -> Result<(), String> {
        self.budget.message(text)
    }

    pub(crate) async fn flush(
        &mut self,
        client: &sift_client_sdk::Client,
        lease: &sift_protocol::AiRunLease,
    ) -> Result<(), String> {
        let text = std::mem::take(&mut self.pending);
        crate::ai_tools::append_text(
            client,
            lease,
            sift_protocol::AiEventKind::MessageDelta,
            &text,
        )
        .await
    }
}

/// Keep Stop effective before provider startup as well as while it is running.
pub(crate) async fn stopped(
    mut signal: tokio::sync::watch::Receiver<Option<sift_protocol::AiRunStatus>>,
) -> sift_protocol::AiRunStatus {
    loop {
        if let Some(status) = *signal.borrow_and_update() {
            return status;
        }
        if signal.changed().await.is_err() {
            return sift_protocol::AiRunStatus::Interrupted;
        }
    }
}

/// Bound pending read/review requests without occupying the desktop command loop.
pub(crate) fn spawn_read<T: Send + 'static>(
    tasks: &mut tokio::task::JoinSet<()>,
    events: AiEventSender,
    read: impl std::future::Future<Output = Result<T, String>> + Send + 'static,
    event: impl FnOnce(Result<T, String>) -> sift_workspace_ui::ExecutorEvent + Send + 'static,
) {
    if tasks.len() >= 16 {
        let _ = events.send(event(Err(
            "AI review requests are busy; retry after the pending review finishes".into(),
        )));
        return;
    }
    tasks.spawn(async move {
        let result = tokio::time::timeout(std::time::Duration::from_secs(30), read)
            .await
            .unwrap_or_else(|_| Err("AI review request timed out; retry it".into()));
        let _ = events.send(event(result));
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_keeps_current_context_exact_and_trims_complete_oldest_turns() {
        let context: AiTurnContext = serde_json::from_value(serde_json::json!({
            "target": {}, "staged_change_count": 0,
            "current_error": "exact source error 🌍"
        }))
        .unwrap();
        let history = (0..8)
            .map(|n| (format!("question {n}"), format!("answer {n}")))
            .collect::<Vec<_>>();
        let built = input(&context, "current question 🌍", &history, false).unwrap();
        assert_eq!(built.omitted_turns, 2);
        assert!(!built.text.contains("question 1"));
        assert!(built.text.contains("question 2"));
        assert!(built.text.contains("Earlier chat turns were omitted"));
        assert!(built.text.ends_with("current question 🌍"));
        assert!(built.text.contains("exact source error 🌍"));
        let earlier = input(&context, "current", &[], true).unwrap();
        assert_eq!(earlier.omitted_turns, 0);
        assert!(earlier.text.contains("Earlier chat turns were omitted"));
        let history = vec![
            ("older".into(), "small".into()),
            ("newest".into(), "🌍".repeat(MAX_HISTORY_BYTES / 4)),
        ];
        let built = input(&context, "current", &history, false).unwrap();
        assert_eq!(built.omitted_turns, 2);
        assert!(!built.text.contains("older"));
        assert!(input(&context, &"a".repeat(MAX_INPUT_BYTES), &[], false).is_err());
        let mut oversized = context;
        oversized.current_error = Some("a".repeat(MAX_INPUT_BYTES));
        assert!(input(&oversized, "current", &[], false).is_err());
    }

    #[tokio::test]
    async fn newline_free_frames_are_bounded_and_utf8_is_checked() {
        let data = vec![b'a'; MAX_FRAME_BYTES + 100];
        let mut reader = tokio::io::BufReader::new(data.as_slice());
        assert!(frame(&mut reader).await.unwrap_err().contains("limit"));
        let mut reader = tokio::io::BufReader::new(&b"\xff\n"[..]);
        assert!(frame(&mut reader).await.unwrap_err().contains("UTF-8"));
        let mut reader = tokio::io::BufReader::new("hello 🌍\n".as_bytes());
        assert_eq!(frame(&mut reader).await.unwrap().unwrap(), "hello 🌍\n");
        assert!(frame(&mut reader).await.unwrap().is_none());
    }

    #[test]
    fn rpc_retries_keep_identity_and_cannot_change_arguments_or_evade_quota() {
        use serde_json::json;
        let mut ids = InvocationIds::new(1);
        let id = ids
            .get(&json!(1), "sift_diagnostics", &json!({"sql":"select 1"}))
            .unwrap();
        assert_eq!(
            ids.get(&json!(1), "sift_diagnostics", &json!({"sql":"select 1"}))
                .unwrap(),
            id
        );
        assert!(ids
            .get(&json!(1), "sift_diagnostics", &json!({"sql":"select 2"}))
            .is_err());
        assert!(ids
            .get(&json!(1), "sift_select", &json!({"sql":"select 1"}))
            .is_err());
        assert!(ids
            .get(&json!(2), "sift_diagnostics", &json!({"sql":"select 1"}))
            .is_err());
        assert!(ids
            .get(&serde_json::Value::Null, "sift_schema", &json!({}))
            .is_err());
    }

    #[test]
    fn progress_does_not_refresh_the_absolute_deadline() {
        let started = chrono::Utc::now();
        let first = remaining(started, 60, started + chrono::Duration::seconds(10)).unwrap();
        let next = remaining(started, 60, started + chrono::Duration::seconds(20)).unwrap();
        assert_eq!(first.as_secs(), 50);
        assert_eq!(next.as_secs(), 40);
        assert!(remaining(started, 60, started + chrono::Duration::seconds(60)).is_err());
        assert_eq!(
            remaining(started, 60, started - chrono::Duration::seconds(5))
                .unwrap()
                .as_secs(),
            60
        );
        assert!(remaining(started, 3601, started).is_err());
    }

    #[test]
    fn streamed_and_completed_answers_have_the_same_byte_bound() {
        let mut budget = AnswerBudget::default();
        budget.delta(&"🌍".repeat(MAX_ANSWER_BYTES / 4)).unwrap();
        assert!(budget.delta("a").is_err());
        budget.message(&"🌍".repeat(MAX_ANSWER_BYTES / 4)).unwrap();
        assert!(budget.message("a").is_err());
        assert!(AnswerBudget::completed(&"a".repeat(MAX_ANSWER_BYTES)).is_ok());
        assert!(AnswerBudget::completed(&"a".repeat(MAX_ANSWER_BYTES + 1)).is_err());
    }
}
