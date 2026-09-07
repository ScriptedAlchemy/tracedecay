//! Settled automation-effect terminals.

use serde::{Deserialize, Serialize};
use tracedecay_application::retained_surfaces::{
    AutomationRunProblemV1, AutomationRunResultV1, AutomationRunTerminalV1, AutomationSkipReasonV1,
    RetainedSurfaceOperation, RetainedSurfaceResultV1,
};
use tracedecay_application::{
    ApplicationOutcome, ResolvedScope, retained_surface_outcome_matches_terminal,
};

use super::journal::DurableAutomationAdmission;

pub type AutomationSettledProblem = AutomationRunProblemV1;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(
    tag = "kind",
    content = "value",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum AutomationSettledTerminal {
    Outcome {
        scope: ResolvedScope,
        outcome: Box<ApplicationOutcome<RetainedSurfaceResultV1>>,
    },
    Problem(AutomationSettledProblem),
}

impl AutomationSettledTerminal {
    pub fn into_outcome(
        self,
    ) -> std::result::Result<
        ApplicationOutcome<RetainedSurfaceResultV1>,
        Box<AutomationSettledProblem>,
    > {
        match self {
            Self::Outcome { outcome, .. } => Ok(*outcome),
            Self::Problem(problem) => Err(Box::new(problem)),
        }
    }

    pub fn matches_admission(&self, admission: &DurableAutomationAdmission) -> bool {
        match self {
            Self::Outcome {
                scope: terminal_scope,
                outcome,
            } => {
                terminal_scope == &admission.scope
                    && retained_surface_outcome_matches_terminal(
                        RetainedSurfaceOperation::FactStoreCurate,
                        &admission.request_id,
                        &admission.scope,
                        outcome,
                    )
                    && matches!(
                        outcome.as_ref(),
                        ApplicationOutcome::Effect(effect)
                            if matches!(
                                effect.payload.as_ref(),
                                Some(RetainedSurfaceResultV1::FactStoreCurate(result))
                                    if result.matches_admission(&admission.request)
                            )
                    )
            }
            Self::Problem(problem) => {
                problem.scope == admission.scope
                    && problem.matches_terminal(&admission.request_id)
                    && problem.matches_admission(&admission.request, &admission.request_id)
            }
        }
    }

    pub fn run_result(&self) -> Option<&AutomationRunResultV1> {
        let Self::Outcome { outcome, .. } = self else {
            return None;
        };
        let ApplicationOutcome::Effect(effect) = outcome.as_ref() else {
            return None;
        };
        let Some(RetainedSurfaceResultV1::FactStoreCurate(result)) = effect.payload.as_ref() else {
            return None;
        };
        Some(result)
    }

    pub fn problem(&self) -> Option<&AutomationSettledProblem> {
        match self {
            Self::Outcome { .. } => None,
            Self::Problem(problem) => Some(problem),
        }
    }

    pub fn is_completed(&self) -> bool {
        let Self::Outcome { outcome, .. } = self else {
            return false;
        };
        let ApplicationOutcome::Effect(effect) = outcome.as_ref() else {
            return false;
        };
        let Some(RetainedSurfaceResultV1::FactStoreCurate(result)) = effect.payload.as_ref() else {
            return false;
        };
        matches!(result.terminal, AutomationRunTerminalV1::Completed { .. })
    }

    pub fn is_retirement_terminal(&self) -> bool {
        let Self::Outcome { outcome, .. } = self else {
            return false;
        };
        let ApplicationOutcome::Effect(effect) = outcome.as_ref() else {
            return false;
        };
        let Some(RetainedSurfaceResultV1::FactStoreCurate(result)) = effect.payload.as_ref() else {
            return false;
        };
        matches!(
            &result.terminal,
            AutomationRunTerminalV1::Skipped { reason, .. }
                if Some(*reason)
                    == AutomationSkipReasonV1::from_ledger_reason(
                        "shipped_fact_proposal_history_retired"
                    )
        ) && result.committed_receipts.is_empty()
    }
}
