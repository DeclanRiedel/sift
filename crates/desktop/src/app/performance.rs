//! Poll active progress beside the original request, with one acknowledged UI update.
use super::*;
use sift_protocol::PerformanceRunKind;

pub(super) struct ProgressTarget {
    pub item_id: u64,
    pub instance_id: String,
    pub profile_id: i64,
    pub session: SessionId,
    pub connection: ConnectionId,
    pub run_id: uuid::Uuid,
    pub mode: PerformanceRunKind,
}

pub(super) async fn with_progress<T>(
    client: &Client,
    target: ProgressTarget,
    events: &tokio::sync::mpsc::UnboundedSender<ExecutorEvent>,
    result: impl std::future::Future<Output = T>,
) -> T {
    let session = target.session;
    let connection = target.connection;
    let run = target.run_id;
    let mode = target.mode;
    drive_progress(
        target,
        events,
        || async move {
            client
                .performance_progress(session, connection, run, mode)
                .await
                .ok()
                .flatten()
        },
        result,
    )
    .await
}

async fn drive_progress<T, F, R>(
    target: ProgressTarget,
    events: &tokio::sync::mpsc::UnboundedSender<ExecutorEvent>,
    mut read: F,
    result: impl std::future::Future<Output = T>,
) -> T
where
    F: FnMut() -> R,
    R: std::future::Future<Output = Option<sift_protocol::PerformanceProgress>>,
{
    let progress = async {
        loop {
            // Sleep after each acknowledged response: at most two reads per second,
            // with no catch-up burst and no second request or event in flight.
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            let observation = tokio::time::timeout(std::time::Duration::from_secs(2), read()).await;
            let Ok(Some(progress)) = observation else {
                continue;
            };
            if progress.run_id != target.run_id || progress.kind != target.mode {
                continue;
            }
            let (acknowledge, received) = tokio::sync::oneshot::channel();
            if events
                .send(ExecutorEvent::PerformanceProgress {
                    item_id: target.item_id,
                    instance_id: target.instance_id.clone(),
                    profile_id: target.profile_id,
                    session: target.session,
                    connection: target.connection,
                    progress,
                    acknowledge,
                })
                .is_err()
            {
                std::future::pending::<()>().await;
            }
            if received.await.is_err() {
                std::future::pending::<()>().await;
            }
        }
    };
    tokio::pin!(result);
    tokio::pin!(progress);
    tokio::select! {
        biased;
        result = &mut result => result,
        _ = &mut progress => unreachable!("progress lives only beside the original result"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    #[tokio::test]
    async fn slow_ui_bounds_progress_to_one_event_and_completion_stops_polling() {
        let (events, mut received) = tokio::sync::mpsc::unbounded_channel();
        let (finish, result) = tokio::sync::oneshot::channel();
        let reads = Arc::new(AtomicUsize::new(0));
        let observed = reads.clone();
        let run_id = uuid::Uuid::new_v4();
        let target = ProgressTarget {
            item_id: 1,
            instance_id: "test".into(),
            profile_id: 2,
            session: SessionId(3),
            connection: ConnectionId(4),
            run_id,
            mode: PerformanceRunKind::Benchmark,
        };
        let task = tokio::spawn(async move {
            drive_progress(
                target,
                &events,
                || {
                    observed.fetch_add(1, Ordering::SeqCst);
                    async move {
                        Some(sift_protocol::PerformanceProgress {
                            run_id,
                            kind: PerformanceRunKind::Benchmark,
                            stage: sift_protocol::PerformanceStage::Measuring,
                            elapsed_ms: 1000,
                            benchmark: None,
                            cancellation_requested: false,
                        })
                    }
                },
                result,
            )
            .await
        });
        let first = tokio::time::timeout(Duration::from_secs(2), received.recv())
            .await
            .unwrap()
            .unwrap();
        tokio::time::sleep(Duration::from_millis(650)).await;
        assert_eq!(
            reads.load(Ordering::SeqCst),
            1,
            "no reads while a UI update is unacknowledged"
        );
        assert!(received.try_recv().is_err(), "no queued progress backlog");
        let acknowledged = std::time::Instant::now();
        let ExecutorEvent::PerformanceProgress { acknowledge, .. } = first else {
            panic!("progress event");
        };
        acknowledge.send(()).unwrap();
        let second = tokio::time::timeout(Duration::from_secs(2), received.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(
            acknowledged.elapsed() >= Duration::from_millis(500),
            "no catch-up burst after an acknowledgement"
        );
        assert_eq!(reads.load(Ordering::SeqCst), 2);
        // The result must win even while the next progress event has no acknowledgement.
        finish.send(42).unwrap();
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), task)
                .await
                .unwrap()
                .unwrap()
                .unwrap(),
            42
        );
        tokio::time::sleep(Duration::from_millis(550)).await;
        assert_eq!(
            reads.load(Ordering::SeqCst),
            2,
            "completion drops the polling future"
        );
        drop(second);
    }
}
