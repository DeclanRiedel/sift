//! Local review of a typed extension approval request.

use std::fmt::Write;

use sha2::{Digest, Sha256};
use sift_protocol::{CreateOperationApprovalRequest, InvokeExtensionRequest};

#[derive(Clone)]
pub(super) struct ApprovalReview {
    pub invocation: InvokeExtensionRequest,
    pub instance_id: String,
    pub confirmation: String,
    pub input_fingerprint: String,
}

impl ApprovalReview {
    pub fn new(invocation: InvokeExtensionRequest, instance_id: String) -> Result<Self, String> {
        if !sift_protocol::classification_requires_approval(invocation.operation.classification) {
            return Err("This action does not require an explicit approval request".into());
        }
        let input_fingerprint = fingerprint(&invocation.arguments)?;
        let target = invocation
            .operation
            .target_id
            .as_deref()
            .unwrap_or(&instance_id);
        let confirmation = format!(
            "REQUEST {}#{} ON {}:{}",
            invocation.operation.contribution_id,
            invocation.operation.action,
            invocation.operation.target_kind,
            target
        );
        Ok(Self {
            invocation,
            instance_id,
            confirmation,
            input_fingerprint,
        })
    }

    pub fn confirmed_request(
        &self,
        current: &InvokeExtensionRequest,
        current_instance_id: &str,
        confirmation: &str,
    ) -> Result<CreateOperationApprovalRequest, String> {
        if current != &self.invocation || current_instance_id != self.instance_id {
            return Err("Action, inputs, or target changed; review the approval again".into());
        }
        if confirmation != self.confirmation {
            return Err(format!("Type {} to confirm", self.confirmation));
        }
        Ok(CreateOperationApprovalRequest {
            operation: self.invocation.operation.clone(),
            input_fingerprint: self.input_fingerprint.clone(),
            context: self.invocation.context.clone(),
        })
    }
}

pub(super) fn fingerprint(value: &serde_json::Value) -> Result<String, String> {
    let mut canonical = String::new();
    write_canonical(value, &mut canonical)?;
    let digest = Sha256::digest(canonical.as_bytes());
    let mut encoded = String::with_capacity(64);
    for byte in digest {
        write!(&mut encoded, "{byte:02x}").expect("writing to String cannot fail");
    }
    Ok(encoded)
}

fn write_canonical(value: &serde_json::Value, output: &mut String) -> Result<(), String> {
    match value {
        serde_json::Value::Object(object) => {
            output.push('{');
            let mut fields = object.iter().collect::<Vec<_>>();
            fields.sort_by_key(|(key, _)| *key);
            for (index, (key, value)) in fields.into_iter().enumerate() {
                if index != 0 {
                    output.push(',');
                }
                output.push_str(&serde_json::to_string(key).map_err(|error| error.to_string())?);
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
        value => output.push_str(&serde_json::to_string(value).map_err(|error| error.to_string())?),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprint_matches_server_canonical_json_contract() {
        let left = serde_json::json!({"b": {"y": 2, "x": true}, "a": 1});
        let right = serde_json::json!({"a": 1, "b": {"x": true, "y": 2}});
        assert_eq!(fingerprint(&left).unwrap(), fingerprint(&right).unwrap());
        assert_eq!(
            fingerprint(&serde_json::json!({})).unwrap(),
            "44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a"
        );
        assert_ne!(
            fingerprint(&left).unwrap(),
            fingerprint(&serde_json::json!({"a": 2})).unwrap()
        );
    }

    #[test]
    fn confirmation_binds_operation_arguments_and_target_context() {
        let invocation = InvokeExtensionRequest {
            operation: sift_protocol::ExtensionOperation {
                extension_id: sift_protocol::ExtensionId::new("acme/tools").unwrap(),
                contribution_id: sift_protocol::ContributionId::new("acme/tools/command/purge")
                    .unwrap(),
                action: sift_protocol::SegmentId::new("purge").unwrap(),
                classification: sift_protocol::OperationClassification::Destructive,
                target_kind: sift_protocol::SegmentId::new("room").unwrap(),
                target_id: Some("42".into()),
                sanitized_arguments: Default::default(),
            },
            arguments: serde_json::json!({"scope": "old"}),
            context: Some(sift_protocol::ToolContext {
                tenant_id: Some(1),
                room_id: Some(42),
                profile_id: None,
                connection_id: None,
                document_id: None,
            }),
            approval_id: None,
        };
        let review = ApprovalReview::new(invocation.clone(), "server-a".into()).unwrap();
        assert!(review
            .confirmed_request(&invocation, "server-a", "REQUEST wrong")
            .is_err());
        let request = review
            .confirmed_request(&invocation, "server-a", &review.confirmation)
            .unwrap();
        assert_eq!(request.context, invocation.context);
        assert_eq!(request.operation, invocation.operation);
        let mut changed = invocation.clone();
        changed.arguments = serde_json::json!({"scope": "all"});
        assert!(review
            .confirmed_request(&changed, "server-a", &review.confirmation)
            .is_err());
        changed = invocation.clone();
        changed.context.as_mut().unwrap().room_id = Some(43);
        assert!(review
            .confirmed_request(&changed, "server-a", &review.confirmation)
            .is_err());
        assert!(review
            .confirmed_request(&invocation, "server-b", &review.confirmation)
            .is_err());
    }
}
