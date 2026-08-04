//! Retained lifecycle for advisory and Context Scout post-open setup.

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use tracedecay_application::now_micros;

use super::service::invocation::{
    AdvisoryHookOrchestrationPortV1, AdvisoryRuntimeReadinessV1,
    AdvisoryRuntimeUnavailableReasonV1, DeferredAdvisoryHookOrchestratorV1,
};
use crate::errors::{Result, TraceDecayError};

const POST_OPEN_ADVISORY_SETUP_BUDGET: Duration = Duration::from_secs(15);
const POST_OPEN_ADVISORY_CANCELLATION_GRACE: Duration = Duration::from_millis(250);

/// Starts at most one bounded setup for a retained deferred gateway.
///
/// The gateway itself is already published, so callers see `warming` while
/// this future runs. Project-runtime retirement cancels the shared token and
/// drops the setup future before it can publish a stale delegate.
pub(super) async fn schedule_bounded_post_open_advisory_setup<F, Fut>(
    deferred: Arc<DeferredAdvisoryHookOrchestratorV1>,
    setup: F,
) -> bool
where
    F: FnOnce(crate::application::context::CancellationToken) -> Fut + Send + 'static,
    Fut: Future<Output = Result<Arc<dyn AdvisoryHookOrchestrationPortV1>>> + Send + 'static,
{
    schedule_bounded_post_open_advisory_setup_with_budget(
        deferred,
        setup,
        POST_OPEN_ADVISORY_SETUP_BUDGET,
    )
    .await
}

async fn schedule_bounded_post_open_advisory_setup_with_budget<F, Fut>(
    deferred: Arc<DeferredAdvisoryHookOrchestratorV1>,
    setup: F,
    budget: Duration,
) -> bool
where
    F: FnOnce(crate::application::context::CancellationToken) -> Fut + Send + 'static,
    Fut: Future<Output = Result<Arc<dyn AdvisoryHookOrchestrationPortV1>>> + Send + 'static,
{
    if !deferred.claim_setup() {
        return false;
    }
    let (start, started) = tokio::sync::oneshot::channel();
    let retained = Arc::clone(&deferred);
    let task = tokio::spawn(async move {
        let _ = started.await;
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
        let work_cancellation = crate::application::context::CancellationToken::new();
        let mut setup_task = tokio::spawn(setup(work_cancellation.clone()));
        let outcome = tokio::select! {
            biased;
            result = &mut setup_task => match result {
                Ok(Ok(runtime)) => SetupOutcome::Ready(runtime),
                Ok(Err(error)) => SetupOutcome::Failed(error),
                Err(error) => SetupOutcome::Failed(TraceDecayError::Config {
                    message: format!("advisory runtime setup task failed: {error}"),
                }),
            },
            () = cancellation.cancelled() => {
                work_cancellation.cancel();
                join_cancelled_setup(&mut setup_task).await;
                SetupOutcome::Cancelled
            },
            () = tokio::time::sleep(budget) => {
                work_cancellation.cancel();
                join_cancelled_setup(&mut setup_task).await;
                SetupOutcome::DeadlineExceeded
            }
        };
        let finished_at = now_micros();
        match outcome {
            SetupOutcome::Ready(runtime) => {
                if deferred.mark_ready(runtime, finished_at) {
                    tracing::info!(
                        event = "advisory_runtime_setup",
                        state = "ready",
                        started_at_micros = setup_started_at.0,
                        finished_at_micros = finished_at.0,
                    );
                } else {
                    tracing::info!(
                        event = "advisory_runtime_setup",
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
                    event = "advisory_runtime_setup",
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
                    event = "advisory_runtime_setup",
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
                    event = "advisory_runtime_setup",
                    state = "unavailable",
                    reason = "registration_failed",
                    started_at_micros = setup_started_at.0,
                    finished_at_micros = finished_at.0,
                    error = %error,
                );
            }
        }
        deferred.setup_task_finished().await;
    });
    if let Err(task) = retained.retain_setup_task(task).await {
        task.abort();
        let _ = task.await;
        retained.mark_unavailable(AdvisoryRuntimeUnavailableReasonV1::Cancelled, now_micros());
        return false;
    }
    let _ = start.send(());
    true
}

async fn join_cancelled_setup(
    task: &mut tokio::task::JoinHandle<Result<Arc<dyn AdvisoryHookOrchestrationPortV1>>>,
) {
    if tokio::time::timeout(POST_OPEN_ADVISORY_CANCELLATION_GRACE, &mut *task)
        .await
        .is_err()
    {
        task.abort();
        let _ = task.await;
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::*;

    #[tokio::test]
    async fn post_open_advisory_setup_has_a_truthful_terminal_deadline() {
        let deferred = DeferredAdvisoryHookOrchestratorV1::new(now_micros());
        assert!(
            schedule_bounded_post_open_advisory_setup_with_budget(
                Arc::clone(&deferred),
                |_| std::future::pending(),
                Duration::from_millis(1),
            )
            .await
        );
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
        assert!(
            schedule_bounded_post_open_advisory_setup_with_budget(
                Arc::clone(&deferred),
                |_| PendingSetup(Arc::clone(&setup_dropped)),
                Duration::from_secs(10),
            )
            .await
        );
        tokio::task::yield_now().await;
        deferred.cancel_and_join().await;
        assert!(setup_dropped.load(Ordering::Acquire));
        assert!(matches!(
            deferred.readiness(),
            AdvisoryRuntimeReadinessV1::Unavailable {
                reason: AdvisoryRuntimeUnavailableReasonV1::Cancelled,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn failed_setup_can_be_retried_without_republishing_the_gateway() {
        struct Ready;
        impl AdvisoryHookOrchestrationPortV1 for Ready {
            fn admit(
                &self,
                _request: super::super::service::invocation::AdvisoryHookOrchestrationRequestV1,
            ) -> super::super::service::invocation::AdvisoryHookOrchestrationAdmissionV1
            {
                super::super::service::invocation::AdvisoryHookOrchestrationAdmissionV1::Enqueued
            }
        }

        let deferred = DeferredAdvisoryHookOrchestratorV1::new(now_micros());
        assert!(
            schedule_bounded_post_open_advisory_setup_with_budget(
                Arc::clone(&deferred),
                |_| async {
                    Err(TraceDecayError::Config {
                        message: "injected setup failure".to_owned(),
                    })
                },
                Duration::from_secs(1),
            )
            .await
        );
        tokio::task::yield_now().await;
        while matches!(
            deferred.readiness(),
            AdvisoryRuntimeReadinessV1::Warming { .. }
        ) {
            tokio::task::yield_now().await;
        }
        assert!(
            schedule_bounded_post_open_advisory_setup_with_budget(
                Arc::clone(&deferred),
                |_| async { Ok(Arc::new(Ready) as Arc<dyn AdvisoryHookOrchestrationPortV1>) },
                Duration::from_secs(1),
            )
            .await
        );
        while !matches!(
            deferred.readiness(),
            AdvisoryRuntimeReadinessV1::Ready { .. }
        ) {
            tokio::task::yield_now().await;
        }
    }
}
