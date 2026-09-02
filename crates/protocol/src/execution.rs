//! Versioned query-execution event contract (ADR-053).
//!
//! `Page` remains the protocol-v1 compatibility surface. New execution work
//! uses explicit execution, statement, and result-set identities so a client
//! can retain every row set and distinguish row-set completion from batch
//! completion.

use serde::{Deserialize, Serialize};

use crate::{ColumnMetadata, DriverError, DriverWarning, Page, Row};

pub const EXECUTION_EVENT_VERSION: u16 = 2;
pub const MAX_EXECUTION_NOTICE_BYTES: usize = 16 * 1024;
pub const MAX_COMMAND_TAG_BYTES: usize = 256;
pub const MAX_NATIVE_PROGRESS_PHASE_BYTES: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(transparent)]
pub struct ExecutionId(pub u64);

impl std::fmt::Display for ExecutionId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(formatter)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(transparent)]
pub struct ResultSetId(pub u64);

impl std::fmt::Display for ResultSetId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(formatter)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionPhase {
    Queued,
    WaitingForConnection,
    Preparing,
    Executing,
    WaitingForFirstRow,
    Streaming,
    Spilling,
    Cancelling,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionNoticeSeverity {
    Information,
    Warning,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum NativeProgressSource {
    PostgresStatistics,
    SqlServerRequest,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct NativeExecutionProgress {
    pub source: NativeProgressSource,
    /// Hundredths of one percent, bounded to `0..=10_000` by the server.
    pub basis_points: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub estimated_remaining_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ExecutionProgress {
    pub phase: ExecutionPhase,
    pub elapsed_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub statement_ordinal: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub statement_count: Option<u32>,
    pub result_sets_seen: u32,
    pub rows_received: u64,
    pub bytes_received: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native: Option<NativeExecutionProgress>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ResultSetSummaryV2 {
    pub result_set_id: ResultSetId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub statement_ordinal: Option<u32>,
    pub row_count: u64,
    pub duration_ms: u64,
    #[serde(default)]
    pub warnings: Vec<DriverWarning>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct CommandSummaryV2 {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub statement_ordinal: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub affected_rows: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command_tag: Option<String>,
    pub duration_ms: u64,
    #[serde(default)]
    pub warnings: Vec<DriverWarning>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ExecutionSummaryV2 {
    pub duration_ms: u64,
    pub result_set_count: u32,
    pub command_count: u32,
    pub rows_received: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub affected_rows: Option<u64>,
    pub warning_count: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ExecutionEventV2 {
    ExecutionStarted {
        execution_id: ExecutionId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        statement_count: Option<u32>,
    },
    StatementStarted {
        execution_id: ExecutionId,
        statement_ordinal: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        statement_id: Option<String>,
    },
    ResultSetStarted {
        execution_id: ExecutionId,
        result_set_id: ResultSetId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        statement_ordinal: Option<u32>,
        columns: Vec<ColumnMetadata>,
    },
    Rows {
        execution_id: ExecutionId,
        result_set_id: ResultSetId,
        rows: Vec<Row>,
    },
    ResultSetCompleted {
        execution_id: ExecutionId,
        summary: ResultSetSummaryV2,
    },
    CommandCompleted {
        execution_id: ExecutionId,
        summary: CommandSummaryV2,
    },
    Notice {
        execution_id: ExecutionId,
        severity: ExecutionNoticeSeverity,
        message: String,
    },
    Progress {
        execution_id: ExecutionId,
        progress: ExecutionProgress,
    },
    ExecutionCompleted {
        execution_id: ExecutionId,
        summary: ExecutionSummaryV2,
    },
    Error {
        execution_id: ExecutionId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        statement_ordinal: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        result_set_id: Option<ResultSetId>,
        error: DriverError,
    },
}

impl ExecutionEventV2 {
    pub const fn execution_id(&self) -> ExecutionId {
        match self {
            Self::ExecutionStarted { execution_id, .. }
            | Self::StatementStarted { execution_id, .. }
            | Self::ResultSetStarted { execution_id, .. }
            | Self::Rows { execution_id, .. }
            | Self::ResultSetCompleted { execution_id, .. }
            | Self::CommandCompleted { execution_id, .. }
            | Self::Notice { execution_id, .. }
            | Self::Progress { execution_id, .. }
            | Self::ExecutionCompleted { execution_id, .. }
            | Self::Error { execution_id, .. } => *execution_id,
        }
    }

    pub fn validate(&self) -> Result<(), ExecutionContractError> {
        match self {
            Self::CommandCompleted { summary, .. }
                if summary
                    .command_tag
                    .as_ref()
                    .is_some_and(|tag| tag.len() > MAX_COMMAND_TAG_BYTES) =>
            {
                Err(ExecutionContractError::CommandTagTooLong)
            }
            Self::Notice { message, .. } if message.len() > MAX_EXECUTION_NOTICE_BYTES => {
                Err(ExecutionContractError::NoticeTooLong)
            }
            Self::Progress {
                progress:
                    ExecutionProgress {
                        native: Some(native),
                        ..
                    },
                ..
            } if native.basis_points > 10_000 => {
                Err(ExecutionContractError::NativeProgressOutOfRange)
            }
            Self::Progress {
                progress:
                    ExecutionProgress {
                        native: Some(native),
                        ..
                    },
                ..
            } if native
                .phase
                .as_ref()
                .is_some_and(|phase| phase.len() > MAX_NATIVE_PROGRESS_PHASE_BYTES) =>
            {
                Err(ExecutionContractError::NativePhaseTooLong)
            }
            _ => Ok(()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ExecutionContractError {
    #[error("execution notice exceeds its wire limit")]
    NoticeTooLong,
    #[error("execution command tag exceeds its wire limit")]
    CommandTagTooLong,
    #[error("native execution progress is outside 0..=10000 basis points")]
    NativeProgressOutOfRange,
    #[error("native execution phase exceeds its wire limit")]
    NativePhaseTooLong,
}

/// Protocol-v1 page projection for clients that explicitly requested the
/// compatibility surface. Extra result sets are never silently merged into the
/// first one.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct LegacyExecutionProjection {
    pub pages: Vec<Page>,
    pub truncated_extra_results: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ExecutionProjectionError {
    #[error("execution event stream did not start with execution_started")]
    MissingStart,
    #[error("execution event stream mixed execution ids")]
    MixedExecutionIds,
    #[error("execution event stream contains rows outside their result set")]
    RowsOutsideResultSet,
    #[error("execution event stream completed a different result set")]
    ResultSetMismatch,
    #[error("execution event stream has multiple terminal events")]
    MultipleTerminalEvents,
    #[error("execution event stream did not terminate")]
    MissingTerminalEvent,
    #[error("execution event payload violates its wire bounds")]
    InvalidPayload,
}

/// Validate ordered lifecycle and project the first result set into protocol-v1
/// pages. Intended for the legacy HTTP/WS adapters during execution-v2 rollout.
pub fn project_legacy_first_result(
    events: impl IntoIterator<Item = ExecutionEventV2>,
) -> Result<LegacyExecutionProjection, ExecutionProjectionError> {
    let mut events = events.into_iter();
    let Some(first) = events.next() else {
        return Err(ExecutionProjectionError::MissingStart);
    };
    let ExecutionEventV2::ExecutionStarted { execution_id, .. } = first else {
        return Err(ExecutionProjectionError::MissingStart);
    };

    let mut pages = Vec::new();
    let mut first_result = None;
    let mut open_result = None;
    let mut truncated_extra_results = false;
    let mut terminal = false;
    let mut command_affected_rows = None;
    let mut command_warnings = Vec::new();

    for event in events {
        event
            .validate()
            .map_err(|_| ExecutionProjectionError::InvalidPayload)?;
        if event.execution_id() != execution_id {
            return Err(ExecutionProjectionError::MixedExecutionIds);
        }
        if terminal {
            return Err(ExecutionProjectionError::MultipleTerminalEvents);
        }
        match event {
            ExecutionEventV2::ExecutionStarted { .. } => {
                return Err(ExecutionProjectionError::MultipleTerminalEvents);
            }
            ExecutionEventV2::ResultSetStarted {
                result_set_id,
                columns,
                ..
            } => {
                if open_result.is_some() {
                    return Err(ExecutionProjectionError::ResultSetMismatch);
                }
                open_result = Some(result_set_id);
                if first_result.is_none() {
                    first_result = Some(result_set_id);
                    pages.push(Page::NextResult { columns });
                } else {
                    truncated_extra_results = true;
                }
            }
            ExecutionEventV2::Rows {
                result_set_id,
                rows,
                ..
            } => {
                if open_result != Some(result_set_id) {
                    return Err(ExecutionProjectionError::RowsOutsideResultSet);
                }
                if first_result == Some(result_set_id) {
                    pages.push(Page::Rows { rows });
                }
            }
            ExecutionEventV2::ResultSetCompleted { summary, .. } => {
                if open_result != Some(summary.result_set_id) {
                    return Err(ExecutionProjectionError::ResultSetMismatch);
                }
                open_result = None;
                if first_result == Some(summary.result_set_id) {
                    pages.push(Page::Done {
                        affected_rows: None,
                        warnings: summary.warnings,
                    });
                }
            }
            ExecutionEventV2::CommandCompleted { summary, .. } => {
                if first_result.is_none() {
                    command_affected_rows = summary.affected_rows;
                    command_warnings.extend(summary.warnings);
                }
            }
            ExecutionEventV2::ExecutionCompleted { .. } => {
                if open_result.is_some() {
                    return Err(ExecutionProjectionError::ResultSetMismatch);
                }
                if first_result.is_none() {
                    pages.push(Page::Done {
                        affected_rows: command_affected_rows,
                        warnings: std::mem::take(&mut command_warnings),
                    });
                }
                terminal = true;
            }
            ExecutionEventV2::Error { error, .. } => {
                pages.push(Page::Error { error });
                terminal = true;
            }
            ExecutionEventV2::StatementStarted { .. }
            | ExecutionEventV2::Notice { .. }
            | ExecutionEventV2::Progress { .. } => {}
        }
    }

    if !terminal {
        return Err(ExecutionProjectionError::MissingTerminalEvent);
    }
    Ok(LegacyExecutionProjection {
        pages,
        truncated_extra_results,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{PrimitiveType, TypeRef, Value};

    fn columns(name: &str) -> Vec<ColumnMetadata> {
        vec![ColumnMetadata::new(
            name,
            TypeRef::Primitive(PrimitiveType::Int64),
        )]
    }

    #[test]
    fn wire_tags_are_stable_and_additive_fields_are_omitted() {
        let json = serde_json::to_value(ExecutionEventV2::ExecutionStarted {
            execution_id: ExecutionId(7),
            statement_count: None,
        })
        .unwrap();
        assert_eq!(json["kind"], "execution_started");
        assert_eq!(json["execution_id"], 7);
        assert!(json.get("statement_count").is_none());
        assert_eq!(EXECUTION_EVENT_VERSION, 2);

        let decoded: ExecutionEventV2 = serde_json::from_value(serde_json::json!({
            "kind": "execution_started",
            "execution_id": 7,
            "statement_count": 2,
            "future_additive_field": true
        }))
        .unwrap();
        assert_eq!(decoded.execution_id(), ExecutionId(7));
    }

    #[test]
    fn payload_bounds_reject_invalid_native_percentage() {
        let event = ExecutionEventV2::Progress {
            execution_id: ExecutionId(1),
            progress: ExecutionProgress {
                phase: ExecutionPhase::Executing,
                elapsed_ms: 1,
                statement_ordinal: None,
                statement_count: None,
                result_sets_seen: 0,
                rows_received: 0,
                bytes_received: 0,
                native: Some(NativeExecutionProgress {
                    source: NativeProgressSource::SqlServerRequest,
                    basis_points: 10_001,
                    phase: None,
                    estimated_remaining_ms: None,
                }),
            },
        };
        assert_eq!(
            event.validate(),
            Err(ExecutionContractError::NativeProgressOutOfRange)
        );
    }

    #[test]
    fn legacy_projection_keeps_first_result_and_reports_truncation() {
        let execution_id = ExecutionId(4);
        let first = ResultSetId(10);
        let second = ResultSetId(11);
        let projected = project_legacy_first_result([
            ExecutionEventV2::ExecutionStarted {
                execution_id,
                statement_count: Some(2),
            },
            ExecutionEventV2::ResultSetStarted {
                execution_id,
                result_set_id: first,
                statement_ordinal: Some(0),
                columns: columns("one"),
            },
            ExecutionEventV2::Rows {
                execution_id,
                result_set_id: first,
                rows: vec![Row::new(vec![Value::Int64(1)])],
            },
            ExecutionEventV2::ResultSetCompleted {
                execution_id,
                summary: ResultSetSummaryV2 {
                    result_set_id: first,
                    statement_ordinal: Some(0),
                    row_count: 1,
                    duration_ms: 2,
                    warnings: Vec::new(),
                },
            },
            ExecutionEventV2::ResultSetStarted {
                execution_id,
                result_set_id: second,
                statement_ordinal: Some(1),
                columns: columns("two"),
            },
            ExecutionEventV2::Rows {
                execution_id,
                result_set_id: second,
                rows: vec![Row::new(vec![Value::Int64(2)])],
            },
            ExecutionEventV2::ResultSetCompleted {
                execution_id,
                summary: ResultSetSummaryV2 {
                    result_set_id: second,
                    statement_ordinal: Some(1),
                    row_count: 1,
                    duration_ms: 3,
                    warnings: Vec::new(),
                },
            },
            ExecutionEventV2::ExecutionCompleted {
                execution_id,
                summary: ExecutionSummaryV2 {
                    duration_ms: 5,
                    result_set_count: 2,
                    command_count: 0,
                    rows_received: 2,
                    affected_rows: None,
                    warning_count: 0,
                },
            },
        ])
        .unwrap();

        assert!(projected.truncated_extra_results);
        assert_eq!(projected.pages.len(), 3);
        assert!(matches!(
            &projected.pages[0],
            Page::NextResult { columns } if columns[0].name == "one"
        ));
        assert!(matches!(
            &projected.pages[1],
            Page::Rows { rows } if rows.len() == 1
        ));
        assert!(matches!(&projected.pages[2], Page::Done { .. }));
    }

    #[test]
    fn legacy_projection_rejects_cross_result_rows() {
        let execution_id = ExecutionId(4);
        let error = project_legacy_first_result([
            ExecutionEventV2::ExecutionStarted {
                execution_id,
                statement_count: None,
            },
            ExecutionEventV2::ResultSetStarted {
                execution_id,
                result_set_id: ResultSetId(1),
                statement_ordinal: None,
                columns: columns("one"),
            },
            ExecutionEventV2::Rows {
                execution_id,
                result_set_id: ResultSetId(2),
                rows: Vec::new(),
            },
        ])
        .unwrap_err();
        assert_eq!(error, ExecutionProjectionError::RowsOutsideResultSet);
    }

    #[test]
    fn command_only_projection_preserves_affected_rows() {
        let execution_id = ExecutionId(9);
        let projected = project_legacy_first_result([
            ExecutionEventV2::ExecutionStarted {
                execution_id,
                statement_count: Some(1),
            },
            ExecutionEventV2::CommandCompleted {
                execution_id,
                summary: CommandSummaryV2 {
                    statement_ordinal: Some(0),
                    affected_rows: Some(3),
                    command_tag: Some("UPDATE".into()),
                    duration_ms: 1,
                    warnings: Vec::new(),
                },
            },
            ExecutionEventV2::ExecutionCompleted {
                execution_id,
                summary: ExecutionSummaryV2 {
                    duration_ms: 1,
                    result_set_count: 0,
                    command_count: 1,
                    rows_received: 0,
                    affected_rows: Some(3),
                    warning_count: 0,
                },
            },
        ])
        .unwrap();
        assert!(matches!(
            projected.pages.as_slice(),
            [Page::Done {
                affected_rows: Some(3),
                ..
            }]
        ));
    }
}
