//! Typed application settlement for manual automation runs.

use std::path::PathBuf;
use std::sync::Arc;

use serde_json::{Value, json};
use tracedecay_agent_hosts::automation::{
    AutomationCommittedReceipt, run_ledger::AutomationRunLedgerRecord,
    runner::RetainedAutomationRun,
};

use crate::daemon::automation_effect::{AutomationEffectAuthority, AutomationSettledTerminal};
use crate::errors::{Result, TraceDecayError};

pub(super) type AutomationRunObserver =
    Box<dyn FnOnce(&AutomationRunLedgerRecord) + Send + 'static>;

pub(super) fn automation_run_observer(
    producer: Arc<tracedecay_usecases::observability::BoundedObservabilityProducerV1>,
    project_root: PathBuf,
    surface: &'static str,
) -> AutomationRunObserver {
    Box::new(move |ledger_record| {
        crate::daemon::record_project_automation_run(
            producer.as_ref(),
            &project_root,
            ledger_record,
            surface,
        );
    })
}

pub(super) fn require_observation(
    service: Option<&crate::daemon::DaemonInvocationService>,
) -> Result<&crate::daemon::DaemonInvocationService> {
    service.ok_or_else(|| TraceDecayError::Config {
        message: "manual automation observation authority is unavailable".to_owned(),
    })
}

pub(super) fn decode_options<T: serde::de::DeserializeOwned>(options: Value) -> Result<T> {
    serde_json::from_value(options).map_err(|error| TraceDecayError::Config {
        message: format!("invalid tracedecay_admin_project automation options: {error}"),
    })
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn run_skill_writer(
    invocation_service: &crate::daemon::DaemonInvocationService,
    cg: &crate::tracedecay::TraceDecay,
    request_id: tracedecay_application::RequestId,
    deadline: tracedecay_application::Deadline,
    cancellation: &tracedecay_application::CancellationSignal,
    configuration_digest: tracedecay_domain::ManifestDigest,
    config: &tracedecay_agent_hosts::automation::config::AutomationConfig,
    revision_id: &tracedecay_domain::configuration::ConfigurationRevisionId,
    backend: &tracedecay_agent_hosts::automation::backend::CodexAppServerBackend,
    options: tracedecay_agent_hosts::automation::runner::SkillWriterAutomationOptions,
    observer: AutomationRunObserver,
) -> Result<(Value, Option<AutomationRunLedgerRecord>)> {
    let run_id = options
        .run_id
        .as_deref()
        .ok_or_else(|| TraceDecayError::Config {
            message: "manual skill writer requires its pre-admitted run identity".to_owned(),
        })?;
    let admission = AutomationEffectAuthority::prepare(
        invocation_service,
        cg,
        cg.project_root(),
        &cg.store_layout().dashboard_root,
        request_id,
        deadline,
        cancellation,
        tracedecay_application::now_micros(),
        configuration_digest,
        crate::daemon::automation_effect::skill_writer_run_request(run_id, &options)?,
    )
    .await?;
    let effect = match admission {
        crate::daemon::automation_effect::AutomationEffectAdmission::Execute(effect) => effect,
        crate::daemon::automation_effect::AutomationEffectAdmission::Replay(terminal) => {
            return Ok((terminal_response_value(&terminal)?, None));
        }
        crate::daemon::automation_effect::AutomationEffectAdmission::Conflict => {
            return Ok((admission_conflict_value(), None));
        }
        crate::daemon::automation_effect::AutomationEffectAdmission::PreAdmissionProblem(
            problem,
        ) => {
            return Ok((pre_admission_problem_value(problem)?, None));
        }
    };
    settle_retained_run(
        tracedecay_agent_hosts::automation::runner::run_skill_writer_with_backend_for_retained_settlement(
            cg,
            config,
            revision_id,
            backend,
            options,
        )
        .await,
        effect,
        observer,
    )
    .await
}

fn problem_value(
    problem: crate::daemon::automation_effect::AutomationSettledProblem,
) -> Result<Value> {
    Ok(serde_json::to_value(problem)?)
}

pub(super) trait AutomationRunTerminal {
    fn into_terminal_parts(
        self,
    ) -> (
        AutomationRunLedgerRecord,
        Option<AutomationCommittedReceipt>,
    );
}

impl AutomationRunTerminal
    for tracedecay_agent_hosts::automation::runner::SessionReflectorAutomationRun
{
    fn into_terminal_parts(
        self,
    ) -> (
        AutomationRunLedgerRecord,
        Option<AutomationCommittedReceipt>,
    ) {
        (self.ledger_record, self.committed_receipt)
    }
}

impl AutomationRunTerminal
    for tracedecay_agent_hosts::automation::runner::SkillWriterAutomationRun
{
    fn into_terminal_parts(
        self,
    ) -> (
        AutomationRunLedgerRecord,
        Option<AutomationCommittedReceipt>,
    ) {
        (self.ledger_record, self.committed_receipt)
    }
}

pub(super) async fn settle_retained_run<T: AutomationRunTerminal>(
    retained: RetainedAutomationRun<T>,
    effect: AutomationEffectAuthority,
    observer: AutomationRunObserver,
) -> Result<(Value, Option<AutomationRunLedgerRecord>)> {
    let (result, settlement_guard) = retained.into_parts();
    match result {
        Ok(run) => {
            let (ledger, committed_receipt) = run.into_terminal_parts();
            let waiter = effect.start_deferred_run_settlement_observed(
                ledger,
                committed_receipt,
                settlement_guard,
                Some(observer),
            );
            match waiter.wait().await {
                Ok((terminal, published_ledger)) => {
                    terminal_value(&terminal).map(|value| (value, Some(published_ledger)))
                }
                Err(error) => Err(error),
            }
        }
        Err(error) => {
            let waiter = effect.start_deferred_problem_settlement_observed(
                error,
                settlement_guard,
                Some(observer),
            );
            match waiter.wait().await {
                Ok((problem, published_ledger)) => problem_value(problem).map(|problem| {
                    (
                        json!({ "kind": "problem", "value": problem }),
                        published_ledger,
                    )
                }),
                Err(error) => Err(error),
            }
        }
    }
}

pub(super) fn terminal_value(terminal: &AutomationSettledTerminal) -> Result<Value> {
    if let Some(run) = terminal.run_result() {
        return Ok(serde_json::to_value(run)?);
    }
    if let Some(problem) = terminal.problem() {
        return Ok(json!({ "kind": "problem", "value": problem_value(problem.clone())? }));
    }
    Err(TraceDecayError::Config {
        message: "automation terminal has neither a run nor a problem".to_owned(),
    })
}

pub(super) fn terminal_response_value(terminal: &AutomationSettledTerminal) -> Result<Value> {
    let value = terminal_value(terminal)?;
    if terminal.problem().is_some() {
        return Ok(value);
    }
    Ok(json!({ "run": value }))
}

pub(super) fn pre_admission_problem_value(
    problem: tracedecay_application::ApplicationProblemEnvelope,
) -> Result<Value> {
    Ok(json!({
        "kind": "problem",
        "value": serde_json::to_value(problem)?,
    }))
}

pub(super) fn admission_conflict_value() -> Value {
    json!({
        "kind": "conflict",
        "detail": "automation run identity conflicts with its durable admission",
    })
}

#[cfg(test)]
mod tests {
    use tracedecay_application::{
        ApplicationProblem, ApplicationProblemEnvelope, RequestId, RetainedSurfaceOperation,
        retained_surface_application_operation,
    };

    use super::*;

    #[test]
    fn pre_admission_problem_keeps_the_canonical_application_envelope() {
        let operation =
            retained_surface_application_operation(RetainedSurfaceOperation::FactStoreCurate)
                .unwrap();
        let request_id = RequestId::new("run.mcp.pre-admission".to_owned()).unwrap();
        let envelope = ApplicationProblemEnvelope::new(
            operation.result_contract().clone(),
            request_id.clone(),
            ApplicationProblem::cancelled_before_admission(),
        )
        .unwrap();

        let value = pre_admission_problem_value(envelope).unwrap();
        assert_eq!(value["kind"], "problem");
        assert_eq!(value["value"]["request_id"], request_id.as_str());
        assert_eq!(value["value"]["problem"]["kind"], "cancelled");
        assert!(value["value"].get("run_id").is_none());
    }

    #[test]
    fn admission_conflict_is_a_distinct_terminal_without_a_run() {
        let value = admission_conflict_value();
        assert_eq!(value["kind"], "conflict");
        assert!(value.get("run").is_none());
        assert!(value.get("value").is_none());
    }
}
