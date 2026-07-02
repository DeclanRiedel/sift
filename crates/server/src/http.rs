//! axum router + handlers. Routes versioned under `/v1`. The `AppState`
//! carries the `SessionStore` (which in turn carries the `DriverRegistry`).

use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::{header::HeaderName, HeaderValue, Request};
use axum::middleware::{from_fn, Next};
use axum::response::Response;
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::json;

use sift_protocol::{
    CancelRequest, ExecuteRequest, ExecuteRequestHttp, Health, ObjectPath, OpenConnectionRequest,
    OpenSessionRequest, SchemaFilter, SchemaScope, PROTOCOL_VERSION,
};

use crate::error::{ApiError, ApiResult};
use crate::session::SessionStore;
use crate::VERSION;

#[derive(Clone)]
pub struct AppState {
    pub sessions: SessionStore,
}

pub fn app(state: AppState) -> Router {
    Router::new()
        .route("/v1/health", get(health))
        .route("/v1/sessions", post(create_session).get(list_sessions))
        .route("/v1/sessions/:id", get(get_session).delete(close_session))
        .route(
            "/v1/sessions/:id/connections",
            post(open_connection).get(list_connections),
        )
        .route(
            "/v1/sessions/:id/connections/:conn_id",
            delete(close_connection),
        )
        .route(
            "/v1/sessions/:id/connections/:conn_id/ping",
            post(ping_connection),
        )
        .route(
            "/v1/sessions/:id/connections/:conn_id/schema",
            get(get_schema),
        )
        .route("/v1/sessions/:id/queries", post(execute_query))
        .route(
            "/v1/sessions/:id/queries/:cursor_id/cancel",
            post(cancel_query),
        )
        .layer(from_fn(protocol_version_header))
        .with_state(state)
}

async fn protocol_version_header(req: Request<Body>, next: Next) -> Response {
    let mut response = next.run(req).await;
    response.headers_mut().insert(
        HeaderName::from_static("x-sift-protocol-version"),
        HeaderValue::from_static(PROTOCOL_VERSION),
    );
    response
}

async fn health(State(state): State<AppState>) -> Json<Health> {
    Json(Health {
        status: "ok".to_string(),
        version: VERSION.to_string(),
        engines: state.sessions.registry().engines(),
    })
}

async fn create_session(
    State(state): State<AppState>,
    body: Option<Json<OpenSessionRequest>>,
) -> ApiResult<Json<sift_protocol::SessionInfo>> {
    let req = match body {
        Some(Json(b)) => b,
        None => OpenSessionRequest { tag: None },
    };
    Ok(Json(state.sessions.open_session(req)))
}

async fn list_sessions(
    State(state): State<AppState>,
) -> ApiResult<Json<Vec<sift_protocol::SessionInfo>>> {
    Ok(Json(state.sessions.list_sessions()))
}

async fn get_session(
    State(state): State<AppState>,
    Path(id): Path<sift_protocol::SessionId>,
) -> ApiResult<Json<sift_protocol::SessionInfo>> {
    Ok(Json(state.sessions.session_info(id)?))
}

async fn close_session(
    State(state): State<AppState>,
    Path(id): Path<sift_protocol::SessionId>,
) -> ApiResult<Json<serde_json::Value>> {
    state.sessions.close_session(id)?;
    Ok(Json(json!({"ok": true})))
}

async fn open_connection(
    State(state): State<AppState>,
    Path(id): Path<sift_protocol::SessionId>,
    Json(req): Json<OpenConnectionRequest>,
) -> ApiResult<Json<sift_protocol::ConnectionInfo>> {
    let engine = req.engine;
    let spec = req.spec;
    let info = state.sessions.open_connection(id, engine, spec).await?;
    Ok(Json(info))
}

async fn list_connections(
    State(state): State<AppState>,
    Path(id): Path<sift_protocol::SessionId>,
) -> ApiResult<Json<Vec<sift_protocol::ConnectionInfo>>> {
    Ok(Json(state.sessions.list_connections(id)?))
}

async fn close_connection(
    State(state): State<AppState>,
    Path((id, conn_id)): Path<(sift_protocol::SessionId, sift_protocol::ConnectionId)>,
) -> ApiResult<Json<serde_json::Value>> {
    state.sessions.close_connection(id, conn_id).await?;
    Ok(Json(json!({"ok": true})))
}

async fn ping_connection(
    State(state): State<AppState>,
    Path((id, conn_id)): Path<(sift_protocol::SessionId, sift_protocol::ConnectionId)>,
) -> ApiResult<Json<sift_protocol::ServerInfo>> {
    Ok(Json(state.sessions.ping(id, conn_id).await?))
}

#[derive(Deserialize)]
struct SchemaQuery {
    /// `shallow` (default) or `deep`. Deep requires `schema` and `object`.
    #[serde(default)]
    depth: Option<String>,
    schema: Option<String>,
    object: Option<String>,
    name_pattern: Option<String>,
}

async fn get_schema(
    State(state): State<AppState>,
    Path((id, conn_id)): Path<(sift_protocol::SessionId, sift_protocol::ConnectionId)>,
    Query(q): Query<SchemaQuery>,
) -> ApiResult<Json<sift_protocol::SchemaSnapshot>> {
    let scope = build_scope(q)?;
    Ok(Json(state.sessions.schema(id, conn_id, scope).await?))
}

fn build_scope(q: SchemaQuery) -> ApiResult<SchemaScope> {
    match q.depth.as_deref().unwrap_or("shallow") {
        "shallow" => {
            let mut scope = SchemaScope::shallow();
            if q.name_pattern.is_some() {
                scope.filter = Some(SchemaFilter {
                    catalogs: None,
                    schemas: None,
                    kinds: None,
                    name_pattern: q.name_pattern,
                });
            }
            Ok(scope)
        }
        "deep" => {
            let schema = q.schema.ok_or_else(|| {
                ApiError::BadRequest("`depth=deep` requires `schema` query param".into())
            })?;
            let object = q.object.ok_or_else(|| {
                ApiError::BadRequest("`depth=deep` requires `object` query param".into())
            })?;
            Ok(SchemaScope::deep(ObjectPath {
                catalog: None,
                schema: Some(schema),
                name: object,
                kind: None,
            }))
        }
        other => Err(ApiError::BadRequest(format!(
            "unknown depth `{other}` (want `shallow` or `deep`)"
        ))),
    }
}

async fn execute_query(
    State(state): State<AppState>,
    Path(id): Path<sift_protocol::SessionId>,
    Json(req): Json<ExecuteRequestHttp>,
) -> ApiResult<Json<sift_protocol::ExecuteResponse>> {
    let inner = ExecuteRequest::new(req.sql);
    // tx handle is part of the protocol surface but the transactions
    // endpoint is TBD; for now the sync execute path is autocommit only.
    let _ = req.tx;
    let resp = state.sessions.execute(id, req.connection, inner).await?;
    Ok(Json(resp))
}

async fn cancel_query(
    State(state): State<AppState>,
    Path((id, cursor_id)): Path<(sift_protocol::SessionId, sift_protocol::CursorId)>,
    Json(req): Json<CancelRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    if req.cursor != cursor_id {
        return Err(ApiError::BadRequest(
            "`cursor` body value must match cursor id in path".into(),
        ));
    }
    state.sessions.cancel(id, req.connection, cursor_id).await?;
    Ok(Json(json!({"ok": true})))
}
