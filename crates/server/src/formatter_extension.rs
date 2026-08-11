use std::{collections::HashMap, sync::Arc, time::Duration};

use arc_swap::ArcSwap;
use jsonschema::Validator;
use sift_extension_protocol::{ContributionId, Request, RequestContext, ResponseResult, WireId};

use crate::extension_dispatch::ActionInvoker;

const MAX_FORMATTER_FRAME_BYTES: usize = 256 * 1024;

pub struct FormatterRegistration {
    pub id: ContributionId,
    pub version: String,
    pub options_schema: serde_json::Value,
    pub timeout: Duration,
    pub invoker: Arc<dyn ActionInvoker>,
}

struct CompiledFormatter {
    registration: FormatterRegistration,
    options: Validator,
}

#[derive(Clone, Default)]
pub struct FormatterRegistry {
    formats: Arc<ArcSwap<HashMap<String, Arc<CompiledFormatter>>>>,
}

#[derive(Debug, thiserror::Error)]
pub enum FormatterError {
    #[error("formatter is not installed or active")]
    NotFound,
    #[error("formatter version or options are invalid")]
    InvalidConfiguration,
    #[error("formatter input or output frame exceeds the negotiated limit")]
    FrameTooLarge,
    #[error("formatter invocation failed")]
    InvocationFailed,
    #[error("formatter returned a malformed frame")]
    InvalidFrame,
}

#[derive(Debug, Clone, Copy)]
pub enum FormatterPhase {
    Start,
    Data,
    Finish,
    Cancel,
}

impl FormatterRegistry {
    pub fn replace(
        &self,
        registrations: impl IntoIterator<Item = FormatterRegistration>,
    ) -> Result<(), FormatterError> {
        let mut formats = HashMap::new();
        for registration in registrations {
            let options = jsonschema::draft202012::new(&registration.options_schema)
                .map_err(|_| FormatterError::InvalidConfiguration)?;
            let key = registration.id.as_str().to_string();
            if formats
                .insert(
                    key,
                    Arc::new(CompiledFormatter {
                        registration,
                        options,
                    }),
                )
                .is_some()
            {
                return Err(FormatterError::InvalidConfiguration);
            }
        }
        self.formats.store(Arc::new(formats));
        Ok(())
    }

    pub fn validates(&self, id: &str, version: &str, options: &serde_json::Value) -> bool {
        self.formats.load().get(id).is_some_and(|format| {
            format.registration.version == version && format.options.is_valid(options)
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn invoke(
        &self,
        id: &str,
        version: &str,
        tenant_id: Option<i64>,
        room_id: Option<i64>,
        transfer_id: &str,
        direction: &str,
        phase: FormatterPhase,
        options: &serde_json::Value,
        data: &[u8],
    ) -> Result<(Vec<u8>, Option<String>), FormatterError> {
        if data.len() > MAX_FORMATTER_FRAME_BYTES {
            return Err(FormatterError::FrameTooLarge);
        }
        let format = self
            .formats
            .load()
            .get(id)
            .cloned()
            .ok_or(FormatterError::NotFound)?;
        if format.registration.version != version || !format.options.is_valid(options) {
            return Err(FormatterError::InvalidConfiguration);
        }
        let request = Request {
            id: WireId::from_u128(uuid::Uuid::new_v4().as_u128()),
            contribution_id: format.registration.id.clone(),
            method: "format".into(),
            payload: serde_json::json!({
                "contract_version": 1,
                "transfer_id": transfer_id,
                "direction": direction,
                "phase": match phase {
                    FormatterPhase::Start => "start",
                    FormatterPhase::Data => "data",
                    FormatterPhase::Finish => "finish",
                    FormatterPhase::Cancel => "cancel",
                },
                "options": options,
                "data": data,
                "maximum_output_bytes": MAX_FORMATTER_FRAME_BYTES,
            }),
            correlation_id: WireId::from_u128(uuid::Uuid::new_v4().as_u128()),
            deadline_unix_ms: chrono::Utc::now().timestamp_millis()
                + i64::try_from(format.registration.timeout.as_millis()).unwrap_or(i64::MAX),
            context: Some(RequestContext { tenant_id, room_id }),
            stream_id: None,
        };
        let response = format
            .registration
            .invoker
            .request(tenant_id, request)
            .await
            .map_err(|_| FormatterError::InvocationFailed)?;
        let payload = match response.result {
            ResponseResult::Ok { payload } => payload,
            _ => return Err(FormatterError::InvocationFailed),
        };
        let output = payload
            .get("data")
            .cloned()
            .map(serde_json::from_value::<Vec<u8>>)
            .transpose()
            .map_err(|_| FormatterError::InvalidFrame)?
            .unwrap_or_default();
        if output.len() > MAX_FORMATTER_FRAME_BYTES {
            return Err(FormatterError::FrameTooLarge);
        }
        let content_type = payload
            .get("content_type")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string);
        Ok((output, content_type))
    }
}

#[cfg(test)]
mod tests {
    use sift_extension_protocol::{Response, ResponseResult};

    use super::*;

    struct Echo;

    #[async_trait::async_trait]
    impl ActionInvoker for Echo {
        async fn request(
            &self,
            _tenant_id: Option<i64>,
            request: Request,
        ) -> Result<Response, sift_plugin_host::SupervisorError> {
            Ok(Response {
                id: request.id,
                result: ResponseResult::Ok {
                    payload: serde_json::json!({
                        "data": request.payload["data"],
                        "content_type": "application/x-test"
                    }),
                },
            })
        }
    }

    #[tokio::test]
    async fn validates_options_and_enforces_framed_invocation() {
        let registry = FormatterRegistry::default();
        registry
            .replace([FormatterRegistration {
                id: ContributionId::new("acme/test/export_format/test").unwrap(),
                version: "2.0.0".into(),
                options_schema: serde_json::json!({
                    "type": "object",
                    "properties": {"pretty": {"type": "boolean"}},
                    "additionalProperties": false
                }),
                timeout: Duration::from_secs(1),
                invoker: Arc::new(Echo),
            }])
            .unwrap();
        assert!(registry.validates(
            "acme/test/export_format/test",
            "2.0.0",
            &serde_json::json!({"pretty": true})
        ));
        assert!(!registry.validates(
            "acme/test/export_format/test",
            "2.0.0",
            &serde_json::json!({"unknown": true})
        ));
        let (bytes, content_type) = registry
            .invoke(
                "acme/test/export_format/test",
                "2.0.0",
                Some(1),
                Some(2),
                "transfer",
                "export",
                FormatterPhase::Data,
                &serde_json::json!({"pretty": true}),
                b"row\n",
            )
            .await
            .unwrap();
        assert_eq!(bytes, b"row\n");
        assert_eq!(content_type.as_deref(), Some("application/x-test"));
    }
}
