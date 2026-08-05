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
const POST_OPEN_ADVISORY_RETIREMENT_GRACE: Duration = Duration::from_secs(15);
const POST_OPEN_ADVISORY_DEADLINE_GRACE: Duration = Duration::from_millis(250);

struct AdvisorySetupSettlementGuardV1(Arc<DeferredAdvisoryHookOrchestratorV1>);

impl Drop for AdvisorySetupSettlementGuardV1 {
    fn drop(&mut self) {
        self.0.mark_setup_settled();
    }
}

pub(super) struct PreparedAdvisoryRuntimeV1 {
    runtime: Arc<dyn AdvisoryHookOrchestrationPortV1>,
    commit: Option<Box<dyn FnOnce() + Send>>,
}

impl PreparedAdvisoryRuntimeV1 {
    pub(super) fn new(
        runtime: Arc<dyn AdvisoryHookOrchestrationPortV1>,
        commit: impl FnOnce() + Send + 'static,
    ) -> Self {
        Self {
            runtime,
            commit: Some(Box::new(commit)),
        }
    }

    fn commit(mut self) {
        if let Some(commit) = self.commit.take() {
            commit();
        }
    }
}

/// Starts at most one bounded setup for a retained deferred gateway.
///
/// The gateway itself is already published, so callers see `warming` while
/// this future runs. Project-runtime retirement cancels and joins setup until
/// every staged publication has either committed or rolled back.
pub(super) async fn schedule_bounded_post_open_advisory_setup<F, Fut>(
    project_root: PathBuf,
    deferred: Arc<DeferredAdvisoryHookOrchestratorV1>,
    setup: F,
) -> bool
where
    F: FnOnce(crate::application::context::CancellationToken) -> Fut + Send + 'static,
    Fut: Future<Output = Result<PreparedAdvisoryRuntimeV1>> + Send + 'static,
{
    schedule_bounded_post_open_advisory_setup_with_budget(
        project_root,
        deferred,
        setup,
        POST_OPEN_ADVISORY_SETUP_BUDGET,
    )
    .await
}

async fn schedule_bounded_post_open_advisory_setup_with_budget<F, Fut>(
    project_root: PathBuf,
    deferred: Arc<DeferredAdvisoryHookOrchestratorV1>,
    setup: F,
    budget: Duration,
) -> bool
where
    F: FnOnce(crate::application::context::CancellationToken) -> Fut + Send + 'static,
    Fut: Future<Output = Result<PreparedAdvisoryRuntimeV1>> + Send + 'static,
{
    if !deferred.claim_setup() {
        deferred.join_setup().await;
        return false;
    }
    let task_deferred = Arc::clone(&deferred);
    let task = tokio::spawn(async move {
        let _settlement = AdvisorySetupSettlementGuardV1(Arc::clone(&deferred));
        let cancellation = deferred.cancellation();
        let setup_started_at = match deferred.readiness() {
            AdvisoryRuntimeReadinessV1::Warming { started_at }
            | AdvisoryRuntimeReadinessV1::Ready { started_at, .. }
            | AdvisoryRuntimeReadinessV1::Unavailable { started_at, .. } => started_at,
        };
        enum SetupOutcome {
            Ready(PreparedAdvisoryRuntimeV1),
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
                join_cancelled_setup(&mut setup_task, POST_OPEN_ADVISORY_RETIREMENT_GRACE).await;
                SetupOutcome::Cancelled
            },
            () = tokio::time::sleep(budget) => {
                work_cancellation.cancel();
                join_cancelled_setup(&mut setup_task, POST_OPEN_ADVISORY_DEADLINE_GRACE).await;
                SetupOutcome::DeadlineExceeded
            }
        };
        let finished_at = now_micros();
        match outcome {
            SetupOutcome::Ready(prepared) => {
                let runtime = Arc::clone(&prepared.runtime);
                if deferred.mark_ready(runtime, finished_at) {
                    prepared.commit();
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
    task_deferred.retain_setup_task(task);
    true
}

async fn join_cancelled_setup(
    task: &mut tokio::task::JoinHandle<Result<PreparedAdvisoryRuntimeV1>>,
    grace: Duration,
) {
    match tokio::time::timeout(grace, &mut *task).await {
        Ok(_) => {}
        Err(_) => {
            task.abort();
            let _ = task.await;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::*;

    #[tokio::test(start_paused = true)]
    async fn post_open_advisory_setup_has_a_truthful_terminal_deadline() {
        let deferred = DeferredAdvisoryHookOrchestratorV1::new(now_micros());
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        assert!(
            schedule_bounded_post_open_advisory_setup_with_budget(
                PathBuf::from("/project"),
                Arc::clone(&deferred),
                move |_| async move {
                    started_tx.send(()).unwrap();
                    std::future::pending().await
                },
                Duration::from_secs(1),
            )
            .await
        );
        started_rx.await.unwrap();
        tokio::time::advance(Duration::from_secs(1)).await;
        deferred.join_setup().await;
        assert!(matches!(
            deferred.readiness(),
            AdvisoryRuntimeReadinessV1::Unavailable {
                reason: AdvisoryRuntimeUnavailableReasonV1::DeadlineExceeded,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn deadline_rolls_back_a_late_success_before_reporting_terminal() {
        struct Ready;
        impl AdvisoryHookOrchestrationPortV1 for Ready {
            fn admit(
                &self,
                _request: super::super::service::invocation::AdvisoryHookOrchestrationRequestV1,
            ) -> super::super::service::invocation::AdvisoryHookOrchestrationAdmissionV1
            {
                super::super::service::invocation::AdvisoryHookOrchestrationAdmissionV1::Unavailable
            }
        }
        struct PublicationGuard {
            committed: bool,
            rolled_back: Arc<AtomicBool>,
        }
        impl PublicationGuard {
            fn commit(mut self) {
                self.committed = true;
            }
        }
        impl Drop for PublicationGuard {
            fn drop(&mut self) {
                if !self.committed {
                    self.rolled_back.store(true, Ordering::Release);
                }
            }
        }

        let deferred = DeferredAdvisoryHookOrchestratorV1::new(now_micros());
        let rolled_back = Arc::new(AtomicBool::new(false));
        let observed_rollback = Arc::clone(&rolled_back);
        assert!(
            schedule_bounded_post_open_advisory_setup_with_budget(
                PathBuf::from("/project"),
                Arc::clone(&deferred),
                move |cancellation| async move {
                    cancellation.cancelled().await;
                    let guard = PublicationGuard {
                        committed: false,
                        rolled_back: observed_rollback,
                    };
                    Ok(PreparedAdvisoryRuntimeV1::new(Arc::new(Ready), move || {
                        guard.commit();
                    }))
                },
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
        .expect("late success reached a terminal");
        deferred.cancel_and_join_setup().await;
        assert!(rolled_back.load(Ordering::Acquire));
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
        struct SetupDrop(Arc<AtomicBool>);

        impl Drop for SetupDrop {
            fn drop(&mut self) {
                self.0.store(true, Ordering::Release);
            }
        }

        let deferred = DeferredAdvisoryHookOrchestratorV1::new(now_micros());
        let setup_dropped = Arc::new(AtomicBool::new(false));
        let setup_drop = Arc::clone(&setup_dropped);
        assert!(
            schedule_bounded_post_open_advisory_setup_with_budget(
                PathBuf::from("/project"),
                Arc::clone(&deferred),
                move |cancellation| async move {
                    let _drop = SetupDrop(setup_drop);
                    cancellation.cancelled().await;
                    Err(TraceDecayError::Config {
                        message: "fixture setup cancelled".to_owned(),
                    })
                },
                Duration::from_secs(10),
            )
            .await
        );
        tokio::task::yield_now().await;
        deferred.cancel_and_join_setup().await;
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

    #[tokio::test]
    async fn duplicate_setup_claim_joins_the_retained_task() {
        let deferred = DeferredAdvisoryHookOrchestratorV1::new(now_micros());
        let (release, setup) = tokio::sync::oneshot::channel::<()>();
        assert!(
            schedule_bounded_post_open_advisory_setup_with_budget(
                PathBuf::from("/project"),
                Arc::clone(&deferred),
                move |_| async move {
                    setup.await.map_err(|error| TraceDecayError::Config {
                        message: error.to_string(),
                    })?;
                    Err(TraceDecayError::Config {
                        message: "fixture setup stopped".to_owned(),
                    })
                },
                Duration::from_secs(10),
            )
            .await
        );

        let joiner_deferred = Arc::clone(&deferred);
        let joiner = tokio::spawn(async move {
            schedule_bounded_post_open_advisory_setup_with_budget(
                PathBuf::from("/project"),
                joiner_deferred,
                |_| std::future::pending(),
                Duration::from_secs(10),
            )
            .await
        });
        tokio::task::yield_now().await;
        assert!(!joiner.is_finished(), "duplicate setup must join its owner");
        release.send(()).unwrap();
        assert!(!joiner.await.unwrap());
    }

    #[tokio::test(start_paused = true)]
    async fn deadline_rolls_back_partial_publication_before_setup_join() {
        struct FixtureRuntime;
        impl AdvisoryHookOrchestrationPortV1 for FixtureRuntime {
            fn admit(
                &self,
                _request: super::super::service::invocation::AdvisoryHookOrchestrationRequestV1,
            ) -> super::super::service::invocation::AdvisoryHookOrchestrationAdmissionV1
            {
                super::super::service::invocation::AdvisoryHookOrchestrationAdmissionV1::Unavailable
            }
        }
        struct PublicationGuard {
            committed: bool,
            rolled_back: Arc<AtomicBool>,
        }
        impl PublicationGuard {
            fn commit(mut self) {
                self.committed = true;
            }
        }
        impl Drop for PublicationGuard {
            fn drop(&mut self) {
                if !self.committed {
                    self.rolled_back.store(true, Ordering::Release);
                }
            }
        }

        let deferred = DeferredAdvisoryHookOrchestratorV1::new(now_micros());
        let rolled_back = Arc::new(AtomicBool::new(false));
        let setup_rolled_back = Arc::clone(&rolled_back);
        let (staged_tx, staged_rx) = tokio::sync::oneshot::channel();
        assert!(
            schedule_bounded_post_open_advisory_setup_with_budget(
                PathBuf::from("/project"),
                Arc::clone(&deferred),
                move |_| async move {
                    let guard = PublicationGuard {
                        committed: false,
                        rolled_back: setup_rolled_back,
                    };
                    staged_tx.send(()).unwrap();
                    std::future::pending::<()>().await;
                    let runtime: Arc<dyn AdvisoryHookOrchestrationPortV1> =
                        Arc::new(FixtureRuntime);
                    Ok(PreparedAdvisoryRuntimeV1::new(runtime, move || {
                        guard.commit();
                    }))
                },
                Duration::from_secs(1),
            )
            .await
        );
        staged_rx.await.unwrap();
        tokio::time::advance(Duration::from_secs(1)).await;
        deferred.join_setup().await;

        assert!(rolled_back.load(Ordering::Acquire));
        assert!(matches!(
            deferred.readiness(),
            AdvisoryRuntimeReadinessV1::Unavailable {
                reason: AdvisoryRuntimeUnavailableReasonV1::DeadlineExceeded,
                ..
            }
        ));
    }
}
