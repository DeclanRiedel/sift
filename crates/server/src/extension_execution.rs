//! Retained extension RPC owner; observers never own the issued request future.
use super::*;
use sift_metadata::{ApprovalBinding, ExtensionExecutionSettlement, NewExtensionExecution};
use sift_protocol::{
    ExtensionExecutionCode as Code, ExtensionExecutionReceipt, ExtensionExecutionState as State,
    ExtensionResponseKind,
};

#[derive(Debug)]
pub struct DispatchObservation {
    pub receipt: ExtensionExecutionReceipt,
    /// Replays observe the existing owner and cannot retain or replace its output.
    pub replayed: bool,
    /// Only the first timely, currently authorized observation can receive a body.
    pub result: Option<serde_json::Value>,
}

/// Owned AI work remains registered through immutable RPC settlement.
pub(crate) struct AiExtensionDispatch {
    pub id: uuid::Uuid,
    pub fingerprint: String,
    pub deadline_unix_ms: i64,
    pub work: crate::ai_cancellation::AiWorkGuard,
    pub on_claim: Option<Box<dyn FnOnce(sift_protocol::AiExtensionExecutionSummary) + Send>>,
}

impl ExtensionOperationDispatcher {
    pub(crate) fn delivery_authorized(
        &self,
        prepared: &PreparedExtensionAction,
        context: &DispatchContext,
    ) -> bool {
        self.ensure_prepared(prepared, context).is_ok() && self.check_package(prepared).is_ok()
    }

    pub(super) fn ensure_prepared(
        &self,
        prepared: &PreparedExtensionAction,
        context: &DispatchContext,
    ) -> Result<(), ExtensionDispatchError> {
        let key = action_key(
            &prepared.operation.contribution_id,
            &prepared.operation.action,
        );
        if !self
            .actions
            .load()
            .get(&key)
            .is_some_and(|current| Arc::ptr_eq(current, &prepared.action))
            || prepared.target != context.target
            || prepared.source_fingerprint != context.source_fingerprint
        {
            return Err(ExtensionDispatchError::DescriptorMismatch);
        }
        authorize_extension(&context.authorization, prepared.operation.classification)
            .map_err(|e| ExtensionDispatchError::Denied(AuthorizationDenialDisplay(e)))?;
        Ok(())
    }

    fn check_package(&self, prepared: &PreparedExtensionAction) -> Result<(), SupervisorError> {
        let registration = &prepared.action.registration;
        if let Some(expected) = &registration.package_identity {
            let current = self
                .metadata
                .selected_extension_package(registration.extension_id.as_str())
                .map_err(|_| SupervisorError::AdmissionRefused)?;
            if !current.selection.enabled
                || current.selection.quarantine_reason.is_some()
                || *expected
                    != serde_json::json!({"archive_sha256":current.selection.selected_archive_sha256,
                    "manifest_sha256":current.manifest_sha256,"selection_revision":current.selection.revision})
            {
                return Err(SupervisorError::AdmissionRefused);
            }
            if let Some(tenant) = prepared.target.tenant_id {
                if !self
                    .metadata
                    .extension_tenant_allowed(registration.extension_id.as_str(), tenant)
                    .map_err(|_| SupervisorError::AdmissionRefused)?
                {
                    return Err(SupervisorError::AdmissionRefused);
                }
            }
        }
        Ok(())
    }

    fn validate_dispatch(
        &self,
        prepared: &PreparedExtensionAction,
        arguments: &serde_json::Value,
        context: &DispatchContext,
        binding: &ApprovalBinding,
    ) -> Result<(), ExtensionDispatchError> {
        self.ensure_prepared(prepared, context)?;
        prepared.validate_arguments(arguments)?;
        if binding.principal_id != context.principal_id
            || binding.input_fingerprint
                != crate::automation::fingerprint(arguments)
                    .map_err(|_| ExtensionDispatchError::InvalidArguments)?
        {
            return Err(ExtensionDispatchError::InvalidArguments);
        }
        Ok(())
    }

    pub async fn dispatch_reviewed(
        &self,
        prepared: PreparedExtensionAction,
        arguments: serde_json::Value,
        context: DispatchContext,
        approval_id: Option<String>,
        binding: ApprovalBinding,
        request_id: Option<uuid::Uuid>,
    ) -> Result<DispatchObservation, ExtensionDispatchError> {
        self.validate_dispatch(&prepared, &arguments, &context, &binding)?;
        let input = NewExtensionExecution {
            request_id: request_id.unwrap_or_else(uuid::Uuid::new_v4),
            binding,
            approval_id,
            operation: prepared.operation.clone(),
            context: context.target.clone(),
            deadline_unix_ms: deadline_unix_ms(prepared.action.registration.timeout),
            correlation_id: context.correlation_id.clone(),
        };
        self.dispatch_owned(prepared, arguments, context, input, None)
            .await
    }

    pub(crate) async fn dispatch_ai_reviewed(
        &self,
        prepared: PreparedExtensionAction,
        arguments: serde_json::Value,
        context: DispatchContext,
        binding: ApprovalBinding,
        ai: AiExtensionDispatch,
    ) -> Result<DispatchObservation, ExtensionDispatchError> {
        self.validate_dispatch(&prepared, &arguments, &context, &binding)?;
        let input = NewExtensionExecution {
            request_id: ai.id,
            binding,
            approval_id: None,
            operation: prepared.operation.clone(),
            context: context.target.clone(),
            deadline_unix_ms: ai
                .deadline_unix_ms
                .min(deadline_unix_ms(prepared.action.registration.timeout)),
            correlation_id: context.correlation_id.clone(),
        };
        self.dispatch_owned(prepared, arguments, context, input, Some(ai))
            .await
    }

    async fn dispatch_owned(
        &self,
        prepared: PreparedExtensionAction,
        arguments: serde_json::Value,
        context: DispatchContext,
        input: NewExtensionExecution,
        ai: Option<AiExtensionDispatch>,
    ) -> Result<DispatchObservation, ExtensionDispatchError> {
        let permit = self
            .owners
            .clone()
            .try_acquire_owned()
            .map_err(|_| ExtensionDispatchError::Capacity)?;
        let dispatcher = self.clone();
        let guard = crate::shutdown::current_query_guard();
        let correlation = context.correlation_id.clone();
        // Spawn before the first claim await. Caller loss cannot strand a successful
        // claim between SQLite returning and construction of its native owner.
        tokio::spawn(async move {
            let _permit = permit;
            crate::correlation::scope(
                correlation,
                crate::shutdown::scope_query(guard, async move {
                    dispatcher
                        .run_owned(prepared, arguments, context, input, ai)
                        .await
                }),
            )
            .await
        })
        .await
        .map_err(|_| ExtensionDispatchError::InvocationFailed)?
    }

    async fn run_owned(
        &self,
        prepared: PreparedExtensionAction,
        arguments: serde_json::Value,
        context: DispatchContext,
        input: NewExtensionExecution,
        mut ai: Option<AiExtensionDispatch>,
    ) -> Result<DispatchObservation, ExtensionDispatchError> {
        let prepared = Arc::new(prepared);
        let deadline = input.deadline_unix_ms;
        let metadata = self.metadata.clone();
        let claim = if let Some(ai) = ai.as_mut() {
            let claimed = metadata
                .claim_ai_extension_execution(
                    ai.id,
                    context.principal_id,
                    ai.fingerprint.clone(),
                    input,
                )
                .await?;
            if let Some(deliver) = ai.on_claim.take() {
                deliver(claimed.summary);
            }
            claimed.execution
        } else {
            tokio::task::spawn_blocking(move || metadata.claim_extension_execution(&input))
                .await
                .map_err(|_| ExtensionDispatchError::InvocationFailed)??
        };
        if !claim.claimed {
            return Ok(DispatchObservation {
                receipt: claim.receipt,
                replayed: true,
                result: None,
            });
        }
        let id = claim.receipt.id;
        let actor = context.principal_id;
        let ai_review = ai.as_ref().map(|ai| ai.id);
        let admission: ActionAdmission = Box::new({
            let prepared = prepared.clone();
            let context = context.clone();
            let dispatcher = self.clone();
            let guard = crate::shutdown::current_query_guard();
            move || {
                Box::pin(async move {
                    if let Some(authority) = &context.reauthorize {
                        authority(prepared.operation.classification).await?;
                    }
                    tokio::task::spawn_blocking(move || {
                        let _guard = guard;
                        dispatcher
                            .ensure_prepared(&prepared, &context)
                            .map_err(|_| SupervisorError::AdmissionRefused)?;
                        dispatcher.check_package(&prepared)?;
                        let source = context.metadata_fingerprint.as_deref();
                        let package = prepared.action.registration.package_identity.as_ref();
                        let credential = context.credential_proof.as_ref();
                        match ai_review {
                            Some(review) => {
                                dispatcher.metadata.mark_ai_extension_dispatching_guarded(
                                    id, actor, source, package, credential, review,
                                )
                            }
                            None => dispatcher.metadata.mark_extension_dispatching_guarded(
                                id, actor, source, package, credential,
                            ),
                        }
                        .map_err(|_| SupervisorError::AdmissionRefused)
                    })
                    .await
                    .map_err(|_| SupervisorError::AdmissionRefused)?
                })
            }
        });
        let request = Request {
            id: WireId::from_u128(0),
            contribution_id: prepared.operation.contribution_id.clone(),
            method: "invoke".into(),
            payload: serde_json::to_value(InvokeActionRequest {
                action: prepared.operation.action.clone(),
                target_kind: prepared.operation.target_kind.clone(),
                target_id: prepared.operation.target_id.clone(),
                arguments,
            })
            .map_err(|_| ExtensionDispatchError::InvalidArguments)?,
            correlation_id: WireId::from_u128(u128::from(
                self.correlation_counter.fetch_add(1, Ordering::Relaxed),
            )),
            deadline_unix_ms: deadline,
            context: Some(RequestContext {
                tenant_id: context.target.tenant_id,
                room_id: context.target.room_id,
            }),
            stream_id: None,
        };
        let observed = if let Some(ai) = &ai {
            let token = ai.work.token.clone();
            prepared
                .action
                .registration
                .invoker
                .request_observed_cancellable(
                    context.target.tenant_id,
                    request,
                    admission,
                    Box::pin(async move { token.cancelled().await }),
                )
                .await
        } else {
            prepared
                .action
                .registration
                .invoker
                .request_observed(context.target.tenant_id, request, admission)
                .await
        };
        let frame_started = observed.frame_started;
        let late = observed.late_response.is_some();
        let timed_out = matches!(observed.result, Err(SupervisorError::RequestTimeout));
        let cancelled = matches!(observed.result, Err(SupervisorError::RequestCancelled));
        let interrupted_code = if cancelled {
            Code::OwnerInterrupted
        } else {
            Code::RequestTimedOut
        };
        let response = match observed.result {
            Ok(response) => Some(response),
            Err(_) => observed.late_response,
        };
        let mut settlement = ExtensionExecutionSettlement {
            state: if frame_started {
                State::OutcomeUnknown
            } else {
                State::Refused
            },
            frame_started: Some(frame_started),
            response_kind: None,
            late_response: late,
            result_code: if timed_out || cancelled {
                interrupted_code
            } else if frame_started {
                Code::TransportFailed
            } else {
                Code::AdmissionRefused
            },
        };
        let mut result = None;
        if let Some(response) = response {
            match response.result {
                ResponseResult::Ok { payload } => {
                    let valid = serde_json::from_value::<InvokeActionResponse>(payload)
                        .ok()
                        .and_then(|response| {
                            let bytes = serde_json::to_vec(&response.result).ok()?;
                            (bytes.len() as u64 <= prepared.action.registration.max_result_bytes
                                && prepared.action.output.is_valid(&response.result))
                            .then_some(response.result)
                        });
                    if let Some(value) = valid {
                        settlement.state = State::Acknowledged;
                        settlement.response_kind = Some(ExtensionResponseKind::Result);
                        settlement.result_code = if late {
                            interrupted_code
                        } else {
                            Code::Acknowledged
                        };
                        if !late {
                            result = Some(value);
                        }
                    } else if !late {
                        settlement.result_code = Code::InvalidResponse;
                    }
                }
                ResponseResult::Error { .. } => {
                    settlement.state = State::Acknowledged;
                    settlement.response_kind = Some(ExtensionResponseKind::Error);
                    settlement.result_code = if late {
                        interrupted_code
                    } else {
                        Code::ReportedError
                    };
                }
                _ => {
                    if !late {
                        settlement.result_code = Code::InvalidResponse;
                    }
                }
            }
        }
        // Output delivery is separate from RPC acknowledgement. Revocation cannot
        // erase known response facts or expose a result after source loss.
        let current = {
            let dispatcher = self.clone();
            let prepared = prepared.clone();
            let context = context.clone();
            tokio::task::spawn_blocking(move || {
                dispatcher
                    .ensure_prepared(&prepared, &context)
                    .map_err(|_| SupervisorError::AdmissionRefused)?;
                dispatcher.check_package(&prepared)
            })
            .await
            .map_err(|_| ExtensionDispatchError::InvocationFailed)?
        };
        let authorized = if let Some(authority) = &context.reauthorize {
            authority(prepared.operation.classification).await.is_ok()
        } else {
            true
        };
        if current.is_err() || !authorized {
            result = None;
            if settlement.state == State::Acknowledged && !late {
                settlement.result_code = Code::DeliveryDenied;
            }
        }
        let metadata = self.metadata.clone();
        let receipt = tokio::task::spawn_blocking(move || {
            metadata.settle_extension_execution(id, actor, settlement)
        })
        .await
        .map_err(|_| ExtensionDispatchError::InvocationFailed)??;
        // Settlement can wait for storage. Recheck once more after persistence;
        // the immutable receipt records RPC evidence, never client delivery.
        if result.is_some() {
            let dispatcher = self.clone();
            let original = prepared.clone();
            let captured = context.clone();
            let current = tokio::task::spawn_blocking(move || {
                dispatcher
                    .ensure_prepared(&original, &captured)
                    .map_err(|_| SupervisorError::AdmissionRefused)?;
                dispatcher.check_package(&original)
            })
            .await
            .map_err(|_| ExtensionDispatchError::InvocationFailed)?;
            let allowed = if let Some(authority) = &context.reauthorize {
                authority(prepared.operation.classification).await.is_ok()
            } else {
                true
            };
            if current.is_err() || !allowed {
                result = None;
            }
        }
        Ok(DispatchObservation {
            receipt,
            result,
            replayed: false,
        })
    }
}
