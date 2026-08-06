//! Background maintenance owned by the daemon root.
//!
//! Long-lived-process opt-in for session-store maintenance, and the periodic
//! semantic artifact GC whose task is joined during daemon shutdown.
//!
//! Relocated verbatim from `daemon.rs` as a pure structural split; no logic
//! or signatures changed. `use super::*` re-exposes every name the parent
//! `daemon` module had in scope so the moved code resolves unchanged.

use std::sync::Arc;

use super::*;

/// Enables background maintenance only for long-lived daemon/MCP processes.
///
/// Session-store mounts retain the registered database authority for the
/// lifetime of each maintenance task. One-shot commands never enable it.
pub fn mark_process_long_lived_for_session_maintenance() {
    store_runtime::session_registry::mark_process_long_lived_for_session_maintenance();
}

const SEMANTIC_ARTIFACT_GC_PERIOD: Duration = Duration::from_hours(24);

#[derive(Clone)]
pub(super) struct SemanticArtifactGcMaintenanceTask {
    task: Arc<std::sync::Mutex<Option<JoinHandle<()>>>>,
}

impl SemanticArtifactGcMaintenanceTask {
    pub(super) fn cancel(&self) {
        if let Some(task) = self
            .task
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
        {
            task.abort();
        }
    }

    pub(super) async fn shutdown(self) -> std::result::Result<(), String> {
        let task = self
            .task
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        let Some(task) = task else {
            return Ok(());
        };
        match task.await {
            Ok(()) => Ok(()),
            Err(error) if error.is_cancelled() => Ok(()),
            Err(error) => Err(error.to_string()),
        }
    }
}

impl Drop for SemanticArtifactGcMaintenanceTask {
    fn drop(&mut self) {
        self.cancel();
    }
}

pub(super) fn spawn_semantic_artifact_gc_maintenance() -> SemanticArtifactGcMaintenanceTask {
    let task = tokio::spawn(async {
        let mut interval = tokio::time::interval(SEMANTIC_ARTIFACT_GC_PERIOD);
        loop {
            interval.tick().await;
            let Some(owner) = crate::semantic_code::SemanticModelLifecycleOwnerV1::mounted_shared()
            else {
                continue;
            };
            let now_unix = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            if owner.run_daemon_artifact_gc(now_unix).is_err() {
                log_daemon_event(
                    "semantic_artifact_gc",
                    &[("outcome", "retry_next_interval".to_owned())],
                );
            }
        }
    });
    SemanticArtifactGcMaintenanceTask {
        task: Arc::new(std::sync::Mutex::new(Some(task))),
    }
}
