//! Periodic semantic-artifact GC whose task handle is joined during shutdown.

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tokio::task::JoinHandle;

use crate::DaemonSessionRuntimeRegistryV1;

const SEMANTIC_ARTIFACT_GC_PERIOD: Duration = Duration::from_hours(24);

/// Admitted handle for the process-wide semantic artifact GC task.
#[derive(Clone)]
pub struct SemanticArtifactGcMaintenanceTask {
    task: Arc<tokio::sync::Mutex<Option<JoinHandle<()>>>>,
}

impl SemanticArtifactGcMaintenanceTask {
    pub fn cancel(&self) {
        if let Ok(task) = self.task.try_lock()
            && let Some(task) = task.as_ref()
        {
            task.abort();
        }
    }

    #[hotpath::skip]
    pub async fn shutdown(self) -> std::result::Result<(), String> {
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

/// Spawn the admitted semantic-artifact GC task for a live session registry.
pub fn spawn_semantic_artifact_gc_maintenance(
    registry: Arc<DaemonSessionRuntimeRegistryV1>,
) -> SemanticArtifactGcMaintenanceTask {
    let task = tokio::spawn(hotpath::future!(
        async move {
            let mut interval = tokio::time::interval(SEMANTIC_ARTIFACT_GC_PERIOD);
            loop {
                interval.tick().await;
                let owner = match registry.profile_semantic_lifecycle().await {
                    Ok(owner) => owner,
                    Err(error) => {
                        tracing::warn!(%error, "semantic artifact maintenance owner unavailable");
                        continue;
                    }
                };
                let now_unix = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs();
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
                        crate::session_registry::log_store_runtime_event(
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
