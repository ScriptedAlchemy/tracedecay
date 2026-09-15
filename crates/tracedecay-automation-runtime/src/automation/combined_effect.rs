//! Combined-review admission and terminal policy. The daemon retains admitted
//! effect lifetimes and wires observation; this owner decides which legs execute,
//! settle, or abandon and when standalone fallback is legal.

use super::runner::{
    CombinedFailureTerminals, CombinedMemoryCompletedSkillFailure, CombinedRecordedFailure,
    CombinedReflectorPartial, CombinedReviewDispatch, CombinedSkillPartial,
};
use tracedecay_domain::errors::{Result, TraceDecayError};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AdmissionState {
    Execute,
    Replay,
    Conflict,
    Problem,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PairMode {
    Combined,
    SkillOnly,
    ReflectorOnly,
    Replayed,
    ProblemAbandonSkill,
    ProblemAbandonReflector,
    ProblemNoAbandon,
    ConflictAbandonSkill,
    ConflictAbandonReflector,
    ConflictNoAbandon,
}

pub fn pair_mode(reflector: AdmissionState, skill: AdmissionState) -> PairMode {
    match (reflector, skill) {
        (AdmissionState::Execute, AdmissionState::Execute) => PairMode::Combined,
        (AdmissionState::Replay, AdmissionState::Execute) => PairMode::SkillOnly,
        (AdmissionState::Execute, AdmissionState::Replay) => PairMode::ReflectorOnly,
        (AdmissionState::Replay, AdmissionState::Replay) => PairMode::Replayed,
        (AdmissionState::Problem, AdmissionState::Execute) => PairMode::ProblemAbandonSkill,
        (AdmissionState::Execute, AdmissionState::Problem) => PairMode::ProblemAbandonReflector,
        (AdmissionState::Conflict, AdmissionState::Execute) => PairMode::ConflictAbandonSkill,
        (AdmissionState::Execute, AdmissionState::Conflict) => PairMode::ConflictAbandonReflector,
        (AdmissionState::Conflict, _) | (_, AdmissionState::Conflict) => {
            PairMode::ConflictNoAbandon
        }
        _ => PairMode::ProblemNoAbandon,
    }
}

pub struct DeferredRunTerminal {
    pub record: crate::automation::run_ledger::AutomationRunLedgerRecord,
    pub committed: Option<crate::automation::AutomationCommittedReceipt>,
}

pub struct DeferredProblemTerminal {
    pub error: crate::automation::AutomationRunError,
}

pub enum DeferredLegTerminal {
    Run(Box<DeferredRunTerminal>),
    Problem(Box<DeferredProblemTerminal>),
    Abandon,
}

fn failed_leg_terminal(
    record: Option<crate::automation::run_ledger::AutomationRunLedgerRecord>,
    error: Option<tracedecay_domain::errors::TraceDecayError>,
    fallback_message: String,
) -> DeferredLegTerminal {
    match record {
        Some(record) => DeferredLegTerminal::Run(Box::new(DeferredRunTerminal {
            record,
            committed: None,
        })),
        None => DeferredLegTerminal::Problem(Box::new(DeferredProblemTerminal {
            error: error
                .unwrap_or(tracedecay_domain::errors::TraceDecayError::Config {
                    message: fallback_message,
                })
                .into(),
        })),
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PairResultOrder {
    ReflectorFirst,
    SkillFirst,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PairResultMode {
    CompletedIfBoth,
    Handled,
    DeferredIfBothAbandoned,
}

/// Resolve the runner outcome before either admitted leg is settled.
/// The observer preserves typed per-leg diagnostics without owning daemon state.
pub fn combined_dispatch_terminals(
    result: Result<CombinedReviewDispatch>,
    mut observe_error: impl FnMut(super::backend::AgentTaskKind, &TraceDecayError),
) -> (
    DeferredLegTerminal,
    DeferredLegTerminal,
    PairResultOrder,
    PairResultMode,
) {
    match result {
        Ok(CombinedReviewDispatch::Ran(run)) => (
            DeferredLegTerminal::Run(Box::new(DeferredRunTerminal {
                record: run.session_reflector.ledger_record,
                committed: run.session_reflector.committed_receipt,
            })),
            DeferredLegTerminal::Run(Box::new(DeferredRunTerminal {
                record: run.skill_writer.ledger_record,
                committed: run.skill_writer.committed_receipt,
            })),
            PairResultOrder::ReflectorFirst,
            PairResultMode::CompletedIfBoth,
        ),
        Ok(CombinedReviewDispatch::MemoryCompletedSkillFailure(failure)) => {
            let CombinedMemoryCompletedSkillFailure {
                session_reflector,
                skill_writer_record,
                skill_writer_record_error,
                error,
            } = *failure;
            observe_error(
                crate::automation::backend::AgentTaskKind::SkillWriter,
                &error,
            );
            if let Some(error) = skill_writer_record_error.as_ref() {
                observe_error(
                    crate::automation::backend::AgentTaskKind::SkillWriter,
                    error,
                );
            }
            let skill_terminal = match skill_writer_record {
                Some(record) => DeferredLegTerminal::Run(Box::new(DeferredRunTerminal {
                    record,
                    committed: None,
                })),
                None => DeferredLegTerminal::Problem(Box::new(DeferredProblemTerminal {
                    error: error.into(),
                })),
            };
            (
                DeferredLegTerminal::Run(Box::new(DeferredRunTerminal {
                    record: session_reflector.ledger_record,
                    committed: session_reflector.committed_receipt,
                })),
                skill_terminal,
                PairResultOrder::ReflectorFirst,
                PairResultMode::Handled,
            )
        }
        Ok(CombinedReviewDispatch::RecordedFailure(failure)) => {
            let CombinedRecordedFailure { run, error } = *failure;
            observe_error(
                crate::automation::backend::AgentTaskKind::CombinedReview,
                &error,
            );
            (
                DeferredLegTerminal::Run(Box::new(DeferredRunTerminal {
                    record: run.session_reflector.ledger_record,
                    committed: None,
                })),
                DeferredLegTerminal::Run(Box::new(DeferredRunTerminal {
                    record: run.skill_writer.ledger_record,
                    committed: None,
                })),
                PairResultOrder::ReflectorFirst,
                PairResultMode::Handled,
            )
        }
        Ok(CombinedReviewDispatch::FailureTerminals(failure)) => {
            let CombinedFailureTerminals {
                reflector_record,
                reflector_error,
                skill_writer_record,
                skill_writer_error,
                error,
            } = *failure;
            let fallback_message = error.to_string();
            observe_error(
                crate::automation::backend::AgentTaskKind::CombinedReview,
                &error,
            );
            if reflector_record.is_none()
                && let Some(error) = reflector_error.as_ref()
            {
                observe_error(
                    crate::automation::backend::AgentTaskKind::SessionReflector,
                    error,
                );
            }
            if skill_writer_record.is_none()
                && let Some(error) = skill_writer_error.as_ref()
            {
                observe_error(
                    crate::automation::backend::AgentTaskKind::SkillWriter,
                    error,
                );
            }
            let reflector_terminal =
                failed_leg_terminal(reflector_record, reflector_error, fallback_message.clone());
            let skill_terminal =
                failed_leg_terminal(skill_writer_record, skill_writer_error, fallback_message);
            (
                reflector_terminal,
                skill_terminal,
                PairResultOrder::ReflectorFirst,
                PairResultMode::Handled,
            )
        }
        Ok(CombinedReviewDispatch::ReflectorPartial(partial)) => {
            let CombinedReflectorPartial {
                run_id,
                committed_receipt,
                ledger_record,
                reflector_record_error,
                skill_writer_record,
                skill_writer_error,
                detail,
            } = *partial;
            if let Some(error) = reflector_record_error.as_ref() {
                observe_error(
                    crate::automation::backend::AgentTaskKind::SessionReflector,
                    error,
                );
            }
            let reflector_terminal =
                DeferredLegTerminal::Problem(Box::new(DeferredProblemTerminal {
                    error: crate::automation::AutomationRunError::PartialEffect {
                        run_id,
                        committed_receipt: Box::new(committed_receipt),
                        ledger_record: ledger_record.map(Box::new),
                        detail,
                    },
                }));
            let skill_terminal = match (skill_writer_record, skill_writer_error) {
                (Some(record), error) => {
                    if let Some(error) = error.as_ref() {
                        observe_error(
                            crate::automation::backend::AgentTaskKind::SkillWriter,
                            error,
                        );
                    }
                    DeferredLegTerminal::Run(Box::new(DeferredRunTerminal {
                        record,
                        committed: None,
                    }))
                }
                (None, Some(error)) => {
                    observe_error(
                        crate::automation::backend::AgentTaskKind::SkillWriter,
                        &error,
                    );
                    DeferredLegTerminal::Problem(Box::new(DeferredProblemTerminal {
                        error: error.into(),
                    }))
                }
                (None, None) => DeferredLegTerminal::Abandon,
            };
            (
                reflector_terminal,
                skill_terminal,
                PairResultOrder::ReflectorFirst,
                PairResultMode::Handled,
            )
        }
        Ok(CombinedReviewDispatch::SkillPartial(partial)) => {
            let CombinedSkillPartial {
                completed_session_reflector,
                run_id,
                committed_receipt,
                ledger_record,
                skill_writer_record_error,
                detail,
            } = *partial;
            if let Some(error) = skill_writer_record_error.as_ref() {
                observe_error(
                    crate::automation::backend::AgentTaskKind::SkillWriter,
                    error,
                );
            }
            let skill_terminal = DeferredLegTerminal::Problem(Box::new(DeferredProblemTerminal {
                error: crate::automation::AutomationRunError::PartialEffect {
                    run_id,
                    committed_receipt: Box::new(committed_receipt),
                    ledger_record: ledger_record.map(Box::new),
                    detail,
                },
            }));
            (
                DeferredLegTerminal::Run(Box::new(DeferredRunTerminal {
                    record: completed_session_reflector.ledger_record,
                    committed: completed_session_reflector.committed_receipt,
                })),
                skill_terminal,
                PairResultOrder::SkillFirst,
                PairResultMode::Handled,
            )
        }
        Ok(CombinedReviewDispatch::NotCombined { .. }) => (
            DeferredLegTerminal::Abandon,
            DeferredLegTerminal::Abandon,
            PairResultOrder::ReflectorFirst,
            PairResultMode::DeferredIfBothAbandoned,
        ),
        Err(error) => {
            let message = error.to_string();
            (
                DeferredLegTerminal::Problem(Box::new(DeferredProblemTerminal {
                    error: error.into(),
                })),
                DeferredLegTerminal::Problem(Box::new(DeferredProblemTerminal {
                    error: tracedecay_domain::errors::TraceDecayError::Config { message }.into(),
                })),
                PairResultOrder::ReflectorFirst,
                PairResultMode::Handled,
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_not_combined_dispatch_abandons_both_legs_for_fallback() {
        let (reflector, skill, order, mode) = combined_dispatch_terminals(
            Ok(CombinedReviewDispatch::NotCombined {
                reason: "standalone gates",
            }),
            |_, _| panic!("non-error dispatch must not emit an error"),
        );
        assert!(matches!(reflector, DeferredLegTerminal::Abandon));
        assert!(matches!(skill, DeferredLegTerminal::Abandon));
        assert_eq!(order, PairResultOrder::ReflectorFirst);
        assert_eq!(mode, PairResultMode::DeferredIfBothAbandoned);

        let (reflector, skill, _, mode) = combined_dispatch_terminals(
            Err(TraceDecayError::Config {
                message: "runner failed".into(),
            }),
            |_, _| {},
        );
        assert!(matches!(reflector, DeferredLegTerminal::Problem(_)));
        assert!(matches!(skill, DeferredLegTerminal::Problem(_)));
        assert_eq!(mode, PairResultMode::Handled);
    }

    #[test]
    fn admission_matrix_never_reruns_a_replayed_leg() {
        assert_eq!(
            pair_mode(AdmissionState::Execute, AdmissionState::Execute),
            PairMode::Combined
        );
        assert_eq!(
            pair_mode(AdmissionState::Replay, AdmissionState::Execute),
            PairMode::SkillOnly
        );
        assert_eq!(
            pair_mode(AdmissionState::Execute, AdmissionState::Replay),
            PairMode::ReflectorOnly
        );
        assert_eq!(
            pair_mode(AdmissionState::Replay, AdmissionState::Replay),
            PairMode::Replayed
        );
        assert_eq!(
            pair_mode(AdmissionState::Problem, AdmissionState::Execute),
            PairMode::ProblemAbandonSkill
        );
        assert_eq!(
            pair_mode(AdmissionState::Execute, AdmissionState::Problem),
            PairMode::ProblemAbandonReflector
        );
        assert_eq!(
            pair_mode(AdmissionState::Problem, AdmissionState::Replay),
            PairMode::ProblemNoAbandon
        );
        assert_eq!(
            pair_mode(AdmissionState::Conflict, AdmissionState::Execute),
            PairMode::ConflictAbandonSkill
        );
        assert_eq!(
            pair_mode(AdmissionState::Execute, AdmissionState::Conflict),
            PairMode::ConflictAbandonReflector
        );
        assert_eq!(
            pair_mode(AdmissionState::Conflict, AdmissionState::Replay),
            PairMode::ConflictNoAbandon
        );
    }
}
