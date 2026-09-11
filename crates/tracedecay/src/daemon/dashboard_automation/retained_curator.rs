//! Retained application adapter for the canonical Memory Curator.

use std::sync::Arc;

use crate::tracedecay::TraceDecay;
use tracedecay_automation_runtime::automation::AutomationRunControl;
use tracedecay_automation_runtime::automation::backend::CodexAppServerBackend;
use tracedecay_automation_runtime::automation::config::from_configuration_snapshot;
use tracedecay_automation_runtime::automation::run_ledger::AutomationTrigger;
use tracedecay_automation_runtime::automation::runner::{
    MemoryCuratorAutomationOptions, run_memory_curator_with_backend_for_retained_settlement,
};
use tracedecay_contracts::ApplicationOutcome;
use tracedecay_contracts::now_micros;
use tracedecay_contracts::retained_surfaces::{
    FactStoreCurateRequestV1, RetainedSurfaceExecutionContextV1, RetainedSurfaceExecutionErrorV1,
    RetainedSurfaceResultV1,
};
use tracedecay_daemon_service::DaemonInvocationService;

const MEMORY_CURATOR_REQUEST_TIMEOUT_SECS: u64 = 80;

#[hotpath::measure(label = "daemon.dashboard.automation.curate", future = true)]
#[expect(
    clippy::too_many_lines,
    reason = "Curation pins the live configuration digest and admits one effect before the curator backend runs; a failed pin never starts a run."
)]
pub(crate) async fn execute_retained_memory_curator(
    cg: &TraceDecay,
    invocation_service: &DaemonInvocationService,
    context: &RetainedSurfaceExecutionContextV1<'_>,
    request: &FactStoreCurateRequestV1,
) -> Result<ApplicationOutcome<RetainedSurfaceResultV1>, RetainedSurfaceExecutionErrorV1> {
    let pinned = cg
        .configuration_runtime()
        .client()
        .current()
        .await
        .map_err(|error| {
            RetainedSurfaceExecutionErrorV1::unavailable(format!(
                "the automation configuration could not be loaded: {error}"
            ))
        })?;
    let mut config = from_configuration_snapshot(pinned.snapshot()).map_err(|error| {
        RetainedSurfaceExecutionErrorV1::unavailable(format!(
            "the automation configuration snapshot is invalid: {error}"
        ))
    })?;
    let min_confidence = f64::from(request.min_confidence_millionths) / 1_000_000.0;
    config.timeout_secs = config.timeout_secs.min(MEMORY_CURATOR_REQUEST_TIMEOUT_SECS);
    let backend = CodexAppServerBackend::from_automation_config(&config);
    let configuration_digest =
        tracedecay_automation_runtime::automation::effect_runtime::pinned_automation_configuration_digest(
            pinned.revision_id(),
            &pinned.snapshot().effective_behavior_digest,
            &pinned.snapshot().resolution_provenance_digest,
        )
        .map_err(|error| {
            RetainedSurfaceExecutionErrorV1::unavailable(format!(
                "the pinned automation configuration digest could not be assembled: {error}"
            ))
        })?;
    let automation_request = request
        .automation_request(context.request_context.request_id())
        .map_err(|_| RetainedSurfaceExecutionErrorV1::InvalidRequest)?;
    let run_id = automation_request.run_id.as_str().to_owned();
    let automation_context = cg.automation_project_context().map_err(|error| {
        RetainedSurfaceExecutionErrorV1::unavailable(format!(
            "the automation project context could not be composed: {error}"
        ))
    })?;
    let admission = crate::daemon::automation_effect::prepare(
        invocation_service,
        cg,
        automation_context.project_root(),
        &automation_context.dashboard_root,
        context.request_context.request_id().clone(),
        context.request_context.deadline().clone(),
        context.cancellation_signal,
        context.observed_at,
        configuration_digest,
        automation_request,
    )
    .await
    .map_err(|error| {
        RetainedSurfaceExecutionErrorV1::unavailable(format!(
            "the automation effect authority could not be prepared: {error}"
        ))
    })?;
    let effect = match admission {
        tracedecay_automation_runtime::automation::effect_runtime::AutomationEffectAdmission::Execute(effect) => effect,
        tracedecay_automation_runtime::automation::effect_runtime::AutomationEffectAdmission::Replay(terminal) => {
            return terminal.into_outcome().map_err(automation_problem);
        }
        tracedecay_automation_runtime::automation::effect_runtime::AutomationEffectAdmission::Conflict => {
            return Err(RetainedSurfaceExecutionErrorV1::Conflict);
        }
        tracedecay_automation_runtime::automation::effect_runtime::AutomationEffectAdmission::PreAdmissionProblem(
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
        automation_context.project_root(),
    )
    .await;
    let project_root = automation_context.project_root().to_path_buf();
    let observer = observation_producer.map(|producer| {
        super::automation_run_observer(producer, project_root, "fact_store_curate")
    });
    let retained_run = run_memory_curator_with_backend_for_retained_settlement(
        &automation_context,
        &config,
        pinned.revision_id(),
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
    let settlement = waiter.wait().await.map_err(|error| {
        RetainedSurfaceExecutionErrorV1::unavailable(format!(
            "the automation run settlement could not be observed: {error}"
        ))
    })?;
    match settlement {
        tracedecay_automation_runtime::automation::effect_runtime::RetainedAutomationSettlementOutcome::Run {
            terminal,
            record: _record,
        } => terminal.into_outcome().map_err(automation_problem),
        tracedecay_automation_runtime::automation::effect_runtime::RetainedAutomationSettlementOutcome::Problem {
            problem,
            record: _record,
        } => Err(automation_problem(problem)),
        tracedecay_automation_runtime::automation::effect_runtime::RetainedAutomationSettlementOutcome::Reused {
            record: _record,
        }
        | tracedecay_automation_runtime::automation::effect_runtime::RetainedAutomationSettlementOutcome::AbandonedObserved {
            record: _record,
        } => Err(RetainedSurfaceExecutionErrorV1::unavailable(
            "the automation run settled without a retained terminal (reused or abandoned)",
        )),
    }
}

fn automation_problem(
    problem: Box<
        tracedecay_automation_runtime::automation::effect_runtime::AutomationSettledProblem,
    >,
) -> RetainedSurfaceExecutionErrorV1 {
    RetainedSurfaceExecutionErrorV1::ApplicationProblem(problem.problem.problem.source().clone())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use tracedecay_contracts::{
        CancellationContext, CancellationSignal, CapabilityGrantId, CapabilityGrantSnapshot,
        Deadline, DisclosureClass, RequestContext, RequestId, ResolvedScope,
        RetainedSurfaceExecutionContextV1, RetainedSurfaceOperation,
        retained_surface_application_operation,
    };
    use tracedecay_domain::{
        ActorId, ProjectId, RepositoryId, UtcMicros, WorktreeId, canonical_sha256,
    };

    use super::{
        DaemonInvocationService, FactStoreCurateRequestV1, RetainedSurfaceExecutionErrorV1,
    };

    #[tokio::test]
    async fn context_failure_precedes_retained_curator_admission() {
        let directory = tempfile::tempdir().expect("temporary project");
        let project_root = directory.path().join("project");
        let profile_root = directory.path().join("profile");
        std::fs::create_dir_all(project_root.join("src")).expect("project source directory");
        std::fs::write(project_root.join("src/lib.rs"), "pub fn fixture() {}\n")
            .expect("project source");
        let options = crate::tracedecay::TraceDecayOpenOptions {
            profile_root: Some(profile_root.clone()),
            global_db_path: Some(profile_root.join("global.db")),
        };
        let writable =
            crate::tracedecay::TraceDecay::init_with_options(&project_root, options.clone())
                .await
                .expect("initialize retained curator project");
        let dashboard_root = writable.store_layout().dashboard_root.clone();
        writable.close();
        let read_only =
            crate::tracedecay::TraceDecay::open_read_only_with_options(&project_root, options)
                .await
                .expect("open read-only retained curator project");
        let operation =
            retained_surface_application_operation(RetainedSurfaceOperation::FactStoreCurate)
                .expect("retained operation");
        let actor = ActorId::new("actor.retained-context-failure").expect("actor");
        let scope = ResolvedScope::new(
            ProjectId::new("project.retained-context-failure").expect("project id"),
            RepositoryId::new("repository.retained-context-failure").expect("repository id"),
            WorktreeId::new("worktree.retained-context-failure").expect("worktree id"),
            None,
        )
        .expect("scope");
        let request_id = RequestId::new("request.retained-context-failure").expect("request id");
        let cancellation_id = "cancel.retained-context-failure".to_owned();
        let grant = CapabilityGrantSnapshot::new(
            CapabilityGrantId::new("grant.retained-context-failure").expect("grant id"),
            1,
            canonical_sha256(&"retained-context-failure").expect("grant digest"),
            actor.clone(),
            UtcMicros(1),
            UtcMicros(i64::MAX - 1),
            scope.clone(),
            BTreeSet::from([operation.capability_id().clone()]),
            BTreeSet::from([operation.use_case_id().clone()]),
            DisclosureClass::Evidence,
        )
        .expect("grant");
        let request_context = RequestContext::new(
            actor,
            scope,
            grant,
            request_id,
            Deadline::new(UtcMicros(i64::MAX)).expect("deadline"),
            CancellationContext::active(cancellation_id.clone()).expect("cancellation context"),
        )
        .expect("request context");
        let cancellation =
            CancellationSignal::active(cancellation_id).expect("cancellation signal");
        let execution = RetainedSurfaceExecutionContextV1 {
            request_context: &request_context,
            cancellation_signal: &cancellation,
            operation: &operation,
            observed_at: UtcMicros(2),
        };

        let error = super::execute_retained_memory_curator(
            &read_only,
            &DaemonInvocationService::default(),
            &execution,
            &FactStoreCurateRequestV1::default(),
        )
        .await
        .expect_err("read-only automation context must fail before admission");

        let RetainedSurfaceExecutionErrorV1::Unavailable { detail } = error else {
            panic!("context failure must win over admission: {error:?}");
        };
        assert!(detail.contains("open read-only"));
        assert!(
            !dashboard_root.join("automation_effects").exists(),
            "context failure must not leave a durable automation reservation"
        );
    }
}
