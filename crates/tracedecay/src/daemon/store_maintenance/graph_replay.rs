use std::path::Path;

use super::{TraceDecay, log_daemon_event};
use tracedecay_code_index_retention::code_index_generations::{
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

/// Removes the retired generation's sealed read bundle files from the
/// durable generations root. Idempotent; an absent bundle is a success.
fn retire_generation_read_bundle(store_root: &Path, generation_file: &str) -> Result<(), String> {
    let digest = generation_file
        .strip_prefix("generation-")
        .and_then(|value| value.strip_suffix(".json"))
        .ok_or_else(|| "sealed generation filename is invalid".to_owned())?;
    let sealed = tracedecay_graph_db::SealedGraphStateDigest::try_from(format!("sha256:{digest}"))
        .map_err(|error| error.to_string())?;
    tracedecay_graph_db::retire_sealed_read_bundle(&store_root.join("code-generations-v1"), &sealed)
        .map_err(|error| error.to_string())
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

/// The degraded event with the typed error attached. A bare failure label
/// proved undiagnosable in production: `graph_replay_release_failed` recurred
/// on every retention tick with no way to tell an unregistered graph shard
/// from a pool-lock deadline from a conflict.
fn log_code_generation_retention_degraded_with_error(failure: &str, error: &dyn std::fmt::Debug) {
    log_daemon_event(
        "retention_degraded",
        &[
            ("pass", "code_generations".to_string()),
            ("failure", failure.to_string()),
            ("error", format!("{error:?}")),
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
            Err(error) => {
                log_code_generation_retention_degraded_with_error(
                    "graph_replay_release_evidence_invalid",
                    &error,
                );
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
                    // The generation's graph replay is retired; its sealed
                    // read bundle (derived read artifacts) retires with it.
                    // Runs before the release checkpoint so a crash here
                    // retries the idempotent sweep on the next pass.
                    if let Err(error) = retire_generation_read_bundle(
                        store_root,
                        &release.generation.generation_file,
                    ) {
                        log_daemon_event(
                            "retention_degraded",
                            &[
                                ("pass", "code_generations".to_string()),
                                ("failure", "graph_read_bundle_retire_failed".to_string()),
                                ("error", error),
                            ],
                        );
                        return ReconcileOutcome::Failed;
                    }
                    if complete_code_generation_graph_replay_release(store_root, &release).is_err()
                    {
                        log_code_generation_retention_degraded(
                            "graph_replay_release_checkpoint_failed",
                        );
                        return ReconcileOutcome::Failed;
                    }
                }
                Ok(false) => retained = true,
                Err(error) => {
                    log_code_generation_retention_degraded_with_error(
                        "graph_replay_release_failed",
                        &error,
                    );
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
