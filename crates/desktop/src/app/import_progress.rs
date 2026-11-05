//! Observe an owned import without dropping its request on cancellation.
use super::*;

pub(super) struct Target {
    pub generation: u64,
    pub source: sift_workspace_ui::CsvImportSource,
    pub recipe: bool,
    pub run_id: uuid::Uuid,
}

pub(super) async fn run<T>(
    client: &Client,
    target: Target,
    events: &tokio::sync::mpsc::UnboundedSender<ExecutorEvent>,
    mut cancelled: tokio::sync::oneshot::Receiver<()>,
    result: impl std::future::Future<Output = T>,
) -> T {
    let Target {
        generation,
        source,
        recipe,
        run_id,
    } = target;
    let observe = async {
        loop {
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            let progress = tokio::time::timeout(
                std::time::Duration::from_secs(2),
                client.import_progress(source.session_id, source.connection_id, run_id),
            )
            .await;
            if let Ok(Ok(Some(progress))) = progress {
                if progress.run_id != run_id {
                    continue;
                }
                let (acknowledge, received) = tokio::sync::oneshot::channel();
                if events
                    .send(ExecutorEvent::ImportProgress {
                        generation,
                        source: source.clone(),
                        recipe,
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
        }
    };
    let cancel = async {
        let _ = (&mut cancelled).await;
        // Registration follows bounded parsing/opening. An inactive acknowledgement
        // does not prove cancellation; retry beside the still-owned original RPC.
        loop {
            if matches!(tokio::time::timeout(std::time::Duration::from_secs(2), client.cancel_import(source.session_id, source.connection_id, run_id)).await, Ok(Ok(response)) if response.cancel_requested)
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        }
        std::future::pending::<()>().await;
    };
    tokio::pin!(result, observe, cancel);
    tokio::select! { biased;
        result = &mut result => result,
        _ = &mut observe => unreachable!(),
        _ = &mut cancel => unreachable!(),
    }
}
