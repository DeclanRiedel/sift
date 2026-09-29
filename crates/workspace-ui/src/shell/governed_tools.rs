//! Authorized automation registry, distinct from host-projected extension UI actions.

use super::*;

pub(super) const MAX_TOOLS: usize = 200;
const MAX_ARGUMENT_CHARS: usize = 32_768;
const MAX_RESULT_CHARS: usize = 16_384;

#[derive(Clone)]
pub(super) struct ToolReview {
    request: sift_protocol::InvokeToolRequest,
    instance_id: String,
    fingerprint: String,
    confirmation: Option<String>,
}

#[derive(Default)]
pub(super) struct GovernedToolsUi {
    pub generation: u64,
    pub instance_id: String,
    pub context: Option<sift_protocol::ToolContext>,
    pub tools: Vec<sift_protocol::GovernedToolDescriptor>,
    pub truncated: bool,
    pub selected: Option<usize>,
    pub arguments: Option<Entity<TextInput>>,
    pub confirmation: Option<Entity<TextInput>>,
    pub review: Option<ToolReview>,
    pub approval: Option<sift_protocol::OperationApproval>,
    pub pending_request: Option<sift_protocol::InvokeToolRequest>,
    pub result: Option<serde_json::Value>,
    pub loading: bool,
    pub pending: bool,
    pub error: Option<String>,
}

impl WorkspaceShell {
    pub(super) fn invalidate_governed_tools(&mut self, reason: &str) {
        let generation = self.governed_tools.generation.wrapping_add(1);
        self.governed_tools = GovernedToolsUi {
            generation,
            error: (self.modal == Some(Modal::GovernedTools)).then(|| reason.to_owned()),
            ..Default::default()
        };
    }

    pub(super) fn governed_target_is_current(&self, cx: &Context<Self>) -> bool {
        self.selected_instance_id.as_deref().unwrap_or("local") == self.governed_tools.instance_id
            && self.governed_tools.context.as_ref() == Some(&self.current_extension_context(cx))
    }

    pub(super) fn open_governed_tools(&mut self, cx: &mut Context<Self>) {
        self.modal = Some(Modal::GovernedTools);
        let generation = self.governed_tools.generation.wrapping_add(1);
        let instance_id = self
            .selected_instance_id
            .as_deref()
            .unwrap_or("local")
            .to_owned();
        let context = self.current_extension_context(cx);
        self.governed_tools = GovernedToolsUi {
            generation,
            instance_id: instance_id.clone(),
            context: Some(context.clone()),
            arguments: Some(cx.new(|cx| TextInput::new("{}", "JSON arguments", cx))),
            loading: true,
            ..Default::default()
        };
        if self.executor_sender.as_ref().is_none_or(|sender| {
            sender
                .send(ExecutorCommand::LoadGovernedTools {
                    generation,
                    instance_id,
                    context,
                })
                .is_err()
        }) {
            self.governed_tools.loading = false;
            self.governed_tools.error = Some("Governed tool service is unavailable".into());
        }
        cx.notify();
    }

    fn select_governed_tool(&mut self, index: usize, cx: &mut Context<Self>) {
        if self.governed_tools.pending || index >= self.governed_tools.tools.len() {
            return;
        }
        self.governed_tools.selected = Some(index);
        self.governed_tools.review = None;
        self.governed_tools.confirmation = None;
        self.governed_tools.approval = None;
        self.governed_tools.pending_request = None;
        self.governed_tools.result = None;
        self.governed_tools.error = None;
        if let Some(input) = &self.governed_tools.arguments {
            input.update(cx, |input, cx| input.set_text("{}", cx));
        }
        cx.notify();
    }

    fn governed_request_from_form(
        &self,
        cx: &Context<Self>,
    ) -> Result<sift_protocol::InvokeToolRequest, String> {
        if !self.governed_target_is_current(cx) {
            return Err("Server or context changed; refresh governed tools".into());
        }
        let ui = &self.governed_tools;
        let tool = ui
            .selected
            .and_then(|index| ui.tools.get(index))
            .ok_or("Select a governed tool")?;
        let raw = ui
            .arguments
            .as_ref()
            .ok_or("Arguments are unavailable")?
            .read(cx)
            .text()
            .to_owned();
        if raw.len() > MAX_ARGUMENT_CHARS {
            return Err("Arguments exceed the desktop input limit".into());
        }
        let arguments: serde_json::Value = serde_json::from_str(&raw)
            .map_err(|error| format!("Arguments must be JSON: {error}"))?;
        Ok(sift_protocol::InvokeToolRequest {
            tool_id: tool.id.clone(),
            arguments,
            context: ui.context.clone().ok_or("Tool context is unavailable")?,
            approval_id: None,
        })
    }

    fn preview_governed_tool(&mut self, cx: &mut Context<Self>) {
        if self.governed_tools.pending {
            return;
        }
        let review = self.governed_request_from_form(cx).and_then(|request| {
            let tool = self
                .governed_tools
                .selected
                .and_then(|index| self.governed_tools.tools.get(index))
                .ok_or("Select a governed tool")?;
            let fingerprint = approval_review::fingerprint(&request.arguments)?;
            let confirmation =
                sift_protocol::classification_requires_approval(tool.operation.classification)
                    .then(|| format!("INVOKE {} ON {}", tool.id, self.governed_tools.instance_id));
            Ok(ToolReview {
                request,
                instance_id: self.governed_tools.instance_id.clone(),
                fingerprint,
                confirmation,
            })
        });
        match review {
            Ok(review) => {
                self.governed_tools.confirmation = review
                    .confirmation
                    .as_ref()
                    .map(|_| cx.new(|cx| TextInput::new("", "Type the invocation phrase", cx)));
                self.governed_tools.review = Some(review);
                self.governed_tools.error = None;
            }
            Err(error) => {
                self.governed_tools.review = None;
                self.governed_tools.confirmation = None;
                self.governed_tools.error = Some(error);
            }
        }
        cx.notify();
    }

    fn invoke_reviewed_governed_tool(&mut self, approved: bool, cx: &mut Context<Self>) {
        if self.governed_tools.pending {
            return;
        }
        let current = match self.governed_request_from_form(cx) {
            Ok(request) => request,
            Err(error) => {
                self.governed_tools.error = Some(error);
                cx.notify();
                return;
            }
        };
        let ui = &mut self.governed_tools;
        let request = if approved {
            match (&ui.approval, &ui.pending_request) {
                (Some(approval), Some(original))
                    if approval.approved_at.is_some() && *original == current =>
                {
                    let mut request = current;
                    request.approval_id = Some(approval.id.clone());
                    Ok(request)
                }
                _ => Err("Approval or inputs changed; review the tool again".to_owned()),
            }
        } else {
            match &ui.review {
                Some(review)
                    if review.request == current && review.instance_id == ui.instance_id =>
                {
                    if let Some(phrase) = &review.confirmation {
                        let typed = ui
                            .confirmation
                            .as_ref()
                            .map(|input| input.read(cx).text().to_owned());
                        if typed.as_deref() != Some(phrase) {
                            Err(format!("Type {phrase} to confirm"))
                        } else {
                            Ok(current)
                        }
                    } else {
                        Ok(current)
                    }
                }
                _ => Err("Inputs or context changed; review the tool again".to_owned()),
            }
        };
        let request = match request {
            Ok(request) => request,
            Err(error) => {
                ui.error = Some(error);
                cx.notify();
                return;
            }
        };
        ui.pending_request = Some(request.clone());
        ui.pending = true;
        ui.error = None;
        if self.executor_sender.as_ref().is_none_or(|sender| {
            sender
                .send(ExecutorCommand::InvokeGovernedTool {
                    generation: ui.generation,
                    instance_id: ui.instance_id.clone(),
                    request,
                })
                .is_err()
        }) {
            ui.pending = false;
            ui.pending_request = None;
            ui.error = Some("Governed tool service is unavailable".into());
        }
        cx.notify();
    }

    fn approve_governed_tool(&mut self, cx: &mut Context<Self>) {
        if self.governed_tools.pending || !self.governed_target_is_current(cx) {
            return;
        }
        let current = match self.governed_request_from_form(cx) {
            Ok(request) => request,
            Err(error) => {
                self.governed_tools.error = Some(error);
                cx.notify();
                return;
            }
        };
        if self.governed_tools.pending_request.as_ref() != Some(&current) {
            self.governed_tools.error = Some("Inputs changed; review the tool again".into());
            cx.notify();
            return;
        }
        let ui = &mut self.governed_tools;
        let Some(approval) = ui
            .approval
            .as_ref()
            .filter(|approval| approval.approved_at.is_none())
        else {
            return;
        };
        ui.pending = true;
        if self.executor_sender.as_ref().is_none_or(|sender| {
            sender
                .send(ExecutorCommand::ApproveGovernedTool {
                    generation: ui.generation,
                    instance_id: ui.instance_id.clone(),
                    approval_id: approval.id.clone(),
                    expected_revision: approval.revision,
                })
                .is_err()
        }) {
            ui.pending = false;
            ui.error = Some("Governed tool approval service is unavailable".into());
        }
        cx.notify();
    }

    pub(super) fn handle_governed_tools_key(
        &mut self,
        event: &KeystrokeEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if event.keystroke.modifiers.modified() {
            return false;
        }
        let key = event.keystroke.unparse();
        if event
            .context_stack
            .iter()
            .any(|context| context.contains("SiftTextInput"))
        {
            if matches!(key.as_str(), "enter" | "escape") {
                self.focus_handle.focus(window, cx);
                return true;
            }
            return false;
        }
        if self.governed_tools.pending && !matches!(key.as_str(), "escape") {
            return true;
        }
        match key.as_str() {
            "j" | "down" => {
                let next = self
                    .governed_tools
                    .selected
                    .map_or(0, |index| index.saturating_add(1));
                if next < self.governed_tools.tools.len() {
                    self.select_governed_tool(next, cx);
                }
            }
            "k" | "up" => {
                if let Some(index) = self
                    .governed_tools
                    .selected
                    .and_then(|index| index.checked_sub(1))
                {
                    self.select_governed_tool(index, cx);
                }
            }
            "i" => {
                if let Some(input) = &self.governed_tools.arguments {
                    input.read(cx).focus_handle(cx).focus(window, cx);
                }
            }
            "p" => self.preview_governed_tool(cx),
            "c" => self.invoke_reviewed_governed_tool(false, cx),
            "a" => self.approve_governed_tool(cx),
            "v" if self.governed_tools.confirmation.is_some()
                && self.governed_tools.approval.is_none() =>
            {
                self.governed_tools
                    .confirmation
                    .as_ref()
                    .unwrap()
                    .read(cx)
                    .focus_handle(cx)
                    .focus(window, cx);
            }
            "v" => self.invoke_reviewed_governed_tool(true, cx),
            "r" => self.open_governed_tools(cx),
            "e" => self.open_extension_contributions(cx),
            _ => return false,
        }
        cx.notify();
        true
    }

    pub(super) fn render_governed_tools(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let colors = cx.theme().colors;
        let ui = &self.governed_tools;
        let selected = ui.selected.and_then(|index| ui.tools.get(index));
        let result = ui.result.as_ref().map(|value| {
            let text =
                serde_json::to_string_pretty(value).unwrap_or_else(|_| "<invalid result>".into());
            let truncated = text.chars().count() > MAX_RESULT_CHARS;
            let bounded = text.chars().take(MAX_RESULT_CHARS).collect::<String>();
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(if truncated {
                    "Result (truncated to 16,384 characters)"
                } else {
                    "Result"
                })
                .child(div().font_family("monospace").text_xs().child(bounded))
        });
        div().h(px(560.)).flex().flex_col().gap_3()
            .child(div().flex().items_center().justify_between()
                .child(div().text_lg().font_weight(gpui::FontWeight::SEMIBOLD).child("Governed tools"))
                .child(Button::new("refresh-governed-tools", "Refresh").tone(ButtonTone::Ghost)
                    .disabled(ui.pending).on_click(cx.listener(|shell, _, _, cx| shell.open_governed_tools(cx))))
            )
            .child(div().text_xs().text_color(colors.muted_text)
                .child("Tools are filtered by server policy and current context. Vim: j/k select · i JSON · p preview · v phrase or approved run · c invoke · a approve · r refresh · e Extensions · Esc close"))
            .children(ui.loading.then(|| div().text_sm().child("Loading tools…")))
            .children(ui.truncated.then(|| div().text_xs().text_color(colors.warning).child("Showing first 200 tools; refine the active context to narrow the list")))
            .child(div().id("governed-tool-list").max_h(px(170.)).overflow_y_scroll().flex().flex_col()
                .children(ui.tools.iter().enumerate().map(|(index, tool)| {
                    let selected = ui.selected == Some(index);
                    let label = format!("{} · {} · {:?}", tool.title, tool.id, tool.operation.classification);
                    Button::new(("governed-tool", index), label)
                        .tone(if selected { ButtonTone::Accent } else { ButtonTone::Ghost })
                        .disabled(ui.pending)
                        .on_click(cx.listener(move |shell, _, _, cx| shell.select_governed_tool(index, cx)))
                })))
            .children(selected.map(|tool| {
                div().flex().flex_col().gap_2()
                    .child(div().text_sm().child(tool.description.chars().take(512).collect::<String>()))
                    .child(div().text_xs().text_color(colors.muted_text).child(format!("Classification: {:?} · required context: {:?}", tool.operation.classification, tool.required_context)))
                    .child(div().text_xs().text_color(colors.muted_text).child(format!("Capabilities: interactive={} · schedulable={} · MCP exposable={}", tool.interactive, tool.schedulable, tool.mcp_exposable)))
                    .child(div().text_xs().font_family("monospace").child(format!("Input schema: {}", serde_json::to_string(&tool.input_schema).unwrap_or_default().chars().take(4096).collect::<String>())))
                    .children(ui.arguments.clone())
                    .child(Button::new("preview-governed-tool", "Review invocation").tone(ButtonTone::Neutral)
                        .disabled(ui.pending).on_click(cx.listener(|shell, _, _, cx| shell.preview_governed_tool(cx))))
            }))
            .children(ui.review.as_ref().map(|review| {
                div().flex().flex_col().gap_1()
                    .child(div().text_sm().font_weight(gpui::FontWeight::SEMIBOLD).child(format!("Review {} on {}", review.request.tool_id, review.instance_id)))
                    .child(div().text_xs().font_family("monospace").child(format!("Context: {}", serde_json::to_string(&review.request.context).unwrap_or_default().chars().take(256).collect::<String>())))
                    .child(div().text_xs().font_family("monospace").child(format!("Input SHA-256: {}", review.fingerprint)))
                    .children(review.confirmation.as_ref().map(|phrase| div().text_xs().text_color(colors.warning).child(format!("Type {phrase} before invoking"))))
                    .children(ui.confirmation.clone())
                    .child(Button::new("invoke-governed-tool", "Invoke reviewed tool").tone(ButtonTone::Accent)
                        .disabled(ui.pending || ui.approval.is_some())
                        .on_click(cx.listener(|shell, _, _, cx| shell.invoke_reviewed_governed_tool(false, cx))))
            }))
            .children(ui.approval.as_ref().map(|approval| {
                div().flex().items_center().gap_2()
                    .child(div().text_sm().text_color(colors.warning).child(if approval.approved_at.is_some() { "Approved; run the same request" } else { "Server approval required" }))
                    .children(approval.approved_at.is_none().then(|| Button::new("approve-governed-tool", "Approve")
                        .tone(ButtonTone::Neutral).disabled(ui.pending)
                        .on_click(cx.listener(|shell, _, _, cx| shell.approve_governed_tool(cx)))))
                    .children(approval.approved_at.is_some().then(|| Button::new("run-approved-governed-tool", "Run approved tool")
                        .tone(ButtonTone::Accent).disabled(ui.pending)
                        .on_click(cx.listener(|shell, _, _, cx| shell.invoke_reviewed_governed_tool(true, cx)))))
            }))
            .children(result)
            .children(ui.error.clone().map(|error| div().text_sm().text_color(colors.danger).child(error)))
            .into_any_element()
    }
}
