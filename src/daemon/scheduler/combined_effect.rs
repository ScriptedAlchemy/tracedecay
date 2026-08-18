use std::path::Path;

use tracedecay_agent_hosts::automation::AutomationRunControl;
use tracedecay_agent_hosts::automation::runner::CombinedReviewAutomationOptions;
use tracedecay_agent_hosts::automation::runner::{
    CombinedFailureTerminals, CombinedMemoryCompletedSkillFailure, CombinedRecordedFailure,
    CombinedReflectorPartial, CombinedReviewDispatch, CombinedSkillPartial,
    RetainedAutomationSettlementDisposition,
    run_combined_review_with_backend_and_retrieval_for_retained_settlement,
    run_session_reflector_with_backend_and_retrieval_for_retained_settlement,
    run_skill_writer_with_backend_and_retrieval_for_retained_settlement,
};

use super::scheduler_automation_effect;
use crate::daemon::DaemonEngine;
use crate::daemon::automation_effect::{
    AutomationEffectAdmission, AutomationEffectAuthority, AutomationSettledTerminal,
    DeferredProblemSettlementRequest, DeferredRunSettlementRequest, DeferredSettlementOutcome,
    DeferredSettlementRequest,
};
use crate::errors::Result;
use crate::tracedecay::TraceDecay;

pub(super) enum CombinedEffectAdmission {
    Execute {
        run_id: String,
        run_control: AutomationRunControl,
        reflector: Box<AutomationEffectAuthority>,
        skill: Box<AutomationEffectAuthority>,
    },
    ReflectorReplay {
        reflector: Box<AutomationSettledTerminal>,
        skill_run_id: String,
        skill_control: AutomationRunControl,
        skill: Box<AutomationEffectAuthority>,
    },
    SkillReplay {
        run_id: String,
        reflector_control: AutomationRunControl,
        reflector: Box<AutomationEffectAuthority>,
        skill: Box<AutomationSettledTerminal>,
    },
    Replay {
        reflector: Box<AutomationSettledTerminal>,
        skill: Box<AutomationSettledTerminal>,
    },
    Conflict,
    PreAdmissionProblem(Vec<tracedecay_application::ApplicationProblemEnvelope>),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AdmissionState {
    Execute,
    Replay,
    Conflict,
    Problem,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PairMode {
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

fn pair_mode(reflector: AdmissionState, skill: AdmissionState) -> PairMode {
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

fn admission_state(admission: &AutomationEffectAdmission) -> AdmissionState {
    match admission {
        AutomationEffectAdmission::Execute(_) => AdmissionState::Execute,
        AutomationEffectAdmission::Replay(_) => AdmissionState::Replay,
        AutomationEffectAdmission::Conflict => AdmissionState::Conflict,
        AutomationEffectAdmission::PreAdmissionProblem(_) => AdmissionState::Problem,
    }
}

struct DeferredRunTerminal {
    record: tracedecay_agent_hosts::automation::run_ledger::AutomationRunLedgerRecord,
    committed: Option<tracedecay_agent_hosts::automation::AutomationCommittedReceipt>,
}

struct DeferredProblemTerminal {
    error: tracedecay_agent_hosts::automation::AutomationRunError,
}

enum DeferredLegTerminal {
    Run(Box<DeferredRunTerminal>),
    Problem(Box<DeferredProblemTerminal>),
    Abandon,
}

fn failed_leg_terminal(
    record: Option<tracedecay_agent_hosts::automation::run_ledger::AutomationRunLedgerRecord>,
    error: Option<crate::errors::TraceDecayError>,
    fallback_message: String,
) -> DeferredLegTerminal {
    match record {
        Some(record) => DeferredLegTerminal::Run(Box::new(DeferredRunTerminal {
            record,
            committed: None,
        })),
        None => DeferredLegTerminal::Problem(Box::new(DeferredProblemTerminal {
            error: error
                .unwrap_or_else(|| crate::errors::TraceDecayError::Config {
                    message: fallback_message,
                })
                .into(),
        })),
    }
}

fn deferred_settlement_request(
    terminal: DeferredLegTerminal,
    engine: &DaemonEngine,
    project_id: &tracedecay_domain::ProjectId,
    project_path: &Path,
) -> DeferredSettlementRequest {
    match terminal {
        DeferredLegTerminal::Run(terminal) => {
            DeferredSettlementRequest::Run(Box::new(DeferredRunSettlementRequest {
                ledger: terminal.record,
                committed: terminal.committed,
                observer: Some(super::scheduler_run_observer(
                    engine,
                    project_id,
                    project_path,
                )),
            }))
        }
        DeferredLegTerminal::Problem(terminal) => {
            DeferredSettlementRequest::Problem(Box::new(DeferredProblemSettlementRequest {
                error: terminal.error,
                observer: Some(super::scheduler_run_observer(
                    engine,
                    project_id,
                    project_path,
                )),
            }))
        }
        DeferredLegTerminal::Abandon => DeferredSettlementRequest::Abandon,
    }
}

fn collect_settlement_result(
    project_path: &Path,
    task: tracedecay_agent_hosts::automation::backend::AgentTaskKind,
    first_error: &mut Option<crate::errors::TraceDecayError>,
    result: Result<DeferredSettlementOutcome>,
) -> Option<DeferredSettlementOutcome> {
    match result {
        Ok(outcome) => {
            if let DeferredSettlementOutcome::Settled(settled) = &outcome
                && let Some(problem) = settled.terminal.problem()
            {
                super::log_daemon_event(
                    "scheduler_task_application_problem",
                    &super::scheduler_application_problem_log_fields(project_path, task, problem),
                );
            }
            Some(outcome)
        }
        Err(error) => {
            super::log_scheduler_task_error(project_path, task, &error);
            first_error.get_or_insert(error);
            None
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PairResultOrder {
    ReflectorFirst,
    SkillFirst,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PairResultMode {
    CompletedIfBoth,
    Handled,
    DeferredIfBothAbandoned,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum CombinedEffectOutcome {
    Completed,
    Handled,
    Deferred,
}

impl CombinedEffectOutcome {
    pub(super) fn handled(self) -> bool {
        self != Self::Deferred
    }

    pub(super) fn completed(self) -> bool {
        self == Self::Completed
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn run_combined_scheduler_effect(
    admission: CombinedEffectAdmission,
    engine: &DaemonEngine,
    memory: &TraceDecay,
    project_id: &tracedecay_domain::ProjectId,
    project_path: &Path,
    config: &tracedecay_agent_hosts::automation::config::AutomationConfig,
    configuration_revision_id: &tracedecay_domain::configuration::ConfigurationRevisionId,
    backend: &dyn tracedecay_agent_hosts::automation::backend::AgentTaskBackend,
    retrieval: &dyn tracedecay_agent_hosts::automation::runner::AutomationSessionRetrieval,
    options: CombinedReviewAutomationOptions,
    first_error: &mut Option<crate::errors::TraceDecayError>,
) -> CombinedEffectOutcome {
    match admission {
        CombinedEffectAdmission::Conflict => {
            super::log_scheduler_admission_conflict(
                project_path,
                tracedecay_agent_hosts::automation::backend::AgentTaskKind::CombinedReview,
            );
            CombinedEffectOutcome::Handled
        }
        CombinedEffectAdmission::PreAdmissionProblem(problems) => {
            for problem in problems {
                super::log_scheduler_pre_admission_problem(
                    project_path,
                    tracedecay_agent_hosts::automation::backend::AgentTaskKind::CombinedReview,
                    &problem,
                );
            }
            CombinedEffectOutcome::Handled
        }
        CombinedEffectAdmission::Replay { reflector, skill } => {
            super::log_scheduler_automation_replay(
                project_path,
                tracedecay_agent_hosts::automation::backend::AgentTaskKind::SessionReflector,
                &reflector,
            );
            super::log_scheduler_automation_replay(
                project_path,
                tracedecay_agent_hosts::automation::backend::AgentTaskKind::SkillWriter,
                &skill,
            );
            if reflector.is_completed() && skill.is_completed() {
                CombinedEffectOutcome::Completed
            } else {
                CombinedEffectOutcome::Handled
            }
        }
        CombinedEffectAdmission::ReflectorReplay {
            reflector,
            skill_run_id,
            skill_control,
            skill,
        } => {
            super::log_scheduler_automation_replay(
                project_path,
                tracedecay_agent_hosts::automation::backend::AgentTaskKind::SessionReflector,
                &reflector,
            );
            let mut skill_options = options.skill_writer;
            skill_options.run_id = Some(skill_run_id);
            skill_options.trigger = options.trigger;
            let replay_completed = reflector.is_completed();
            let retained = run_skill_writer_with_backend_and_retrieval_for_retained_settlement(
                memory,
                config,
                configuration_revision_id,
                backend,
                retrieval,
                skill_options,
            )
            .await;
            match retained.into_settlement_disposition() {
                RetainedAutomationSettlementDisposition::Current {
                    result,
                    settlement_guard,
                } => {
                    super::synchronize_scheduler_effect_control(&skill_control);
                    let terminal = match result {
                        Ok(run) => skill
                            .start_deferred_run_settlement_observed(
                                run.ledger_record,
                                run.committed_receipt,
                                settlement_guard,
                                Some(super::scheduler_run_observer(
                                    engine,
                                    project_id,
                                    project_path,
                                )),
                            )
                            .wait()
                            .await
                            .map(|(terminal, _)| terminal),
                        Err(error) => skill
                            .start_deferred_problem_settlement_observed(
                                error,
                                settlement_guard,
                                Some(super::scheduler_run_observer(
                                    engine,
                                    project_id,
                                    project_path,
                                )),
                            )
                            .wait()
                            .await
                            .map(|(problem, _)| AutomationSettledTerminal::Problem(problem)),
                    };
                    match terminal {
                        Ok(terminal) if replay_completed && terminal.is_completed() => {
                            CombinedEffectOutcome::Completed
                        }
                        Ok(terminal) => {
                            if let Some(problem) = terminal.problem() {
                                super::log_daemon_event(
                                    "scheduler_task_application_problem",
                                    &super::scheduler_application_problem_log_fields(
                                        project_path,
                                        tracedecay_agent_hosts::automation::backend::AgentTaskKind::SkillWriter,
                                        problem,
                                    ),
                                );
                            }
                            CombinedEffectOutcome::Handled
                        }
                        Err(error) => {
                            first_error.get_or_insert(error);
                            CombinedEffectOutcome::Handled
                        }
                    }
                }
                RetainedAutomationSettlementDisposition::ReusedSchedulerSkip {
                    reused,
                    settlement_guard,
                } => {
                    if let Some(error) = super::effect_admission::abandon_reused_scheduler_skip(
                        engine,
                        project_id,
                        project_path,
                        tracedecay_agent_hosts::automation::backend::AgentTaskKind::SkillWriter,
                        &skill_control,
                        *skill,
                        reused,
                        settlement_guard,
                    )
                    .await
                    {
                        first_error.get_or_insert(error);
                    }
                    CombinedEffectOutcome::Handled
                }
            }
        }
        CombinedEffectAdmission::SkillReplay {
            run_id,
            reflector_control,
            reflector,
            skill,
        } => {
            super::log_scheduler_automation_replay(
                project_path,
                tracedecay_agent_hosts::automation::backend::AgentTaskKind::SkillWriter,
                &skill,
            );
            let mut reflector_options = options.session_reflector;
            reflector_options.run_id = Some(run_id);
            reflector_options.trigger = options.trigger;
            let replay_completed = skill.is_completed();
            let retained =
                run_session_reflector_with_backend_and_retrieval_for_retained_settlement(
                    memory,
                    config,
                    &reflector_control,
                    configuration_revision_id,
                    backend,
                    retrieval,
                    reflector_options,
                )
                .await;
            match retained.into_settlement_disposition() {
                RetainedAutomationSettlementDisposition::Current {
                    result,
                    settlement_guard,
                } => {
                    super::synchronize_scheduler_effect_control(&reflector_control);
                    let terminal = match result {
                        Ok(run) => reflector
                            .start_deferred_run_settlement_observed(
                                run.ledger_record,
                                run.committed_receipt,
                                settlement_guard,
                                Some(super::scheduler_run_observer(
                                    engine,
                                    project_id,
                                    project_path,
                                )),
                            )
                            .wait()
                            .await
                            .map(|(terminal, _)| terminal),
                        Err(error) => reflector
                            .start_deferred_problem_settlement_observed(
                                error,
                                settlement_guard,
                                Some(super::scheduler_run_observer(
                                    engine,
                                    project_id,
                                    project_path,
                                )),
                            )
                            .wait()
                            .await
                            .map(|(problem, _)| AutomationSettledTerminal::Problem(problem)),
                    };
                    match terminal {
                        Ok(terminal) if replay_completed && terminal.is_completed() => {
                            CombinedEffectOutcome::Completed
                        }
                        Ok(terminal) => {
                            if let Some(problem) = terminal.problem() {
                                super::log_daemon_event(
                                    "scheduler_task_application_problem",
                                    &super::scheduler_application_problem_log_fields(
                                        project_path,
                                        tracedecay_agent_hosts::automation::backend::AgentTaskKind::SessionReflector,
                                        problem,
                                    ),
                                );
                            }
                            CombinedEffectOutcome::Handled
                        }
                        Err(error) => {
                            first_error.get_or_insert(error);
                            CombinedEffectOutcome::Handled
                        }
                    }
                }
                RetainedAutomationSettlementDisposition::ReusedSchedulerSkip {
                    reused,
                    settlement_guard,
                } => {
                    if let Some(error) = super::effect_admission::abandon_reused_scheduler_skip(
                        engine,
                        project_id,
                        project_path,
                        tracedecay_agent_hosts::automation::backend::AgentTaskKind::SessionReflector,
                        &reflector_control,
                        *reflector,
                        reused,
                        settlement_guard,
                    )
                    .await
                    {
                        first_error.get_or_insert(error);
                    }
                    CombinedEffectOutcome::Handled
                }
            }
        }
        CombinedEffectAdmission::Execute {
            run_id,
            run_control,
            reflector,
            skill,
        } => {
            run_execute_pair(
                run_id,
                run_control,
                *reflector,
                *skill,
                engine,
                memory,
                project_id,
                project_path,
                config,
                configuration_revision_id,
                backend,
                retrieval,
                options,
                first_error,
            )
            .await
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_execute_pair(
    run_id: String,
    run_control: AutomationRunControl,
    reflector: AutomationEffectAuthority,
    skill: AutomationEffectAuthority,
    engine: &DaemonEngine,
    memory: &TraceDecay,
    project_id: &tracedecay_domain::ProjectId,
    project_path: &Path,
    config: &tracedecay_agent_hosts::automation::config::AutomationConfig,
    configuration_revision_id: &tracedecay_domain::configuration::ConfigurationRevisionId,
    backend: &dyn tracedecay_agent_hosts::automation::backend::AgentTaskBackend,
    retrieval: &dyn tracedecay_agent_hosts::automation::runner::AutomationSessionRetrieval,
    options: CombinedReviewAutomationOptions,
    first_error: &mut Option<crate::errors::TraceDecayError>,
) -> CombinedEffectOutcome {
    let retained = run_combined_review_with_backend_and_retrieval_for_retained_settlement(
        memory,
        config,
        configuration_revision_id,
        backend,
        retrieval,
        CombinedReviewAutomationOptions {
            run_id: Some(run_id.clone()),
            ..options
        },
        &run_control,
    )
    .await;
    let mut settlement = AutomationEffectAuthority::start_retained_combined_settlement_pair(
        retained, reflector, skill,
    );
    super::synchronize_scheduler_effect_control(&run_control);
    let result = match settlement.take_payload() {
        Ok(result) => result,
        Err(error) => {
            first_error.get_or_insert(error);
            return CombinedEffectOutcome::Handled;
        }
    };
    let (reflector_terminal, skill_terminal, result_order, result_mode) = match result {
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
            super::log_scheduler_task_error(
                project_path,
                tracedecay_agent_hosts::automation::backend::AgentTaskKind::SkillWriter,
                &error,
            );
            if let Some(error) = skill_writer_record_error.as_ref() {
                super::log_scheduler_task_error(
                    project_path,
                    tracedecay_agent_hosts::automation::backend::AgentTaskKind::SkillWriter,
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
            super::log_scheduler_task_error(
                project_path,
                tracedecay_agent_hosts::automation::backend::AgentTaskKind::CombinedReview,
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
            super::log_scheduler_task_error(
                project_path,
                tracedecay_agent_hosts::automation::backend::AgentTaskKind::CombinedReview,
                &error,
            );
            if reflector_record.is_none()
                && let Some(error) = reflector_error.as_ref()
            {
                super::log_scheduler_task_error(
                    project_path,
                    tracedecay_agent_hosts::automation::backend::AgentTaskKind::SessionReflector,
                    error,
                );
            }
            if skill_writer_record.is_none()
                && let Some(error) = skill_writer_error.as_ref()
            {
                super::log_scheduler_task_error(
                    project_path,
                    tracedecay_agent_hosts::automation::backend::AgentTaskKind::SkillWriter,
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
                super::log_scheduler_task_error(
                    project_path,
                    tracedecay_agent_hosts::automation::backend::AgentTaskKind::SessionReflector,
                    error,
                );
            }
            let reflector_terminal =
                DeferredLegTerminal::Problem(Box::new(DeferredProblemTerminal {
                    error: tracedecay_agent_hosts::automation::AutomationRunError::PartialEffect {
                        run_id,
                        committed_receipt: Box::new(committed_receipt),
                        ledger_record: ledger_record.map(Box::new),
                        detail,
                    },
                }));
            let skill_terminal = match (skill_writer_record, skill_writer_error) {
                (Some(record), error) => {
                    if let Some(error) = error.as_ref() {
                        super::log_scheduler_task_error(
                            project_path,
                            tracedecay_agent_hosts::automation::backend::AgentTaskKind::SkillWriter,
                            error,
                        );
                    }
                    DeferredLegTerminal::Run(Box::new(DeferredRunTerminal {
                        record,
                        committed: None,
                    }))
                }
                (None, Some(error)) => {
                    super::log_scheduler_task_error(
                        project_path,
                        tracedecay_agent_hosts::automation::backend::AgentTaskKind::SkillWriter,
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
                super::log_scheduler_task_error(
                    project_path,
                    tracedecay_agent_hosts::automation::backend::AgentTaskKind::SkillWriter,
                    error,
                );
            }
            let skill_terminal = DeferredLegTerminal::Problem(Box::new(DeferredProblemTerminal {
                error: tracedecay_agent_hosts::automation::AutomationRunError::PartialEffect {
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
                    error: crate::errors::TraceDecayError::Config { message }.into(),
                })),
                PairResultOrder::ReflectorFirst,
                PairResultMode::Handled,
            )
        }
    };

    let reflector_request =
        deferred_settlement_request(reflector_terminal, engine, project_id, project_path);
    let skill_request =
        deferred_settlement_request(skill_terminal, engine, project_id, project_path);
    let settlement = match settlement.submit(reflector_request, skill_request) {
        Ok(settlement) => settlement,
        Err(error) => {
            first_error.get_or_insert(error);
            return CombinedEffectOutcome::Handled;
        }
    };
    let (reflector_result, skill_result) = settlement.wait().await;

    let (reflector_outcome, skill_outcome) = match result_order {
        PairResultOrder::ReflectorFirst => {
            let reflector_outcome = collect_settlement_result(
                project_path,
                tracedecay_agent_hosts::automation::backend::AgentTaskKind::SessionReflector,
                first_error,
                reflector_result,
            );
            let skill_outcome = collect_settlement_result(
                project_path,
                tracedecay_agent_hosts::automation::backend::AgentTaskKind::SkillWriter,
                first_error,
                skill_result,
            );
            (reflector_outcome, skill_outcome)
        }
        PairResultOrder::SkillFirst => {
            let skill_outcome = collect_settlement_result(
                project_path,
                tracedecay_agent_hosts::automation::backend::AgentTaskKind::SkillWriter,
                first_error,
                skill_result,
            );
            let reflector_outcome = collect_settlement_result(
                project_path,
                tracedecay_agent_hosts::automation::backend::AgentTaskKind::SessionReflector,
                first_error,
                reflector_result,
            );
            (reflector_outcome, skill_outcome)
        }
    };

    match result_mode {
        PairResultMode::CompletedIfBoth
            if matches!(
                reflector_outcome.as_ref(),
                Some(DeferredSettlementOutcome::Settled(settled))
                    if settled.terminal.is_completed()
            ) && matches!(
                skill_outcome.as_ref(),
                Some(DeferredSettlementOutcome::Settled(settled))
                    if settled.terminal.is_completed()
            ) =>
        {
            CombinedEffectOutcome::Completed
        }
        PairResultMode::DeferredIfBothAbandoned
            if matches!(
                reflector_outcome.as_ref(),
                Some(DeferredSettlementOutcome::Abandoned)
            ) && matches!(
                skill_outcome.as_ref(),
                Some(DeferredSettlementOutcome::Abandoned)
            ) =>
        {
            CombinedEffectOutcome::Deferred
        }
        PairResultMode::CompletedIfBoth
        | PairResultMode::Handled
        | PairResultMode::DeferredIfBothAbandoned => CombinedEffectOutcome::Handled,
    }
}

pub(super) async fn prepare_combined_effects(
    engine: &DaemonEngine,
    memory: &TraceDecay,
    parent_control: &AutomationRunControl,
    project_path: &Path,
    dashboard_root: &Path,
    requested_run_id: Option<&str>,
    configuration_digest: tracedecay_domain::ManifestDigest,
    options: &CombinedReviewAutomationOptions,
) -> Result<CombinedEffectAdmission> {
    let (reflector, run_id, reflector_control) = scheduler_automation_effect(
        engine,
        memory,
        parent_control,
        project_path,
        dashboard_root,
        requested_run_id,
        configuration_digest.clone(),
        |run_id| {
            crate::daemon::automation_effect::session_reflector_run_request(
                run_id,
                &options.session_reflector,
            )
        },
    )
    .await?;
    let skill_run_id = format!("{run_id}_skills");
    let skill_preparation = scheduler_automation_effect(
        engine,
        memory,
        parent_control,
        project_path,
        dashboard_root,
        Some(&skill_run_id),
        configuration_digest,
        |run_id| {
            crate::daemon::automation_effect::skill_writer_run_request(
                run_id,
                &options.skill_writer,
            )
        },
    )
    .await;
    let (skill, _, skill_control) = match skill_preparation {
        Ok(prepared) => prepared,
        Err(error) => {
            if let AutomationEffectAdmission::Execute(reflector) = reflector {
                reflector.abandon_uncommitted().await?;
            }
            return Err(error);
        }
    };

    let mode = pair_mode(admission_state(&reflector), admission_state(&skill));
    match (mode, reflector, skill) {
        (
            PairMode::Combined,
            AutomationEffectAdmission::Execute(reflector),
            AutomationEffectAdmission::Execute(skill),
        ) => {
            let reflector_signal = reflector_control.read_control().clone();
            let skill_signal = skill_control.read_control().clone();
            let run_control =
                AutomationRunControl::from_interrupted(std::sync::Arc::new(move || {
                    reflector_signal.interrupted() | skill_signal.interrupted()
                }));
            Ok(CombinedEffectAdmission::Execute {
                run_id,
                run_control,
                reflector,
                skill,
            })
        }
        (
            PairMode::SkillOnly,
            AutomationEffectAdmission::Replay(reflector),
            AutomationEffectAdmission::Execute(skill),
        ) => Ok(CombinedEffectAdmission::ReflectorReplay {
            reflector,
            skill_run_id,
            skill_control,
            skill,
        }),
        (
            PairMode::ReflectorOnly,
            AutomationEffectAdmission::Execute(reflector),
            AutomationEffectAdmission::Replay(skill),
        ) => Ok(CombinedEffectAdmission::SkillReplay {
            run_id,
            reflector_control,
            reflector,
            skill,
        }),
        (
            PairMode::Replayed,
            AutomationEffectAdmission::Replay(reflector),
            AutomationEffectAdmission::Replay(skill),
        ) => Ok(CombinedEffectAdmission::Replay { reflector, skill }),
        (
            PairMode::ProblemAbandonSkill,
            AutomationEffectAdmission::PreAdmissionProblem(problem),
            AutomationEffectAdmission::Execute(skill),
        ) => {
            skill.abandon_uncommitted().await?;
            Ok(CombinedEffectAdmission::PreAdmissionProblem(vec![problem]))
        }
        (
            PairMode::ProblemAbandonReflector,
            AutomationEffectAdmission::Execute(reflector),
            AutomationEffectAdmission::PreAdmissionProblem(problem),
        ) => {
            reflector.abandon_uncommitted().await?;
            Ok(CombinedEffectAdmission::PreAdmissionProblem(vec![problem]))
        }
        (
            PairMode::ProblemNoAbandon,
            AutomationEffectAdmission::PreAdmissionProblem(reflector),
            AutomationEffectAdmission::PreAdmissionProblem(skill),
        ) => Ok(CombinedEffectAdmission::PreAdmissionProblem(vec![
            reflector, skill,
        ])),
        (
            PairMode::ProblemNoAbandon,
            AutomationEffectAdmission::PreAdmissionProblem(problem),
            AutomationEffectAdmission::Replay(_),
        )
        | (
            PairMode::ProblemNoAbandon,
            AutomationEffectAdmission::Replay(_),
            AutomationEffectAdmission::PreAdmissionProblem(problem),
        ) => Ok(CombinedEffectAdmission::PreAdmissionProblem(vec![problem])),
        (
            PairMode::ConflictAbandonSkill,
            AutomationEffectAdmission::Conflict,
            AutomationEffectAdmission::Execute(skill),
        ) => {
            skill.abandon_uncommitted().await?;
            Ok(CombinedEffectAdmission::Conflict)
        }
        (
            PairMode::ConflictAbandonReflector,
            AutomationEffectAdmission::Execute(reflector),
            AutomationEffectAdmission::Conflict,
        ) => {
            reflector.abandon_uncommitted().await?;
            Ok(CombinedEffectAdmission::Conflict)
        }
        (PairMode::ConflictNoAbandon, AutomationEffectAdmission::Conflict, _)
        | (PairMode::ConflictNoAbandon, _, AutomationEffectAdmission::Conflict) => {
            Ok(CombinedEffectAdmission::Conflict)
        }
        _ => Err(crate::errors::TraceDecayError::Config {
            message: "combined automation admission matrix was internally inconsistent".to_owned(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use fs2::FileExt;
    use tempfile::TempDir;
    use tracedecay_agent_hosts::automation::AutomationRunControl;
    use tracedecay_agent_hosts::automation::backend::{
        AgentTaskBackend, AgentTaskKind, AgentTaskRequest, AgentTaskResponse,
    };
    use tracedecay_agent_hosts::automation::config::AutomationConfig;
    use tracedecay_agent_hosts::automation::run_ledger::{
        AutomationRunStatus, AutomationTrigger, find_run_record_exact_bounded_blocking,
        run_ledger_path,
    };
    use tracedecay_agent_hosts::automation::runner::{
        AutomationSessionRetrieval, AutomationSessionRetrievalFuture, AutomationTemporalRetrieval,
        CombinedReviewAutomationOptions, RetainedAutomationSettlementDisposition,
        run_skill_writer_with_backend_and_retrieval,
    };
    use tracedecay_application::{
        CancellationSignal, ObservabilityHorizonV1, ObservabilityQueryPort, ObservabilityQueryV1,
    };
    use tracedecay_domain::{
        AutomationTerminalV1, ManifestDigest, ObservabilityPayloadV1, ProjectId, RunId, SessionId,
        canonical_sha256,
    };
    use tracedecay_usecases::observability::RegisteredObservabilityPortV1;

    use super::{
        AdmissionState, AutomationEffectAdmission, CombinedEffectAdmission, CombinedEffectOutcome,
        DaemonEngine, PairMode, TraceDecay, pair_mode, prepare_combined_effects,
        run_combined_scheduler_effect,
        run_session_reflector_with_backend_and_retrieval_for_retained_settlement,
        scheduler_automation_effect,
    };

    struct CombinedAdmissionFixture {
        _temp: TempDir,
        engine: DaemonEngine,
        memory: Arc<TraceDecay>,
        project_root: PathBuf,
        dashboard_root: PathBuf,
        project_id: ProjectId,
        configuration_revision_id: tracedecay_domain::configuration::ConfigurationRevisionId,
        configuration_digest: ManifestDigest,
    }

    impl CombinedAdmissionFixture {
        async fn new() -> Self {
            let temp = TempDir::new().expect("combined admission fixture");
            let project_root = temp.path().join("project");
            let profile_root = temp.path().join("profile");
            std::fs::create_dir_all(project_root.join("src"))
                .expect("combined admission source directory");
            std::fs::write(project_root.join("src/lib.rs"), "pub fn fixture() {}\n")
                .expect("combined admission source");
            let memory = Arc::new(
                TraceDecay::init_with_options(
                    &project_root,
                    crate::tracedecay::TraceDecayOpenOptions {
                        profile_root: Some(profile_root.clone()),
                        global_db_path: Some(profile_root.join("global.db")),
                    },
                )
                .await
                .expect("initialize combined admission project"),
            );
            let project_root = memory
                .project_root()
                .canonicalize()
                .expect("canonical combined admission project");
            let dashboard_root = memory.store_layout().dashboard_root.clone();
            let project_id = memory
                .configuration_runtime()
                .configuration_target()
                .project_id
                .clone();
            let scope = crate::daemon::project_open_owners::resolved_scope_for_project(
                &project_root,
                &project_id,
            )
            .expect("combined admission scope");
            let observed_at = tracedecay_application::now_micros();
            let configuration = memory
                .configuration_runtime()
                .client()
                .current()
                .await
                .expect("combined admission configuration");
            let configuration_revision_id = configuration.revision_id.clone();
            let access = crate::daemon::project_open_owners::daemon_owned_project_source_access_at(
                &scope,
                &project_root,
                &configuration,
                observed_at,
            )
            .expect("combined admission retained access");
            let grant = crate::daemon::project_open_owners::project_open_retained_grant(
                &access,
                observed_at,
            )
            .expect("combined admission retained grant");
            let engine = DaemonEngine::default();
            let invocation_service = engine.invocation.invocation_service();
            let retained_ports = crate::daemon::retained_owner::retained_surface_ports(
                crate::daemon::retained_owner::ProductionRetainedAuthoritiesV1 {
                    cg: Arc::new(tokio::sync::RwLock::new(Arc::clone(&memory))),
                    project_root: project_root.clone(),
                    project_id: project_id.clone(),
                    mounted_profile_id: None,
                    mounted_session_store_id: None,
                    mounted_session_root_id: None,
                    registered_session_db: None,
                    project_refresh: None,
                    project_retrieval: None,
                    project_workflow_index: None,
                    project_lcm: None,
                    configuration_digest: access.configuration_digest.clone(),
                    invocation_service: Some(invocation_service),
                },
            );
            engine
                .invocation
                .retained_runtime_registrar()
                .register(
                    project_root.clone(),
                    scope,
                    access.requester,
                    grant,
                    retained_ports,
                )
                .await
                .expect("register combined admission retained runtime");

            Self {
                _temp: temp,
                engine,
                memory,
                project_root,
                dashboard_root,
                project_id,
                configuration_revision_id,
                configuration_digest: access.configuration_digest,
            }
        }

        async fn mount_observability(&self) -> crate::global_db::RegisteredGlobalDbLeaseV1 {
            let session_db = self
                .memory
                .store_runtime_registry()
                .project_sessions(self.project_id.clone(), [self.project_root.clone()])
                .await
                .expect("combined admission project session database");
            let policy_digest = canonical_sha256(&(
                "tracedecay.combined-partial-replay.observability-policy.v1",
                &self.project_id,
                &self.configuration_digest,
            ))
            .expect("combined partial replay observability policy");
            self.engine
                .invocation
                .invocation_service()
                .mount_observability_producer(
                    self.project_root.clone(),
                    session_db.clone(),
                    self.project_id.clone(),
                    self.configuration_digest.clone(),
                    policy_digest,
                )
                .await
                .expect("mount combined partial replay observability");
            session_db
        }
    }

    #[derive(Clone, Copy)]
    enum ConflictingLeg {
        Reflector,
        Skill,
    }

    fn automation_journal_path(dashboard_root: &Path, run_id: &str) -> PathBuf {
        let run_id = RunId::new(run_id).expect("automation run id");
        let digest = canonical_sha256(&("tracedecay.automation-run.terminal-key.v1", &run_id))
            .expect("automation journal key");
        dashboard_root.join("automation_effects").join(format!(
            "{}.json",
            digest.as_str().trim_start_matches("sha256:")
        ))
    }

    fn pending_journal_files(dashboard_root: &Path) -> Vec<String> {
        let bytes = std::fs::read(
            dashboard_root
                .join("automation_effects")
                .join("pending-index.json"),
        )
        .expect("automation pending index");
        let value: serde_json::Value =
            serde_json::from_slice(&bytes).expect("valid automation pending index");
        let mut journals = value["entries"]
            .as_array()
            .expect("automation pending entries")
            .iter()
            .map(|entry| {
                entry["journal_file"]
                    .as_str()
                    .expect("pending journal filename")
                    .to_owned()
            })
            .collect::<Vec<_>>();
        journals.sort();
        journals
    }

    fn automation_terminal_sidecar_path(journal_path: &Path) -> PathBuf {
        let filename = journal_path
            .file_name()
            .and_then(|name| name.to_str())
            .expect("automation journal filename");
        journal_path.with_file_name(format!("{filename}.terminal"))
    }

    fn exact_spool_files(dashboard_root: &Path) -> Vec<PathBuf> {
        let mut paths = match std::fs::read_dir(dashboard_root.join("automation_run_spool")) {
            Ok(entries) => entries
                .filter_map(std::result::Result::ok)
                .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_file()))
                .map(|entry| entry.path())
                .collect::<Vec<_>>(),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(error) => panic!("read exact automation spool: {error}"),
        };
        paths.sort();
        paths
    }

    fn now_secs() -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("wall clock")
            .as_secs() as i64
    }

    struct RecordingEarlyGateBackend {
        calls: AtomicUsize,
    }

    impl RecordingEarlyGateBackend {
        const fn new() -> Self {
            Self {
                calls: AtomicUsize::new(0),
            }
        }
    }

    impl AgentTaskBackend for RecordingEarlyGateBackend {
        fn run_task(
            &self,
            _request: &AgentTaskRequest,
        ) -> std::result::Result<
            AgentTaskResponse,
            tracedecay_agent_hosts::automation::backend::AgentTaskError,
        > {
            self.calls.fetch_add(1, Ordering::SeqCst);
            panic!("a disabled scheduler run must not invoke its backend")
        }
    }

    struct RecordingEarlyGateRetrieval {
        anchor_session_id: SessionId,
        calls: AtomicUsize,
    }

    impl RecordingEarlyGateRetrieval {
        fn new() -> Self {
            Self {
                anchor_session_id: SessionId::new("combined-partial-replay")
                    .expect("combined partial replay session id"),
                calls: AtomicUsize::new(0),
            }
        }
    }

    impl AutomationSessionRetrieval for RecordingEarlyGateRetrieval {
        fn anchor_session_id(&self) -> &SessionId {
            &self.anchor_session_id
        }

        fn retrieve(
            &self,
            _query: tracedecay_usecases::session::SessionTemporalQuery,
        ) -> AutomationSessionRetrievalFuture<'_> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Box::pin(async { AutomationTemporalRetrieval::CompleteZero })
        }
    }

    async fn assert_conflict_abandons_fresh_sibling(conflicting_leg: ConflictingLeg) {
        let fixture = CombinedAdmissionFixture::new().await;
        let run_id = match conflicting_leg {
            ConflictingLeg::Reflector => "combined-conflict-reflector",
            ConflictingLeg::Skill => "combined-conflict-skill",
        };
        let skill_run_id = format!("{run_id}_skills");
        let options = CombinedReviewAutomationOptions::default();
        let parent_control = AutomationRunControl::from_interrupted(Arc::new(|| false));

        let existing_admission = match conflicting_leg {
            ConflictingLeg::Reflector => {
                let mut conflicting = options.session_reflector.clone();
                conflicting.query.push_str(" conflict");
                scheduler_automation_effect(
                    &fixture.engine,
                    fixture.memory.as_ref(),
                    &parent_control,
                    &fixture.project_root,
                    &fixture.dashboard_root,
                    Some(run_id),
                    fixture.configuration_digest.clone(),
                    |run_id| {
                        crate::daemon::automation_effect::session_reflector_run_request(
                            run_id,
                            &conflicting,
                        )
                    },
                )
                .await
                .expect("reserve conflicting reflector")
                .0
            }
            ConflictingLeg::Skill => {
                let mut conflicting = options.skill_writer.clone();
                conflicting.query.push_str(" conflict");
                scheduler_automation_effect(
                    &fixture.engine,
                    fixture.memory.as_ref(),
                    &parent_control,
                    &fixture.project_root,
                    &fixture.dashboard_root,
                    Some(&skill_run_id),
                    fixture.configuration_digest.clone(),
                    |run_id| {
                        crate::daemon::automation_effect::skill_writer_run_request(
                            run_id,
                            &conflicting,
                        )
                    },
                )
                .await
                .expect("reserve conflicting skill")
                .0
            }
        };
        let AutomationEffectAdmission::Execute(existing) = existing_admission else {
            panic!("fresh conflicting leg must own an Execute reservation")
        };
        let (conflicting_run_id, fresh_run_id) = match conflicting_leg {
            ConflictingLeg::Reflector => (run_id, skill_run_id.as_str()),
            ConflictingLeg::Skill => (skill_run_id.as_str(), run_id),
        };
        let conflicting_journal =
            automation_journal_path(&fixture.dashboard_root, conflicting_run_id);
        let fresh_journal = automation_journal_path(&fixture.dashboard_root, fresh_run_id);
        assert!(conflicting_journal.is_file());

        let admission = prepare_combined_effects(
            &fixture.engine,
            fixture.memory.as_ref(),
            &parent_control,
            &fixture.project_root,
            &fixture.dashboard_root,
            Some(run_id),
            fixture.configuration_digest.clone(),
            &options,
        )
        .await
        .expect("resolve combined admission conflict");

        assert!(matches!(admission, CombinedEffectAdmission::Conflict));
        assert!(
            conflicting_journal.is_file(),
            "the conflicting sibling remains owned by its original authority"
        );
        assert!(
            !fresh_journal.exists(),
            "the newly reserved sibling must be abandoned before Conflict returns"
        );
        assert_eq!(
            pending_journal_files(&fixture.dashboard_root),
            vec![
                conflicting_journal
                    .file_name()
                    .and_then(|name| name.to_str())
                    .expect("conflicting journal filename")
                    .to_owned()
            ],
            "only the intentionally conflicting live authority remains recoverable"
        );

        let cancellation = CancellationSignal::active(format!("cancel.{run_id}"))
            .expect("combined recovery cancellation");
        let report =
            crate::daemon::automation_effect::reconcile_reserved_automation_effects_for_project(
                fixture.memory.as_ref(),
                &fixture.dashboard_root,
                &cancellation,
            )
            .await
            .expect("inspect preserved conflicting authority");
        assert_eq!(report.inspected, 1);
        assert_eq!(report.deferred, 1);
        assert_eq!(report.partial_effects, 0);
        assert_eq!(report.reset_required, 0);
        assert_eq!(report.indeterminate, 0);
        assert_eq!(report.already_terminal, 0);

        existing
            .abandon_uncommitted()
            .await
            .expect("abandon preserved conflicting authority");
        assert!(!conflicting_journal.exists());
        assert!(pending_journal_files(&fixture.dashboard_root).is_empty());
        let report =
            crate::daemon::automation_effect::reconcile_reserved_automation_effects_for_project(
                fixture.memory.as_ref(),
                &fixture.dashboard_root,
                &cancellation,
            )
            .await
            .expect("verify conflict cleanup has no false recovery");
        assert_eq!(report.inspected, 0);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn partial_replay_reuses_prior_scheduler_skip_without_current_publication() {
        let fixture = CombinedAdmissionFixture::new().await;
        let session_db = fixture.mount_observability().await;
        let backend = RecordingEarlyGateBackend::new();
        let retrieval = RecordingEarlyGateRetrieval::new();
        let config = AutomationConfig {
            enabled: false,
            ..AutomationConfig::default()
        };
        let parent_control = AutomationRunControl::from_interrupted(Arc::new(|| false));
        let options = CombinedReviewAutomationOptions::default();
        assert_eq!(options.trigger, AutomationTrigger::Scheduler);
        assert_eq!(options.skill_writer.trigger, AutomationTrigger::ManualCli);

        let prior_skill_run_id = "combined-partial-replay-prior-skill";
        let prior_skill = run_skill_writer_with_backend_and_retrieval(
            fixture.memory.as_ref(),
            &config,
            &fixture.configuration_revision_id,
            &backend,
            &retrieval,
            tracedecay_agent_hosts::automation::runner::SkillWriterAutomationOptions {
                trigger: AutomationTrigger::Scheduler,
                run_id: Some(prior_skill_run_id.to_owned()),
                ..options.skill_writer.clone()
            },
        )
        .await
        .expect("publish prior exact scheduler skip");
        assert_eq!(
            prior_skill.ledger_record.status,
            AutomationRunStatus::Skipped
        );
        assert_eq!(
            prior_skill.ledger_record.error.as_deref(),
            Some("automation_disabled")
        );

        let combined_run_id = "combined-partial-replay-current";
        let reflector_admission = scheduler_automation_effect(
            &fixture.engine,
            fixture.memory.as_ref(),
            &parent_control,
            &fixture.project_root,
            &fixture.dashboard_root,
            Some(combined_run_id),
            fixture.configuration_digest.clone(),
            |run_id| {
                crate::daemon::automation_effect::session_reflector_run_request(
                    run_id,
                    &options.session_reflector,
                )
            },
        )
        .await
        .expect("reserve replayed reflector")
        .0;
        let AutomationEffectAdmission::Execute(reflector_authority) = reflector_admission else {
            panic!("replayed reflector fixture must start as Execute")
        };
        let mut reflector_options = options.session_reflector.clone();
        reflector_options.trigger = AutomationTrigger::Scheduler;
        reflector_options.run_id = Some(combined_run_id.to_owned());
        let reflector_run =
            run_session_reflector_with_backend_and_retrieval_for_retained_settlement(
                fixture.memory.as_ref(),
                &config,
                &parent_control,
                &fixture.configuration_revision_id,
                &backend,
                &retrieval,
                reflector_options,
            )
            .await;
        let RetainedAutomationSettlementDisposition::Current {
            result: Ok(reflector_run),
            settlement_guard,
        } = reflector_run.into_settlement_disposition()
        else {
            panic!("first exact reflector skip must be a current retained terminal")
        };
        let replayed_reflector = reflector_authority
            .start_deferred_run_settlement_observed(
                reflector_run.ledger_record,
                reflector_run.committed_receipt,
                settlement_guard,
                None,
            )
            .wait()
            .await
            .expect("settle replayed reflector")
            .0;
        assert!(!replayed_reflector.is_completed());

        let ledger_path = run_ledger_path(&fixture.dashboard_root);
        let ledger_before = std::fs::read(&ledger_path).expect("partial replay ledger before run");
        let spool_before = exact_spool_files(&fixture.dashboard_root);
        let current_skill_run_id = format!("{combined_run_id}_skills");
        let current_skill_journal =
            automation_journal_path(&fixture.dashboard_root, &current_skill_run_id);
        let current_skill_sidecar = automation_terminal_sidecar_path(&current_skill_journal);
        let admission = prepare_combined_effects(
            &fixture.engine,
            fixture.memory.as_ref(),
            &parent_control,
            &fixture.project_root,
            &fixture.dashboard_root,
            Some(combined_run_id),
            fixture.configuration_digest.clone(),
            &options,
        )
        .await
        .expect("prepare combined partial replay");
        assert!(matches!(
            &admission,
            CombinedEffectAdmission::ReflectorReplay { .. }
        ));
        assert!(current_skill_journal.is_file());

        let journal_lock = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(crate::storage::append_lock_path(&current_skill_journal))
            .expect("open current skill journal lock");
        journal_lock
            .lock_exclusive()
            .expect("block current skill abandonment");
        let mut first_error = None;
        let mut effect = Box::pin(run_combined_scheduler_effect(
            admission,
            &fixture.engine,
            fixture.memory.as_ref(),
            &fixture.project_id,
            &fixture.project_root,
            &config,
            &fixture.configuration_revision_id,
            &backend,
            &retrieval,
            options,
            &mut first_error,
        ));
        let current_skill_task_lock = fixture
            .dashboard_root
            .join("automation_locks")
            .join("skill_writer.lock");
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while !current_skill_task_lock.is_file() {
                tokio::select! {
                    outcome = &mut effect => {
                        panic!("partial replay returned before current abandonment: {outcome:?}")
                    }
                    () = tokio::task::yield_now() => {}
                }
            }
        })
        .await
        .expect("current skill task lock appears while abandonment is blocked");
        assert!(
            tracedecay_agent_hosts::automation::scheduler::AutomationTaskLock::try_acquire(
                &fixture.dashboard_root,
                AgentTaskKind::SkillWriter,
                None,
                now_secs(),
            )
            .await
            .expect("competing skill task lock")
            .is_none(),
            "the current retained guard owns the task lock through abandonment"
        );
        assert!(current_skill_journal.is_file());
        assert!(!current_skill_sidecar.exists());
        assert_eq!(
            std::fs::read(&ledger_path).expect("ledger while abandonment blocked"),
            ledger_before
        );
        assert!(
            find_run_record_exact_bounded_blocking(&fixture.dashboard_root, &current_skill_run_id,)
                .expect("current exact ledger lookup while blocked")
                .is_none()
        );
        assert_eq!(exact_spool_files(&fixture.dashboard_root), spool_before);
        assert_eq!(pending_journal_files(&fixture.dashboard_root).len(), 1);

        FileExt::unlock(&journal_lock).expect("release current skill abandonment");
        assert_eq!((&mut effect).await, CombinedEffectOutcome::Handled);
        drop(effect);
        assert!(first_error.is_none());
        assert_eq!(backend.calls.load(Ordering::SeqCst), 0);
        assert_eq!(retrieval.calls.load(Ordering::SeqCst), 0);
        assert_eq!(
            std::fs::read(&ledger_path).expect("ledger after reused abandonment"),
            ledger_before
        );
        assert_eq!(
            find_run_record_exact_bounded_blocking(&fixture.dashboard_root, prior_skill_run_id)
                .expect("prior exact ledger lookup after reuse"),
            Some(prior_skill.ledger_record.clone())
        );
        assert!(
            find_run_record_exact_bounded_blocking(&fixture.dashboard_root, &current_skill_run_id,)
                .expect("current exact ledger absence after reuse")
                .is_none()
        );
        assert!(!current_skill_journal.exists());
        assert!(!current_skill_sidecar.exists());
        assert!(pending_journal_files(&fixture.dashboard_root).is_empty());
        assert_eq!(exact_spool_files(&fixture.dashboard_root), spool_before);
        assert!(
            tracedecay_agent_hosts::automation::scheduler::AutomationTaskLock::try_acquire(
                &fixture.dashboard_root,
                AgentTaskKind::SkillWriter,
                None,
                now_secs(),
            )
            .await
            .expect("post-abandonment skill task lock")
            .is_some()
        );

        fixture
            .engine
            .invocation
            .invocation_service()
            .expire_all()
            .await;
        let observed = RegisteredObservabilityPortV1::new(session_db.as_ref())
            .query(ObservabilityQueryV1 {
                authorized_scope_ref: fixture.project_id.as_str().to_owned(),
                event_kinds: vec!["automation.funnel.observed.v1".to_owned()],
                horizon: ObservabilityHorizonV1 {
                    since_micros: 0,
                    until_micros: i64::MAX,
                },
                after_watermark: None,
                limit: 32,
            })
            .await
            .expect("query partial replay observations");
        let prior_observations = observed
            .events
            .iter()
            .filter(|event| {
                matches!(
                    &event.payload,
                    ObservabilityPayloadV1::AutomationFunnel(observation)
                        if observation.run_ref == prior_skill_run_id
                            && observation.terminal == AutomationTerminalV1::Skipped
                )
            })
            .count();
        assert_eq!(prior_observations, 1, "exact prior row is observed once");
        assert!(observed.events.iter().all(|event| {
            !matches!(
                &event.payload,
                ObservabilityPayloadV1::AutomationFunnel(observation)
                    if observation.run_ref == current_skill_run_id
            )
        }));
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

    #[tokio::test]
    async fn conflicting_reflector_abandons_only_the_fresh_skill_reservation() {
        assert_conflict_abandons_fresh_sibling(ConflictingLeg::Reflector).await;
    }

    #[tokio::test]
    async fn conflicting_skill_abandons_only_the_fresh_reflector_reservation() {
        assert_conflict_abandons_fresh_sibling(ConflictingLeg::Skill).await;
    }

    #[test]
    fn host_receipt_requires_both_exact_completed_terminals() {
        assert!(CombinedEffectOutcome::Completed.completed());
        assert!(!CombinedEffectOutcome::Handled.completed());
        assert!(!CombinedEffectOutcome::Deferred.completed());
    }

    #[test]
    fn only_not_combined_dispatch_falls_back_to_standalone_gates() {
        assert!(CombinedEffectOutcome::Completed.handled());
        assert!(CombinedEffectOutcome::Handled.handled());
        assert!(!CombinedEffectOutcome::Deferred.handled());
    }
}
