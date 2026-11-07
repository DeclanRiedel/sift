//! Export observation audit and HTTP body ownership.
use super::*;

pub(super) async fn export_query(
    State(state): State<AppState>,
    auth: Option<Extension<AuthContext>>,
    Path((id, conn_id)): Path<(sift_protocol::SessionId, sift_protocol::ConnectionId)>,
    Json(req): Json<sift_protocol::ExportRequest>,
) -> ApiResult<Response> {
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
    let correlation = crate::correlation::current().unwrap_or_else(crate::correlation::generate);
    let mut audit = crate::correlation::sync_scope(correlation.clone(), || {
        query_audit::QueryOperationAudit::new(
            state.sessions.clone(),
            Operation::ExportQuery {
                session: id,
                connection: conn_id,
            },
            actor,
        )
    });
    let prepared = (|| {
        if state.shutdown.is_draining() {
            return Err(ApiError::ServiceDraining);
        }
        let permit = Arc::new(state.sessions.reserve_query_settlement()?);
        let source = state.sessions.authorize_connection_operation(
            id,
            conn_id,
            sift_protocol::OperationKind::ExportQuery,
            Some(&req.sql),
            &[],
        )?;
        let scope = ws_query::actor_scope(auth.as_ref().map(|Extension(auth)| auth), &source)?;
        let effect = crate::sql_policy::statement_effect(source.driver.semantic_engine(), &req.sql);
        let ledger = native_settlement::NativeLedger {
            scope,
            effect,
            workspace: None,
            room: None,
            transaction: None,
            source: None,
            fingerprint: crate::fingerprint::sql(&req.sql),
            workflow: "direct_export",
        };
        let tenant = match source.provenance {
            crate::session::ConnectionProvenance::Managed { tenant_id, .. } => Some(tenant_id.0),
            _ => None,
        };
        let sessions = state.sessions.bind_source(id, conn_id, source)?;
        Ok((permit, sessions, ledger, tenant))
    })();
    let (permit, sessions, ledger, tenant) = match prepared {
        Ok(prepared) => prepared,
        Err(error) => {
            audit.failure(&error);
            return Err(error);
        }
    };
    let guard = Arc::new(state.shutdown.track_query());
    tokio::spawn(crate::correlation::scope(
        correlation,
        crate::shutdown::scope_query(Some(guard.clone()), async move {
            use crate::session::native_execution::{self, NativeExecution, StartupGuard};
            let observation = Arc::new(NativeExecution::default());
            let startup = StartupGuard(observation.clone());
            ledger.spawn(
                state.clone(),
                observation.clone(),
                permit.clone(),
                crate::shutdown::current_query_guard(),
            );
            let format = req.format;
            let result =
                native_execution::scope(observation, sessions.export_stream(id, conn_id, req))
                    .await;
            drop(startup);
            let stream = match result {
                Ok(stream) => stream,
                Err(error) => {
                    audit.failure(&error);
                    return Err(error);
                }
            };
            let pacing = auth.map(|Extension(auth)| {
                (
                    state.auth.rate_limiter.clone(),
                    sessions,
                    id,
                    auth.principal_id.0,
                    tenant,
                    auth.trusted_local,
                )
            });
            let body = axum::body::Body::from_stream(pace_http_export(
                stream,
                pacing,
                state.shutdown.clone(),
                guard,
                audit,
                permit,
            ));
            let mut response = body.into_response();
            response.headers_mut().insert(
                axum::http::header::CONTENT_TYPE,
                crate::export::content_type(format).parse().unwrap(),
            );
            Ok(response)
        }),
    ))
    .await
    .map_err(|_| ApiError::Internal("export startup owner failed".into()))?
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
    permit: Arc<tokio::sync::OwnedSemaphorePermit>,
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
                        .map(|error| crate::error::audit_code(&error.code))
                        .unwrap_or_else(|| "export_encoding_failed".into());
                    audit.record(OperationStatus::Failed, Some(code), None,
                        Some("export stream failed; native outcome may be uncertain".into()));
                    yield Err(std::io::Error::other("export stream failed"));
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

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn native_settlement_admission_refusal_is_audited() {
        let state = AppState {
            sessions: SessionStore::new(crate::DriverRegistry::new()),
            rooms: Default::default(),
            shutdown: Default::default(),
            auth: Default::default(),
            metadata: None,
        };
        let mut permits = Vec::new();
        while let Ok(permit) = state.sessions.reserve_query_settlement() {
            permits.push(permit);
        }
        let result = export_query(
            State(state.clone()),
            None,
            Path((sift_protocol::SessionId(1), sift_protocol::ConnectionId(2))),
            Json(sift_protocol::ExportRequest {
                sql: "SELECT 1".into(),
                params: vec![],
                format: sift_protocol::ExportFormat::Csv,
                header: false,
                null_display: None,
            }),
        )
        .await;
        assert!(result.is_err());
        let audit = state.sessions.list_operations();
        assert_eq!(audit.len(), 1);
        assert!(matches!(audit[0].operation, Operation::ExportQuery { .. }));
        assert_eq!(audit[0].status, OperationStatus::Failed);
        assert_eq!(state.shutdown.in_flight(), 0);
    }
}
