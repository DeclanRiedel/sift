//! Typed HTTP/schema boundary with concrete codecs compiled in api-types.
use super::*;
use axum::extract::FromRequest;
use sift_api_types::DelegationWireJson;

pub(super) struct DelegationJson<T>(pub T);

#[async_trait::async_trait]
impl<S, T> FromRequest<S> for DelegationJson<T>
where
    S: Send + Sync,
    T: DelegationWireJson + Send,
{
    type Rejection = Response;

    async fn from_request(req: Request<Body>, state: &S) -> Result<Self, Self::Rejection> {
        let content_type = req.headers().get(header::CONTENT_TYPE).cloned();
        let bytes = bytes::Bytes::from_request(req, state)
            .await
            .map_err(IntoResponse::into_response)?;
        let mut validation = Request::new(Body::from(bytes.clone()));
        if let Some(value) = content_type {
            validation.headers_mut().insert(header::CONTENT_TYPE, value);
        }
        // Reuse Axum's exact content-type and syntax rejection semantics. Decode
        // the original bytes afterwards so duplicate fields remain rejected.
        let _ = Json::<serde::de::IgnoredAny>::from_request(validation, state)
            .await
            .map_err(IntoResponse::into_response)?;
        T::decode(&bytes).map(Self).map_err(|_| {
            (StatusCode::UNPROCESSABLE_ENTITY, "Invalid delegation JSON").into_response()
        })
    }
}

impl<T: DelegationWireJson> IntoResponse for DelegationJson<T> {
    fn into_response(self) -> Response {
        match self.0.encode() {
            Ok(bytes) => ([(header::CONTENT_TYPE, "application/json")], bytes).into_response(),
            Err(_) => ApiError::Internal("Delegation response cannot be represented".into())
                .into_response(),
        }
    }
}

impl<T: schemars::JsonSchema> aide::OperationInput for DelegationJson<T> {
    fn operation_input(ctx: &mut aide::gen::GenContext, op: &mut aide::openapi::Operation) {
        <Json<T> as aide::OperationInput>::operation_input(ctx, op);
    }
    fn inferred_early_responses(
        ctx: &mut aide::gen::GenContext,
        op: &mut aide::openapi::Operation,
    ) -> Vec<(Option<u16>, aide::openapi::Response)> {
        <Json<T> as aide::OperationInput>::inferred_early_responses(ctx, op)
    }
}
impl<T: schemars::JsonSchema> aide::OperationOutput for DelegationJson<T> {
    type Inner = T;
    fn operation_response(
        ctx: &mut aide::gen::GenContext,
        op: &mut aide::openapi::Operation,
    ) -> Option<aide::openapi::Response> {
        <Json<T> as aide::OperationOutput>::operation_response(ctx, op)
    }
    fn inferred_responses(
        ctx: &mut aide::gen::GenContext,
        op: &mut aide::openapi::Operation,
    ) -> Vec<(Option<u16>, aide::openapi::Response)> {
        <Json<T> as aide::OperationOutput>::inferred_responses(ctx, op)
    }
}
