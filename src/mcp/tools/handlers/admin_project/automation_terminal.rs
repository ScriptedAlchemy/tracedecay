//! Typed application settlement for manual automation runs.

use serde_json::{Value, json};
use tracedecay_agent_hosts::automation::{
    AutomationCommittedReceipt, AutomationRunError, AutomationRunResult,
    run_ledger::AutomationRunLedgerRecord,
};

use crate::daemon::automation_effect::{AutomationEffectAuthority, AutomationSettledTerminal};
use crate::errors::{Result, TraceDecayError};

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
    settle_run(
        tracedecay_agent_hosts::automation::runner::run_skill_writer_with_backend(
            cg,
            config,
            revision_id,
            backend,
            options,
        )
        .await,
        effect,
    )
    .await
}

fn problem_value(
    problem: crate::daemon::automation_effect::AutomationSettledProblem,
) -> Result<Value> {
    Ok(serde_json::to_value(problem)?)
}

pub(super) trait AutomationRunTerminal: serde::Serialize {
    fn ledger_record(&self) -> &AutomationRunLedgerRecord;

    fn committed_receipt(&self) -> Option<&AutomationCommittedReceipt>;
}

impl AutomationRunTerminal
    for tracedecay_agent_hosts::automation::runner::SessionReflectorAutomationRun
{
    fn ledger_record(&self) -> &AutomationRunLedgerRecord {
        &self.ledger_record
    }

    fn committed_receipt(&self) -> Option<&AutomationCommittedReceipt> {
        self.committed_receipt.as_ref()
    }
}

impl AutomationRunTerminal
    for tracedecay_agent_hosts::automation::runner::SkillWriterAutomationRun
{
    fn ledger_record(&self) -> &AutomationRunLedgerRecord {
        &self.ledger_record
    }

    fn committed_receipt(&self) -> Option<&AutomationCommittedReceipt> {
        self.committed_receipt.as_ref()
    }
}

pub(super) async fn settle_run<T: AutomationRunTerminal>(
    result: AutomationRunResult<T>,
    effect: AutomationEffectAuthority,
) -> Result<(Value, Option<AutomationRunLedgerRecord>)> {
    match result {
        Ok(run) => {
            let ledger = run.ledger_record().clone();
            let terminal = effect
                .settle_run(run.ledger_record(), run.committed_receipt())
                .await?;
            Ok((terminal_value(&terminal)?, Some(ledger)))
        }
        Err(error) => {
            let partial_ledger = match &error {
                AutomationRunError::PartialEffect { ledger_record, .. } => ledger_record.clone(),
                AutomationRunError::Runtime(_) => None,
            };
            let problem = effect.settle_problem(&error).await?;
            Ok((
                json!({ "kind": "problem", "value": problem_value(problem)? }),
                partial_ledger,
            ))
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
