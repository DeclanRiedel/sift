use std::{
    collections::{BTreeMap, HashMap},
    sync::{atomic::AtomicU64, atomic::Ordering, Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use arc_swap::ArcSwap;
use jsonschema::Validator;
use sift_extension_protocol::{
    ContributionId, ExtensionId, InvokeActionRequest, InvokeActionResponse,
    OperationClassification, Request, RequestContext, ResponseResult, SegmentId, WireId,
};
use sift_metadata::{MetadataStore, NewOperationAudit, PrincipalId};
use sift_plugin_host::SupervisedProcess;
use sift_protocol::{ExtensionOperation, InvokeExtensionResponse};

use crate::authorization::{authorize_extension, AuthorizationDenial, AuthorizationScope};

const MAX_ARGUMENT_BYTES: usize = 1024 * 1024;
const MAX_ACTION_TIMEOUT: Duration = Duration::from_secs(5 * 60);

pub struct ActionRegistration {
    pub extension_id: ExtensionId,
    pub contribution_id: ContributionId,
    pub action: SegmentId,
    pub classification: OperationClassification,
    pub input_schema: serde_json::Value,
    pub output_schema: serde_json::Value,
    pub timeout: Duration,
    pub max_result_bytes: u64,
    pub process: Arc<SupervisedProcess>,
}

struct CompiledAction {
    registration: ActionRegistration,
    input: Validator,
    output: Validator,
}

#[derive(Clone)]
pub struct ExtensionOperationDispatcher {
    actions: Arc<ArcSwap<HashMap<String, Arc<CompiledAction>>>>,
    mutation: Arc<Mutex<()>>,
    metadata: Arc<MetadataStore>,
    correlation_counter: Arc<AtomicU64>,
}

pub struct DispatchContext {
    pub authorization: AuthorizationScope,
    pub principal_id: PrincipalId,
    pub tenant_id: Option<i64>,
    pub room_id: Option<i64>,
    pub correlation_id: String,
}

#[derive(Debug, thiserror::Error)]
pub enum ExtensionDispatchError {
    #[error("extension action is not registered")]
    NotFound,
    #[error("extension operation descriptor does not match the registered action")]
    DescriptorMismatch,
    #[error("{0}")]
    Denied(#[from] AuthorizationDenialDisplay),
    #[error("extension arguments exceed the byte limit")]
    ArgumentsTooLarge,
    #[error("extension arguments do not match the declared schema")]
    InvalidArguments,
    #[error("extension result does not match the declared schema")]
    InvalidResult,
    #[error("extension result exceeds the declared byte limit")]
    ResultTooLarge,
    #[error("extension operation failed")]
    InvocationFailed,
    #[error("operation audit failed: {0}")]
    Audit(#[from] sift_metadata::MetadataError),
    #[error("invalid extension action registration: {0}")]
    InvalidRegistration(String),
}

#[derive(Debug)]
pub struct AuthorizationDenialDisplay(AuthorizationDenial);

impl std::fmt::Display for AuthorizationDenialDisplay {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.0.public_reason())
    }
}

impl std::error::Error for AuthorizationDenialDisplay {}

impl ExtensionOperationDispatcher {
    pub fn new(metadata: Arc<MetadataStore>) -> Self {
        Self {
            actions: Arc::new(ArcSwap::from_pointee(HashMap::new())),
            mutation: Arc::new(Mutex::new(())),
            metadata,
            correlation_counter: Arc::new(AtomicU64::new(1)),
        }
    }

    pub fn replace(
        &self,
        registrations: impl IntoIterator<Item = ActionRegistration>,
    ) -> Result<(), ExtensionDispatchError> {
        let mut compiled = HashMap::new();
        for registration in registrations {
            validate_registration(&registration)?;
            let key = action_key(&registration.contribution_id, &registration.action);
            if compiled.contains_key(&key) {
                return Err(ExtensionDispatchError::InvalidRegistration(format!(
                    "duplicate action `{key}`"
                )));
            }
            reject_external_references(&registration.input_schema)?;
            reject_external_references(&registration.output_schema)?;
            let input = jsonschema::draft202012::new(&registration.input_schema)
                .map_err(|error| ExtensionDispatchError::InvalidRegistration(error.to_string()))?;
            let output = jsonschema::draft202012::new(&registration.output_schema)
                .map_err(|error| ExtensionDispatchError::InvalidRegistration(error.to_string()))?;
            compiled.insert(
                key,
                Arc::new(CompiledAction {
                    registration,
                    input,
                    output,
                }),
            );
        }
        let _guard = self.mutation.lock().expect("action mutation lock poisoned");
        self.actions.store(Arc::new(compiled));
        Ok(())
    }

    pub async fn dispatch(
        &self,
        mut operation: ExtensionOperation,
        arguments: serde_json::Value,
        context: DispatchContext,
    ) -> Result<(ExtensionOperation, InvokeExtensionResponse), ExtensionDispatchError> {
        let key = action_key(&operation.contribution_id, &operation.action);
        let action = self
            .actions
            .load()
            .get(&key)
            .cloned()
            .ok_or(ExtensionDispatchError::NotFound)?;
        if operation.extension_id != action.registration.extension_id
            || operation.contribution_id != action.registration.contribution_id
            || operation.action != action.registration.action
            || operation.classification != action.registration.classification
        {
            return Err(ExtensionDispatchError::DescriptorMismatch);
        }
        authorize_extension(&context.authorization, action.registration.classification)
            .map_err(|denial| ExtensionDispatchError::Denied(AuthorizationDenialDisplay(denial)))?;
        let encoded_arguments =
            serde_json::to_vec(&arguments).map_err(|_| ExtensionDispatchError::InvalidArguments)?;
        if encoded_arguments.len() > MAX_ARGUMENT_BYTES {
            return Err(ExtensionDispatchError::ArgumentsTooLarge);
        }
        if !action.input.is_valid(&arguments) {
            return Err(ExtensionDispatchError::InvalidArguments);
        }

        operation.sanitized_arguments =
            audit_safe_projection(&action.registration.input_schema, &arguments);
        let audit = self.metadata.record_operation_audit(NewOperationAudit {
            actor_principal_id: Some(context.principal_id),
            action: operation.action.as_str().into(),
            target: operation.contribution_id.as_str().into(),
            target_id: None,
            status: "started".into(),
            result_code: None,
            row_count: None,
            error_message: None,
            correlation_id: Some(context.correlation_id.clone()),
        })?;
        let result = self.invoke(&action, &operation, arguments, &context).await;
        match result {
            Ok(result) => {
                self.metadata
                    .finish_operation_audit(audit.id, "succeeded", None, None)?;
                Ok((operation, InvokeExtensionResponse { result }))
            }
            Err(error) => {
                self.metadata.finish_operation_audit(
                    audit.id,
                    "failed",
                    Some(error.code()),
                    Some("extension operation failed"),
                )?;
                Err(error)
            }
        }
    }

    async fn invoke(
        &self,
        action: &CompiledAction,
        operation: &ExtensionOperation,
        arguments: serde_json::Value,
        context: &DispatchContext,
    ) -> Result<serde_json::Value, ExtensionDispatchError> {
        let request = Request {
            id: WireId::from_u128(0),
            contribution_id: action.registration.contribution_id.clone(),
            method: "invoke".into(),
            payload: serde_json::to_value(InvokeActionRequest {
                action: action.registration.action.clone(),
                target_kind: operation.target_kind.clone(),
                target_id: operation.target_id.clone(),
                arguments,
            })
            .map_err(|_| ExtensionDispatchError::InvalidArguments)?,
            correlation_id: WireId::from_u128(u128::from(
                self.correlation_counter.fetch_add(1, Ordering::Relaxed),
            )),
            deadline_unix_ms: deadline_unix_ms(action.registration.timeout),
            context: Some(RequestContext {
                tenant_id: context.tenant_id,
                room_id: context.room_id,
            }),
            stream_id: None,
        };
        let response = action
            .registration
            .process
            .request(request)
            .await
            .map_err(|_| ExtensionDispatchError::InvocationFailed)?;
        let payload = match response.result {
            ResponseResult::Ok { payload } => payload,
            _ => return Err(ExtensionDispatchError::InvocationFailed),
        };
        let response: InvokeActionResponse =
            serde_json::from_value(payload).map_err(|_| ExtensionDispatchError::InvalidResult)?;
        let bytes = serde_json::to_vec(&response.result)
            .map_err(|_| ExtensionDispatchError::InvalidResult)?;
        if bytes.len() as u64 > action.registration.max_result_bytes {
            return Err(ExtensionDispatchError::ResultTooLarge);
        }
        if !action.output.is_valid(&response.result) {
            return Err(ExtensionDispatchError::InvalidResult);
        }
        Ok(response.result)
    }
}

impl ExtensionDispatchError {
    fn code(&self) -> &'static str {
        match self {
            Self::NotFound => "not_found",
            Self::DescriptorMismatch | Self::InvalidArguments | Self::InvalidRegistration(_) => {
                "invalid_parameter_value"
            }
            Self::Denied(_) => "permission_denied",
            Self::ArgumentsTooLarge | Self::ResultTooLarge => "result_too_large",
            Self::InvalidResult | Self::InvocationFailed => "driver_internal",
            Self::Audit(_) => "audit_failed",
        }
    }
}

fn validate_registration(registration: &ActionRegistration) -> Result<(), ExtensionDispatchError> {
    let prefix = format!("{}/", registration.extension_id);
    if !registration.contribution_id.as_str().starts_with(&prefix) {
        return Err(ExtensionDispatchError::InvalidRegistration(
            "contribution is outside the extension namespace".into(),
        ));
    }
    if registration.timeout.is_zero() || registration.timeout > MAX_ACTION_TIMEOUT {
        return Err(ExtensionDispatchError::InvalidRegistration(
            "action timeout is outside host limits".into(),
        ));
    }
    if registration.max_result_bytes == 0 {
        return Err(ExtensionDispatchError::InvalidRegistration(
            "action result limit must be positive".into(),
        ));
    }
    Ok(())
}

fn reject_external_references(schema: &serde_json::Value) -> Result<(), ExtensionDispatchError> {
    match schema {
        serde_json::Value::Object(object) => {
            if object
                .get("$ref")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|reference| !reference.starts_with('#'))
            {
                return Err(ExtensionDispatchError::InvalidRegistration(
                    "external JSON Schema references are not allowed".into(),
                ));
            }
            for value in object.values() {
                reject_external_references(value)?;
            }
        }
        serde_json::Value::Array(values) => {
            for value in values {
                reject_external_references(value)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn audit_safe_projection(
    schema: &serde_json::Value,
    arguments: &serde_json::Value,
) -> BTreeMap<String, serde_json::Value> {
    let Some(properties) = schema
        .get("properties")
        .and_then(serde_json::Value::as_object)
    else {
        return BTreeMap::new();
    };
    let Some(arguments) = arguments.as_object() else {
        return BTreeMap::new();
    };
    properties
        .iter()
        .filter(|(_, schema)| {
            schema
                .get("x-sift-data-classification")
                .and_then(serde_json::Value::as_str)
                == Some("audit_safe")
        })
        .filter_map(|(key, _)| {
            arguments
                .get(key)
                .cloned()
                .map(|value| (key.clone(), value))
        })
        .collect()
}

fn action_key(contribution: &ContributionId, action: &SegmentId) -> String {
    format!("{contribution}#{action}")
}

fn deadline_unix_ms(timeout: Duration) -> i64 {
    let millis = SystemTime::now()
        .checked_add(timeout)
        .and_then(|deadline| deadline.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_millis())
        .unwrap_or(i64::MAX as u128);
    i64::try_from(millis).unwrap_or(i64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audit_projection_is_schema_owned_and_fail_closed() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": {
                "label": {
                    "type": "string",
                    "x-sift-data-classification": "audit_safe"
                },
                "password": {
                    "type": "string",
                    "x-sift-data-classification": "secret"
                }
            }
        });
        let projection = audit_safe_projection(
            &schema,
            &serde_json::json!({"label": "visible", "password": "hidden", "extra": true}),
        );
        assert_eq!(projection.len(), 1);
        assert_eq!(projection["label"], "visible");
    }

    #[test]
    fn remote_schema_references_are_rejected() {
        let schema = serde_json::json!({
            "allOf": [{"$ref": "https://example.invalid/schema.json"}]
        });
        assert!(reject_external_references(&schema).is_err());
        assert!(reject_external_references(&serde_json::json!({
            "$defs": {"local": {"type": "string"}},
            "$ref": "#/$defs/local"
        }))
        .is_ok());
    }
}
