use std::future::Future;
use std::path::Path;

use tracedecay_agent_hosts::automation::AutomationRunControl;
use tracedecay_agent_hosts::automation::runner::CombinedReviewAutomationOptions;
use tracedecay_agent_hosts::automation::runner::{
    CombinedReviewDispatch, run_combined_review_with_backend_and_retrieval,
    run_session_reflector_with_backend_and_retrieval, run_skill_writer_with_backend_and_retrieval,
};

use super::scheduler_automation_effect;
use crate::daemon::DaemonEngine;
use crate::daemon::automation_effect::{
    AutomationEffectAdmission, AutomationEffectAuthority, AutomationSettledTerminal,
};
use crate::errors::Result;
use crate::tracedecay::TraceDecay;

pub(super) enum CombinedEffectAdmission {
    Execute {
        run_id: String,
        run_control: AutomationRunControl,
        reflector: AutomationEffectAuthority,
        skill: AutomationEffectAuthority,
    },
    ReflectorReplay {
        reflector: AutomationSettledTerminal,
        skill_run_id: String,
        skill_control: AutomationRunControl,
        skill: AutomationEffectAuthority,
    },
    SkillReplay {
        run_id: String,
        reflector_control: AutomationRunControl,
        reflector: AutomationEffectAuthority,
        skill: AutomationSettledTerminal,
    },
    Replay {
        reflector: AutomationSettledTerminal,
        skill: AutomationSettledTerminal,
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PartialOwner {
    SkillThenReflector,
    ReflectorThenSkill,
}

fn partial_owner(completed_reflector: bool) -> PartialOwner {
    if completed_reflector {
        PartialOwner::ReflectorThenSkill
    } else {
        PartialOwner::SkillThenReflector
    }
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

async fn attempt_both<A, B>(
    first: impl Future<Output = A>,
    second: impl Future<Output = B>,
) -> (A, B) {
    let first = first.await;
    let second = second.await;
    (first, second)
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

#[cfg(test)]
mod tests {
    use super::{
        AdmissionState, CombinedEffectOutcome, PairMode, PartialOwner, attempt_both, pair_mode,
        partial_owner,
    };
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    async fn assert_second_attempted_after_first_failure() {
        let attempted = Arc::new(AtomicBool::new(false));
        let observed = Arc::clone(&attempted);
        let (first, second) = attempt_both(async { Err::<(), _>("first") }, async move {
            observed.store(true, Ordering::SeqCst);
            Ok::<_, &str>(())
        })
        .await;
        assert_eq!(first, Err("first"));
        assert_eq!(second, Ok(()));
        assert!(attempted.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn completed_and_failed_ledgers_attempt_both_terminals() {
        assert_second_attempted_after_first_failure().await;
    }

    #[tokio::test]
    async fn skill_partial_attempts_problem_after_reflector_terminal_failure() {
        assert_second_attempted_after_first_failure().await;
    }

    #[tokio::test]
    async fn not_combined_attempts_both_abandonments() {
        assert_second_attempted_after_first_failure().await;
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

    #[test]
    fn completed_reflector_moves_partial_settlement_to_skill_leg() {
        assert_eq!(partial_owner(true), PartialOwner::ReflectorThenSkill);
        assert_eq!(partial_owner(false), PartialOwner::SkillThenReflector);
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
            let replay_completed = reflector.is_completed();
            match run_skill_writer_with_backend_and_retrieval(
                memory,
                config,
                configuration_revision_id,
                backend,
                retrieval,
                skill_options,
            )
            .await
            {
                Ok(run) => {
                    super::synchronize_scheduler_effect_control(&skill_control);
                    match skill
                        .settle_run(&run.ledger_record, run.committed_receipt.as_ref())
                        .await
                    {
                        Ok(_) => {
                            super::record_scheduler_run(
                                engine,
                                project_id,
                                project_path,
                                &run.ledger_record,
                            );
                            if replay_completed
                                && run.ledger_record.status
                                    == tracedecay_agent_hosts::automation::run_ledger::AutomationRunStatus::Succeeded
                            {
                                return CombinedEffectOutcome::Completed;
                            }
                        }
                        Err(error) => {
                            first_error.get_or_insert(error);
                        }
                    }
                }
                Err(error) => {
                    if let Some(error) = super::settle_scheduler_automation_error(
                        engine,
                        project_id,
                        project_path,
                        tracedecay_agent_hosts::automation::backend::AgentTaskKind::SkillWriter,
                        &skill_control,
                        skill,
                        error,
                    )
                    .await
                    {
                        first_error.get_or_insert(error);
                    }
                }
            }
            CombinedEffectOutcome::Handled
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
            let replay_completed = skill.is_completed();
            match run_session_reflector_with_backend_and_retrieval(
                memory,
                config,
                &reflector_control,
                configuration_revision_id,
                backend,
                retrieval,
                reflector_options,
            )
            .await
            {
                Ok(run) => {
                    super::synchronize_scheduler_effect_control(&reflector_control);
                    match reflector
                        .settle_run(&run.ledger_record, run.committed_receipt.as_ref())
                        .await
                    {
                        Ok(_) => {
                            super::record_scheduler_run(
                                engine,
                                project_id,
                                project_path,
                                &run.ledger_record,
                            );
                            if replay_completed
                                && run.ledger_record.status
                                    == tracedecay_agent_hosts::automation::run_ledger::AutomationRunStatus::Succeeded
                            {
                                return CombinedEffectOutcome::Completed;
                            }
                        }
                        Err(error) => {
                            first_error.get_or_insert(error);
                        }
                    }
                }
                Err(error) => {
                    if let Some(error) = super::settle_scheduler_automation_error(
                        engine,
                        project_id,
                        project_path,
                        tracedecay_agent_hosts::automation::backend::AgentTaskKind::SessionReflector,
                        &reflector_control,
                        reflector,
                        error,
                    )
                    .await
                    {
                        first_error.get_or_insert(error);
                    }
                }
            }
            CombinedEffectOutcome::Handled
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
                reflector,
                skill,
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
    let result = run_combined_review_with_backend_and_retrieval(
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
    super::synchronize_scheduler_effect_control(&run_control);
    match result {
        Ok(CombinedReviewDispatch::Ran(run)) => {
            let (reflector_result, skill_result) = attempt_both(
                reflector.settle_run(
                    &run.session_reflector.ledger_record,
                    run.session_reflector.committed_receipt.as_ref(),
                ),
                skill.settle_run(
                    &run.skill_writer.ledger_record,
                    run.skill_writer.committed_receipt.as_ref(),
                ),
            )
            .await;
            if let Err(error) = reflector_result.and(skill_result) {
                first_error.get_or_insert(error);
            } else {
                super::record_combined_scheduler_run(engine, project_id, project_path, &run);
            }
            if first_error.is_none()
                && run.session_reflector.ledger_record.status
                    == tracedecay_agent_hosts::automation::run_ledger::AutomationRunStatus::Succeeded
                && run.skill_writer.ledger_record.status
                    == tracedecay_agent_hosts::automation::run_ledger::AutomationRunStatus::Succeeded
            {
                CombinedEffectOutcome::Completed
            } else {
                CombinedEffectOutcome::Handled
            }
        }
        Ok(CombinedReviewDispatch::MemoryCompletedSkillFailure {
            session_reflector,
            skill_writer_record,
            error,
        }) => {
            if let Some(record) = skill_writer_record.as_ref() {
                let (reflector_result, skill_result) = attempt_both(
                    reflector.settle_run(
                        &session_reflector.ledger_record,
                        session_reflector.committed_receipt.as_ref(),
                    ),
                    skill.settle_run(record, None),
                )
                .await;
                if reflector_result.is_ok() {
                    super::record_scheduler_run(
                        engine,
                        project_id,
                        project_path,
                        &session_reflector.ledger_record,
                    );
                }
                if skill_result.is_ok() {
                    super::record_scheduler_run(engine, project_id, project_path, record);
                }
                if let Err(error) = reflector_result {
                    first_error.get_or_insert(error);
                }
                if let Err(error) = skill_result {
                    first_error.get_or_insert(error);
                }
                super::log_scheduler_task_error(
                    project_path,
                    tracedecay_agent_hosts::automation::backend::AgentTaskKind::SkillWriter,
                    &error,
                );
            } else {
                let (reflector_result, skill_result) = attempt_both(
                    reflector.settle_run(
                        &session_reflector.ledger_record,
                        session_reflector.committed_receipt.as_ref(),
                    ),
                    super::settle_scheduler_automation_error(
                        engine,
                        project_id,
                        project_path,
                        tracedecay_agent_hosts::automation::backend::AgentTaskKind::SkillWriter,
                        &run_control,
                        skill,
                        tracedecay_agent_hosts::automation::AutomationRunError::Runtime(error),
                    ),
                )
                .await;
                if reflector_result.is_ok() {
                    super::record_scheduler_run(
                        engine,
                        project_id,
                        project_path,
                        &session_reflector.ledger_record,
                    );
                }
                if let Err(error) = reflector_result {
                    first_error.get_or_insert(error);
                }
                if let Some(error) = skill_result {
                    first_error.get_or_insert(error);
                }
            }
            CombinedEffectOutcome::Handled
        }
        Ok(CombinedReviewDispatch::RecordedFailure { run, .. }) => {
            let (reflector_result, skill_result) = attempt_both(
                reflector.settle_run(&run.session_reflector.ledger_record, None),
                skill.settle_run(&run.skill_writer.ledger_record, None),
            )
            .await;
            super::record_combined_scheduler_run(engine, project_id, project_path, &run);
            if let Err(error) = reflector_result.and(skill_result) {
                first_error.get_or_insert(error);
            }
            CombinedEffectOutcome::Handled
        }
        Ok(CombinedReviewDispatch::PartialEffect {
            run,
            completed_session_reflector,
            run_id: partial_run_id,
            committed_receipt,
            detail,
        }) => {
            let owner = partial_owner(completed_session_reflector.is_some());
            if owner == PartialOwner::ReflectorThenSkill {
                let Some(completed) = completed_session_reflector else {
                    first_error.get_or_insert(crate::errors::TraceDecayError::Config {
                        message: "skill partial effect lost its completed reflector".to_owned(),
                    });
                    return CombinedEffectOutcome::Handled;
                };
                let partial_error =
                    tracedecay_agent_hosts::automation::AutomationRunError::PartialEffect {
                        run_id: partial_run_id,
                        committed_receipt,
                        ledger_record: None,
                        detail,
                    };
                let (reflector_result, skill_result) = attempt_both(
                    reflector.settle_run(
                        &completed.ledger_record,
                        completed.committed_receipt.as_ref(),
                    ),
                    super::settle_scheduler_automation_error(
                        engine,
                        project_id,
                        project_path,
                        tracedecay_agent_hosts::automation::backend::AgentTaskKind::SkillWriter,
                        &run_control,
                        skill,
                        partial_error,
                    ),
                )
                .await;
                if reflector_result.is_ok() {
                    super::record_scheduler_run(
                        engine,
                        project_id,
                        project_path,
                        &completed.ledger_record,
                    );
                }
                if let Err(error) = reflector_result {
                    first_error.get_or_insert(error);
                }
                if let Some(error) = skill_result {
                    first_error.get_or_insert(error);
                }
            } else {
                if let Some(run) = run.as_deref() {
                    super::record_combined_scheduler_run(engine, project_id, project_path, run);
                    if let Err(error) = skill
                        .settle_run(&run.skill_writer.ledger_record, None)
                        .await
                    {
                        first_error.get_or_insert(error);
                    }
                } else if let Err(error) = skill.abandon_uncommitted().await {
                    first_error.get_or_insert(error);
                }
                let error = tracedecay_agent_hosts::automation::AutomationRunError::PartialEffect {
                    run_id: partial_run_id,
                    committed_receipt,
                    ledger_record: None,
                    detail,
                };
                if let Some(error) = super::settle_scheduler_automation_error(
                    engine,
                    project_id,
                    project_path,
                    tracedecay_agent_hosts::automation::backend::AgentTaskKind::SessionReflector,
                    &run_control,
                    reflector,
                    error,
                )
                .await
                {
                    first_error.get_or_insert(error);
                }
            }
            CombinedEffectOutcome::Handled
        }
        Ok(CombinedReviewDispatch::NotCombined { .. }) => {
            let (reflector_abandonment, skill_abandonment) =
                attempt_both(reflector.abandon_uncommitted(), skill.abandon_uncommitted()).await;
            if let Err(error) = reflector_abandonment {
                first_error.get_or_insert(error);
            }
            if let Err(error) = skill_abandonment {
                first_error.get_or_insert(error);
            }
            if first_error.is_some() {
                CombinedEffectOutcome::Handled
            } else {
                CombinedEffectOutcome::Deferred
            }
        }
        Err(error) => {
            let message = error.to_string();
            if let Some(error) = super::settle_scheduler_automation_error(
                engine,
                project_id,
                project_path,
                tracedecay_agent_hosts::automation::backend::AgentTaskKind::SessionReflector,
                &run_control,
                reflector,
                tracedecay_agent_hosts::automation::AutomationRunError::Runtime(error),
            )
            .await
            {
                first_error.get_or_insert(error);
            }
            let sibling_error = crate::errors::TraceDecayError::Config { message };
            if let Some(error) = super::settle_scheduler_automation_error(
                engine,
                project_id,
                project_path,
                tracedecay_agent_hosts::automation::backend::AgentTaskKind::SkillWriter,
                &run_control,
                skill,
                tracedecay_agent_hosts::automation::AutomationRunError::Runtime(sibling_error),
            )
            .await
            {
                first_error.get_or_insert(error);
            }
            CombinedEffectOutcome::Handled
        }
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
