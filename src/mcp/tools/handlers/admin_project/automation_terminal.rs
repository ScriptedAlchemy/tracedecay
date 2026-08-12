//! Typed application settlement for manual memory automation runs.

use serde_json::{Value, json};
use tracedecay_agent_hosts::automation::{
    AutomationRunError, AutomationRunResult, run_ledger::AutomationRunLedgerRecord,
};

use crate::daemon::automation_effect::{AutomationEffectAuthority, AutomationSettledTerminal};
use crate::errors::{Result, TraceDecayError};

fn problem_value(
    problem: crate::daemon::automation_effect::AutomationSettledProblem,
) -> Result<Value> {
    Ok(serde_json::to_value(problem)?)
}

trait MemoryAutomationRunTerminal: serde::Serialize {
    fn ledger_record(&self) -> &AutomationRunLedgerRecord;

    fn committed_receipt(
        &self,
    ) -> Option<&tracedecay_agent_hosts::automation::AutomationCommittedReceipt>;
}

impl MemoryAutomationRunTerminal
    for tracedecay_agent_hosts::automation::runner::MemoryCuratorAutomationRun
{
    fn ledger_record(&self) -> &AutomationRunLedgerRecord {
        &self.ledger_record
    }

    fn committed_receipt(
        &self,
    ) -> Option<&tracedecay_agent_hosts::automation::AutomationCommittedReceipt> {
        self.committed_receipt.as_ref()
    }
}

impl MemoryAutomationRunTerminal
    for tracedecay_agent_hosts::automation::runner::SessionReflectorAutomationRun
{
    fn ledger_record(&self) -> &AutomationRunLedgerRecord {
        &self.ledger_record
    }

    fn committed_receipt(
        &self,
    ) -> Option<&tracedecay_agent_hosts::automation::AutomationCommittedReceipt> {
        self.committed_receipt.as_ref()
    }
}

pub(super) async fn settle_run<T: MemoryAutomationRunTerminal>(
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
        Err(error) => match effect.settle_problem(&error).await? {
            Some(problem) => Ok((
                json!({ "kind": "problem", "value": problem_value(problem)? }),
                None,
            )),
            None => match error {
                AutomationRunError::Runtime(error) => Err(error),
                AutomationRunError::PartialEffect { .. } => Err(TraceDecayError::Config {
                    message: "automation partial effect did not produce an application terminal"
                        .to_owned(),
                }),
            },
        },
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
        message: "memory automation terminal has neither a run nor a problem".to_owned(),
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
            retained_surface_application_operation(RetainedSurfaceOperation::MemoryAutomationRun)
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
}
