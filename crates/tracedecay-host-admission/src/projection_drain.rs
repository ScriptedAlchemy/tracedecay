use std::collections::BTreeSet;
use std::future::Future;
use std::pin::Pin;

use tracedecay_sessions::admission::{HostAdmissionOutcome, HostProjectionDrainOutcome};
use tracedecay_sessions::runtime::git_correlation::SystemGit;
use tracedecay_store::ProjectionPersistOutcome;

use super::*;

/// Queue items projected per write transaction by the batched drain path.
/// Bounds both the write-lease hold and the cancellation-check granularity.
const PROJECTION_DRAIN_TXN_WINDOW: usize = 32;

impl HostAdmissionFacade<'_> {
    #[tracing::instrument(
        name = "usecases.admission.drain_projection",
        level = "trace",
        skip_all
    )]
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
        let predecessor: Pin<
            Box<
                dyn Future<
                        Output = tracedecay_store::ProjectionStoreResult<
                            tracedecay_store::ProjectionPredecessorConvergence,
                        >,
                    > + Send
                    + '_,
            >,
        > = Box::pin(store.converge_projection_predecessor());
        let predecessor = predecessor.await.map_err(|error| {
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
        let replay: Pin<
            Box<
                dyn Future<
                        Output = Result<
                            tracedecay_session_memory::external_source_store::RuntimeProjectionReplayOutcomeV1,
                            tracedecay_session_memory::external_source_store::RuntimeExternalSourceErrorV1,
                        >,
                    > + Send
                    + '_,
            >,
        > = Box::pin(external_source.drain_host_projection_replay(max, cancellation));
        let external_replay = replay.await.map_err(|error| {
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
            let queued: Pin<
                Box<
                    dyn Future<
                            Output = tracedecay_store::ProjectionStoreResult<
                                Option<tracedecay_store::ProjectionDrainBatch>,
                            >,
                        > + Send
                        + '_,
                >,
            > = Box::pin(store.project_queued_observations(window));
            match queued.await {
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
            let next_queued: Pin<
                Box<
                    dyn Future<
                            Output = tracedecay_store::ProjectionStoreResult<
                                Option<tracedecay_domain::CanonicalObservationIdV1>,
                            >,
                        > + Send
                        + '_,
                >,
            > = Box::pin(store.next_queued_observation());
            let Some(observation_id) = next_queued.await.map_err(|error| {
                warn_projection_drain_store_error(
                    &error,
                    "projection store operation failed during host drain",
                );
                projection_error_outcome(&error)
            })?
            else {
                observation_queue_exhausted = true;
                break;
            };
            let projected_item: Pin<
                Box<
                    dyn Future<
                            Output = tracedecay_store::ProjectionStoreResult<
                                tracedecay_store::ProjectionPersistOutcome,
                            >,
                        > + Send
                        + '_,
                >,
            > = Box::pin(store.project_observation(&observation_id));
            let projected = match projected_item.await {
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
                    // next 60s host tick, a stall, not a cheaper path. Keep
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
                    let loaded: Pin<
                        Box<
                            dyn Future<
                                    Output = tracedecay_store::ObservationStoreResult<
                                        Option<tracedecay_store::StoredObservation>,
                                    >,
                                > + Send
                                + '_,
                        >,
                    > = Box::pin(store.get_observation(&observation_id));
                    if let Some(observation) = loaded.await.map_err(|error| {
                        warn_projection_drain_store_error(
                            &error,
                            "projection store operation failed during host drain",
                        );
                        observation_store_drain_outcome(&error)
                    })? {
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
            let suffix: Pin<
                Box<
                    dyn Future<
                            Output = tracedecay_store::ProjectionStoreResult<
                                Option<tracedecay_domain::CanonicalObservationIdV1>,
                            >,
                        > + Send
                        + '_,
                >,
            > = Box::pin(store.next_queued_observation());
            observation_deferred = suffix
                .await
                .map_err(|error| {
                    warn_projection_drain_store_error(
                        &error,
                        "projection suffix probe failed during host drain",
                    );
                    projection_error_outcome(&error)
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
            let git_evidence: Pin<
                Box<
                    dyn Future<
                            Output = std::result::Result<
                                tracedecay_sessions::runtime::git_correlation::GitEvidencePassOutcome,
                                tracedecay_sessions::runtime::git_correlation::GitCorrelationError,
                            >,
                        > + Send
                        + '_,
                >,
            > = Box::pin(database.converge_session_git_evidence(
                &SystemGit,
                self.authorities.project_root.as_deref(),
            ));
            let convergence = git_evidence.await;
            if cancellation.is_cancelled() {
                return Err(classify_error(&ObservationApplicationError::Cancelled));
            }
            match convergence {
                Ok(convergence) => {
                    if let Some(error) = &convergence.later_failure {
                        tracing::warn!(%error, "Git evidence convergence committed but attribution will retry");
                    }
                    outcome.deferred |= git_evidence_convergence_deferred(&convergence);
                }
                Err(error) => {
                    tracing::warn!(%error, "Git evidence convergence deferred during host drain");
                    outcome.deferred = true;
                }
            }
        }
        outcome.session_ids = session_ids.into_iter().collect();
        Ok(outcome)
    }
}

fn git_evidence_convergence_deferred(
    convergence: &tracedecay_sessions::runtime::git_correlation::GitEvidencePassOutcome,
) -> bool {
    convergence.later_failure.is_some() || convergence.pass.backfill.skipped_git_error > 0
}

/// Host drain warnings that used `%error` drop the source chain. Callers pass
/// either a projection-store or observation-store error; Display of
/// `ObservationStoreError::Storage` is the journal line
/// `dispatch observation runtime read failed`.
fn warn_projection_drain_store_error(error: &dyn std::error::Error, message: &'static str) {
    tracing::warn!(
        error = %tracedecay_store::durable_error_detail(error),
        "{message}"
    );
}

fn observation_store_drain_outcome(
    error: &tracedecay_store::ObservationStoreError,
) -> HostAdmissionOutcome {
    if tracedecay_store::error_chain_is_sqlite_disk_image_corrupt(error) {
        HostAdmissionOutcome::terminal_unavailable("projection_store_corrupt")
    } else {
        projection_store_unavailable()
    }
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
    use std::sync::{Arc, Mutex};

    use super::{
        SimulatedProjectOutcome, git_evidence_convergence_deferred,
        observation_store_drain_outcome, simulate_drain_project_calls,
        warn_projection_drain_store_error,
    };
    use tracedecay_sessions::admission::HostAdmissionStatus;
    use tracedecay_store::ObservationStoreError;

    struct ErrorFieldVisitor(String);

    impl tracing::field::Visit for ErrorFieldVisitor {
        fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
            if field.name() == "error" {
                self.0 = format!("{value:?}");
            }
        }

        fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
            if field.name() == "error" {
                self.0 = value.to_owned();
            }
        }
    }

    struct ErrorFieldCapture {
        fields: Arc<Mutex<Vec<String>>>,
    }

    impl ErrorFieldCapture {
        fn new() -> (Self, Arc<Mutex<Vec<String>>>) {
            let fields = Arc::new(Mutex::new(Vec::new()));
            (
                Self {
                    fields: Arc::clone(&fields),
                },
                fields,
            )
        }
    }

    impl tracing::Subscriber for ErrorFieldCapture {
        fn enabled(&self, _metadata: &tracing::Metadata<'_>) -> bool {
            true
        }

        fn new_span(&self, _span: &tracing::span::Attributes<'_>) -> tracing::span::Id {
            tracing::span::Id::from_u64(1)
        }

        fn record(&self, _span: &tracing::span::Id, _values: &tracing::span::Record<'_>) {}

        fn record_follows_from(&self, _span: &tracing::span::Id, _follows: &tracing::span::Id) {}

        fn event(&self, event: &tracing::Event<'_>) {
            let mut visitor = ErrorFieldVisitor(String::new());
            event.record(&mut visitor);
            if !visitor.0.is_empty() {
                self.fields
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push(visitor.0);
            }
        }

        fn enter(&self, _span: &tracing::span::Id) {}

        fn exit(&self, _span: &tracing::span::Id) {}
    }

    fn reader_pool_saturated_observation_error() -> ObservationStoreError {
        ObservationStoreError::Storage {
            operation: "dispatch observation runtime read",
            source: Box::new(std::io::Error::other(
                "reader acquisition saturated: ReaderPool",
            )),
        }
    }

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
        let pass =
            |backfill| tracedecay_sessions::runtime::git_correlation::GitEvidencePassOutcome {
                pass: tracedecay_sessions::runtime::git_correlation::GitEvidencePass {
                    backfill,
                    frontier:
                        tracedecay_sessions::runtime::git_correlation::GitHistoryIndexFrontier {
                            change_sequence: 0,
                            source_rowid: 0,
                        },
                    generation: None,
                },
                later_failure: None,
            };
        let convergence = pass(
            tracedecay_sessions::runtime::git_correlation::BackfillStats {
                skipped_no_window: 1,
                skipped_not_worktree: 1,
                // Verified unborn history advances the frontier without
                // writing evidence or recording a retryable Git error.
                frontier_advanced: true,
                ..Default::default()
            },
        );
        assert!(!git_evidence_convergence_deferred(&convergence));

        let transient = pass(
            tracedecay_sessions::runtime::git_correlation::BackfillStats {
                skipped_git_error: 1,
                ..Default::default()
            },
        );
        assert!(git_evidence_convergence_deferred(&transient));
    }

    /// `%error` uses Display, which stops at `ObservationStoreError::Storage`
    /// (`observation storage operation … failed`) and drops the ReaderPool
    /// cause. Drain warnings must include the source chain so the journal
    /// names the real stall.
    #[test]
    fn projection_drain_warnings_include_the_source_chain() {
        let error = reader_pool_saturated_observation_error();
        assert_eq!(
            error.to_string(),
            "observation storage operation dispatch observation runtime read failed"
        );
        assert!(
            !error
                .to_string()
                .contains("reader acquisition saturated: ReaderPool"),
            "Display must omit the source so this test stays a real reproduction"
        );

        let (subscriber, fields) = ErrorFieldCapture::new();
        let _guard = tracedecay_runtime_core::logging::set_tracing_capture(subscriber);
        warn_projection_drain_store_error(
            &error,
            "projection store operation failed during host drain",
        );
        drop(_guard);
        let captured = fields
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        assert!(
            captured
                .iter()
                .any(|field| field.contains("reader acquisition saturated: ReaderPool")),
            "drain warning must include the source chain, got {captured:?}"
        );
    }

    #[test]
    fn observation_store_corrupt_read_is_blocked_not_retryable() {
        let error = ObservationStoreError::Storage {
            operation: "dispatch observation runtime read",
            source: Box::new(std::io::Error::other(
                "SQLite execute failed: database disk image is malformed",
            )),
        };
        let outcome = observation_store_drain_outcome(&error);
        assert_eq!(outcome.reason_code, Some("projection_store_corrupt"));
        assert!(!outcome.retryable);
        assert_eq!(outcome.status, HostAdmissionStatus::Unavailable);
    }

    #[test]
    fn observation_store_transient_read_stays_retryable() {
        let outcome = observation_store_drain_outcome(&reader_pool_saturated_observation_error());
        assert_eq!(outcome.reason_code, Some("projection_store_unavailable"));
        assert!(outcome.retryable);
    }
}
