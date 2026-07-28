use std::{
    collections::HashMap,
    fmt::Write as _,
    sync::{Arc, Mutex},
};

use arc_swap::ArcSwap;
use sha2::{Digest, Sha256};
use sift_extension_protocol::{ContributionContext, OperationClassification};
use sift_metadata::{ApprovalBinding, MetadataStore, PrincipalId};
use sift_protocol::{
    classification_requires_approval, GovernedToolDescriptor, InvokeToolRequest,
    InvokeToolResponse, OperationApproval, ToolContext,
};

use crate::{
    authorization::{authorize_extension, AuthorizationScope},
    extension_dispatch::{DispatchContext, ExtensionDispatchError, ExtensionOperationDispatcher},
};

#[derive(Debug, Clone, Copy, Default)]
pub struct ToolApprovalPolicy {
    pub reads: bool,
    pub execute_reads: bool,
}

#[derive(Clone)]
pub struct GovernedToolRegistry {
    tools: Arc<ArcSwap<HashMap<String, GovernedToolDescriptor>>>,
    mutation: Arc<Mutex<()>>,
    dispatcher: ExtensionOperationDispatcher,
    metadata: Arc<MetadataStore>,
    approval_policy: ToolApprovalPolicy,
}

#[derive(Debug, thiserror::Error)]
pub enum ToolRegistryError {
    #[error("tool is not registered or unavailable in this context")]
    NotFound,
    #[error("tool descriptor is invalid: {0}")]
    InvalidDescriptor(String),
    #[error("tool operation is not authorized")]
    Denied,
    #[error("operation approval failed: {0}")]
    Approval(#[from] sift_metadata::MetadataError),
    #[error("extension dispatch failed: {0}")]
    Dispatch(#[from] ExtensionDispatchError),
    #[error("tool input cannot be fingerprinted")]
    InvalidInput,
}

impl GovernedToolRegistry {
    pub fn new(
        dispatcher: ExtensionOperationDispatcher,
        metadata: Arc<MetadataStore>,
        approval_policy: ToolApprovalPolicy,
    ) -> Self {
        Self {
            tools: Arc::new(ArcSwap::from_pointee(HashMap::new())),
            mutation: Arc::new(Mutex::new(())),
            dispatcher,
            metadata,
            approval_policy,
        }
    }

    pub fn replace(
        &self,
        descriptors: impl IntoIterator<Item = GovernedToolDescriptor>,
    ) -> Result<(), ToolRegistryError> {
        let mut tools = HashMap::new();
        for descriptor in descriptors {
            validate_descriptor(&descriptor)?;
            if tools.insert(descriptor.id.clone(), descriptor).is_some() {
                return Err(ToolRegistryError::InvalidDescriptor(
                    "duplicate tool id".into(),
                ));
            }
        }
        let _guard = self.mutation.lock().expect("tool mutation lock poisoned");
        self.tools.store(Arc::new(tools));
        Ok(())
    }

    pub fn list(
        &self,
        authorization: &AuthorizationScope,
        context: &ToolContext,
        mcp_only: bool,
    ) -> Vec<GovernedToolDescriptor> {
        let mut tools: Vec<_> = self
            .tools
            .load()
            .values()
            .filter(|tool| !mcp_only || tool.mcp_exposable)
            .filter(|tool| context_satisfies(context, &tool.required_context))
            .filter(|tool| {
                authorize_extension(authorization, tool.operation.classification).is_ok()
            })
            .cloned()
            .collect();
        tools.sort_by(|left, right| left.id.cmp(&right.id));
        tools
    }

    pub async fn invoke(
        &self,
        request: InvokeToolRequest,
        dispatch: DispatchContext,
    ) -> Result<InvokeToolResponse, ToolRegistryError> {
        let tool = self
            .tools
            .load()
            .get(&request.tool_id)
            .cloned()
            .filter(|tool| context_satisfies(&request.context, &tool.required_context))
            .ok_or(ToolRegistryError::NotFound)?;
        authorize_extension(&dispatch.authorization, tool.operation.classification)
            .map_err(|_| ToolRegistryError::Denied)?;
        let binding = ApprovalBinding {
            principal_id: dispatch.principal_id,
            operation_id: tool.id.clone(),
            context_fingerprint: fingerprint(
                &serde_json::to_value(&request.context)
                    .map_err(|_| ToolRegistryError::InvalidInput)?,
            )?,
            input_fingerprint: fingerprint(&request.arguments)?,
        };
        if requires_approval(tool.operation.classification, self.approval_policy) {
            if let Some(approval_id) = &request.approval_id {
                self.metadata
                    .consume_operation_approval(approval_id, &binding)?;
            } else {
                let approval = self.metadata.create_operation_approval(&binding, None)?;
                return Ok(InvokeToolResponse::ApprovalRequired { approval });
            }
        }
        let (_, response) = self
            .dispatcher
            .dispatch(tool.operation, request.arguments, dispatch)
            .await?;
        Ok(InvokeToolResponse::Completed {
            result: response.result,
        })
    }

    pub fn approve(
        &self,
        approval_id: &str,
        principal_id: PrincipalId,
        expected_revision: u64,
    ) -> Result<OperationApproval, ToolRegistryError> {
        self.metadata
            .approve_operation(approval_id, principal_id, expected_revision)
            .map_err(Into::into)
    }
}

fn validate_descriptor(descriptor: &GovernedToolDescriptor) -> Result<(), ToolRegistryError> {
    if descriptor.id.is_empty()
        || descriptor.id.len() > 128
        || !descriptor
            .id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
    {
        return Err(ToolRegistryError::InvalidDescriptor(
            "tool id is not MCP-safe".into(),
        ));
    }
    if descriptor.title.is_empty() || descriptor.description.is_empty() {
        return Err(ToolRegistryError::InvalidDescriptor(
            "tool title and description are required".into(),
        ));
    }
    if !descriptor.input_schema.is_object() || !descriptor.output_schema.is_object() {
        return Err(ToolRegistryError::InvalidDescriptor(
            "tool schemas must be JSON objects".into(),
        ));
    }
    Ok(())
}

fn context_satisfies(context: &ToolContext, required: &[ContributionContext]) -> bool {
    required.iter().all(|required| match required {
        ContributionContext::Instance => true,
        ContributionContext::Tenant => context.tenant_id.is_some(),
        ContributionContext::Room => context.room_id.is_some(),
        ContributionContext::Profile => context.profile_id.is_some(),
        ContributionContext::Connection => context.connection_id.is_some(),
        ContributionContext::Document => context.document_id.is_some(),
    })
}

fn requires_approval(classification: OperationClassification, policy: ToolApprovalPolicy) -> bool {
    classification_requires_approval(classification)
        || (classification == OperationClassification::Read && policy.reads)
        || (classification == OperationClassification::ExecuteRead && policy.execute_reads)
}

fn fingerprint(value: &serde_json::Value) -> Result<String, ToolRegistryError> {
    let mut canonical = String::new();
    write_canonical(value, &mut canonical)?;
    let digest = Sha256::digest(canonical.as_bytes());
    let mut encoded = String::with_capacity(64);
    for byte in digest {
        write!(&mut encoded, "{byte:02x}").expect("writing to String cannot fail");
    }
    Ok(encoded)
}

fn write_canonical(
    value: &serde_json::Value,
    output: &mut String,
) -> Result<(), ToolRegistryError> {
    match value {
        serde_json::Value::Object(object) => {
            output.push('{');
            let mut fields: Vec<_> = object.iter().collect();
            fields.sort_by_key(|(key, _)| *key);
            for (index, (key, value)) in fields.into_iter().enumerate() {
                if index != 0 {
                    output.push(',');
                }
                output.push_str(
                    &serde_json::to_string(key).map_err(|_| ToolRegistryError::InvalidInput)?,
                );
                output.push(':');
                write_canonical(value, output)?;
            }
            output.push('}');
        }
        serde_json::Value::Array(values) => {
            output.push('[');
            for (index, value) in values.iter().enumerate() {
                if index != 0 {
                    output.push(',');
                }
                write_canonical(value, output)?;
            }
            output.push(']');
        }
        value => output
            .push_str(&serde_json::to_string(value).map_err(|_| ToolRegistryError::InvalidInput)?),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprints_ignore_object_insertion_order_but_not_context() {
        let left = serde_json::json!({"a": 1, "b": {"x": true, "y": 2}});
        let right = serde_json::json!({"b": {"y": 2, "x": true}, "a": 1});
        assert_eq!(fingerprint(&left).unwrap(), fingerprint(&right).unwrap());
        assert_ne!(
            fingerprint(&left).unwrap(),
            fingerprint(&serde_json::json!({"a": 2, "b": {"x": true, "y": 2}})).unwrap()
        );
    }

    #[test]
    fn required_context_is_exact() {
        let context = ToolContext {
            tenant_id: Some(1),
            room_id: None,
            profile_id: None,
            connection_id: None,
            document_id: None,
        };
        assert!(context_satisfies(
            &context,
            &[ContributionContext::Instance, ContributionContext::Tenant]
        ));
        assert!(!context_satisfies(&context, &[ContributionContext::Room]));
    }
}
