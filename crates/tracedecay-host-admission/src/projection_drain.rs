use std::collections::BTreeSet;

use tracedecay_sessions::admission::{HostAdmissionOutcome, HostProjectionDrainOutcome};
use tracedecay_sessions::runtime::git_correlation::{
    DEFAULT_AUTO_BACKFILL_SESSIONS_PER_PASS, DEFAULT_GIT_EVIDENCE_PUBLICATION_REPLAY_LIMIT,
    SystemGit,
};
use tracedecay_store::ProjectionPersistOutcome;

use super::*;

/// Queue items projected per write transaction by the batched drain path.
/// Bounds both the write-lease hold and the cancellation-check granularity.
const PROJECTION_DRAIN_TXN_WINDOW: usize = 32;

impl HostAdmissionFacade<'_> {
    #[hotpath::measure(label = "usecases.admission.drain_projection", future = true)]
    pub async fn drain_projection_queue(
        &self,
        provider: &str,
        scope: &ObservationScopeV1,
        cancellation: &ObservationCancellation,
        max: usize,
    ) -> Result<HostProjectionDrainOutcome, HostAdmissionOutcome> {
        if cancellation.is_cancelled() {
            return Err(classify_error(&ObservationApplicationError::Cancelled));
        }
        let database = self
            .authorities
            .registered_database(host_scope(scope))?
            .ok_or_else(HostAdmissionOutcome::registered_authority_unavailable)?;
        let store = self.store(provider, scope)?;
        let predecessor = store
            .converge_projection_predecessor()
            .await
            .map_err(|error| {
                tracing::warn!(
                    error = %error.durable_detail(),
                    "predecessor projection convergence failed during host drain"
                );
                projection_error_outcome(&error)
            })?;
        if cancellation.is_cancelled() {
            return Err(classify_error(&ObservationApplicationError::Cancelled));
        }
        if predecessor
            .rebuild()
            .is_some_and(|rebuild| !rebuild.is_complete())
        {
            return Ok(HostProjectionDrainOutcome {
                deferred: true,
                ..HostProjectionDrainOutcome::default()
            });
        }
        let external_source =
            tracedecay_session_memory::external_source_store::RuntimeExternalSourceStore::new(
                database.runtime_client(),
            );
        let external_replay = external_source
            .drain_host_projection_replay(max, cancellation)
            .await
            .map_err(|error| {
                tracing::warn!(%error, "external-source projection replay failed during host drain");
                HostAdmissionOutcome::retained_unavailable("external_source_projection_unavailable")
            })?;
        if cancellation.is_cancelled() {
            return Err(classify_error(&ObservationApplicationError::Cancelled));
        }
        let mut outcome = HostProjectionDrainOutcome {
            deferred: external_replay.deferred,
            ..HostProjectionDrainOutcome::default()
        };
        let mut session_ids = BTreeSet::new();
        let mut observation_deferred = false;
        let mut observation_queue_exhausted = false;
        let mut remaining = max;
        // Batched fast path: each window projects up to
        // `PROJECTION_DRAIN_TXN_WINDOW` queue heads in one write transaction.
        // Any window error falls back to the per-item drain below, whose
        // durable retry and skip dispositions stay the failure authority.
        let mut per_item_drain_required = false;
        while remaining > 0 {
            if cancellation.is_cancelled() {
                return Err(classify_error(&ObservationApplicationError::Cancelled));
            }
            let window = PROJECTION_DRAIN_TXN_WINDOW.min(remaining);
            match store.project_queued_observations(window).await {
                Ok(Some(batch)) => {
                    let item_count = batch.items.len();
                    for item in batch.items {
                        match item.outcome {
                            ProjectionPersistOutcome::Projected(projected) => {
                                outcome.projected = outcome.projected.saturating_add(1);
                                outcome.projected_outputs =
                                    outcome.projected_outputs.saturating_add(
                                        u64::try_from(projected.output_count()).unwrap_or(u64::MAX),
                                    );
                                session_ids.insert(item.session_id);
                            }
                            ProjectionPersistOutcome::Skipped { .. } => {
                                outcome.skipped = outcome.skipped.saturating_add(1);
                            }
                            ProjectionPersistOutcome::ExactDuplicate(_) => {
                                outcome.exact_duplicates =
                                    outcome.exact_duplicates.saturating_add(1);
                            }
                        }
                    }
                    remaining = remaining.saturating_sub(item_count);
                    if item_count < window || remaining == 0 {
                        // The window undershot its budget (empty queue or a
                        // retry-deferred head) or the drain budget is spent;
                        // `has_more` is the in-transaction suffix truth.
                        observation_deferred = batch.has_more;
                        observation_queue_exhausted = !batch.has_more;
                        break;
                    }
                }
                Ok(None) => {
                    per_item_drain_required = true;
                    break;
                }
                Err(error) => {
                    tracing::debug!(
                        error = %error.durable_detail(),
                        "batched projection window failed; falling back to per-item drain"
                    );
                    per_item_drain_required = true;
                    break;
                }
            }
        }
        while per_item_drain_required && remaining > 0 {
            remaining = remaining.saturating_sub(1);
            if cancellation.is_cancelled() {
                return Err(classify_error(&ObservationApplicationError::Cancelled));
            }
            let Some(observation_id) = store.next_queued_observation().await.map_err(|error| {
                tracing::warn!(%error, "projection store operation failed during host drain");
                projection_store_unavailable()
            })?
            else {
                observation_queue_exhausted = true;
                break;
            };
            let projected = match store.project_observation(&observation_id).await {
                Ok(projected) => projected,
                Err(ProjectionStoreError::RetryDeferred { .. }) => {
                    observation_deferred = true;
                    break;
                }
                Err(
                    error @ (ProjectionStoreError::Contract(_)
                    | ProjectionStoreError::SanitizationRefused { .. }),
                ) => {
                    // Store already recorded a durable skip and consumed this
                    // queue item (`persist_projection_rejection_on_database` via
                    // `apply_skip_disposition` + `consume_projection_queue_item`).
                    // Breaking here would stall later healthy items until the
                    // next 60s host tick — a stall, not a cheaper path. Keep
                    // draining. The typed skip count is the durable signal;
                    // keep per-item detail below WARN so invalid input cannot
                    // become a repeated alert loop.
                    tracing::debug!(
                        %error,
                        observation = observation_id.as_str(),
                        "deterministic projection rejection committed"
                    );
                    outcome.skipped = outcome.skipped.saturating_add(1);
                    continue;
                }
                Err(error) => {
                    // The head-of-queue failure aborts the drain (fail-closed
                    // sequence ordering); the full source chain must land in
                    // the log or the stall is undiagnosable from outside.
                    tracing::warn!(
                        error = %error.durable_detail(),
                        observation = observation_id.as_str(),
                        "projection store operation failed during host drain"
                    );
                    return Err(projection_error_outcome(&error));
                }
            };
            match projected {
                ProjectionPersistOutcome::Projected(projected) => {
                    outcome.projected = outcome.projected.saturating_add(1);
                    outcome.projected_outputs = outcome.projected_outputs.saturating_add(
                        u64::try_from(projected.output_count()).unwrap_or(u64::MAX),
                    );
                    if let Some(observation) = store
                        .get_observation(&observation_id)
                        .await
                        .map_err(|error| {
                            tracing::warn!(
                                %error,
                                "projection store operation failed during host drain"
                            );
                            projection_store_unavailable()
                        })?
                    {
                        session_ids.insert(
                            observation
                                .observation()
                                .source()
                                .session_id()
                                .as_str()
                                .to_owned(),
                        );
                    }
                }
                ProjectionPersistOutcome::Skipped { .. } => {
                    outcome.skipped = outcome.skipped.saturating_add(1);
                }
                ProjectionPersistOutcome::ExactDuplicate(_) => {
                    outcome.exact_duplicates = outcome.exact_duplicates.saturating_add(1);
                }
            }
        }
        if !observation_deferred && !observation_queue_exhausted {
            if cancellation.is_cancelled() {
                return Err(classify_error(&ObservationApplicationError::Cancelled));
            }
            observation_deferred = store
                .next_queued_observation()
                .await
                .map_err(|error| {
                    tracing::warn!(%error, "projection suffix probe failed during host drain");
                    projection_store_unavailable()
                })?
                .is_some();
            if cancellation.is_cancelled() {
                return Err(classify_error(&ObservationApplicationError::Cancelled));
            }
        }
        outcome.deferred |= observation_deferred;
        if max > 0 && matches!(scope, ObservationScopeV1::Project { .. }) {
            if cancellation.is_cancelled() {
                return Err(classify_error(&ObservationApplicationError::Cancelled));
            }
            let convergence = database
                .converge_session_git_evidence(
                    &SystemGit,
                    DEFAULT_AUTO_BACKFILL_SESSIONS_PER_PASS,
                    DEFAULT_GIT_EVIDENCE_PUBLICATION_REPLAY_LIMIT,
                )
                .await
                .map_err(|error| {
                    tracing::warn!(%error, "Git evidence convergence failed during host drain");
                    HostAdmissionOutcome::retained_unavailable(
                        "git_evidence_convergence_unavailable",
                    )
                })?;
            if let Some(error) = convergence.later_failure() {
                tracing::warn!(%error, "Git evidence convergence made partial progress during host drain");
            }
            if cancellation.is_cancelled() {
                return Err(classify_error(&ObservationApplicationError::Cancelled));
            }
            outcome.deferred |= git_evidence_convergence_deferred(&convergence);
        }
        outcome.session_ids = session_ids.into_iter().collect();
        Ok(outcome)
    }
}

fn git_evidence_convergence_deferred(
    convergence: &tracedecay_global_db::GitEvidenceConvergenceOutcome,
) -> bool {
    let stats = convergence.stats();
    convergence.later_failure().is_some()
        || stats.pending_publications.is_none_or(|pending| pending > 0)
        || stats.backfill_page_saturated
        || stats.backfill.skipped_git_error > 0
}

#[cfg(test)]
#[derive(Clone, Copy)]
enum SimulatedProjectOutcome {
    Refusal,
    Projected,
}

#[cfg(test)]
fn simulate_drain_project_calls(batch: &[SimulatedProjectOutcome]) -> (u64, usize) {
    let mut skipped: u64 = 0;
    let mut project_calls = 0_usize;
    for outcome in batch {
        project_calls = project_calls.saturating_add(1);
        match outcome {
            SimulatedProjectOutcome::Refusal => {
                skipped = skipped.saturating_add(1);
            }
            SimulatedProjectOutcome::Projected => {}
        }
    }
    (skipped, project_calls)
}

#[cfg(test)]
mod tests {
    use super::{
        SimulatedProjectOutcome, git_evidence_convergence_deferred, simulate_drain_project_calls,
    };

    #[test]
    fn multi_item_batch_continues_after_durable_refusals() {
        let batch = [
            SimulatedProjectOutcome::Projected,
            SimulatedProjectOutcome::Refusal,
            SimulatedProjectOutcome::Projected,
            SimulatedProjectOutcome::Refusal,
            SimulatedProjectOutcome::Projected,
        ];
        let (skipped, project_calls) = simulate_drain_project_calls(&batch);
        assert_eq!(skipped, 2);
        assert_eq!(
            project_calls,
            batch.len(),
            "durably refused items must not stall healthy items later in the batch"
        );
    }

    #[test]
    fn permanent_git_exclusions_do_not_defer_host_admission() {
        let convergence = tracedecay_global_db::GitEvidenceConvergenceOutcome::Complete(
            tracedecay_global_db::GitEvidenceConvergenceStats {
                replayed_publications: 0,
                pending_publications: Some(0),
                backfill: tracedecay_sessions::runtime::git_correlation::BackfillStats {
                    skipped_no_window: 1,
                    skipped_not_worktree: 1,
                    // An unborn repository is deterministic, not retryable: a
                    // pass that only saw those must still settle.
                    skipped_no_history: 1,
                    ..Default::default()
                },
                backfill_page_saturated: false,
            },
        );

        assert!(!git_evidence_convergence_deferred(&convergence));

        let transient = tracedecay_global_db::GitEvidenceConvergenceOutcome::Complete(
            tracedecay_global_db::GitEvidenceConvergenceStats {
                replayed_publications: 0,
                pending_publications: Some(0),
                backfill: tracedecay_sessions::runtime::git_correlation::BackfillStats {
                    skipped_git_error: 1,
                    ..Default::default()
                },
                backfill_page_saturated: false,
            },
        );
        assert!(git_evidence_convergence_deferred(&transient));
    }
}
