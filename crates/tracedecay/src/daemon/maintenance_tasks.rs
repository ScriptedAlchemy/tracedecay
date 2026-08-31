//! Background maintenance owned by the daemon root.
//!
//! Long-lived-process opt-in for session-store maintenance, and the periodic
//! semantic artifact GC whose task is joined during daemon shutdown.

use std::sync::Arc;

use super::*;

/// Enables background maintenance only for long-lived daemon/MCP processes.
///
/// Session-store mounts retain the registered database authority for the
/// lifetime of each maintenance task. One-shot commands never enable it.
pub fn mark_process_long_lived_for_session_maintenance() {
    tracedecay_store_runtime::mark_process_long_lived_for_session_maintenance();
}

const SEMANTIC_ARTIFACT_GC_PERIOD: Duration = Duration::from_hours(24);

#[derive(Clone)]
pub(super) struct SemanticArtifactGcMaintenanceTask {
    task: Arc<tokio::sync::Mutex<Option<JoinHandle<()>>>>,
}

impl SemanticArtifactGcMaintenanceTask {
    pub(super) fn cancel(&self) {
        if let Ok(task) = self.task.try_lock()
            && let Some(task) = task.as_ref()
        {
            task.abort();
        }
    }

    pub(super) async fn shutdown(self) -> std::result::Result<(), String> {
        let mut retained = self.task.lock().await;
        let Some(task) = retained.as_mut() else {
            return Ok(());
        };
        task.abort();
        let result = match task.await {
            Ok(()) => Ok(()),
            Err(error) if error.is_cancelled() => Ok(()),
            Err(error) => Err(error.to_string()),
        };
        retained.take();
        result
    }
}

impl Drop for SemanticArtifactGcMaintenanceTask {
    fn drop(&mut self) {
        self.cancel();
    }
}

pub(super) fn spawn_semantic_artifact_gc_maintenance() -> SemanticArtifactGcMaintenanceTask {
    let task = tokio::spawn(hotpath::future!(
        async {
            let mut interval = tokio::time::interval(SEMANTIC_ARTIFACT_GC_PERIOD);
            loop {
                interval.tick().await;
                let Some(owner) =
                    tracedecay_semantic::SemanticModelLifecycleOwnerV1::mounted_shared()
                else {
                    continue;
                };
                let now_unix = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs();
                // The task-lifetime future above measures the whole loop; this
                // wall span is one GC sweep, the unit a hang or cost regression
                // is diagnosed against.
                let receipts = hotpath::measure_block!(
                    "daemon.maintenance.semantic_artifact_gc_sweep",
                    owner.run_daemon_artifact_gc(now_unix)
                );
                match receipts {
                    Ok(receipts) => {
                        hotpath::gauge!("daemon.maintenance.semantic_artifact_gc.receipts_total")
                            .inc(receipts.len() as u64);
                    }
                    Err(_) => {
                        hotpath::gauge!("daemon.maintenance.semantic_artifact_gc.failed_total")
                            .inc(1_u64);
                        log_daemon_event(
                            "semantic_artifact_gc",
                            &[("outcome", "retry_next_interval".to_owned())],
                        );
                    }
                }
            }
        },
        label = "daemon.maintenance.semantic_artifact_gc"
    ));
    SemanticArtifactGcMaintenanceTask {
        task: Arc::new(tokio::sync::Mutex::new(Some(task))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn shutdown_aborts_and_joins_semantic_artifact_gc_task() {
        let task = SemanticArtifactGcMaintenanceTask {
            task: Arc::new(tokio::sync::Mutex::new(Some(tokio::spawn(
                std::future::pending(),
            )))),
        };
        let observer = task.clone();

        task.cancel();
        task.shutdown().await.expect("join cancelled GC task");

        assert!(
            observer.task.lock().await.is_none(),
            "shutdown must consume the retained task handle"
        );
    }
}
