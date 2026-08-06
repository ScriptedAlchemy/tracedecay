//! Background maintenance owned by the daemon root.
//!
//! Periodic semantic artifact GC whose task handle aborts on drop.
//!
//! Relocated verbatim from `daemon.rs` as a pure structural split; no logic
//! or signatures changed. `use super::*` re-exposes every name the parent
//! `daemon` module had in scope so the moved code resolves unchanged.

use super::*;

const SEMANTIC_ARTIFACT_GC_PERIOD: Duration = Duration::from_hours(24);

pub(super) struct SemanticArtifactGcMaintenanceTask(JoinHandle<()>);

impl Drop for SemanticArtifactGcMaintenanceTask {
    fn drop(&mut self) {
        self.0.abort();
    }
}

pub(super) fn spawn_semantic_artifact_gc_maintenance() -> SemanticArtifactGcMaintenanceTask {
    SemanticArtifactGcMaintenanceTask(tokio::spawn(async {
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
    }))
}
