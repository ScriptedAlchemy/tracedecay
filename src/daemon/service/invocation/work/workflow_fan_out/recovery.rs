//! Durable restart recovery for workflow fan-out runs.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use tracedecay_application::{
    ApplicationContractError, CancellationContext, Deadline, RequestContext, RequestId,
};
use tracedecay_domain::UtcMicros;

use crate::daemon_contract::DaemonInvocationProblem;

use super::super::super::current_micros;
use super::super::workflow_run_control::workflow_run_storage_problem;
use super::super::{RegisteredWorkRuntime, work_background_context};
use super::reconcile_workflow_fan_out;

const RECOVERY_RETRY_DELAY: std::time::Duration = std::time::Duration::from_secs(5);

pub(in crate::daemon::service::invocation) fn reconcile_active_workflow_fan_out(
    registered: &RegisteredWorkRuntime,
    attempt_processes: Arc<super::super::super::work_attempt_exec::WorkAttemptProcessRegistryV1>,
    project_root: &Path,
    observability_producer: Option<
        Arc<tracedecay_usecases::observability::BoundedObservabilityProducerV1>,
    >,
) -> Result<(), DaemonInvocationProblem> {
    reconcile_active_workflow_fan_out_page(
        registered,
        attempt_processes,
        project_root,
        observability_producer,
        None,
    )
    .map(|_| ())
}

fn reconcile_active_workflow_fan_out_page(
    registered: &RegisteredWorkRuntime,
    attempt_processes: Arc<super::super::super::work_attempt_exec::WorkAttemptProcessRegistryV1>,
    project_root: &Path,
    observability_producer: Option<
        Arc<tracedecay_usecases::observability::BoundedObservabilityProducerV1>,
    >,
    cursor: Option<&tracedecay_application::WorkflowActiveRunRecoveryCursorV1>,
) -> Result<
    Option<tracedecay_application::WorkflowActiveRunRecoveryCursorV1>,
    DaemonInvocationProblem,
> {
    let services = registered
        .database
        .workflow_application_services()
        .map_err(|_| DaemonInvocationProblem::Unavailable)?;
    let registered_authority = tracedecay_domain::WorkAuthority::new(
        registered.grant.scope.project_id.clone(),
        registered.grant.scope.repository_id.clone(),
        registered.grant.scope.worktree_id.clone(),
        registered.actor.clone(),
        registered.grant.digest.clone(),
    )
    .map_err(|_| DaemonInvocationProblem::Unavailable)?;
    let page = tracedecay_application::WorkflowRunStoragePort::active_projection_page(
        services.effects(),
        &registered_authority,
        cursor,
    )
    .map_err(workflow_run_storage_problem)?;
    for projection in page.projections {
        let Some(identity) = projection
            .fan_out_plans()
            .values()
            .flat_map(|plan| &plan.children)
            .map(|child| &child.attempt_identity)
            .next()
        else {
            continue;
        };
        let context = work_background_context(registered, identity)
            .map_err(|_| DaemonInvocationProblem::Unavailable)?;
        reconcile_workflow_fan_out(
            registered,
            &services,
            &context,
            projection,
            current_micros(),
            Arc::clone(&attempt_processes),
            project_root,
            observability_producer.clone(),
        )?;
    }
    let census_page =
        tracedecay_application::WorkflowFanOutCensusStoragePort::census_backfill_projection_page(
            services.effects(),
            &registered_authority,
            cursor,
        )
        .map_err(|_| DaemonInvocationProblem::Unavailable)?;
    if census_page.continuation != page.continuation {
        return Err(DaemonInvocationProblem::Unavailable);
    }
    for projection in census_page.projections {
        let Some(identity) = projection
            .fan_out_plans()
            .values()
            .flat_map(|plan| &plan.children)
            .map(|child| &child.attempt_identity)
            .next()
        else {
            return Err(DaemonInvocationProblem::Unavailable);
        };
        let context = work_background_context(registered, identity)
            .map_err(|_| DaemonInvocationProblem::Unavailable)?;
        super::super::workflow_census::persist_workflow_fan_out_census(
            registered,
            &services,
            &context,
            &projection,
            current_micros(),
            observability_producer.clone(),
        );
    }
    Ok(page.continuation)
}

fn resume_work_attempts_for_workflow_recovery(
    registered: &RegisteredWorkRuntime,
    workflows: &crate::global_db::RegisteredWorkflowApplicationServicesV1,
    context: &RequestContext,
    attempt_processes: &Arc<super::super::super::work_attempt_exec::WorkAttemptProcessRegistryV1>,
    project_root: &Path,
    observability_producer: Option<
        Arc<tracedecay_usecases::observability::BoundedObservabilityProducerV1>,
    >,
) -> Result<(), DaemonInvocationProblem> {
    let work = registered
        .database
        .work_application_services()
        .map_err(|error| {
            tracing::error!(
                ?error,
                stage = "work_application_services",
                "workflow fan-out startup recovery authority failed"
            );
            DaemonInvocationProblem::Unavailable
        })?;
    let recovery = work
        .attempts()
        .resume(
            context,
            &tracedecay_application::ResumeWorkAttemptsCommand {
                occurred_at: current_micros(),
            },
        )
        .map_err(|error| {
            tracing::error!(
                ?error,
                stage = "resume_work_attempts",
                "workflow fan-out startup recovery authority failed"
            );
            DaemonInvocationProblem::Unavailable
        })?;
    for attempt in recovery.recovery_required {
        let binding = tracedecay_application::WorkflowRunStoragePort::fan_out_binding(
            workflows.effects(),
            attempt.identity(),
        )
        .map_err(workflow_run_storage_problem)?;
        let paused = match binding {
            Some(binding) => {
                tracedecay_application::WorkflowRunStoragePort::projection(
                    workflows.effects(),
                    &binding.run_id,
                )
                .map_err(workflow_run_storage_problem)?
                .status()
                    == tracedecay_domain::WorkflowRunStatus::Paused
            }
            None => false,
        };
        if paused {
            continue;
        }
        super::super::super::work_attempt_exec::spawn_attempt_execution(
            registered.clone(),
            Arc::clone(attempt_processes),
            project_root.to_path_buf(),
            attempt,
            observability_producer.clone(),
        );
    }
    Ok(())
}

fn recover_workflow_fan_out_startup(
    registered: &RegisteredWorkRuntime,
    attempt_processes: &Arc<super::super::super::work_attempt_exec::WorkAttemptProcessRegistryV1>,
    project_root: &Path,
    observability_producer: Option<
        Arc<tracedecay_usecases::observability::BoundedObservabilityProducerV1>,
    >,
) -> Result<(), DaemonInvocationProblem> {
    let workflows = registered
        .database
        .workflow_application_services()
        .map_err(|error| {
            tracing::error!(
                ?error,
                stage = "workflow_application_services",
                "workflow fan-out startup recovery authority failed"
            );
            DaemonInvocationProblem::Unavailable
        })?;
    let context = workflow_fan_out_recovery_context(registered).map_err(|error| {
        tracing::error!(
            ?error,
            stage = "startup_context",
            "workflow fan-out startup recovery authority failed"
        );
        DaemonInvocationProblem::Unavailable
    })?;
    resume_work_attempts_for_workflow_recovery(
        registered,
        &workflows,
        &context,
        attempt_processes,
        project_root,
        observability_producer,
    )
}

fn workflow_fan_out_recovery_context(
    registered: &RegisteredWorkRuntime,
) -> Result<RequestContext, ApplicationContractError> {
    const BACKGROUND_DEADLINE_MICROS: i64 = 86_400_000_000;
    RequestContext::new(
        registered.actor.clone(),
        registered.grant.scope.clone(),
        registered.grant.clone(),
        RequestId::new("workflow-fan-out-startup-recovery")?,
        Deadline::new(UtcMicros(
            current_micros()
                .0
                .saturating_add(BACKGROUND_DEADLINE_MICROS),
        ))?,
        CancellationContext::active("cancel.workflow-fan-out-startup-recovery")?,
    )
}

#[derive(Clone)]
pub(in crate::daemon::service::invocation) struct WorkflowFanOutRecoveryOwnerV1 {
    inner: Arc<WorkflowFanOutRecoveryInnerV1>,
}

struct WorkflowFanOutRecoveryInnerV1 {
    cancellation: tracedecay_runtime_core::cancellation::CancellationToken,
    task: std::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
    registered: Arc<std::sync::Mutex<RegisteredWorkRuntime>>,
}

impl WorkflowFanOutRecoveryOwnerV1 {
    pub(in crate::daemon::service::invocation) fn mount(
        registered: RegisteredWorkRuntime,
        attempt_processes: Arc<
            super::super::super::work_attempt_exec::WorkAttemptProcessRegistryV1,
        >,
        project_root: PathBuf,
        observability_producer: Option<
            Arc<tracedecay_usecases::observability::BoundedObservabilityProducerV1>,
        >,
        holder_admission: crate::daemon::native_integration::WorktreeHolderAdmissionFenceV1,
    ) -> Result<Self, DaemonInvocationProblem> {
        let holder_root = project_root
            .canonicalize()
            .map_err(|_| DaemonInvocationProblem::Unavailable)?;
        let cancellation = tracedecay_runtime_core::cancellation::CancellationToken::new();
        let worker_cancellation = cancellation.clone();
        let registered = Arc::new(std::sync::Mutex::new(registered));
        let worker_registered = Arc::clone(&registered);
        let task = tokio::spawn(async move {
            let mut cursor = None;
            let mut resume_pending = true;
            loop {
                let admission = tokio::select! {
                    biased;
                    () = worker_cancellation.cancelled() => return,
                    admission = holder_admission.admit_holders([holder_root.clone()]) => admission,
                };
                let Some(_holder_admission) = admission else {
                    tracing::warn!(
                        root = %holder_root.display(),
                        "workflow fan-out restart recovery is fenced by native worktree cleanup"
                    );
                    tokio::select! {
                        biased;
                        () = worker_cancellation.cancelled() => return,
                        () = tokio::time::sleep(RECOVERY_RETRY_DELAY) => continue,
                    }
                };

                if resume_pending {
                    let resume_registered = worker_registered
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .clone();
                    let resume_attempt_processes = Arc::clone(&attempt_processes);
                    let resume_project_root = project_root.clone();
                    let resume_producer = observability_producer.clone();
                    let mut resume = tokio::task::spawn_blocking(move || {
                        recover_workflow_fan_out_startup(
                            &resume_registered,
                            &resume_attempt_processes,
                            &resume_project_root,
                            resume_producer,
                        )
                    });
                    let resumed = tokio::select! {
                        biased;
                        () = worker_cancellation.cancelled() => {
                            resume.abort();
                            return;
                        }
                        result = &mut resume => result,
                    };
                    match resumed {
                        Ok(Ok(())) => resume_pending = false,
                        Ok(Err(problem)) => {
                            tracing::warn!(
                                ?problem,
                                "workflow fan-out startup recovery remains pending"
                            );
                            drop(_holder_admission);
                            tokio::select! {
                                biased;
                                () = worker_cancellation.cancelled() => return,
                                () = tokio::time::sleep(RECOVERY_RETRY_DELAY) => continue,
                            }
                        }
                        Err(error) => {
                            tracing::warn!(
                                ?error,
                                "workflow fan-out startup recovery worker failed"
                            );
                            drop(_holder_admission);
                            tokio::select! {
                                biased;
                                () = worker_cancellation.cancelled() => return,
                                () = tokio::time::sleep(RECOVERY_RETRY_DELAY) => continue,
                            }
                        }
                    }
                }

                let page_registered = worker_registered
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .clone();
                let page_attempt_processes = Arc::clone(&attempt_processes);
                let page_project_root = project_root.clone();
                let page_producer = observability_producer.clone();
                let page_cursor = cursor.clone();
                let mut page = tokio::task::spawn_blocking(move || {
                    reconcile_active_workflow_fan_out_page(
                        &page_registered,
                        page_attempt_processes,
                        &page_project_root,
                        page_producer,
                        page_cursor.as_ref(),
                    )
                });
                let delay = tokio::select! {
                    biased;
                    () = worker_cancellation.cancelled() => {
                        page.abort();
                        return;
                    }
                    result = &mut page => match result {
                        Ok(Ok(continuation)) => {
                            cursor = continuation;
                            if cursor.is_some() {
                                std::time::Duration::from_millis(100)
                            } else {
                                RECOVERY_RETRY_DELAY
                            }
                        }
                        Ok(Err(problem)) => {
                            tracing::warn!(
                                ?problem,
                                "bounded workflow fan-out recovery page failed"
                            );
                            RECOVERY_RETRY_DELAY
                        }
                        Err(error) => {
                            tracing::warn!(
                                ?error,
                                "bounded workflow fan-out recovery worker failed"
                            );
                            RECOVERY_RETRY_DELAY
                        }
                    },
                };
                drop(_holder_admission);
                tokio::select! {
                    biased;
                    () = worker_cancellation.cancelled() => return,
                    () = tokio::time::sleep(delay) => {}
                }
            }
        });
        Ok(Self {
            inner: Arc::new(WorkflowFanOutRecoveryInnerV1 {
                cancellation,
                task: std::sync::Mutex::new(Some(task)),
                registered,
            }),
        })
    }

    pub(in crate::daemon::service::invocation) fn refresh_grant(
        &self,
        grant: tracedecay_application::CapabilityGrantSnapshot,
    ) {
        self.inner
            .registered
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .grant = grant;
    }

    pub(in crate::daemon::service) async fn shutdown(&self) {
        self.inner.cancellation.cancel();
        let task = self
            .inner
            .task
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some(task) = task
            && let Err(error) = task.await
        {
            tracing::warn!(%error, "workflow fan-out recovery shutdown failed");
        }
    }
}

impl Drop for WorkflowFanOutRecoveryInnerV1 {
    fn drop(&mut self) {
        self.cancellation.cancel();
        let task = match self.task.get_mut() {
            Ok(task) => task.take(),
            Err(poisoned) => poisoned.into_inner().take(),
        };
        if let Some(task) = task {
            task.abort();
        }
    }
}
