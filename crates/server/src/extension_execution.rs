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
    /// Only the first timely, currently authorized observation can receive a body.
    pub result: Option<serde_json::Value>,
}

impl ExtensionOperationDispatcher {
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

    pub async fn dispatch_reviewed(
        &self,
        prepared: PreparedExtensionAction,
        arguments: serde_json::Value,
        context: DispatchContext,
        approval_id: Option<String>,
        binding: ApprovalBinding,
        request_id: Option<uuid::Uuid>,
    ) -> Result<DispatchObservation, ExtensionDispatchError> {
        self.ensure_prepared(&prepared, &context)?;
        prepared.validate_arguments(&arguments)?;
        if binding.principal_id != context.principal_id
            || binding.input_fingerprint
                != crate::automation::fingerprint(&arguments)
                    .map_err(|_| ExtensionDispatchError::InvalidArguments)?
        {
            return Err(ExtensionDispatchError::InvalidArguments);
        }
        let permit = self
            .owners
            .clone()
            .try_acquire_owned()
            .map_err(|_| ExtensionDispatchError::Capacity)?;
        let deadline = deadline_unix_ms(prepared.action.registration.timeout);
        let request_id = request_id.unwrap_or_else(uuid::Uuid::new_v4);
        let input = NewExtensionExecution {
            request_id,
            binding,
            approval_id,
            operation: prepared.operation.clone(),
            context: context.target.clone(),
            deadline_unix_ms: deadline,
            correlation_id: context.correlation_id.clone(),
        };
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
                        .run_owned(prepared, arguments, context, input)
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
    ) -> Result<DispatchObservation, ExtensionDispatchError> {
        let prepared = Arc::new(prepared);
        let deadline = input.deadline_unix_ms;
        let metadata = self.metadata.clone();
        let claim = tokio::task::spawn_blocking(move || metadata.claim_extension_execution(&input))
            .await
            .map_err(|_| ExtensionDispatchError::InvocationFailed)??;
        if !claim.claimed {
            return Ok(DispatchObservation {
                receipt: claim.receipt,
                result: None,
            });
        }
        let id = claim.receipt.id;
        let actor = context.principal_id;
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
                        dispatcher
                            .metadata
                            .mark_extension_dispatching_guarded(
                                id,
                                actor,
                                context.metadata_fingerprint.as_deref(),
                                prepared.action.registration.package_identity.as_ref(),
                                context.credential_proof.as_ref(),
                            )
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
        let observed = prepared
            .action
            .registration
            .invoker
            .request_observed(context.target.tenant_id, request, admission)
            .await;
        let frame_started = observed.frame_started;
        let late = observed.late_response.is_some();
        let timed_out = matches!(observed.result, Err(SupervisorError::RequestTimeout));
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
            result_code: if timed_out {
                Code::RequestTimedOut
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
                            Code::RequestTimedOut
                        } else {
                            Code::Acknowledged
                        };
                        if !late {
                            result = Some(value);
                        }
                    } else {
                        settlement.result_code = Code::InvalidResponse;
                    }
                }
                ResponseResult::Error { .. } => {
                    settlement.state = State::Acknowledged;
                    settlement.response_kind = Some(ExtensionResponseKind::Error);
                    settlement.result_code = if late {
                        Code::RequestTimedOut
                    } else {
                        Code::ReportedError
                    };
                }
                _ => {
                    settlement.result_code = Code::InvalidResponse;
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
        Ok(DispatchObservation { receipt, result })
    }
}
