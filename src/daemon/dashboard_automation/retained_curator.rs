//! Retained application adapter for the canonical Memory Curator.

use std::sync::Arc;

use crate::tracedecay::TraceDecay;
use tracedecay_agent_hosts::automation::AutomationRunControl;
use tracedecay_agent_hosts::automation::backend::CodexAppServerBackend;
use tracedecay_agent_hosts::automation::config::from_configuration_snapshot;
use tracedecay_agent_hosts::automation::run_ledger::AutomationTrigger;
use tracedecay_agent_hosts::automation::runner::{
    MemoryCuratorAutomationOptions, run_memory_curator_with_backend_for_retained_settlement,
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
    let automation_request = request
        .automation_request(context.request_context.request_id())
        .map_err(|_| RetainedSurfaceExecutionErrorV1::InvalidRequest)?;
    let run_id = automation_request.run_id.as_str().to_owned();
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
        automation_request,
    )
    .await
    .map_err(|_| RetainedSurfaceExecutionErrorV1::Unavailable)?;
    let effect = match admission {
        crate::daemon::automation_effect::AutomationEffectAdmission::Execute(effect) => effect,
        crate::daemon::automation_effect::AutomationEffectAdmission::Replay(terminal) => {
            return terminal.into_outcome().map_err(automation_problem);
        }
        crate::daemon::automation_effect::AutomationEffectAdmission::Conflict => {
            return Err(RetainedSurfaceExecutionErrorV1::Conflict);
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
    let observation_producer = crate::daemon::project_automation_observation_producer(
        invocation_service,
        cg.project_root(),
    )
    .await;
    let project_root = cg.project_root().to_path_buf();
    let observer = observation_producer.map(|producer| {
        super::automation_run_observer(producer, project_root, "fact_store_curate")
    });
    let retained_run = run_memory_curator_with_backend_for_retained_settlement(
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
    .await;
    let waiter = effect.start_retained_automation_settlement(retained_run, observer, |run| {
        (run.ledger_record, run.committed_receipt)
    });
    let settlement = waiter
        .wait()
        .await
        .map_err(|_| RetainedSurfaceExecutionErrorV1::Unavailable)?;
    match settlement {
        crate::daemon::automation_effect::RetainedAutomationSettlementOutcome::Run {
            terminal,
            record: _record,
        } => terminal.into_outcome().map_err(automation_problem),
        crate::daemon::automation_effect::RetainedAutomationSettlementOutcome::Problem {
            problem,
            record: _record,
        } => Err(automation_problem(problem)),
        crate::daemon::automation_effect::RetainedAutomationSettlementOutcome::Reused {
            record: _record,
        }
        | crate::daemon::automation_effect::RetainedAutomationSettlementOutcome::AbandonedObserved {
            record: _record,
        } => Err(RetainedSurfaceExecutionErrorV1::Unavailable),
    }
}

fn automation_problem(
    problem: Box<crate::daemon::automation_effect::AutomationSettledProblem>,
) -> RetainedSurfaceExecutionErrorV1 {
    RetainedSurfaceExecutionErrorV1::ApplicationProblem(problem.problem.problem.source().clone())
}
