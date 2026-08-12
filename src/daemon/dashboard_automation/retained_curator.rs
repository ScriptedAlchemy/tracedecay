//! Retained application adapter for the canonical Memory Curator.

use std::sync::Arc;

use crate::tracedecay::TraceDecay;
use tracedecay_agent_hosts::automation::AutomationRunControl;
use tracedecay_agent_hosts::automation::backend::CodexAppServerBackend;
use tracedecay_agent_hosts::automation::config::from_configuration_snapshot;
use tracedecay_agent_hosts::automation::run_ledger::AutomationTrigger;
use tracedecay_agent_hosts::automation::runner::{
    MemoryCuratorAutomationOptions, run_memory_curator_with_backend,
};
use tracedecay_application::ApplicationOutcome;
use tracedecay_application::now_micros;
use tracedecay_application::retained_surfaces::{
    FactStoreCurateRequestV1, RetainedSurfaceExecutionContextV1, RetainedSurfaceExecutionErrorV1,
    RetainedSurfaceResultV1,
};

const MEMORY_CURATOR_REQUEST_TIMEOUT_SECS: u64 = 80;

pub(crate) async fn execute_retained_memory_curator(
    cg: &TraceDecay,
    invocation_service: &crate::daemon::service::invocation::DaemonInvocationService,
    context: &RetainedSurfaceExecutionContextV1<'_>,
    request: &FactStoreCurateRequestV1,
) -> Result<ApplicationOutcome<RetainedSurfaceResultV1>, RetainedSurfaceExecutionErrorV1> {
    let pinned = cg
        .configuration_runtime()
        .client()
        .current()
        .await
        .map_err(|_| RetainedSurfaceExecutionErrorV1::Unavailable)?;
    let mut config = from_configuration_snapshot(&pinned.snapshot)
        .map_err(|_| RetainedSurfaceExecutionErrorV1::Unavailable)?;
    let min_confidence = f64::from(request.min_confidence_millionths) / 1_000_000.0;
    config.timeout_secs = config.timeout_secs.min(MEMORY_CURATOR_REQUEST_TIMEOUT_SECS);
    let backend = CodexAppServerBackend::from_automation_config(&config);
    let configuration_digest =
        crate::daemon::automation_effect::pinned_automation_configuration_digest(
            &pinned.revision_id,
            &pinned.snapshot.effective_behavior_digest,
            &pinned.snapshot.resolution_provenance_digest,
        )
        .map_err(|_| RetainedSurfaceExecutionErrorV1::Unavailable)?;
    let run_id = context.request_context.request_id().as_str().to_owned();
    let admission = crate::daemon::automation_effect::AutomationEffectAuthority::prepare(
        invocation_service,
        cg,
        cg.project_root(),
        &cg.store_layout().dashboard_root,
        context.request_context.request_id().clone(),
        context.request_context.deadline().clone(),
        context.cancellation_signal,
        context.observed_at,
        configuration_digest,
        crate::daemon::automation_effect::memory_curator_run_request(
            &run_id,
            request.fact_review_limit as usize,
            min_confidence,
        )
        .map_err(|_| RetainedSurfaceExecutionErrorV1::InvalidRequest)?,
    )
    .await
    .map_err(|_| RetainedSurfaceExecutionErrorV1::Unavailable)?;
    let effect = match admission {
        crate::daemon::automation_effect::AutomationEffectAdmission::Execute(effect) => effect,
        crate::daemon::automation_effect::AutomationEffectAdmission::Replay(terminal) => {
            return terminal.into_outcome().map_err(automation_problem);
        }
        crate::daemon::automation_effect::AutomationEffectAdmission::PreAdmissionProblem(
            problem,
        ) => {
            return Err(RetainedSurfaceExecutionErrorV1::ApplicationProblem(
                problem.problem.source().clone(),
            ));
        }
    };
    let control = AutomationRunControl::from_interrupted(Arc::new({
        let cancellation = context.cancellation_signal.clone();
        let deadline = context.request_context.deadline().clone();
        move || cancellation.is_cancelled() || deadline.is_elapsed_at(now_micros())
    }));
    let run = match run_memory_curator_with_backend(
        cg,
        &config,
        &pinned.revision_id,
        &backend,
        MemoryCuratorAutomationOptions {
            trigger: AutomationTrigger::Application,
            run_id: Some(run_id),
            fact_review_limit: request.fact_review_limit as usize,
            min_confidence,
        },
        &control,
    )
    .await
    {
        Ok(run) => run,
        Err(error) => {
            let problem = effect
                .settle_problem(&error)
                .await
                .map_err(|_| RetainedSurfaceExecutionErrorV1::Unavailable)?;
            return Err(automation_problem(problem));
        }
    };
    let terminal = effect
        .settle_run(&run.ledger_record, run.committed_receipt.as_ref())
        .await
        .map_err(|_| RetainedSurfaceExecutionErrorV1::Unavailable)?;
    if let Some(producer) = crate::daemon::project_automation_observation_producer(
        invocation_service,
        cg.project_root(),
    )
    .await
    {
        crate::daemon::record_project_automation_run(
            producer.as_ref(),
            cg.project_root(),
            &run.ledger_record,
            "fact_store_curate",
        );
    }
    terminal.into_outcome().map_err(automation_problem)
}

fn automation_problem(
    problem: crate::daemon::automation_effect::AutomationSettledProblem,
) -> RetainedSurfaceExecutionErrorV1 {
    RetainedSurfaceExecutionErrorV1::ApplicationProblem(problem.problem.problem.source().clone())
}
