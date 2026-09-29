//! Host-owned projection of extension descriptors. No extension content becomes UI code.

use super::approval_review::ApprovalReview;
use super::*;

const MAX_FIELDS: usize = 16;
const MAX_ROWS: usize = 100;
const MAX_CELL_CHARS: usize = 256;
const MAX_CONTRIBUTIONS: usize = 200;

#[derive(Default)]
pub(super) struct ExtensionContributionsUi {
    pub generation: u64,
    pub descriptors: Vec<sift_protocol::ExtensionDescriptor>,
    pub loading: bool,
    pub pending: bool,
    pub error: Option<String>,
    pub selected: Option<(usize, usize)>,
    pub inputs: Vec<Entity<TextInput>>,
    pub fields: Vec<FormField>,
    pub result: Option<serde_json::Value>,
    pub approval: Option<sift_protocol::OperationApproval>,
    pub pending_request: Option<sift_protocol::InvokeExtensionRequest>,
    pub approval_review: Option<ApprovalReview>,
    pub approval_confirmation: Option<Entity<TextInput>>,
}

impl WorkspaceShell {
    pub(super) fn handle_extension_contributions_key(
        &mut self,
        event: &KeystrokeEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if event.keystroke.modifiers.modified() {
            return false;
        }
        let key = event.keystroke.unparse();
        if self.extension_contributions.pending
            && matches!(
                key.as_str(),
                "j" | "down" | "k" | "up" | "i" | "r" | "p" | "c" | "v"
            )
        {
            return true;
        }
        let input_focused = event
            .context_stack
            .iter()
            .any(|context| context.contains("SiftTextInput"));
        if input_focused {
            match key.as_str() {
                "escape" | "enter" => self.focus_handle.focus(window, cx),
                "tab" | "shift-tab" => {
                    let next = self
                        .extension_contributions
                        .inputs
                        .iter()
                        .position(|input| input.read(cx).focus_handle(cx).is_focused(window))
                        .and_then(|index| {
                            if key == "tab" {
                                self.extension_contributions.inputs.get(index + 1)
                            } else {
                                index.checked_sub(1).and_then(|index| {
                                    self.extension_contributions.inputs.get(index)
                                })
                            }
                        });
                    if let Some(input) = next {
                        input.read(cx).focus_handle(cx).focus(window, cx);
                    } else {
                        self.focus_handle.focus(window, cx);
                    }
                }
                _ => return false,
            }
            cx.notify();
            return true;
        }
        let options = self
            .extension_contributions
            .descriptors
            .iter()
            .enumerate()
            .flat_map(|(extension_index, extension)| {
                extension.contributions.iter().enumerate().filter_map(
                    move |(contribution_index, contribution)| {
                        contribution
                            .client
                            .as_ref()
                            .map(|_| (extension_index, contribution_index))
                    },
                )
            })
            .take(MAX_CONTRIBUTIONS)
            .collect::<Vec<_>>();
        match key.as_str() {
            "j" | "down" | "k" | "up" if !options.is_empty() => {
                let current = self
                    .extension_contributions
                    .selected
                    .and_then(|selected| options.iter().position(|option| *option == selected));
                let next = match (key.as_str(), current) {
                    ("j" | "down", Some(index)) => (index + 1).min(options.len() - 1),
                    ("k" | "up", Some(index)) => index.saturating_sub(1),
                    _ => 0,
                };
                let (extension_index, contribution_index) = options[next];
                self.select_extension_contribution(extension_index, contribution_index, cx);
            }
            "enter" if self.extension_contributions.selected.is_some() => {
                self.invoke_selected_extension_contribution(cx);
            }
            "i" if !self.extension_contributions.inputs.is_empty() => {
                self.extension_contributions.inputs[0]
                    .read(cx)
                    .focus_handle(cx)
                    .focus(window, cx);
            }
            "a" if self.extension_contributions.approval.is_some() => {
                self.approve_selected_extension_contribution(cx);
            }
            "p" => self.preview_selected_extension_approval(cx),
            "v" if self.extension_contributions.approval_confirmation.is_some() => {
                self.extension_contributions
                    .approval_confirmation
                    .as_ref()
                    .unwrap()
                    .read(cx)
                    .focus_handle(cx)
                    .focus(window, cx);
            }
            "c" => self.create_selected_extension_approval(cx),
            "r" => self.open_extension_contributions(cx),
            "g" => self.open_governed_tools(cx),
            _ => return false,
        }
        cx.notify();
        true
    }

    pub(super) fn open_extension_contributions(&mut self, cx: &mut Context<Self>) {
        self.modal = Some(Modal::ExtensionContributions);
        if self.extension_contributions.pending {
            cx.notify();
            return;
        }
        self.extension_contributions.generation =
            self.extension_contributions.generation.saturating_add(1);
        self.extension_contributions.descriptors.clear();
        self.extension_contributions.selected = None;
        self.extension_contributions.inputs.clear();
        self.extension_contributions.fields.clear();
        self.extension_contributions.result = None;
        self.extension_contributions.approval = None;
        self.extension_contributions.pending_request = None;
        self.extension_contributions.approval_review = None;
        self.extension_contributions.approval_confirmation = None;
        self.extension_contributions.loading = true;
        self.extension_contributions.error = None;
        let generation = self.extension_contributions.generation;
        if self.executor_sender.as_ref().is_none_or(|sender| {
            sender
                .send(ExecutorCommand::LoadExtensionContributions {
                    generation,
                    instance_id: self
                        .selected_instance_id
                        .as_deref()
                        .unwrap_or("local")
                        .to_owned(),
                })
                .is_err()
        }) {
            self.extension_contributions.loading = false;
            self.extension_contributions.error = Some("Extension service is unavailable".into());
        }
        cx.notify();
    }

    fn select_extension_contribution(
        &mut self,
        extension_index: usize,
        contribution_index: usize,
        cx: &mut Context<Self>,
    ) {
        let ui = &mut self.extension_contributions;
        if ui.pending {
            return;
        }
        ui.selected = Some((extension_index, contribution_index));
        ui.inputs.clear();
        ui.fields.clear();
        ui.error = None;
        ui.result = None;
        ui.approval = None;
        ui.pending_request = None;
        ui.approval_review = None;
        ui.approval_confirmation = None;
        let Some(contribution) = ui
            .descriptors
            .get(extension_index)
            .and_then(|extension| extension.contributions.get(contribution_index))
        else {
            ui.error = Some("Contribution is no longer available".into());
            cx.notify();
            return;
        };
        if let Some(sift_protocol::ClientContributionDescriptor::Form { schema, .. }) =
            &contribution.client
        {
            match form_fields(schema) {
                Ok(fields) => {
                    ui.inputs = fields
                        .iter()
                        .map(|field| {
                            let label = field.label.clone();
                            let masked = field.masked;
                            cx.new(|cx| {
                                let input = TextInput::new("", label.clone(), cx).aria_label(label);
                                if masked {
                                    input.masked()
                                } else {
                                    input
                                }
                            })
                        })
                        .collect();
                    ui.fields = fields;
                }
                Err(error) => ui.error = Some(error),
            }
        }
        cx.notify();
    }

    fn selected_extension_invocation(
        &self,
        cx: &Context<Self>,
    ) -> Result<sift_protocol::InvokeExtensionRequest, String> {
        let ui = &self.extension_contributions;
        let (extension_index, contribution_index) =
            ui.selected.ok_or("Select an extension action")?;
        let extension = ui
            .descriptors
            .get(extension_index)
            .ok_or("Extension is no longer available")?;
        let contribution = extension
            .contributions
            .get(contribution_index)
            .ok_or("Action is no longer available")?;
        let values = ui
            .inputs
            .iter()
            .map(|input| input.read(cx).text().to_owned())
            .collect::<Vec<_>>();
        let arguments = form_arguments(&ui.fields, &values)?;
        invocation(
            extension,
            contribution,
            arguments,
            self.current_extension_context(cx),
        )
    }

    fn preview_selected_extension_approval(&mut self, cx: &mut Context<Self>) {
        if self.extension_contributions.pending {
            return;
        }
        if self.extension_contributions.approval.is_some() {
            self.extension_contributions.error =
                Some("An approval is already attached to this action".into());
            cx.notify();
            return;
        }
        let review = self
            .selected_extension_invocation(cx)
            .and_then(|invocation| {
                ApprovalReview::new(
                    invocation,
                    self.selected_instance_id
                        .as_deref()
                        .unwrap_or("local")
                        .to_owned(),
                )
            });
        match review {
            Ok(review) => {
                let confirmation =
                    cx.new(|cx| TextInput::new("", "Type the approval request phrase", cx));
                self.extension_contributions.approval_review = Some(review);
                self.extension_contributions.approval_confirmation = Some(confirmation);
                self.extension_contributions.error = None;
            }
            Err(error) => {
                self.extension_contributions.approval_review = None;
                self.extension_contributions.approval_confirmation = None;
                self.extension_contributions.error = Some(error);
            }
        }
        cx.notify();
    }

    fn create_selected_extension_approval(&mut self, cx: &mut Context<Self>) {
        if self.extension_contributions.pending {
            return;
        }
        let current = match self.selected_extension_invocation(cx) {
            Ok(request) => request,
            Err(error) => {
                self.extension_contributions.error = Some(error);
                cx.notify();
                return;
            }
        };
        let ui = &mut self.extension_contributions;
        let instance_id = self.selected_instance_id.as_deref().unwrap_or("local");
        let request = match (&ui.approval_review, &ui.approval_confirmation) {
            (Some(review), Some(confirmation)) => {
                review.confirmed_request(&current, instance_id, confirmation.read(cx).text())
            }
            _ => Err("Review the approval request before creating it".into()),
        };
        let request = match request {
            Ok(request) => request,
            Err(error) => {
                ui.error = Some(error);
                cx.notify();
                return;
            }
        };
        ui.pending_request = Some(current);
        ui.pending = true;
        ui.error = None;
        let command = ExecutorCommand::CreateExtensionApproval {
            generation: ui.generation,
            instance_id: instance_id.to_owned(),
            request,
        };
        if self
            .executor_sender
            .as_ref()
            .is_none_or(|sender| sender.send(command).is_err())
        {
            ui.pending = false;
            ui.pending_request = None;
            ui.error = Some("Extension approval service is unavailable".into());
        }
        cx.notify();
    }

    fn invoke_selected_extension_contribution(&mut self, cx: &mut Context<Self>) {
        if self.extension_contributions.pending {
            return;
        }
        let context = self.current_extension_context(cx);
        let ui = &mut self.extension_contributions;
        let Some((extension_index, contribution_index)) = ui.selected else {
            return;
        };
        let Some(extension) = ui.descriptors.get(extension_index) else {
            return;
        };
        let Some(contribution) = extension.contributions.get(contribution_index) else {
            return;
        };
        let request = if let Some(approval) = &ui.approval {
            if approval.approved_at.is_none() {
                ui.error = Some("Approve this action before running it".into());
                cx.notify();
                return;
            }
            let Some(mut request) = ui.pending_request.clone() else {
                return;
            };
            let values = ui
                .inputs
                .iter()
                .map(|input| input.read(cx).text().to_owned())
                .collect::<Vec<_>>();
            let scoped_context = contribution.operation.as_ref().map(|operation| {
                scoped_extension_context(&operation.required_context, context.clone())
            });
            if form_arguments(&ui.fields, &values).ok().as_ref() != Some(&request.arguments)
                || request.context != scoped_context
            {
                ui.error = Some("Inputs changed after approval; select the action again".into());
                cx.notify();
                return;
            }
            request.approval_id = Some(approval.id.clone());
            request
        } else {
            let values = ui
                .inputs
                .iter()
                .map(|input| input.read(cx).text().to_owned())
                .collect::<Vec<_>>();
            let arguments = match form_arguments(&ui.fields, &values) {
                Ok(arguments) => arguments,
                Err(error) => {
                    ui.error = Some(error);
                    cx.notify();
                    return;
                }
            };
            match invocation(extension, contribution, arguments, context) {
                Ok(request) => request,
                Err(error) => {
                    ui.error = Some(error);
                    cx.notify();
                    return;
                }
            }
        };
        ui.pending_request = Some(request.clone());
        ui.approval_review = None;
        ui.approval_confirmation = None;
        ui.pending = true;
        ui.error = None;
        let generation = ui.generation;
        if self.executor_sender.as_ref().is_none_or(|sender| {
            sender
                .send(ExecutorCommand::InvokeExtensionContribution {
                    generation,
                    instance_id: self
                        .selected_instance_id
                        .as_deref()
                        .unwrap_or("local")
                        .to_owned(),
                    request,
                })
                .is_err()
        }) {
            ui.pending = false;
            ui.error = Some("Extension service is unavailable".into());
        }
        cx.notify();
    }

    pub(super) fn current_extension_context(
        &self,
        cx: &Context<Self>,
    ) -> sift_protocol::ToolContext {
        let document = self.panes.get(self.active_pane).and_then(|pane| {
            let pane = pane.read(cx);
            match pane.active_item()?.source.as_ref()? {
                ItemSource::RoomDocument(source) => Some((source.room_id, source.document_id)),
                _ => None,
            }
        });
        let room_id = document
            .map(|(room, _)| room)
            .or_else(|| self.selected_workspace().map(|workspace| workspace.room_id));
        let tenant_id = room_id
            .and_then(|id| {
                self.lifecycle
                    .tenants
                    .iter()
                    .flat_map(|tenant| &tenant.rooms)
                    .find(|room| room.id.0 == id)
                    .map(|room| room.tenant_id.0)
            })
            .or_else(|| self.selected_tenant_id());
        sift_protocol::ToolContext {
            tenant_id,
            room_id,
            profile_id: match self.connection_status {
                ConnectionStatus::Connected { profile_id, .. } => Some(profile_id),
                _ => None,
            },
            connection_id: matches!(self.connection_status, ConnectionStatus::Connected { .. })
                .then(|| "active".to_owned()),
            document_id: document.map(|(_, id)| id.to_string()),
        }
    }

    fn approve_selected_extension_contribution(&mut self, cx: &mut Context<Self>) {
        let ui = &mut self.extension_contributions;
        let Some(approval) = ui.approval.as_ref() else {
            return;
        };
        if ui.pending || approval.approved_at.is_some() {
            return;
        }
        let command = ExecutorCommand::ApproveExtensionContribution {
            generation: ui.generation,
            instance_id: self
                .selected_instance_id
                .as_deref()
                .unwrap_or("local")
                .to_owned(),
            approval_id: approval.id.clone(),
            expected_revision: approval.revision,
        };
        ui.pending = true;
        ui.error = None;
        if self
            .executor_sender
            .as_ref()
            .is_none_or(|sender| sender.send(command).is_err())
        {
            ui.pending = false;
            ui.error = Some("Extension approval service is unavailable".into());
        }
        cx.notify();
    }

    pub(super) fn render_extension_contributions(
        &self,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let colors = cx.theme().colors;
        let ui = &self.extension_contributions;
        let current_context = self.current_extension_context(cx);
        let contribution_count = ui
            .descriptors
            .iter()
            .map(|extension| {
                extension
                    .contributions
                    .iter()
                    .filter(|contribution| contribution.client.is_some())
                    .count()
            })
            .sum::<usize>();
        let options = ui
            .descriptors
            .iter()
            .enumerate()
            .flat_map(|(extension_index, extension)| {
                extension.contributions.iter().enumerate().filter_map(
                    move |(contribution_index, contribution)| {
                        contribution
                            .client
                            .as_ref()
                            .map(|_| (extension_index, contribution_index, extension, contribution))
                    },
                )
            })
            .take(MAX_CONTRIBUTIONS)
            .map(
                |(extension_index, contribution_index, extension, contribution)| {
                    let selected = ui.selected == Some((extension_index, contribution_index));
                    div()
                        .id((
                            "extension-contribution",
                            extension_index * 1000 + contribution_index,
                        ))
                        .debug_selector(move || {
                            format!("extension-contribution-{extension_index}-{contribution_index}")
                        })
                        .child(
                            Button::new(
                                (
                                    "select-extension-contribution",
                                    extension_index * 1000 + contribution_index,
                                ),
                                format!("{} · {}", extension.name, contribution.display_name),
                            )
                            .tone(if selected {
                                ButtonTone::Accent
                            } else {
                                ButtonTone::Ghost
                            })
                            .disabled(ui.pending)
                            .on_click(cx.listener(
                                move |shell, _, _, cx| {
                                    shell.select_extension_contribution(
                                        extension_index,
                                        contribution_index,
                                        cx,
                                    )
                                },
                            )),
                        )
                },
            )
            .collect::<Vec<_>>();
        let selected = ui
            .selected
            .and_then(|(extension_index, contribution_index)| {
                ui.descriptors.get(extension_index).and_then(|extension| {
                    extension
                        .contributions
                        .get(contribution_index)
                        .map(|contribution| (extension, contribution))
                })
            });
        let fields = ui
            .fields
            .iter()
            .zip(&ui.inputs)
            .enumerate()
            .map(|(index, (field, input))| {
                div()
                    .id(("extension-field", index))
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(div().text_sm().child(format!(
                        "{}{}",
                        field.label,
                        if field.required { " *" } else { "" }
                    )))
                    .child(input.clone())
            })
            .collect::<Vec<_>>();
        let result = ui.result.as_ref().map(|result| {
            let projection = selected.and_then(|(_, contribution)| contribution.result.as_ref());
            let (columns, rows) = result_rows_for_descriptor(result, projection);
            if matches!(
                projection,
                Some(sift_protocol::ClientContributionDescriptor::DetailPanel { .. })
            ) {
                let cells = rows.into_iter().next().unwrap_or_default();
                return div()
                    .id("extension-result-detail")
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(div().text_sm().child("Details"))
                    .children(columns.into_iter().zip(cells).enumerate().map(
                        |(index, (label, cell))| {
                            div()
                                .id(("extension-detail-field", index))
                                .flex()
                                .gap_2()
                                .child(
                                    div()
                                        .w(px(160.))
                                        .font_weight(gpui::FontWeight::SEMIBOLD)
                                        .child(label),
                                )
                                .child(div().text_xs().child(cell))
                        },
                    ))
                    .into_any_element();
            }
            let header = div()
                .flex()
                .gap_2()
                .children(columns.into_iter().map(|column| {
                    div()
                        .w(px(160.))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .child(column)
                }));
            div()
                .id("extension-result")
                .flex()
                .flex_col()
                .gap_1()
                .child(div().text_sm().child("Result"))
                .child(header)
                .children(rows.into_iter().enumerate().map(|(index, cells)| {
                    div()
                        .id(("extension-result-row", index))
                        .flex()
                        .gap_2()
                        .children(
                            cells
                                .into_iter()
                                .map(|cell| div().w(px(160.)).text_xs().child(cell)),
                        )
                }))
                .into_any_element()
        });
        div()
            .h(px(560.))
            .flex()
            .flex_col()
            .gap_3()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .text_lg()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child("Extensions"),
                    )
                    .child(div().flex().gap_2()
                        .child(Button::new("browse-governed-tools", "Governed tools")
                            .tone(ButtonTone::Neutral)
                            .on_click(cx.listener(|shell, _, _, cx| shell.open_governed_tools(cx))))
                        .child(Button::new("refresh-extension-contributions", "Refresh")
                            .tone(ButtonTone::Ghost)
                            .disabled(ui.loading || ui.pending)
                            .on_click(cx.listener(|shell, _, _, cx| {
                                shell.open_extension_contributions(cx)
                            })))),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(colors.muted_text)
                    .child("Extension actions use the server's audited operation path."),
            )
            .child(div().text_xs().text_color(colors.muted_text).child(
                "Vim: j/k select · Enter run · i edit fields · p review approval · v type phrase · c create request · a approve · g governed tools · r refresh · Esc close",
            ))
            .child(
                div()
                    .id("extension-contribution-list")
                    .flex()
                    .flex_wrap()
                    .gap_1()
                    .children(options),
            )
            .children((ui.loading).then(|| div().text_sm().child("Loading extensions…")))
            .children((contribution_count > MAX_CONTRIBUTIONS).then(|| {
                div().text_xs().text_color(colors.warning).child(format!(
                    "Showing first {MAX_CONTRIBUTIONS} of {contribution_count} contributions"
                ))
            }))
            .children(
                (!ui.loading && ui.descriptors.is_empty())
                    .then(|| div().text_sm().child("No extensions installed")),
            )
            .children(selected.map(|(_, contribution)| {
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(div().text_sm().child(contribution.display_name.clone()))
                    .children(contribution.operation.as_ref().and_then(|operation| {
                        extension_target_label(&operation.required_context, &current_context)
                            .map(|label| div().text_xs().text_color(colors.muted_text).child(label))
                    }))
                    .children(fields)
                    .children(contribution.operation.as_ref().filter(|operation| {
                        contribution.invocable
                            && sift_protocol::classification_requires_approval(operation.classification)
                    }).map(|_| {
                        Button::new("preview-extension-approval", "Review approval request")
                            .tone(ButtonTone::Neutral)
                            .disabled(ui.pending || ui.approval.is_some())
                            .on_click(cx.listener(|shell, _, _, cx| shell.preview_selected_extension_approval(cx)))
                    }))
                    .children(contribution.invocable.then(|| {
                        Button::new(
                            "invoke-extension-contribution",
                            if contribution.source_contribution_id.is_some() {
                                "Refresh panel"
                            } else if ui
                                .approval
                                .as_ref()
                                .is_some_and(|approval| approval.approved_at.is_some())
                            {
                                "Run approved action"
                            } else {
                                "Run action"
                            },
                        )
                        .tone(ButtonTone::Accent)
                        .loading(ui.pending)
                        .disabled(
                            ui.approval
                                .as_ref()
                                .is_some_and(|approval| approval.approved_at.is_none())
                                || ui.error.as_deref().is_some_and(|error| {
                                    error.starts_with("Field ") || error.starts_with("Form ")
                                }),
                        )
                        .on_click(cx.listener(|shell, _, _, cx| {
                            shell.invoke_selected_extension_contribution(cx)
                        }))
                    }))
                    .children(
                        (!contribution.invocable)
                            .then(|| div().text_xs().child("Action unavailable")),
                    )
            }))
            .children(ui.approval_review.as_ref().map(|review| {
                div().flex().flex_col().gap_2()
                    .child(div().text_sm().font_weight(gpui::FontWeight::SEMIBOLD).child("Approval request review"))
                    .child(div().text_xs().child(format!("Action: {}#{} · {:?}", review.invocation.operation.contribution_id, review.invocation.operation.action, review.invocation.operation.classification)))
                    .child(div().text_xs().child(format!("Target: {}:{} · server {} · current signed-in principal", review.invocation.operation.target_kind, review.invocation.operation.target_id.as_deref().unwrap_or(&review.instance_id), review.instance_id)))
                    .child(div().text_xs().font_family("monospace").child(format!("Input SHA-256: {}", review.input_fingerprint)))
                    .child(div().text_xs().text_color(colors.warning).child(format!("Type {} to create a one-use request (expires after 5 minutes)", review.confirmation)))
                    .children(ui.approval_confirmation.clone())
                    .child(Button::new("create-extension-approval", "Create approval request")
                        .tone(ButtonTone::Accent)
                        .disabled(ui.pending)
                        .on_click(cx.listener(|shell, _, _, cx| shell.create_selected_extension_approval(cx))))
            }))
            .children(ui.approval.as_ref().map(|approval| {
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(div().text_sm().text_color(colors.warning).child(
                        if approval.approved_at.is_some() {
                            "Approved. Review inputs, then run the action again."
                        } else {
                            "Approval required. Review the inputs before approving."
                        },
                    ))
                    .children(approval.approved_at.is_none().then(|| {
                        Button::new("approve-extension-contribution", "Approve")
                            .tone(ButtonTone::Neutral)
                            .loading(ui.pending)
                            .on_click(cx.listener(|shell, _, _, cx| {
                                shell.approve_selected_extension_contribution(cx)
                            }))
                    }))
            }))
            .children(
                ui.error
                    .clone()
                    .map(|error| div().text_sm().text_color(colors.danger).child(error)),
            )
            .child(
                div()
                    .id("extension-contribution-result-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_x_scroll()
                    .overflow_y_scroll()
                    .children(result),
            )
            .into_any_element()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum FieldKind {
    Text,
    Integer,
    Number,
    Boolean,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct FormField {
    pub key: String,
    pub label: String,
    pub kind: FieldKind,
    pub required: bool,
    pub masked: bool,
}

pub(super) fn form_fields(schema: &serde_json::Value) -> Result<Vec<FormField>, String> {
    let object = schema.as_object().ok_or("Form schema must be an object")?;
    if object.get("type").and_then(serde_json::Value::as_str) != Some("object") {
        return Err("Form schema must declare an object".into());
    }
    let Some(properties) = object
        .get("properties")
        .and_then(serde_json::Value::as_object)
    else {
        return Ok(Vec::new());
    };
    if properties.len() > MAX_FIELDS {
        return Err("Form has too many fields for the host renderer".into());
    }
    let required = object
        .get("required")
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut fields = Vec::with_capacity(properties.len());
    for (key, definition) in properties {
        if key.len() > 128 || key.is_empty() {
            return Err("Form contains an invalid field name".into());
        }
        let kind = match definition.get("type").and_then(serde_json::Value::as_str) {
            Some("string") => FieldKind::Text,
            Some("integer") => FieldKind::Integer,
            Some("number") => FieldKind::Number,
            Some("boolean") => FieldKind::Boolean,
            _ => return Err(format!("Field {key} has an unsupported type")),
        };
        let label = definition
            .get("title")
            .and_then(serde_json::Value::as_str)
            .filter(|title| !title.is_empty())
            .unwrap_or(key);
        fields.push(FormField {
            key: key.clone(),
            label: label.chars().take(128).collect(),
            kind,
            required: required.iter().any(|item| item.as_str() == Some(key)),
            masked: definition.get("format").and_then(serde_json::Value::as_str)
                == Some("password"),
        });
    }
    if required.iter().any(|item| {
        item.as_str()
            .is_none_or(|name| !properties.contains_key(name))
    }) {
        return Err("Form requires an unknown field".into());
    }
    Ok(fields)
}

pub(super) fn form_arguments(
    fields: &[FormField],
    values: &[String],
) -> Result<serde_json::Value, String> {
    if fields.len() != values.len() {
        return Err("Form changed while editing".into());
    }
    let mut object = serde_json::Map::new();
    for (field, raw) in fields.iter().zip(values) {
        let value = raw.trim();
        if value.is_empty() {
            if field.required {
                return Err(format!("{} is required", field.label));
            }
            continue;
        }
        let parsed = match field.kind {
            FieldKind::Text => serde_json::Value::String(raw.clone()),
            FieldKind::Integer => value
                .parse::<i64>()
                .map(serde_json::Value::from)
                .map_err(|_| format!("{} must be an integer", field.label))?,
            FieldKind::Number => value
                .parse::<f64>()
                .ok()
                .and_then(serde_json::Number::from_f64)
                .map(serde_json::Value::Number)
                .ok_or_else(|| format!("{} must be a finite number", field.label))?,
            FieldKind::Boolean => value
                .parse::<bool>()
                .map(serde_json::Value::from)
                .map_err(|_| format!("{} must be true or false", field.label))?,
        };
        object.insert(field.key.clone(), parsed);
    }
    Ok(serde_json::Value::Object(object))
}

pub(super) fn result_rows(result: &serde_json::Value) -> (Vec<String>, Vec<Vec<String>>) {
    let rows = match result {
        serde_json::Value::Array(rows) => rows.iter().take(MAX_ROWS).collect::<Vec<_>>(),
        value => vec![value],
    };
    let columns = rows
        .iter()
        .filter_map(|row| row.as_object())
        .flat_map(|object| object.keys())
        .take(MAX_FIELDS)
        .cloned()
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .take(MAX_FIELDS)
        .collect::<Vec<_>>();
    if columns.is_empty() {
        return (
            vec!["Value".into()],
            rows.into_iter().map(|row| vec![cell_text(row)]).collect(),
        );
    }
    let values = rows
        .into_iter()
        .map(|row| {
            columns
                .iter()
                .map(|key| row.get(key).map_or_else(String::new, cell_text))
                .collect()
        })
        .collect();
    (columns, values)
}

pub(super) fn result_rows_for_descriptor(
    result: &serde_json::Value,
    descriptor: Option<&sift_protocol::ClientContributionDescriptor>,
) -> (Vec<String>, Vec<Vec<String>>) {
    let fields = match descriptor {
        Some(sift_protocol::ClientContributionDescriptor::DetailPanel { fields, .. }) => fields,
        Some(sift_protocol::ClientContributionDescriptor::Table { columns, .. }) => columns,
        _ => return result_rows(result),
    };
    let columns = fields
        .iter()
        .take(MAX_FIELDS)
        .map(|field| field.label.clone())
        .collect();
    let values = match result {
        serde_json::Value::Array(values) => values.iter().take(MAX_ROWS).collect::<Vec<_>>(),
        value => vec![value],
    };
    let rows = values
        .into_iter()
        .map(|value| {
            fields
                .iter()
                .take(MAX_FIELDS)
                .map(|field| value.get(&field.key).map_or_else(String::new, cell_text))
                .collect()
        })
        .collect();
    (columns, rows)
}

fn cell_text(value: &serde_json::Value) -> String {
    let text = value
        .as_str()
        .map_or_else(|| value.to_string(), str::to_owned);
    text.chars().take(MAX_CELL_CHARS).collect()
}

pub(super) fn invocation(
    extension: &sift_protocol::ExtensionDescriptor,
    contribution: &sift_protocol::ContributionDescriptor,
    arguments: serde_json::Value,
    context: sift_protocol::ToolContext,
) -> Result<sift_protocol::InvokeExtensionRequest, String> {
    if !extension.enabled || !contribution.active || !contribution.invocable {
        return Err("Extension action is unavailable".into());
    }
    let operation = contribution
        .operation
        .as_ref()
        .ok_or("Extension action has no operation descriptor")?;
    let Some(client) = &contribution.client else {
        return Err("Extension contribution has no trusted client descriptor".into());
    };
    if !matches!(
        client,
        sift_protocol::ClientContributionDescriptor::Command { .. }
            | sift_protocol::ClientContributionDescriptor::ContextAction { .. }
            | sift_protocol::ClientContributionDescriptor::Form { .. }
            | sift_protocol::ClientContributionDescriptor::Table { .. }
            | sift_protocol::ClientContributionDescriptor::DetailPanel { .. }
    ) {
        return Err("Extension contribution is read only".into());
    }
    if matches!(
        client,
        sift_protocol::ClientContributionDescriptor::Table { .. }
            | sift_protocol::ClientContributionDescriptor::DetailPanel { .. }
    ) && contribution.source_contribution_id.is_none()
    {
        return Err("Panel has no governed data source".into());
    }
    let required = &operation.required_context;
    let context = scoped_extension_context(required, context);
    let missing = required.iter().find(|scope| match scope {
        sift_protocol::ContributionContext::Instance => false,
        sift_protocol::ContributionContext::Tenant => context.tenant_id.is_none(),
        sift_protocol::ContributionContext::Room => context.room_id.is_none(),
        sift_protocol::ContributionContext::Profile => context.profile_id.is_none(),
        sift_protocol::ContributionContext::Connection => context.connection_id.is_none(),
        sift_protocol::ContributionContext::Document => context.document_id.is_none(),
    });
    if let Some(scope) = missing {
        return Err(format!(
            "Select a {:?} target before running this action",
            scope
        ));
    }
    let target = required.last().copied();
    let target_kind = match target {
        None | Some(sift_protocol::ContributionContext::Instance) => "instance",
        Some(sift_protocol::ContributionContext::Tenant) => "tenant",
        Some(sift_protocol::ContributionContext::Room) => "room",
        Some(sift_protocol::ContributionContext::Profile) => "profile",
        Some(sift_protocol::ContributionContext::Connection) => "connection",
        Some(sift_protocol::ContributionContext::Document) => "document",
    };
    let target_id = match target {
        None | Some(sift_protocol::ContributionContext::Instance) => None,
        Some(sift_protocol::ContributionContext::Tenant) => {
            context.tenant_id.map(|id| id.to_string())
        }
        Some(sift_protocol::ContributionContext::Room) => context.room_id.map(|id| id.to_string()),
        Some(sift_protocol::ContributionContext::Profile) => {
            context.profile_id.map(|id| id.to_string())
        }
        Some(sift_protocol::ContributionContext::Connection) => context.connection_id.clone(),
        Some(sift_protocol::ContributionContext::Document) => context.document_id.clone(),
    };
    Ok(sift_protocol::InvokeExtensionRequest {
        operation: sift_protocol::ExtensionOperation {
            extension_id: extension.id.clone(),
            contribution_id: contribution
                .source_contribution_id
                .clone()
                .unwrap_or_else(|| contribution.id.clone()),
            action: operation.action.clone(),
            classification: operation.classification,
            target_kind: sift_protocol::SegmentId::new(target_kind.to_owned())
                .expect("static target kind is valid"),
            target_id,
            sanitized_arguments: std::collections::BTreeMap::new(),
        },
        arguments,
        context: Some(context),
        approval_id: None,
    })
}

fn scoped_extension_context(
    required: &[sift_protocol::ContributionContext],
    context: sift_protocol::ToolContext,
) -> sift_protocol::ToolContext {
    let has = |scope| required.contains(&scope);
    sift_protocol::ToolContext {
        tenant_id: has(sift_protocol::ContributionContext::Tenant)
            .then_some(context.tenant_id)
            .flatten(),
        room_id: has(sift_protocol::ContributionContext::Room)
            .then_some(context.room_id)
            .flatten(),
        profile_id: has(sift_protocol::ContributionContext::Profile)
            .then_some(context.profile_id)
            .flatten(),
        connection_id: has(sift_protocol::ContributionContext::Connection)
            .then_some(context.connection_id)
            .flatten(),
        document_id: has(sift_protocol::ContributionContext::Document)
            .then_some(context.document_id)
            .flatten(),
    }
}

fn extension_target_label(
    required: &[sift_protocol::ContributionContext],
    context: &sift_protocol::ToolContext,
) -> Option<String> {
    use sift_protocol::ContributionContext as Scope;
    match required.last()? {
        Scope::Instance => Some("Target: this instance".into()),
        Scope::Tenant => Some(format!(
            "Target: tenant {}",
            context
                .tenant_id
                .map_or_else(|| "none selected".into(), |id| id.to_string())
        )),
        Scope::Room => Some(format!(
            "Target: room {}",
            context
                .room_id
                .map_or_else(|| "none selected".into(), |id| id.to_string())
        )),
        Scope::Profile => Some(format!(
            "Target: profile {}",
            context
                .profile_id
                .map_or_else(|| "none selected".into(), |id| id.to_string())
        )),
        Scope::Connection => Some(if context.connection_id.is_some() {
            "Target: active connection".into()
        } else {
            "Target: no active connection".into()
        }),
        Scope::Document => Some(format!(
            "Target: document {}",
            context.document_id.as_deref().unwrap_or("none selected")
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn form_rejects_unsupported_and_coerces_supported_types() {
        let schema = serde_json::json!({
            "type": "object",
            "required": ["count"],
            "properties": {
                "count": {"type": "integer"},
                "enabled": {"type": "boolean"},
                "name": {"type": "string"}
            }
        });
        let fields = form_fields(&schema).unwrap();
        assert!(form_arguments(&fields, &["", "true", "hi"].map(str::to_owned)).is_err());
        assert_eq!(
            form_arguments(&fields, &["3", "true", "hi"].map(str::to_owned)).unwrap(),
            serde_json::json!({"count": 3, "enabled": true, "name": "hi"})
        );
        assert!(
            form_fields(&serde_json::json!({"type":"object", "properties": {
                "nested": {"type":"object"}
            }}))
            .is_err()
        );
    }

    #[test]
    fn result_projection_is_bounded_and_read_only() {
        let result = serde_json::Value::Array(
            (0..101)
                .map(|_| serde_json::json!({"a": "x".repeat(300), "b": 2}))
                .collect(),
        );
        let (columns, rows) = result_rows(&result);
        assert_eq!(columns, ["a", "b"]);
        assert_eq!(rows.len(), 100);
        assert_eq!(rows[0][0].chars().count(), 256);
        let descriptor = sift_protocol::ClientContributionDescriptor::Table {
            title: "Usage".into(),
            columns: vec![sift_protocol::ClientFieldDescriptor {
                key: "a".into(),
                label: "First".into(),
            }],
        };
        let (columns, rows) = result_rows_for_descriptor(&result, Some(&descriptor));
        assert_eq!(columns, ["First"]);
        assert_eq!(rows[0].len(), 1);
    }

    #[test]
    fn panel_refresh_invokes_its_governed_source() {
        let extension_id = sift_protocol::ExtensionId::new("acme/usage").unwrap();
        let source_id =
            sift_protocol::ContributionId::new("acme/usage/command/read-usage").unwrap();
        let panel = sift_protocol::ContributionDescriptor {
            id: sift_protocol::ContributionId::new("acme/usage/client_panel/usage").unwrap(),
            kind: "client_panel".into(),
            display_name: "Usage".into(),
            active: true,
            invocable: true,
            required_capabilities: Vec::new(),
            operation: Some(sift_protocol::ExtensionActionDescriptor {
                action: sift_protocol::SegmentId::new("read-usage").unwrap(),
                classification: sift_protocol::OperationClassification::Read,
                required_context: Vec::new(),
                input_schema: serde_json::json!({"type":"object"}),
                output_schema: serde_json::json!({"type":"object", "properties": {
                    "count": {"type":"integer"}
                }}),
                timeout_ms: 1000,
                max_result_bytes: 1024,
            }),
            client: Some(sift_protocol::ClientContributionDescriptor::DetailPanel {
                title: "Usage".into(),
                fields: vec![sift_protocol::ClientFieldDescriptor {
                    key: "count".into(),
                    label: "Count".into(),
                }],
            }),
            result: None,
            source_contribution_id: Some(source_id.clone()),
        };
        let extension = sift_protocol::ExtensionDescriptor {
            id: extension_id,
            name: "Usage".into(),
            version: "1.0.0".into(),
            archive_sha256: String::new(),
            manifest_sha256: String::new(),
            provenance: sift_protocol::ExtensionProvenance::Local,
            lifecycle: sift_protocol::ExtensionLifecycleState::Ready,
            isolation: sift_protocol::ExtensionIsolation::ProcessOnly,
            enabled: true,
            revision: 1,
            contributions: vec![panel.clone()],
        };
        let request = invocation(
            &extension,
            &panel,
            serde_json::json!({}),
            sift_protocol::ToolContext {
                tenant_id: None,
                room_id: None,
                profile_id: None,
                connection_id: None,
                document_id: None,
            },
        )
        .unwrap();
        assert_eq!(request.operation.contribution_id, source_id);
        let mut tenant_action = panel;
        tenant_action.operation.as_mut().unwrap().required_context =
            vec![sift_protocol::ContributionContext::Tenant];
        tenant_action.client = Some(sift_protocol::ClientContributionDescriptor::ContextAction {
            title: "Tenant usage".into(),
            action: sift_protocol::SegmentId::new("read-usage").unwrap(),
            target_kind: sift_protocol::SegmentId::new("tenant").unwrap(),
        });
        let context = sift_protocol::ToolContext {
            tenant_id: Some(42),
            room_id: None,
            profile_id: None,
            connection_id: None,
            document_id: None,
        };
        let request =
            invocation(&extension, &tenant_action, serde_json::json!({}), context).unwrap();
        assert_eq!(request.operation.target_id.as_deref(), Some("42"));
        assert_eq!(request.context.unwrap().tenant_id, Some(42));
        assert_eq!(request.arguments, serde_json::json!({}));
        assert!(request.operation.sanitized_arguments.is_empty());
    }
}
