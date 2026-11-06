//! Export observation audit and HTTP body ownership.
use super::*;

pub(super) async fn export_query(
    State(state): State<AppState>,
    auth: Option<Extension<AuthContext>>,
    Path((id, conn_id)): Path<(sift_protocol::SessionId, sift_protocol::ConnectionId)>,
    Json(req): Json<sift_protocol::ExportRequest>,
) -> ApiResult<Response> {
    use axum::body::Body;
    use axum::response::IntoResponse;
    let format = req.format;
    let operation = Operation::ExportQuery {
        session: id,
        connection: conn_id,
    };
    let actor = auth
        .as_ref()
        .map(|Extension(auth)| auth.principal_id.0)
        .or_else(|| {
            state
                .sessions
                .session_owner(id)
                .ok()
                .flatten()
                .map(|owner| owner.0)
        });
    let mut audit = query_audit::QueryOperationAudit::new(state.sessions.clone(), operation, actor);
    let permit = match state.sessions.reserve_query_settlement() {
        Ok(permit) => permit,
        Err(error) => {
            audit.failure(&error);
            return Err(error);
        }
    };
    let query_guard = std::sync::Arc::new(state.shutdown.track_query());
    // The body and the independently owned native worker share one logical
    // query lifetime. A lost HTTP observer cannot release native EOF tracking.
    let stream = match crate::shutdown::scope_query(
        Some(query_guard.clone()),
        state.sessions.export_stream(id, conn_id, req),
    )
    .await
    {
        Ok(stream) => stream,
        Err(error) => {
            audit.failure(&error);
            return Err(error);
        }
    };
    let content_type = crate::export::content_type(format);
    let tenant_id = state
        .sessions
        .managed_tenant_for_session(id)
        .map(|tenant| tenant.0);
    let pacing = auth.map(|Extension(auth)| {
        (
            state.auth.rate_limiter.clone(),
            state.sessions.clone(),
            id,
            auth.principal_id.0,
            tenant_id,
            auth.trusted_local,
        )
    });
    let body = Body::from_stream(pace_http_export(
        stream,
        pacing,
        state.shutdown.clone(),
        query_guard,
        audit,
        permit,
    ));
    let mut resp = body.into_response();
    resp.headers_mut().insert(
        axum::http::header::CONTENT_TYPE,
        content_type.parse().unwrap(),
    );
    Ok(resp)
}

pub(super) fn pace_http_export<S>(
    stream: S,
    pacing: Option<(
        crate::rate_limit::RateLimiter,
        SessionStore,
        sift_protocol::SessionId,
        i64,
        Option<i64>,
        bool,
    )>,
    shutdown: crate::shutdown::Shutdown,
    query_guard: std::sync::Arc<crate::shutdown::QueryGuard>,
    mut audit: query_audit::QueryOperationAudit,
    permit: tokio::sync::OwnedSemaphorePermit,
) -> impl futures::Stream<Item = Result<bytes::Bytes, std::io::Error>> + Send + 'static
where
    S: futures::Stream<Item = Result<bytes::Bytes, std::io::Error>> + Send + 'static,
{
    async_stream::stream! {
        let _query_guard = query_guard;
        let _permit = permit;
        futures::pin_mut!(stream);
        loop {
            let next = tokio::select! {
                _ = shutdown.wait_for_drain_start() => {
                    audit.record(OperationStatus::Failed, Some("service_draining".into()), None,
                        Some("export interrupted by server drain; native outcome is uncertain".into()));
                    yield Err(std::io::Error::other("export interrupted by server drain"));
                    return;
                }
                chunk = stream.next() => chunk,
            };
            let Some(chunk) = next else {
                audit.record(OperationStatus::Succeeded, None, None, None);
                return;
            };
            match chunk {
                Ok(bytes) => {
                    let mut bytes = bytes;
                    if let Some((limiter, sessions, session, principal, tenant, trusted_local)) = &pacing {
                        let retained = match sessions
                            .reserve_session_retained_bytes(*session, bytes.len())
                        {
                            Ok(guard) => guard,
                            Err(error) => {
                                audit.failure(&error);
                                yield Err(std::io::Error::other(error.to_string()));
                                return;
                            }
                        };
                        bytes = bytes::Bytes::from_owner(RetainedStreamBytes {
                            bytes,
                            _guard: retained,
                        });
                        let paced = tokio::select! {
                            _ = shutdown.wait_for_drain_start() => None,
                            result = limiter.pace_bytes(
                                *principal,
                                *tenant,
                                bytes.len(),
                                *trusted_local,
                                std::time::Duration::from_secs(5),
                            ) => Some(result),
                        };
                        let Some(paced) = paced else {
                            audit.record(OperationStatus::Failed, Some("service_draining".into()), None,
                                Some("export interrupted by server drain; native outcome is uncertain".into()));
                            yield Err(std::io::Error::other("export interrupted by server drain"));
                            return;
                        };
                        if let Err(retry_after_secs) = paced {
                            sessions.push_operation_full(
                                Operation::RateLimitRejected {
                                    class: sift_protocol::RateLimitClass::StreamBytes,
                                    route: "/v1/sessions/:id/connections/:id/export".into(),
                                    tenant_id: *tenant,
                                },
                                OperationStatus::Failed,
                                Some(*principal),
                                Some("rate_limited".into()),
                                None,
                                Some(format!("retry after {retry_after_secs}s")),
                            );
                            audit.record(OperationStatus::Failed, Some("rate_limited".into()), None,
                                Some("export byte pacing refused delivery".into()));
                            yield Err(std::io::Error::other(format!(
                                "stream byte rate limit exceeded; retry after {retry_after_secs}s"
                            )));
                            return;
                        }
                    }
                    yield Ok(bytes);
                }
                Err(error) => {
                    let code = error.get_ref()
                        .and_then(|error| error.downcast_ref::<sift_protocol::DriverError>())
                        .map(|error| error.code.to_string())
                        .unwrap_or_else(|| "export_encoding_failed".into());
                    audit.record(OperationStatus::Failed, Some(code), None,
                        Some("export stream failed; native outcome may be uncertain".into()));
                    yield Err(error);
                    return;
                }
            }
        }
    }
}

struct RetainedStreamBytes {
    bytes: bytes::Bytes,
    _guard: Option<crate::resources::ResourceGuard>,
}

impl AsRef<[u8]> for RetainedStreamBytes {
    fn as_ref(&self) -> &[u8] {
        &self.bytes
    }
}
