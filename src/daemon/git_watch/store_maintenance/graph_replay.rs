use std::path::Path;

use super::{TraceDecay, log_daemon_event};
use crate::retention::code_index_generations::{
    code_generation_graph_replay_release_page, complete_code_generation_graph_replay_release,
};

pub(super) enum ReconcileOutcome {
    Complete,
    Retained,
    Failed,
}

impl ReconcileOutcome {
    pub(super) fn succeeded(&self) -> bool {
        !matches!(self, Self::Failed)
    }
}

pub(super) fn log_code_generation_retention_degraded(failure: &str) {
    log_daemon_event(
        "retention_degraded",
        &[
            ("pass", "code_generations".to_string()),
            ("failure", failure.to_string()),
        ],
    );
}

pub(super) async fn reconcile_graph_replay_releases(
    graph: &TraceDecay,
    store_root: &Path,
    cancellation: &tracedecay_usecases::context::CancellationToken,
) -> ReconcileOutcome {
    let Some(project_id) = graph.hook_store_layout().identity.project_id.as_ref() else {
        log_code_generation_retention_degraded("graph_replay_project_identity_unavailable");
        return ReconcileOutcome::Failed;
    };
    let project_id = match tracedecay_domain::ProjectId::new(project_id.clone()) {
        Ok(project_id) => project_id,
        Err(_) => {
            log_code_generation_retention_degraded("graph_replay_project_identity_invalid");
            return ReconcileOutcome::Failed;
        }
    };
    let mut after = None;
    let mut retained = false;
    loop {
        let page = match code_generation_graph_replay_release_page(store_root, after.as_deref()) {
            Ok(page) => page,
            Err(_) => {
                log_code_generation_retention_degraded("graph_replay_release_evidence_invalid");
                return ReconcileOutcome::Failed;
            }
        };
        for release in page.releases {
            if cancellation.is_cancelled() {
                return ReconcileOutcome::Failed;
            }
            match graph
                .store_runtime_registry()
                .reconcile_deleted_code_generation_graph_replays(
                    project_id.clone(),
                    graph.db(),
                    &release.generation.generation_id,
                    &release.generation.generation_file,
                    cancellation,
                )
                .await
            {
                Ok(true) => {
                    if complete_code_generation_graph_replay_release(store_root, &release).is_err()
                    {
                        log_code_generation_retention_degraded(
                            "graph_replay_release_checkpoint_failed",
                        );
                        return ReconcileOutcome::Failed;
                    }
                }
                Ok(false) => retained = true,
                Err(_) => {
                    log_code_generation_retention_degraded("graph_replay_release_failed");
                    return ReconcileOutcome::Failed;
                }
            }
        }
        let Some(continuation) = page.continuation else {
            return if retained {
                ReconcileOutcome::Retained
            } else {
                ReconcileOutcome::Complete
            };
        };
        after = Some(continuation);
    }
}
