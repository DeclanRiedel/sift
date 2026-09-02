//! Normalization from the locked driver page stream to execution events.

use std::time::Instant;

use sift_protocol::{
    CommandSummaryV2, ExecutionEventV2, ExecutionId, ExecutionPhase, ExecutionProgress,
    ExecutionSummaryV2, Page, ResultSetId, ResultSetSummaryV2,
};

const PROGRESS_INTERVAL: std::time::Duration = std::time::Duration::from_millis(250);

pub struct ExecutionEventNormalizer {
    execution_id: ExecutionId,
    started_at: Instant,
    result_started_at: Option<Instant>,
    open_result: Option<ResultSetId>,
    next_result_ordinal: u64,
    result_set_count: u32,
    command_count: u32,
    rows_received: u64,
    bytes_received: u64,
    current_result_rows: u64,
    affected_rows: Option<u64>,
    warning_count: u32,
    started_sent: bool,
    terminal_sent: bool,
    last_progress_at: Option<Instant>,
}

impl ExecutionEventNormalizer {
    pub fn new(execution_id: ExecutionId) -> Self {
        Self {
            execution_id,
            started_at: Instant::now(),
            result_started_at: None,
            open_result: None,
            next_result_ordinal: 0,
            result_set_count: 0,
            command_count: 0,
            rows_received: 0,
            bytes_received: 0,
            current_result_rows: 0,
            affected_rows: None,
            warning_count: 0,
            started_sent: false,
            terminal_sent: false,
            last_progress_at: None,
        }
    }

    pub fn events_for_page(&mut self, page: Page) -> Vec<ExecutionEventV2> {
        if self.terminal_sent {
            return Vec::new();
        }
        let mut events = Vec::with_capacity(4);
        self.bytes_received = self.bytes_received.saturating_add(
            serde_json::to_vec(&page)
                .ok()
                .and_then(|bytes| u64::try_from(bytes.len()).ok())
                .unwrap_or(0),
        );
        if !self.started_sent {
            self.started_sent = true;
            events.push(ExecutionEventV2::ExecutionStarted {
                execution_id: self.execution_id,
                statement_count: None,
            });
        }

        match page {
            Page::NextResult { columns } => {
                self.complete_open_result(Vec::new(), &mut events);
                let result_set_id = ResultSetId(self.next_result_ordinal);
                self.next_result_ordinal = self.next_result_ordinal.saturating_add(1);
                self.result_set_count = self.result_set_count.saturating_add(1);
                self.open_result = Some(result_set_id);
                self.current_result_rows = 0;
                self.result_started_at = Some(Instant::now());
                events.push(ExecutionEventV2::ResultSetStarted {
                    execution_id: self.execution_id,
                    result_set_id,
                    statement_ordinal: None,
                    columns,
                });
                self.push_progress(ExecutionPhase::WaitingForFirstRow, &mut events);
            }
            Page::Rows { rows } => {
                let Some(result_set_id) = self.open_result else {
                    return events;
                };
                self.rows_received = self
                    .rows_received
                    .saturating_add(u64::try_from(rows.len()).unwrap_or(u64::MAX));
                self.current_result_rows = self
                    .current_result_rows
                    .saturating_add(u64::try_from(rows.len()).unwrap_or(u64::MAX));
                events.push(ExecutionEventV2::Rows {
                    execution_id: self.execution_id,
                    result_set_id,
                    rows,
                });
                self.push_progress(ExecutionPhase::Streaming, &mut events);
            }
            Page::Done {
                affected_rows,
                warnings,
            } => {
                self.warning_count = self
                    .warning_count
                    .saturating_add(u32::try_from(warnings.len()).unwrap_or(u32::MAX));
                if self.open_result.is_some() {
                    self.complete_open_result(warnings, &mut events);
                } else {
                    self.command_count = self.command_count.saturating_add(1);
                    self.affected_rows = sum_optional(self.affected_rows, affected_rows);
                    events.push(ExecutionEventV2::CommandCompleted {
                        execution_id: self.execution_id,
                        summary: CommandSummaryV2 {
                            statement_ordinal: None,
                            affected_rows,
                            command_tag: None,
                            duration_ms: elapsed_ms(self.started_at),
                            warnings,
                        },
                    });
                }
                events.push(ExecutionEventV2::ExecutionCompleted {
                    execution_id: self.execution_id,
                    summary: ExecutionSummaryV2 {
                        duration_ms: elapsed_ms(self.started_at),
                        result_set_count: self.result_set_count,
                        command_count: self.command_count,
                        rows_received: self.rows_received,
                        affected_rows: self.affected_rows,
                        warning_count: self.warning_count,
                    },
                });
                self.terminal_sent = true;
            }
            Page::Error { error } => {
                events.push(ExecutionEventV2::Error {
                    execution_id: self.execution_id,
                    statement_ordinal: None,
                    result_set_id: self.open_result,
                    error,
                });
                self.terminal_sent = true;
            }
        }
        events
    }

    fn push_progress(&mut self, phase: ExecutionPhase, events: &mut Vec<ExecutionEventV2>) {
        let now = Instant::now();
        if self
            .last_progress_at
            .is_some_and(|last| now.duration_since(last) < PROGRESS_INTERVAL)
        {
            return;
        }
        self.last_progress_at = Some(now);
        events.push(ExecutionEventV2::Progress {
            execution_id: self.execution_id,
            progress: ExecutionProgress {
                phase,
                elapsed_ms: elapsed_ms(self.started_at),
                statement_ordinal: None,
                statement_count: None,
                result_sets_seen: self.result_set_count,
                rows_received: self.rows_received,
                bytes_received: self.bytes_received,
                native: None,
            },
        });
    }

    fn complete_open_result(
        &mut self,
        warnings: Vec<sift_protocol::DriverWarning>,
        events: &mut Vec<ExecutionEventV2>,
    ) {
        let Some(result_set_id) = self.open_result.take() else {
            return;
        };
        let duration_ms = self.result_started_at.take().map_or(0, elapsed_ms);
        events.push(ExecutionEventV2::ResultSetCompleted {
            execution_id: self.execution_id,
            summary: ResultSetSummaryV2 {
                result_set_id,
                statement_ordinal: None,
                row_count: self.current_result_rows,
                duration_ms,
                warnings,
            },
        });
    }
}

fn elapsed_ms(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

fn sum_optional(left: Option<u64>, right: Option<u64>) -> Option<u64> {
    match (left, right) {
        (None, None) => None,
        (left, right) => Some(left.unwrap_or(0).saturating_add(right.unwrap_or(0))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sift_protocol::{ColumnMetadata, DriverWarning, PrimitiveType, Row, TypeRef, Value};

    fn columns(name: &str) -> Vec<ColumnMetadata> {
        vec![ColumnMetadata::new(
            name,
            TypeRef::Primitive(PrimitiveType::Int64),
        )]
    }

    #[test]
    fn boundaries_complete_each_result_before_batch() {
        let mut normalizer = ExecutionEventNormalizer::new(ExecutionId(7));
        let first = normalizer.events_for_page(Page::NextResult {
            columns: columns("a"),
        });
        assert!(matches!(
            first[0],
            ExecutionEventV2::ExecutionStarted { .. }
        ));
        assert!(matches!(
            first[1],
            ExecutionEventV2::ResultSetStarted { .. }
        ));
        assert!(matches!(first[2], ExecutionEventV2::Progress { .. }));

        let rows = normalizer.events_for_page(Page::Rows {
            rows: vec![Row::new(vec![Value::Int64(1)])],
        });
        assert!(matches!(rows.as_slice(), [ExecutionEventV2::Rows { .. }]));

        let boundary = normalizer.events_for_page(Page::NextResult {
            columns: columns("b"),
        });
        assert!(matches!(
            boundary.as_slice(),
            [
                ExecutionEventV2::ResultSetCompleted { .. },
                ExecutionEventV2::ResultSetStarted { .. }
            ]
        ));

        let done = normalizer.events_for_page(Page::Done {
            affected_rows: None,
            warnings: vec![DriverWarning::new("notice")],
        });
        assert!(matches!(
            done.as_slice(),
            [
                ExecutionEventV2::ResultSetCompleted { .. },
                ExecutionEventV2::ExecutionCompleted { .. }
            ]
        ));
    }

    #[test]
    fn command_only_page_has_command_and_batch_completion() {
        let mut normalizer = ExecutionEventNormalizer::new(ExecutionId(9));
        let events = normalizer.events_for_page(Page::Done {
            affected_rows: Some(4),
            warnings: Vec::new(),
        });
        assert!(matches!(
            events.as_slice(),
            [
                ExecutionEventV2::ExecutionStarted { .. },
                ExecutionEventV2::CommandCompleted { .. },
                ExecutionEventV2::ExecutionCompleted { .. }
            ]
        ));
    }

    #[test]
    fn progress_is_coalesced_to_four_updates_per_second() {
        let mut normalizer = ExecutionEventNormalizer::new(ExecutionId(11));
        let first = normalizer.events_for_page(Page::NextResult {
            columns: columns("a"),
        });
        assert_eq!(
            first
                .iter()
                .filter(|event| matches!(event, ExecutionEventV2::Progress { .. }))
                .count(),
            1
        );
        let immediate = normalizer.events_for_page(Page::Rows {
            rows: vec![Row::new(vec![Value::Int64(1)])],
        });
        assert!(!immediate
            .iter()
            .any(|event| matches!(event, ExecutionEventV2::Progress { .. })));

        normalizer.last_progress_at = Some(Instant::now() - PROGRESS_INTERVAL);
        let later = normalizer.events_for_page(Page::Rows {
            rows: vec![Row::new(vec![Value::Int64(2)])],
        });
        let progress = later.iter().find_map(|event| match event {
            ExecutionEventV2::Progress { progress, .. } => Some(progress),
            _ => None,
        });
        assert!(progress.is_some_and(|progress| {
            progress.phase == ExecutionPhase::Streaming
                && progress.rows_received == 2
                && progress.bytes_received > 0
        }));
    }
}
