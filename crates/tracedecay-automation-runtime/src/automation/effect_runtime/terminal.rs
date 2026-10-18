//! Settled automation-effect terminals.

use serde::{Deserialize, Serialize};
use tracedecay_contracts::retained_surfaces::{
    AutomationRunProblemV1, AutomationRunResultV1, AutomationRunTerminalV1,
    FactStoreCurateResultV1, RetainedSurfaceOperation, RetainedSurfaceResultV1,
};
use tracedecay_contracts::{
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
    /// The settled effect receipts carrying the run's own terminal. Callers
    /// of the admitting operation see only its receipt, [`Self::into_outcome`].
    Outcome {
        scope: ResolvedScope,
        outcome: Box<ApplicationOutcome<AutomationRunResultV1>>,
    },
    Problem(AutomationSettledProblem),
}

/// The `fact_store_curate` outcome for a settled run: the same effect
/// receipts, carrying the run's receipt instead of its terminal.
pub fn curate_receipt_outcome(
    outcome: ApplicationOutcome<AutomationRunResultV1>,
) -> ApplicationOutcome<RetainedSurfaceResultV1> {
    outcome.map_payload(|run| {
        RetainedSurfaceResultV1::FactStoreCurate(FactStoreCurateResultV1::for_run(&run))
    })
}

impl AutomationSettledTerminal {
    pub fn into_outcome(
        self,
    ) -> std::result::Result<
        ApplicationOutcome<RetainedSurfaceResultV1>,
        Box<AutomationSettledProblem>,
    > {
        match self {
            Self::Outcome { outcome, .. } => Ok(curate_receipt_outcome(*outcome)),
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
                    && matches!(
                        outcome.as_ref(),
                        ApplicationOutcome::Effect(effect)
                            if effect
                                .payload
                                .as_ref()
                                .is_some_and(|result| result.matches_admission(&admission.request))
                    )
                    && retained_surface_outcome_matches_terminal(
                        RetainedSurfaceOperation::FactStoreCurate,
                        &admission.request_id,
                        &admission.scope,
                        &curate_receipt_outcome(outcome.as_ref().clone()),
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
        effect.payload.as_ref()
    }

    pub fn problem(&self) -> Option<&AutomationSettledProblem> {
        match self {
            Self::Outcome { .. } => None,
            Self::Problem(problem) => Some(problem),
        }
    }

    pub fn is_completed(&self) -> bool {
        self.run_result().is_some_and(|result| {
            matches!(result.terminal, AutomationRunTerminalV1::Completed { .. })
        })
    }
}
