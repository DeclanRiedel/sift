use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use sift_metadata::{
    ConnectionProfileId, MetadataStore, NewQueryHistory, PrincipalId, QueryStatus, RoomId, TenantId,
};
use sift_protocol::{
    BeginTransactionRequest, EndTransactionRequest, ExecuteRequestHttp, OpenSessionRequest,
    ProviderId, RunConfiguration, RunErrorPolicy, RunId, RunState, RunStepState,
    RunTransactionPolicy, RunVariableKind, TxHandleRef, Value,
};
use tokio_util::sync::CancellationToken;

use crate::error::{ApiError, ApiResult};
use crate::http::AppState;
use crate::session::SessionStore;

const MAX_RUN_TIMEOUT_SECS: u64 = 60 * 60;

struct RunSessionGuard {
    sessions: SessionStore,
    session_id: sift_protocol::SessionId,
}

impl Drop for RunSessionGuard {
    fn drop(&mut self) {
        let _ = self.sessions.close_session(self.session_id);
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolvedRunScript {
    pub node_id: sift_protocol::WorkspaceNodeId,
    pub template_sql: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolvedRunPayload {
    pub configuration: RunConfiguration,
    pub scripts: Vec<ResolvedRunScript>,
}

pub fn validate_timeout(value: Option<u64>) -> ApiResult<Duration> {
    let seconds = value.unwrap_or(15 * 60);
    if !(1..=MAX_RUN_TIMEOUT_SECS).contains(&seconds) {
        return Err(ApiError::BadRequest(
            "run timeout must be between 1 and 3600 seconds".into(),
        ));
    }
    Ok(Duration::from_secs(seconds))
}

pub struct RunInvocation {
    pub actor: PrincipalId,
    pub room_id: RoomId,
    pub tenant_id: TenantId,
    pub configuration: RunConfiguration,
    pub run_id: RunId,
    pub variables: BTreeMap<String, serde_json::Value>,
    pub timeout: Duration,
}

pub fn spawn_run(state: AppState, metadata: MetadataStore, invocation: RunInvocation) {
    let cancellation = state.rooms.register_run(invocation.run_id.0);
    tokio::spawn(async move {
        let terminal = execute_run(
            &state,
            &metadata,
            invocation.actor,
            invocation.room_id,
            invocation.tenant_id,
            &invocation.configuration,
            invocation.run_id,
            invocation.variables,
            invocation.timeout,
            cancellation,
        )
        .await;
        if let Err(error) = terminal {
            tracing::warn!(run_id = invocation.run_id.0, error = %error, "foreground run executor failed");
            let _ = metadata.transition_run(
                invocation.run_id,
                invocation.actor,
                &[
                    RunState::Queued,
                    RunState::Admitted,
                    RunState::Preparing,
                    RunState::Running,
                ],
                RunState::Failed,
            );
        }
        if let Ok(record) =
            metadata.run_execution_for_principal(invocation.run_id, invocation.actor, false)
        {
            state.rooms.publish_presence(
                invocation.room_id.0,
                sift_protocol::RoomServerMessage::RunChanged {
                    workspace_id: invocation.configuration.workspace_id.0,
                    run_id: invocation.run_id.0,
                    state: record.run.state,
                    revision: record.run.revision,
                },
            );
        }
        state.rooms.finish_run(invocation.run_id.0);
    });
}

#[allow(clippy::too_many_arguments)]
async fn execute_run(
    state: &AppState,
    metadata: &MetadataStore,
    actor: PrincipalId,
    room_id: RoomId,
    tenant_id: TenantId,
    configuration: &RunConfiguration,
    run_id: RunId,
    variables: BTreeMap<String, serde_json::Value>,
    timeout: Duration,
    cancellation: CancellationToken,
) -> ApiResult<()> {
    metadata.transition_run(run_id, actor, &[RunState::Queued], RunState::Admitted)?;
    metadata.append_run_log(run_id, "info", "run admitted")?;
    metadata.transition_run(run_id, actor, &[RunState::Admitted], RunState::Preparing)?;
    let record = metadata.run_execution_for_principal(run_id, actor, true)?;
    let payload: ResolvedRunPayload = serde_json::from_str(&record.resolved_scripts_json)
        .map_err(|_| ApiError::Internal("stored run manifest is invalid".into()))?;
    if payload.configuration != *configuration {
        return Err(ApiError::Internal(
            "run configuration snapshot does not match its executor".into(),
        ));
    }
    let scripts = payload.scripts;
    validate_variables(configuration, &variables)?;
    let profile_id = ConnectionProfileId(configuration.connection_profile_id);
    let profile = metadata.get_connection_profile_for_principal(profile_id, actor)?;
    if profile.tenant_id != tenant_id {
        return Err(ApiError::Forbidden(
            "run target profile belongs to another tenant".into(),
        ));
    }
    let (connection_configuration, credentials) = metadata
        .resolve_provider_connection(tenant_id, actor, profile_id)
        .await?;
    if cancellation.is_cancelled() {
        metadata.transition_run(run_id, actor, &[RunState::Preparing], RunState::Cancelled)?;
        return Ok(());
    }
    let session = state.sessions.open_session_with_owner(
        OpenSessionRequest {
            tag: Some(format!("run:{}", run_id.0)),
            tenant_id: Some(tenant_id.0),
        },
        Some(actor),
        Some(tenant_id),
        true,
    )?;
    let _session_guard = RunSessionGuard {
        sessions: state.sessions.clone(),
        session_id: session.id,
    };
    let connection = match state
        .sessions
        .open_managed_connection(
            session.id,
            profile.provider_id.clone(),
            connection_configuration,
            credentials,
            actor,
            tenant_id,
            profile_id,
            profile.policy.revision,
            false,
        )
        .await
    {
        Ok(connection) => connection,
        Err(error) => return Err(error),
    };
    if let Some(target_schema) = configuration.target_schema.as_ref() {
        let mut scope = sift_protocol::SchemaScope::shallow();
        scope.filter = Some(sift_protocol::SchemaFilter {
            catalogs: None,
            schemas: Some(vec![target_schema.clone()]),
            kinds: None,
            name_pattern: None,
        });
        let snapshot = state
            .sessions
            .schema(session.id, connection.id, scope)
            .await?;
        if !snapshot.trees.iter().any(|catalog| {
            catalog
                .schemas
                .iter()
                .any(|schema| schema.name == *target_schema)
        }) {
            return Err(ApiError::BadRequest(
                "run target schema was not found".into(),
            ));
        }
        metadata.append_run_log(run_id, "info", "target schema resolved")?;
    }
    for pre_task in &configuration.pre_tasks {
        if cancellation.is_cancelled() {
            metadata.transition_run(run_id, actor, &[RunState::Preparing], RunState::Cancelled)?;
            return Ok(());
        }
        match pre_task {
            sift_protocol::RunPreTask::PingTarget => {
                state.sessions.ping(session.id, connection.id).await?;
            }
            sift_protocol::RunPreTask::RefreshSchema => {
                state
                    .sessions
                    .schema(
                        session.id,
                        connection.id,
                        sift_protocol::SchemaScope::shallow(),
                    )
                    .await?;
            }
        }
        metadata.append_run_log(run_id, "info", "pre-task succeeded")?;
    }
    metadata.transition_run(run_id, actor, &[RunState::Preparing], RunState::Running)?;
    metadata.append_run_log(run_id, "info", "run started")?;
    let deadline = Instant::now() + timeout;
    let mut all_scripts_tx = if configuration.transaction_policy == RunTransactionPolicy::AllScripts
    {
        Some(
            state
                .sessions
                .begin_transaction_as(
                    session.id,
                    BeginTransactionRequest {
                        connection: connection.id,
                        mode: sift_protocol::TxMode::default(),
                    },
                    sift_protocol::OperationKind::ExecuteRun,
                )
                .await?,
        )
    } else {
        None
    };
    let mut failed = false;
    let mut cancelled = false;
    let mut outcome_unknown = false;
    for (ordinal, script) in scripts.iter().enumerate() {
        if cancellation.is_cancelled() || Instant::now() >= deadline {
            cancelled = true;
            break;
        }
        metadata.update_run_step(run_id, ordinal as u32, RunStepState::Running, None, None)?;
        let (sql, params) = substitute_variables(
            &script.template_sql,
            &configuration.variables,
            &variables,
            &profile.provider_id,
        )?;
        if let Some(recipe_id) = configuration.scripts[ordinal].transfer_recipe_id {
            let recipe = metadata.transfer_recipe_for_principal(recipe_id, actor, true)?;
            let remaining = deadline.saturating_duration_since(Instant::now());
            let execute = crate::transfer::execute_recipe(
                &state.sessions,
                metadata,
                actor,
                &recipe,
                sift_metadata::http::ExecuteTransferRecipeRequest {
                    session_id: session.id,
                    connection_id: connection.id,
                    sql: Some(sql),
                    params,
                    data: None,
                    table: None,
                    sheet: None,
                    create_table: false,
                    conflict_policy: None,
                },
            );
            let result = tokio::select! {
                result = execute => Some(result),
                _ = cancellation.cancelled() => None,
                _ = tokio::time::sleep(remaining) => None,
            };
            match result {
                Some(Ok(sift_protocol::TransferExecutionResult::Artifact { artifact })) => {
                    metadata.update_run_step(
                        run_id,
                        ordinal as u32,
                        RunStepState::Succeeded,
                        None,
                        None,
                    )?;
                    metadata.append_run_log(
                        run_id,
                        "info",
                        &format!("transfer artifact {} published", artifact.id.0),
                    )?;
                }
                Some(Ok(_)) | Some(Err(_)) => {
                    metadata.update_run_step(
                        run_id,
                        ordinal as u32,
                        RunStepState::Failed,
                        None,
                        Some("transfer_failed"),
                    )?;
                    failed = true;
                }
                None => {
                    metadata.update_run_step(
                        run_id,
                        ordinal as u32,
                        RunStepState::Cancelled,
                        None,
                        None,
                    )?;
                    cancelled = true;
                }
            }
            if cancelled || (failed && configuration.error_policy == RunErrorPolicy::Stop) {
                break;
            }
            continue;
        }
        let per_script_tx = if configuration.transaction_policy == RunTransactionPolicy::PerScript {
            Some(
                state
                    .sessions
                    .begin_transaction_as(
                        session.id,
                        BeginTransactionRequest {
                            connection: connection.id,
                            mode: sift_protocol::TxMode::default(),
                        },
                        sift_protocol::OperationKind::ExecuteRun,
                    )
                    .await?,
            )
        } else {
            None
        };
        let transaction = per_script_tx.as_ref().or(all_scripts_tx.as_ref());
        let tx = transaction.map(|transaction| TxHandleRef {
            tx_id: transaction.tx_id,
            connection: connection.id,
            mode: transaction.mode,
        });
        let remaining = deadline.saturating_duration_since(Instant::now());
        let started = Instant::now();
        let execute = state.sessions.execute_http_as(
            session.id,
            ExecuteRequestHttp {
                connection: connection.id,
                sql,
                params,
                tx,
                room_id: Some(room_id.0),
                connection_profile_id: Some(profile_id.0),
                transform: None,
                source: None,
            },
            sift_protocol::OperationKind::ExecuteRun,
        );
        let result = tokio::select! {
            result = execute => Some(result),
            _ = cancellation.cancelled() => None,
            _ = tokio::time::sleep(remaining) => None,
        };
        let elapsed_ms = i64::try_from(started.elapsed().as_millis()).unwrap_or(i64::MAX);
        match result {
            Some(Ok(response)) => {
                if let Some(transaction) = per_script_tx {
                    state
                        .sessions
                        .commit_transaction_as(
                            session.id,
                            EndTransactionRequest {
                                connection: connection.id,
                                tx_id: transaction.tx_id,
                            },
                            sift_protocol::OperationKind::ExecuteRun,
                        )
                        .await?;
                }
                let rows = response.affected_rows.or(Some(response.rows.len() as u64));
                metadata.update_run_step(
                    run_id,
                    ordinal as u32,
                    RunStepState::Succeeded,
                    rows,
                    None,
                )?;
                record_query_history(
                    metadata,
                    actor,
                    room_id,
                    profile_id,
                    &script.template_sql,
                    QueryHistoryOutcome {
                        duration_ms: elapsed_ms,
                        rows,
                        status: QueryStatus::Ok,
                    },
                );
            }
            Some(Err(_)) => {
                if let Some(transaction) = per_script_tx {
                    let _ = state
                        .sessions
                        .rollback_transaction_as(
                            session.id,
                            EndTransactionRequest {
                                connection: connection.id,
                                tx_id: transaction.tx_id,
                            },
                            sift_protocol::OperationKind::ExecuteRun,
                        )
                        .await;
                }
                metadata.update_run_step(
                    run_id,
                    ordinal as u32,
                    RunStepState::Failed,
                    None,
                    Some("query_failed"),
                )?;
                metadata.append_run_log(run_id, "error", "script failed")?;
                record_query_history(
                    metadata,
                    actor,
                    room_id,
                    profile_id,
                    &script.template_sql,
                    QueryHistoryOutcome {
                        duration_ms: elapsed_ms,
                        rows: None,
                        status: QueryStatus::Error,
                    },
                );
                failed = true;
                if configuration.transaction_policy == RunTransactionPolicy::AllScripts
                    || configuration.error_policy == RunErrorPolicy::Stop
                {
                    break;
                }
            }
            None => {
                let close = state
                    .sessions
                    .close_connection(session.id, connection.id)
                    .await;
                outcome_unknown = close.is_err();
                cancelled = !outcome_unknown;
                metadata.update_run_step(
                    run_id,
                    ordinal as u32,
                    if outcome_unknown {
                        RunStepState::Failed
                    } else {
                        RunStepState::Cancelled
                    },
                    None,
                    outcome_unknown.then_some("outcome_unknown"),
                )?;
                break;
            }
        }
    }
    if let Some(transaction) = all_scripts_tx.take() {
        if failed || cancelled || outcome_unknown {
            let rollback = state
                .sessions
                .rollback_transaction_as(
                    session.id,
                    EndTransactionRequest {
                        connection: connection.id,
                        tx_id: transaction.tx_id,
                    },
                    sift_protocol::OperationKind::ExecuteRun,
                )
                .await;
            outcome_unknown |= rollback.is_err();
        } else {
            state
                .sessions
                .commit_transaction_as(
                    session.id,
                    EndTransactionRequest {
                        connection: connection.id,
                        tx_id: transaction.tx_id,
                    },
                    sift_protocol::OperationKind::ExecuteRun,
                )
                .await?;
        }
    }
    let terminal = if outcome_unknown {
        RunState::OutcomeUnknown
    } else if cancelled {
        RunState::Cancelled
    } else if failed {
        RunState::Failed
    } else {
        RunState::Succeeded
    };
    metadata.transition_run(run_id, actor, &[RunState::Running], terminal)?;
    metadata.append_run_log(
        run_id,
        if terminal == RunState::Succeeded {
            "info"
        } else {
            "warning"
        },
        match terminal {
            RunState::Succeeded => "run succeeded",
            RunState::Cancelled => "run cancelled",
            RunState::OutcomeUnknown => "run outcome is unknown",
            _ => "run failed",
        },
    )?;
    Ok(())
}

struct QueryHistoryOutcome {
    duration_ms: i64,
    rows: Option<u64>,
    status: QueryStatus,
}

fn record_query_history(
    metadata: &MetadataStore,
    actor: PrincipalId,
    room_id: RoomId,
    profile_id: ConnectionProfileId,
    template: &str,
    outcome: QueryHistoryOutcome,
) {
    let _ = metadata.record_query_history(NewQueryHistory {
        principal_id: actor,
        room_id: Some(room_id),
        connection_profile_id: Some(profile_id),
        sql_text: template.to_string(),
        duration_ms: Some(outcome.duration_ms),
        row_count: outcome.rows.and_then(|rows| i64::try_from(rows).ok()),
        status: outcome.status,
        error_code: None,
        error_message: None,
    });
}

fn validate_variables(
    configuration: &RunConfiguration,
    values: &BTreeMap<String, serde_json::Value>,
) -> ApiResult<()> {
    if values.keys().any(|name| {
        !configuration
            .variables
            .iter()
            .any(|item| &item.name == name)
    }) || configuration
        .variables
        .iter()
        .any(|item| item.required && !values.contains_key(&item.name))
    {
        return Err(ApiError::BadRequest(
            "run variables do not match the configuration schema".into(),
        ));
    }
    Ok(())
}

pub fn substitute_variables(
    template: &str,
    definitions: &[sift_protocol::RunVariableDefinition],
    values: &BTreeMap<String, serde_json::Value>,
    provider: &ProviderId,
) -> ApiResult<(String, Vec<Value>)> {
    let mut output = String::with_capacity(template.len());
    let mut params = Vec::new();
    let bytes = template.as_bytes();
    let mut index = 0;
    let mut single_quote = false;
    let mut double_quote = false;
    let mut line_comment = false;
    let mut block_comment = false;
    while index < bytes.len() {
        if line_comment {
            output.push(bytes[index] as char);
            if bytes[index] == b'\n' {
                line_comment = false;
            }
            index += 1;
            continue;
        }
        if block_comment {
            if bytes[index..].starts_with(b"*/") {
                output.push_str("*/");
                index += 2;
                block_comment = false;
            } else {
                output.push(bytes[index] as char);
                index += 1;
            }
            continue;
        }
        if !single_quote && !double_quote && bytes[index..].starts_with(b"--") {
            output.push_str("--");
            index += 2;
            line_comment = true;
            continue;
        }
        if !single_quote && !double_quote && bytes[index..].starts_with(b"/*") {
            output.push_str("/*");
            index += 2;
            block_comment = true;
            continue;
        }
        if bytes[index] == b'\'' && !double_quote {
            output.push('\'');
            if single_quote && bytes.get(index + 1) == Some(&b'\'') {
                output.push('\'');
                index += 2;
                continue;
            }
            single_quote = !single_quote;
            index += 1;
            continue;
        }
        if bytes[index] == b'"' && !single_quote {
            output.push('"');
            double_quote = !double_quote;
            index += 1;
            continue;
        }
        if !single_quote && !double_quote && bytes[index..].starts_with(b"{{") {
            let rest = &template[index + 2..];
            let end = rest
                .find("}}")
                .ok_or_else(|| ApiError::BadRequest("unterminated run variable".into()))?;
            let name = &rest[..end];
            let definition = definitions
                .iter()
                .find(|definition| definition.name == name)
                .ok_or_else(|| ApiError::BadRequest("undeclared run variable".into()))?;
            let value = values.get(name).unwrap_or(&serde_json::Value::Null);
            if definition.kind == RunVariableKind::Identifier {
                output.push_str(&quote_identifier(value, provider)?);
            } else {
                params.push(value_for_kind(value, definition.kind)?);
                let placeholder = if provider.as_str() == "sift/postgres" {
                    format!("${}", params.len())
                } else if provider.as_str() == "sift/sqlserver" {
                    format!("@P{}", params.len())
                } else {
                    return Err(ApiError::BadRequest(
                        "run variables require PostgreSQL or SQL Server".into(),
                    ));
                };
                output.push_str(&placeholder);
            }
            index += end + 4;
            continue;
        }
        output.push(bytes[index] as char);
        index += 1;
    }
    Ok((output, params))
}

fn value_for_kind(value: &serde_json::Value, kind: RunVariableKind) -> ApiResult<Value> {
    match kind {
        RunVariableKind::String | RunVariableKind::Secret => value
            .as_str()
            .map(|value| Value::Text(value.to_string()))
            .ok_or_else(|| ApiError::BadRequest("run variable must be a string".into())),
        RunVariableKind::Integer => value
            .as_i64()
            .map(Value::Int64)
            .ok_or_else(|| ApiError::BadRequest("run variable must be an integer".into())),
        RunVariableKind::Decimal => {
            let decimal = value
                .as_str()
                .ok_or_else(|| ApiError::BadRequest("decimal variable must be a string".into()))?;
            if decimal.is_empty()
                || decimal.len() > 128
                || decimal.parse::<f64>().is_err()
                || !decimal.bytes().all(|byte| {
                    byte.is_ascii_digit() || matches!(byte, b'+' | b'-' | b'.' | b'e' | b'E')
                })
            {
                return Err(ApiError::BadRequest("decimal variable is invalid".into()));
            }
            Ok(Value::Decimal(decimal.to_string()))
        }
        RunVariableKind::Boolean => value
            .as_bool()
            .map(Value::Bool)
            .ok_or_else(|| ApiError::BadRequest("run variable must be boolean".into())),
        RunVariableKind::Identifier => unreachable!("identifiers are substituted separately"),
    }
}

fn quote_identifier(value: &serde_json::Value, provider: &ProviderId) -> ApiResult<String> {
    let value = value
        .as_str()
        .ok_or_else(|| ApiError::BadRequest("identifier variable must be a string".into()))?;
    let parts = value.split('.').collect::<Vec<_>>();
    if parts.is_empty()
        || parts.iter().any(|part| {
            part.is_empty()
                || part.len() > 128
                || !part
                    .bytes()
                    .all(|byte| byte == b'_' || byte.is_ascii_alphanumeric())
        })
    {
        return Err(ApiError::BadRequest(
            "identifier variable is invalid".into(),
        ));
    }
    let (open, close) = if provider.as_str() == "sift/postgres" {
        ('"', '"')
    } else if provider.as_str() == "sift/sqlserver" {
        ('[', ']')
    } else {
        return Err(ApiError::BadRequest(
            "identifier variables require PostgreSQL or SQL Server".into(),
        ));
    };
    Ok(parts
        .into_iter()
        .map(|part| format!("{open}{part}{close}"))
        .collect::<Vec<_>>()
        .join("."))
}

pub fn manifest_digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use sift_protocol::{RunVariableDefinition, RunVariableKind};

    #[test]
    fn substitution_binds_values_and_ignores_comments_and_literals() {
        let definitions = vec![RunVariableDefinition {
            name: "id".into(),
            kind: RunVariableKind::Integer,
            required: true,
            persist_non_secret_value: false,
            secret_handle_present: false,
        }];
        let values = BTreeMap::from([("id".into(), serde_json::json!(7))]);
        let (sql, params) = substitute_variables(
            "select '{{id}}', id from users where id = {{id}} -- {{id}}",
            &definitions,
            &values,
            &ProviderId::new("sift/postgres").unwrap(),
        )
        .unwrap();
        assert_eq!(
            sql,
            "select '{{id}}', id from users where id = $1 -- {{id}}"
        );
        assert_eq!(params, vec![Value::Int64(7)]);
    }
}
