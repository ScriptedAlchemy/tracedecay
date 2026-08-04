//! Retained lifecycle for advisory and Context Scout post-open setup.

use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use tracedecay_application::now_micros;

use super::service::invocation::{
    AdvisoryHookOrchestrationPortV1, AdvisoryRuntimeReadinessV1,
    AdvisoryRuntimeUnavailableReasonV1, DeferredAdvisoryHookOrchestratorV1,
};
use crate::errors::{Result, TraceDecayError};

const POST_OPEN_ADVISORY_SETUP_BUDGET: Duration = Duration::from_secs(15);

/// Starts at most one bounded setup for a retained deferred gateway.
///
/// The gateway itself is already published, so callers see `warming` while
/// this future runs. Project-runtime retirement cancels the shared token and
/// drops the setup future before it can publish a stale delegate.
pub(super) fn schedule_bounded_post_open_advisory_setup<F>(
    project_root: PathBuf,
    deferred: Arc<DeferredAdvisoryHookOrchestratorV1>,
    setup: F,
) -> bool
where
    F: Future<Output = Result<Arc<dyn AdvisoryHookOrchestrationPortV1>>> + Send + 'static,
{
    schedule_bounded_post_open_advisory_setup_with_budget(
        project_root,
        deferred,
        setup,
        POST_OPEN_ADVISORY_SETUP_BUDGET,
    )
}

fn schedule_bounded_post_open_advisory_setup_with_budget<F>(
    project_root: PathBuf,
    deferred: Arc<DeferredAdvisoryHookOrchestratorV1>,
    setup: F,
    budget: Duration,
) -> bool
where
    F: Future<Output = Result<Arc<dyn AdvisoryHookOrchestrationPortV1>>> + Send + 'static,
{
    if !deferred.claim_setup() {
        return false;
    }
    tokio::spawn(async move {
        let cancellation = deferred.cancellation();
        let setup_started_at = match deferred.readiness() {
            AdvisoryRuntimeReadinessV1::Warming { started_at }
            | AdvisoryRuntimeReadinessV1::Ready { started_at, .. }
            | AdvisoryRuntimeReadinessV1::Unavailable { started_at, .. } => started_at,
        };
        tokio::pin!(setup);
        enum SetupOutcome {
            Ready(Arc<dyn AdvisoryHookOrchestrationPortV1>),
            Cancelled,
            DeadlineExceeded,
            Failed(TraceDecayError),
        }
        let outcome = tokio::select! {
            biased;
            () = cancellation.cancelled() => SetupOutcome::Cancelled,
            () = tokio::time::sleep(budget) => {
                SetupOutcome::DeadlineExceeded
            }
            result = &mut setup => match result {
                Ok(runtime) => SetupOutcome::Ready(runtime),
                Err(error) => SetupOutcome::Failed(error),
            },
        };
        let finished_at = now_micros();
        match outcome {
            SetupOutcome::Ready(runtime) => {
                if deferred.mark_ready(runtime, finished_at) {
                    tracing::info!(
                        event = "project_open_owner_phase",
                        project = %project_root.display(),
                        phase = "advisory_owner_registered",
                        state = "ready",
                        deferred = true,
                        started_at_micros = setup_started_at.0,
                        finished_at_micros = finished_at.0,
                    );
                } else {
                    tracing::info!(
                        event = "project_open_owner_phase",
                        project = %project_root.display(),
                        phase = "advisory_owner_cancelled",
                        state = "unavailable",
                        reason = "cancelled",
                        started_at_micros = setup_started_at.0,
                        finished_at_micros = finished_at.0,
                    );
                }
            }
            SetupOutcome::Cancelled => {
                deferred
                    .mark_unavailable(AdvisoryRuntimeUnavailableReasonV1::Cancelled, finished_at);
                tracing::info!(
                    event = "project_open_owner_phase",
                    project = %project_root.display(),
                    phase = "advisory_owner_cancelled",
                    state = "unavailable",
                    reason = "cancelled",
                    started_at_micros = setup_started_at.0,
                    finished_at_micros = finished_at.0,
                );
            }
            SetupOutcome::DeadlineExceeded => {
                deferred.mark_unavailable(
                    AdvisoryRuntimeUnavailableReasonV1::DeadlineExceeded,
                    finished_at,
                );
                tracing::warn!(
                    event = "project_open_owner_phase",
                    project = %project_root.display(),
                    phase = "advisory_owner_deferred_failed",
                    state = "unavailable",
                    reason = "deadline_exceeded",
                    started_at_micros = setup_started_at.0,
                    finished_at_micros = finished_at.0,
                );
            }
            SetupOutcome::Failed(error) => {
                deferred.mark_unavailable(
                    AdvisoryRuntimeUnavailableReasonV1::RegistrationFailed,
                    finished_at,
                );
                tracing::warn!(
                    event = "project_open_owner_phase",
                    project = %project_root.display(),
                    phase = "advisory_owner_deferred_failed",
                    state = "unavailable",
                    reason = "registration_failed",
                    started_at_micros = setup_started_at.0,
                    finished_at_micros = finished_at.0,
                    error = %error,
                );
            }
        }
    });
    true
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::*;

    #[tokio::test]
    async fn post_open_advisory_setup_has_a_truthful_terminal_deadline() {
        let deferred = DeferredAdvisoryHookOrchestratorV1::new(now_micros());
        assert!(schedule_bounded_post_open_advisory_setup_with_budget(
            PathBuf::from("/project"),
            Arc::clone(&deferred),
            std::future::pending(),
            Duration::from_millis(1),
        ));
        tokio::time::timeout(Duration::from_secs(1), async {
            while matches!(
                deferred.readiness(),
                AdvisoryRuntimeReadinessV1::Warming { .. }
            ) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("bounded setup terminal");
        assert!(matches!(
            deferred.readiness(),
            AdvisoryRuntimeReadinessV1::Unavailable {
                reason: AdvisoryRuntimeUnavailableReasonV1::DeadlineExceeded,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn retained_owner_cancellation_drops_post_open_setup() {
        struct PendingSetup(Arc<AtomicBool>);

        impl Future for PendingSetup {
            type Output = Result<Arc<dyn AdvisoryHookOrchestrationPortV1>>;

            fn poll(
                self: std::pin::Pin<&mut Self>,
                _context: &mut std::task::Context<'_>,
            ) -> std::task::Poll<Self::Output> {
                std::task::Poll::Pending
            }
        }

        impl Drop for PendingSetup {
            fn drop(&mut self) {
                self.0.store(true, Ordering::Release);
            }
        }

        let deferred = DeferredAdvisoryHookOrchestratorV1::new(now_micros());
        let setup_dropped = Arc::new(AtomicBool::new(false));
        assert!(schedule_bounded_post_open_advisory_setup_with_budget(
            PathBuf::from("/project"),
            Arc::clone(&deferred),
            PendingSetup(Arc::clone(&setup_dropped)),
            Duration::from_secs(10),
        ));
        tokio::task::yield_now().await;
        deferred.cancel();
        tokio::time::timeout(Duration::from_secs(1), async {
            while !setup_dropped.load(Ordering::Acquire) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("cancelled setup dropped");
        assert!(matches!(
            deferred.readiness(),
            AdvisoryRuntimeReadinessV1::Unavailable {
                reason: AdvisoryRuntimeUnavailableReasonV1::Cancelled,
                ..
            }
        ));
    }
}
