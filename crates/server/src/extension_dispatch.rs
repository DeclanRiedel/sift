use std::{
    collections::{BTreeMap, HashMap},
    sync::{atomic::AtomicU64, atomic::Ordering, Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use arc_swap::ArcSwap;
use jsonschema::Validator;
use sift_extension_protocol::{
    ContributionContext, ContributionId, ExtensionId, InvokeActionRequest, InvokeActionResponse,
    OperationClassification, Request, RequestContext, ResponseResult, SegmentId, WireId,
};
use sift_metadata::{MetadataStore, NewOperationAudit, PrincipalId};
use sift_plugin_host::{SupervisedProcess, SupervisorError};
use sift_protocol::{ExtensionOperation, InvokeExtensionResponse, ToolContext};

use crate::authorization::{authorize_extension, AuthorizationDenial, AuthorizationScope};

const MAX_ARGUMENT_BYTES: usize = 1024 * 1024;
const MAX_ACTION_TIMEOUT: Duration = Duration::from_secs(5 * 60);

pub struct ActionRegistration {
    pub extension_id: ExtensionId,
    pub contribution_id: ContributionId,
    pub action: SegmentId,
    pub classification: OperationClassification,
    pub required_context: Vec<ContributionContext>,
    pub input_schema: serde_json::Value,
    pub output_schema: serde_json::Value,
    pub timeout: Duration,
    pub max_result_bytes: u64,
    /// Immutable installed package and grant revision, absent for host registrations.
    pub package_identity: Option<serde_json::Value>,
    pub invoker: Arc<dyn ActionInvoker>,
}

#[async_trait::async_trait]
pub trait ActionInvoker: Send + Sync {
    async fn request(
        &self,
        tenant_id: Option<i64>,
        request: Request,
    ) -> Result<sift_extension_protocol::Response, SupervisorError>;
}

#[async_trait::async_trait]
impl ActionInvoker for SupervisedProcess {
    async fn request(
        &self,
        _tenant_id: Option<i64>,
        request: Request,
    ) -> Result<sift_extension_protocol::Response, SupervisorError> {
        SupervisedProcess::request(self, request).await
    }
}

pub struct PreparedExtensionAction {
    action: Arc<CompiledAction>,
    operation: ExtensionOperation,
    target: ToolContext,
    contract: serde_json::Value,
}

impl PreparedExtensionAction {
    pub fn operation(&self) -> &ExtensionOperation {
        &self.operation
    }

    pub fn review_context(&self) -> serde_json::Value {
        serde_json::json!({
            "operation": self.operation, "context": self.target, "contract": self.contract,
        })
    }

    pub fn validate_arguments(
        &self,
        arguments: &serde_json::Value,
    ) -> Result<(), ExtensionDispatchError> {
        let bytes =
            serde_json::to_vec(arguments).map_err(|_| ExtensionDispatchError::InvalidArguments)?;
        if bytes.len() > MAX_ARGUMENT_BYTES {
            return Err(ExtensionDispatchError::ArgumentsTooLarge);
        }
        if !self.action.input.is_valid(arguments) {
            return Err(ExtensionDispatchError::InvalidArguments);
        }
        Ok(())
    }
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

#[derive(Clone)]
pub struct DispatchContext {
    pub authorization: AuthorizationScope,
    pub principal_id: PrincipalId,
    pub target: ToolContext,
    pub correlation_id: String,
}

#[derive(Debug, thiserror::Error)]
pub enum ExtensionDispatchError {
    #[error("extension action is not registered")]
    NotFound,
    #[error("extension operation descriptor does not match the registered action")]
    DescriptorMismatch,
    #[error("extension action requires an authorized target context")]
    InvalidTarget,
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
        operation: ExtensionOperation,
        arguments: serde_json::Value,
        context: DispatchContext,
    ) -> Result<(ExtensionOperation, InvokeExtensionResponse), ExtensionDispatchError> {
        let prepared = self.prepare(operation, &context.target)?;
        self.dispatch_prepared(prepared, arguments, context).await
    }

    pub async fn dispatch_prepared(
        &self,
        prepared: PreparedExtensionAction,
        arguments: serde_json::Value,
        context: DispatchContext,
    ) -> Result<(ExtensionOperation, InvokeExtensionResponse), ExtensionDispatchError> {
        // A prepared action belongs to this exact registration and authorized target.
        // Never load a replacement invoker after approval has been checked.
        let key = action_key(
            &prepared.operation.contribution_id,
            &prepared.operation.action,
        );
        let current = self.actions.load();
        if !current
            .get(&key)
            .is_some_and(|action| Arc::ptr_eq(action, &prepared.action))
            || context.target != prepared.target
        {
            return Err(ExtensionDispatchError::DescriptorMismatch);
        }
        drop(current);
        prepared.validate_arguments(&arguments)?;
        let action = prepared.action;
        let mut operation = prepared.operation;
        authorize_extension(&context.authorization, action.registration.classification)
            .map_err(|denial| ExtensionDispatchError::Denied(AuthorizationDenialDisplay(denial)))?;

        operation.sanitized_arguments =
            audit_safe_projection(&action.registration.input_schema, &arguments);
        let mut audit = NewOperationAudit {
            actor_principal_id: Some(context.principal_id),
            action: operation.action.as_str().into(),
            target: operation.contribution_id.as_str().into(),
            target_id: operation
                .target_id
                .as_deref()
                .and_then(|id| id.parse().ok()),
            status: "failed".into(),
            result_code: None,
            row_count: None,
            error_message: None,
            correlation_id: Some(context.correlation_id.clone()),
        };
        let result = self.invoke(&action, &operation, arguments, &context).await;
        match &result {
            Ok(_) => audit.status = "succeeded".into(),
            Err(error) => {
                audit.result_code = Some(error.code().into());
                audit.error_message = Some("extension operation failed".into());
            }
        }
        // Operation audit stores terminal observations only. Durable pre-dispatch
        // claims belong to the separate extension settlement journal.
        self.metadata.record_operation_audit(audit)?;
        result.map(|result| (operation, InvokeExtensionResponse { result }))
    }

    pub fn bind_operation(
        &self,
        operation: ExtensionOperation,
        context: &ToolContext,
    ) -> Result<ExtensionOperation, ExtensionDispatchError> {
        Ok(self.prepare(operation, context)?.operation)
    }

    pub fn prepare(
        &self,
        mut operation: ExtensionOperation,
        context: &ToolContext,
    ) -> Result<PreparedExtensionAction, ExtensionDispatchError> {
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
        let (target_kind, target_id) =
            bound_target(&action.registration.required_context, context)?;
        operation.target_kind = target_kind;
        operation.target_id = target_id;
        operation.sanitized_arguments.clear();
        let registration = &action.registration;
        let contract = serde_json::json!({
            "package": registration.package_identity,
            "required_context": registration.required_context,
            "input_schema": registration.input_schema,
            "output_schema": registration.output_schema,
            "timeout_ms": registration.timeout.as_millis(),
            "max_result_bytes": registration.max_result_bytes,
        });
        Ok(PreparedExtensionAction {
            action,
            operation,
            target: context.clone(),
            contract,
        })
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
                tenant_id: context.target.tenant_id,
                room_id: context.target.room_id,
            }),
            stream_id: None,
        };
        let response = action
            .registration
            .invoker
            .request(context.target.tenant_id, request)
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
            Self::DescriptorMismatch
            | Self::InvalidTarget
            | Self::InvalidArguments
            | Self::InvalidRegistration(_) => "invalid_parameter_value",
            Self::Denied(_) => "permission_denied",
            Self::ArgumentsTooLarge | Self::ResultTooLarge => "result_too_large",
            Self::InvalidResult | Self::InvocationFailed => "driver_internal",
            Self::Audit(_) => "audit_failed",
        }
    }
}

fn bound_target(
    required: &[ContributionContext],
    context: &ToolContext,
) -> Result<(SegmentId, Option<String>), ExtensionDispatchError> {
    for scope in required {
        let present = match scope {
            ContributionContext::Instance => true,
            ContributionContext::Tenant => context.tenant_id.is_some(),
            ContributionContext::Room => context.room_id.is_some(),
            ContributionContext::Profile => context.profile_id.is_some(),
            ContributionContext::Connection => context.connection_id.is_some(),
            ContributionContext::Document => context.document_id.is_some(),
        };
        if !present {
            return Err(ExtensionDispatchError::InvalidTarget);
        }
    }
    let (kind, id) = match required.last() {
        None | Some(ContributionContext::Instance) => ("instance", None),
        Some(ContributionContext::Tenant) => ("tenant", context.tenant_id.map(|id| id.to_string())),
        Some(ContributionContext::Room) => ("room", context.room_id.map(|id| id.to_string())),
        Some(ContributionContext::Profile) => {
            ("profile", context.profile_id.map(|id| id.to_string()))
        }
        Some(ContributionContext::Connection) => ("connection", context.connection_id.clone()),
        Some(ContributionContext::Document) => ("document", context.document_id.clone()),
    };
    Ok((
        SegmentId::new(kind.to_owned()).expect("static target kind"),
        id,
    ))
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

    struct NeverInvoker;

    #[async_trait::async_trait]
    impl ActionInvoker for NeverInvoker {
        async fn request(
            &self,
            _tenant_id: Option<i64>,
            _request: Request,
        ) -> Result<sift_extension_protocol::Response, SupervisorError> {
            panic!("binding must not invoke an extension")
        }
    }

    #[test]
    fn action_target_comes_from_authorized_context() {
        let context = ToolContext {
            tenant_id: Some(4),
            room_id: Some(8),
            profile_id: None,
            connection_id: None,
            document_id: None,
        };
        let (kind, id) = bound_target(
            &[ContributionContext::Tenant, ContributionContext::Room],
            &context,
        )
        .unwrap();
        assert_eq!(kind.as_str(), "room");
        assert_eq!(id.as_deref(), Some("8"));
        assert!(matches!(
            bound_target(&[ContributionContext::Document], &context),
            Err(ExtensionDispatchError::InvalidTarget)
        ));
        let metadata = Arc::new(
            MetadataStore::open_in_memory(Arc::new(sift_metadata::MemorySecretStore::default()))
                .unwrap(),
        );
        let dispatcher = ExtensionOperationDispatcher::new(metadata);
        let extension_id = ExtensionId::new("acme/usage").unwrap();
        let contribution_id = ContributionId::new("acme/usage/command/read").unwrap();
        let action = SegmentId::new("read").unwrap();
        dispatcher
            .replace([ActionRegistration {
                extension_id: extension_id.clone(),
                contribution_id: contribution_id.clone(),
                action: action.clone(),
                classification: OperationClassification::Read,
                required_context: vec![ContributionContext::Room],
                input_schema: serde_json::json!({"type":"object"}),
                output_schema: serde_json::json!({"type":"object"}),
                timeout: Duration::from_secs(1),
                max_result_bytes: 1024,
                package_identity: None,
                invoker: Arc::new(NeverInvoker),
            }])
            .unwrap();
        let bound = dispatcher
            .bind_operation(
                ExtensionOperation {
                    extension_id,
                    contribution_id,
                    action,
                    classification: OperationClassification::Read,
                    target_kind: SegmentId::new("instance").unwrap(),
                    target_id: Some("spoofed".into()),
                    sanitized_arguments: BTreeMap::from([(
                        "secret".into(),
                        serde_json::json!("spoofed"),
                    )]),
                },
                &context,
            )
            .unwrap();
        assert_eq!(bound.target_kind.as_str(), "room");
        assert_eq!(bound.target_id.as_deref(), Some("8"));
        assert!(bound.sanitized_arguments.is_empty());
    }

    struct ReturningInvoker(Arc<AtomicU64>);

    #[async_trait::async_trait]
    impl ActionInvoker for ReturningInvoker {
        async fn request(
            &self,
            _tenant_id: Option<i64>,
            request: Request,
        ) -> Result<sift_extension_protocol::Response, SupervisorError> {
            self.0.fetch_add(1, Ordering::Relaxed);
            Ok(sift_extension_protocol::Response {
                id: request.id,
                result: ResponseResult::Ok {
                    payload: serde_json::json!({"result":{"received":true}}),
                },
            })
        }
    }

    fn review_fixture() -> (ActionRegistration, ExtensionOperation, ToolContext) {
        let registration = ActionRegistration {
            extension_id: ExtensionId::new("acme/usage").unwrap(),
            contribution_id: ContributionId::new("acme/usage/command/read").unwrap(),
            action: SegmentId::new("read").unwrap(),
            classification: OperationClassification::Read,
            required_context: vec![ContributionContext::Room],
            input_schema: serde_json::json!({"type":"object", "required":["label"],
                "properties":{"label":{"type":"string"}}, "additionalProperties":false}),
            output_schema: serde_json::json!({"type":"object"}),
            timeout: Duration::from_secs(1),
            max_result_bytes: 1024,
            package_identity: Some(serde_json::json!({"archive":"original", "revision":1})),
            invoker: Arc::new(NeverInvoker),
        };
        let operation = ExtensionOperation {
            extension_id: registration.extension_id.clone(),
            contribution_id: registration.contribution_id.clone(),
            action: registration.action.clone(),
            classification: registration.classification,
            target_kind: SegmentId::new("instance").unwrap(),
            target_id: None,
            sanitized_arguments: BTreeMap::new(),
        };
        let target = ToolContext {
            tenant_id: Some(4),
            room_id: Some(8),
            profile_id: None,
            connection_id: None,
            document_id: None,
        };
        (registration, operation, target)
    }

    #[test]
    fn review_contract_changes_for_package_grants_schema_and_limits() {
        let metadata = Arc::new(
            MetadataStore::open_in_memory(Arc::new(sift_metadata::MemorySecretStore::default()))
                .unwrap(),
        );
        let dispatcher = ExtensionOperationDispatcher::new(metadata);
        let (registration, operation, target) = review_fixture();
        dispatcher.replace([registration]).unwrap();
        let original = dispatcher.prepare(operation.clone(), &target).unwrap();
        original
            .validate_arguments(&serde_json::json!({"label":"ok"}))
            .unwrap();
        assert!(matches!(
            original.validate_arguments(&serde_json::json!({"label":7})),
            Err(ExtensionDispatchError::InvalidArguments)
        ));
        assert!(matches!(
            original
                .validate_arguments(&serde_json::json!({"label":"x".repeat(MAX_ARGUMENT_BYTES)})),
            Err(ExtensionDispatchError::ArgumentsTooLarge)
        ));
        let original_hash = crate::automation::fingerprint(&original.review_context()).unwrap();
        for change in 0..6 {
            let (mut registration, _, _) = review_fixture();
            match change {
                0 => {
                    registration.package_identity =
                        Some(serde_json::json!({"archive":"replacement", "revision":1}))
                }
                1 => {
                    registration.package_identity =
                        Some(serde_json::json!({"archive":"original", "revision":2}))
                }
                2 => registration.input_schema["additionalProperties"] = serde_json::json!(true),
                3 => registration.output_schema = serde_json::json!({"type":"array"}),
                4 => registration.timeout = Duration::from_secs(2),
                _ => registration.max_result_bytes = 2048,
            }
            dispatcher.replace([registration]).unwrap();
            let replacement = dispatcher.prepare(operation.clone(), &target).unwrap();
            assert_ne!(
                original_hash,
                crate::automation::fingerprint(&replacement.review_context()).unwrap()
            );
        }
        // An unchanged contract is stable across reconstruction/restart.
        dispatcher.replace([review_fixture().0]).unwrap();
        assert_eq!(
            original_hash,
            crate::automation::fingerprint(
                &dispatcher
                    .prepare(operation, &target)
                    .unwrap()
                    .review_context()
            )
            .unwrap()
        );
    }

    #[tokio::test]
    async fn prepared_dispatch_refuses_replaced_invoker_and_changed_target() {
        let metadata = Arc::new(
            MetadataStore::open_in_memory(Arc::new(sift_metadata::MemorySecretStore::default()))
                .unwrap(),
        );
        let dispatcher = ExtensionOperationDispatcher::new(metadata);
        let (registration, operation, target) = review_fixture();
        dispatcher.replace([registration]).unwrap();
        let prepared = dispatcher.prepare(operation.clone(), &target).unwrap();
        dispatcher.replace([review_fixture().0]).unwrap();
        let context = DispatchContext {
            authorization: AuthorizationScope::trusted_local(),
            principal_id: PrincipalId(1),
            target: target.clone(),
            correlation_id: "review-test".into(),
        };
        assert!(matches!(
            dispatcher
                .dispatch_prepared(prepared, serde_json::json!({"label":"ok"}), context.clone())
                .await,
            Err(ExtensionDispatchError::DescriptorMismatch)
        ));
        let prepared = dispatcher.prepare(operation, &target).unwrap();
        let mut other_context = context;
        other_context.target.room_id = Some(9);
        assert!(matches!(
            dispatcher
                .dispatch_prepared(prepared, serde_json::json!({"label":"ok"}), other_context)
                .await,
            Err(ExtensionDispatchError::DescriptorMismatch)
        ));
    }

    #[tokio::test]
    async fn governed_review_rejects_changed_contract_and_invalid_input_without_spending_approval()
    {
        use crate::automation::{GovernedToolRegistry, ToolApprovalPolicy, ToolRegistryError};
        use sift_protocol::{GovernedToolDescriptor, InvokeToolRequest, InvokeToolResponse};
        let metadata = Arc::new(
            MetadataStore::open_in_memory(Arc::new(sift_metadata::MemorySecretStore::default()))
                .unwrap(),
        );
        metadata.bootstrap_local("extension-review").unwrap();
        let dispatcher = ExtensionOperationDispatcher::new(metadata.clone());
        let (mut registration, operation, target) = review_fixture();
        let calls = Arc::new(AtomicU64::new(0));
        registration.invoker = Arc::new(ReturningInvoker(calls.clone()));
        let tool = GovernedToolDescriptor {
            id: "reviewed_read".into(),
            title: "Read".into(),
            description: "Read fixture".into(),
            operation,
            input_schema: registration.input_schema.clone(),
            output_schema: registration.output_schema.clone(),
            required_context: registration.required_context.clone(),
            mcp_exposable: true,
            schedulable: false,
            interactive: true,
        };
        dispatcher.replace([registration]).unwrap();
        let registry = GovernedToolRegistry::new(
            dispatcher.clone(),
            metadata.clone(),
            ToolApprovalPolicy {
                reads: true,
                execute_reads: false,
            },
        );
        registry.replace([tool.clone()]).unwrap();
        let context = DispatchContext {
            authorization: AuthorizationScope::trusted_local(),
            principal_id: PrincipalId(1),
            target: target.clone(),
            correlation_id: "governed-review-test".into(),
        };
        let mut request = InvokeToolRequest {
            tool_id: tool.id.clone(),
            arguments: serde_json::json!({"label":"ok"}),
            context: target,
            approval_id: None,
        };
        let InvokeToolResponse::ApprovalRequired { approval } = registry
            .invoke(request.clone(), context.clone())
            .await
            .unwrap()
        else {
            panic!("review required")
        };
        registry
            .approve(&approval.id, PrincipalId(1), approval.revision)
            .unwrap();
        request.approval_id = Some(approval.id.clone());
        request.arguments = serde_json::json!({"label":7});
        assert!(matches!(
            registry.invoke(request.clone(), context.clone()).await,
            Err(ToolRegistryError::Dispatch(
                ExtensionDispatchError::InvalidArguments
            ))
        ));
        request.arguments = serde_json::json!({"label":"changed"});
        assert!(matches!(
            registry.invoke(request.clone(), context.clone()).await,
            Err(ToolRegistryError::Approval(_))
        ));
        request.arguments = serde_json::json!({"label":"ok"});
        let mut changed_tool = tool.clone();
        changed_tool.description = "Different reviewed contribution".into();
        registry.replace([changed_tool]).unwrap();
        assert!(matches!(
            registry.invoke(request.clone(), context.clone()).await,
            Err(ToolRegistryError::Approval(_))
        ));
        registry.replace([tool]).unwrap();
        let (mut replacement, _, _) = review_fixture();
        replacement.output_schema = serde_json::json!({"type":"array"});
        dispatcher.replace([replacement]).unwrap();
        assert!(matches!(
            registry.invoke(request.clone(), context.clone()).await,
            Err(ToolRegistryError::Approval(_))
        ));
        let approvals = metadata.list_operation_approvals(PrincipalId(1)).unwrap();
        assert_eq!(approvals.len(), 1);
        assert!(approvals[0].consumed_at.is_none());
        assert_eq!(calls.load(Ordering::Relaxed), 0);
        let (mut restored, _, _) = review_fixture();
        restored.invoker = Arc::new(ReturningInvoker(calls.clone()));
        dispatcher.replace([restored]).unwrap();
        assert!(matches!(
            registry
                .invoke(request.clone(), context.clone())
                .await
                .unwrap(),
            InvokeToolResponse::Completed { .. }
        ));
        assert_eq!(calls.load(Ordering::Relaxed), 1);
        assert!(matches!(
            registry.invoke(request, context).await,
            Err(ToolRegistryError::Approval(_))
        ));
        assert_eq!(calls.load(Ordering::Relaxed), 1);
        let audit = metadata.list_operation_audit(10).unwrap();
        let invocations: Vec<_> = audit.iter().filter(|row| row.action == "read").collect();
        assert_eq!(invocations.len(), 1);
        assert_eq!(invocations[0].status, "succeeded");
        assert_eq!(invocations[0].actor_principal_id, Some(PrincipalId(1)));
        assert_eq!(
            invocations[0].correlation_id.as_deref(),
            Some("governed-review-test")
        );
    }

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
