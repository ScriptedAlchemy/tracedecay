use std::path::Path;

use tracedecay_agent_hosts::automation::AutomationRunControl;
use tracedecay_agent_hosts::automation::backend::AgentTaskKind;

use super::super::{DaemonEngine, DaemonHandshake, log_daemon_event};
use super::{
    automation_scheduler_has_work, await_scheduler_automation_problem,
    effective_automation_config_for_project, log_scheduler_automation_replay,
    log_scheduler_task_error, log_scheduler_task_start, maybe_run_global_retention,
    run_user_jobs_scheduler_pass, scheduler_run_observer,
};
use crate::daemon::automation_effect::AutomationEffectAdmission;
use crate::errors::{Result, TraceDecayError};
use crate::tracedecay::TraceDecay;

pub(super) fn log_scheduler_pre_admission_problem(
    project_path: &Path,
    task: tracedecay_agent_hosts::automation::backend::AgentTaskKind,
    problem: &tracedecay_application::ApplicationProblemEnvelope,
) {
    let mut fields = vec![
        ("project", project_path.display().to_string()),
        (
            "task",
            tracedecay_agent_hosts::automation::backend::task_key(task).to_owned(),
        ),
        ("request_id", problem.request_id.as_str().to_owned()),
        ("problem_kind", format!("{:?}", problem.problem.kind())),
        ("problem_code", problem.problem.code.clone()),
    ];
    match serde_json::to_string(problem) {
        Ok(envelope) => fields.push(("application_problem", envelope)),
        Err(error) => fields.push(("observation_error", error.to_string())),
    }
    log_daemon_event("scheduler_task_application_pre_admission_problem", &fields);
}

pub(super) fn log_scheduler_admission_conflict(
    project_path: &Path,
    task: tracedecay_agent_hosts::automation::backend::AgentTaskKind,
) {
    log_daemon_event(
        "scheduler_task_automation_admission_conflict",
        &[
            ("project", project_path.display().to_string()),
            (
                "task",
                tracedecay_agent_hosts::automation::backend::task_key(task).to_owned(),
            ),
            ("outcome", "skipped".to_owned()),
            ("reason", "durable_admission_conflict".to_owned()),
        ],
    );
}

pub(super) async fn scheduler_automation_effect(
    engine: &DaemonEngine,
    memory: &crate::tracedecay::TraceDecay,
    run_control: &AutomationRunControl,
    project_path: &Path,
    dashboard_root: &Path,
    requested_run_id: Option<&str>,
    configuration_digest: tracedecay_domain::ManifestDigest,
    request: impl FnOnce(
        &str,
    )
        -> Result<tracedecay_application::retained_surfaces::AutomationRunRequestV1>,
) -> Result<(
    crate::daemon::automation_effect::AutomationEffectAdmission,
    String,
    AutomationRunControl,
)> {
    let request_id = scheduler_automation_request_id(requested_run_id)?;
    let cancellation = tracedecay_application::CancellationSignal::active(format!(
        "cancel.{}",
        request_id.as_str()
    ))
    .map_err(|error| TraceDecayError::Config {
        message: format!("automation scheduler cancellation is invalid: {error}"),
    })?;
    let observed_at = tracedecay_application::now_micros();
    let deadline = tracedecay_application::Deadline::new(tracedecay_domain::UtcMicros(i64::MAX))
        .map_err(|error| TraceDecayError::Config {
            message: format!("automation scheduler deadline is invalid: {error}"),
        })?;
    let effect_run_control =
        scheduler_effect_run_control(run_control, cancellation.clone(), deadline.clone());
    synchronize_scheduler_effect_control(&effect_run_control);
    let run_id = requested_run_id
        .map(str::to_owned)
        .unwrap_or_else(|| request_id.as_str().to_owned());
    let request = request(&run_id)?;
    let effect = crate::daemon::automation_effect::AutomationEffectAuthority::prepare(
        &engine.invocation.invocation_service(),
        memory,
        project_path,
        dashboard_root,
        request_id,
        deadline,
        &cancellation,
        observed_at,
        configuration_digest,
        request,
    )
    .await?;
    Ok((effect, run_id, effect_run_control))
}

fn scheduler_effect_run_control(
    run_control: &AutomationRunControl,
    cancellation: tracedecay_application::CancellationSignal,
    deadline: tracedecay_application::Deadline,
) -> AutomationRunControl {
    let parent = run_control.read_control().clone();
    AutomationRunControl::from_interrupted(std::sync::Arc::new(move || {
        let observed_at = tracedecay_application::now_micros();
        if parent.interrupted() {
            let _ = cancellation.cancel(observed_at);
        }
        cancellation.is_cancelled() || deadline.is_elapsed_at(observed_at)
    }))
}

pub(super) fn synchronize_scheduler_effect_control(run_control: &AutomationRunControl) {
    run_control.read_control().interrupted();
}

pub(super) async fn abandon_reused_scheduler_skip(
    engine: &DaemonEngine,
    project_id: &tracedecay_domain::ProjectId,
    project_path: &Path,
    task: AgentTaskKind,
    run_control: &AutomationRunControl,
    effect: crate::daemon::automation_effect::AutomationEffectAuthority,
    reused: tracedecay_agent_hosts::automation::runner::ReusedSchedulerSkip,
    settlement_guard: tracedecay_agent_hosts::automation::runner::AutomationRunSettlementGuard,
) -> Option<TraceDecayError> {
    synchronize_scheduler_effect_control(run_control);
    let settlement = match effect.start_reused_scheduler_skip_abandonment_observed(
        reused,
        settlement_guard,
        Some(scheduler_run_observer(engine, project_id, project_path)),
    ) {
        Ok(settlement) => settlement,
        Err(error) => {
            log_scheduler_task_error(project_path, task, &error);
            return Some((*error).into_error());
        }
    };
    match settlement.wait().await {
        Ok(_) => None,
        Err(error) => {
            log_scheduler_task_error(project_path, task, &error);
            Some(error)
        }
    }
}

pub(in crate::daemon) async fn run_automation_scheduler_tick(
    project_path: &Path,
    cg: &TraceDecay,
    handshake: &DaemonHandshake,
    engine: &DaemonEngine,
    run_control: &AutomationRunControl,
) -> Result<()> {
    use tracedecay_agent_hosts::automation::backend::CodexAppServerBackend;
    use tracedecay_agent_hosts::automation::run_ledger::AutomationTrigger;
    use tracedecay_agent_hosts::automation::runner::{
        CombinedReviewAutomationOptions, MemoryCuratorAutomationOptions,
        RetainedAutomationSettlementDisposition, SessionReflectorAutomationOptions,
        SkillWriterAutomationOptions, registered_project_automation_retrieval,
        run_memory_curator_with_backend_for_retained_settlement,
        run_session_reflector_with_backend_and_retrieval_for_retained_settlement,
        run_skill_writer_with_backend_and_retrieval_for_retained_settlement,
    };

    let control = tracedecay_agent_hosts::automation::scheduler::load_scheduler_control(
        &cg.store_layout().dashboard_root,
    )
    .await?;
    if control.paused {
        log_daemon_event(
            "scheduler_tick",
            &[
                ("project", project_path.display().to_string()),
                ("outcome", "skipped".to_string()),
                ("reason", "paused".to_string()),
            ],
        );
        return Ok(());
    }
    let configuration = effective_automation_config_for_project(cg).await?;
    let config = &configuration.settings;
    if !automation_scheduler_has_work(cg, config).await? {
        log_daemon_event(
            "scheduler_tick",
            &[
                ("project", project_path.display().to_string()),
                ("outcome", "skipped".to_string()),
                ("reason", "not_configured".to_string()),
            ],
        );
        return Ok(());
    }
    if let Ok(profile_database) = engine
        .store_administration
        .registered_profile_database()
        .await
    {
        maybe_run_global_retention(profile_database.as_ref(), &cg.get_config().sync.retention)
            .await;
    }
    let backend = CodexAppServerBackend::from_automation_config(config);
    let authoritative_project_id = cg
        .store_layout()
        .identity
        .project_id
        .as_deref()
        .ok_or_else(|| TraceDecayError::Config {
            message: "automation scheduler requires an authoritative project identity".to_string(),
        })?;
    let project_id = tracedecay_domain::ProjectId::new(authoritative_project_id.to_string())
        .map_err(|error| TraceDecayError::Config {
            message: format!(
                "automation scheduler has an invalid authoritative project identity: {error}"
            ),
        })?;
    let session_database = engine
        .store_administration
        .registered_project_session_database(project_path, cg.store_layout())
        .await?;
    let profile_identity = engine.store_administration.profile_identity()?.clone();
    let retrieval =
        registered_project_automation_retrieval(session_database, &profile_identity, &project_id)
            .await?;
    let mut first_error: Option<TraceDecayError> = None;

    log_scheduler_task_start(project_path, AgentTaskKind::MemoryCurator);
    let memory_curator_options = MemoryCuratorAutomationOptions {
        trigger: AutomationTrigger::Scheduler,
        ..MemoryCuratorAutomationOptions::default()
    };
    match scheduler_automation_effect(
        engine,
        cg,
        run_control,
        project_path,
        &cg.store_layout().dashboard_root,
        None,
        configuration.configuration_digest.clone(),
        |run_id| {
            crate::daemon::automation_effect::memory_curator_run_request(
                run_id,
                memory_curator_options.fact_review_limit,
                memory_curator_options.min_confidence,
            )
        },
    )
    .await
    {
        Ok((admission, run_id, effect_run_control)) => match admission {
            AutomationEffectAdmission::Conflict => {
                log_scheduler_admission_conflict(project_path, AgentTaskKind::MemoryCurator);
            }
            AutomationEffectAdmission::PreAdmissionProblem(problem) => {
                log_scheduler_pre_admission_problem(
                    project_path,
                    AgentTaskKind::MemoryCurator,
                    &problem,
                );
            }
            AutomationEffectAdmission::Replay(terminal) => {
                log_scheduler_automation_replay(
                    project_path,
                    AgentTaskKind::MemoryCurator,
                    &terminal,
                );
            }
            AutomationEffectAdmission::Execute(effect) => {
                let mut options = memory_curator_options;
                options.run_id = Some(run_id);
                let retained_run = run_memory_curator_with_backend_for_retained_settlement(
                    cg,
                    config,
                    &configuration.configuration_revision_id,
                    &backend,
                    options,
                    &effect_run_control,
                )
                .await;
                match retained_run.into_settlement_disposition() {
                    RetainedAutomationSettlementDisposition::Current {
                        result: Ok(run),
                        settlement_guard,
                    } => {
                        synchronize_scheduler_effect_control(&effect_run_control);
                        let settlement = effect.start_deferred_run_settlement_observed(
                            run.ledger_record,
                            run.committed_receipt,
                            settlement_guard,
                            Some(scheduler_run_observer(engine, &project_id, project_path)),
                        );
                        if let Err(error) = settlement.wait().await {
                            log_scheduler_task_error(
                                project_path,
                                AgentTaskKind::MemoryCurator,
                                &error,
                            );
                            first_error.get_or_insert(error);
                        }
                    }
                    RetainedAutomationSettlementDisposition::Current {
                        result: Err(error),
                        settlement_guard,
                    } => {
                        synchronize_scheduler_effect_control(&effect_run_control);
                        let settlement = effect.start_deferred_problem_settlement_observed(
                            error,
                            settlement_guard,
                            Some(scheduler_run_observer(engine, &project_id, project_path)),
                        );
                        if let Some(error) = await_scheduler_automation_problem(
                            project_path,
                            AgentTaskKind::MemoryCurator,
                            settlement,
                        )
                        .await
                        {
                            first_error.get_or_insert(error);
                        }
                    }
                    RetainedAutomationSettlementDisposition::ReusedSchedulerSkip {
                        reused,
                        settlement_guard,
                    } => {
                        if let Some(error) = abandon_reused_scheduler_skip(
                            engine,
                            &project_id,
                            project_path,
                            AgentTaskKind::MemoryCurator,
                            &effect_run_control,
                            effect,
                            reused,
                            settlement_guard,
                        )
                        .await
                        {
                            first_error.get_or_insert(error);
                        }
                    }
                }
            }
        },
        Err(error) => {
            log_scheduler_task_error(project_path, AgentTaskKind::MemoryCurator, &error);
            first_error.get_or_insert(error);
        }
    }
    // When both the reflector and the skill writer are due in this tick, the
    // combined path serves them with one backend call. Any other outcome
    // (combined mode disabled, only one task due, missing evidence) falls
    // back to the sequential per-task runs below.
    let mut combined_handled = false;
    if config.combine_due_tasks {
        log_scheduler_task_start(project_path, AgentTaskKind::CombinedReview);
        let combined_options = CombinedReviewAutomationOptions {
            skill_writer: SkillWriterAutomationOptions {
                profile_root: Some(profile_identity.profile_root().to_path_buf()),
                ..SkillWriterAutomationOptions::default()
            },
            ..CombinedReviewAutomationOptions::default()
        };
        match super::combined_effect::prepare_combined_effects(
            engine,
            cg,
            run_control,
            project_path,
            &cg.store_layout().dashboard_root,
            None,
            configuration.configuration_digest.clone(),
            &combined_options,
        )
        .await
        {
            Ok(admission) => {
                combined_handled = super::combined_effect::run_combined_scheduler_effect(
                    admission,
                    engine,
                    cg,
                    &project_id,
                    project_path,
                    config,
                    &configuration.configuration_revision_id,
                    &backend,
                    retrieval.as_ref(),
                    combined_options,
                    &mut first_error,
                )
                .await
                .handled();
            }
            Err(error) => {
                log_scheduler_task_error(project_path, AgentTaskKind::CombinedReview, &error);
                first_error.get_or_insert(error);
            }
        }
    }
    if !combined_handled {
        log_scheduler_task_start(project_path, AgentTaskKind::SessionReflector);
        let session_options = SessionReflectorAutomationOptions {
            trigger: AutomationTrigger::Scheduler,
            ..SessionReflectorAutomationOptions::default()
        };
        let session_effect = scheduler_automation_effect(
            engine,
            cg,
            run_control,
            project_path,
            &cg.store_layout().dashboard_root,
            None,
            configuration.configuration_digest.clone(),
            |run_id| {
                crate::daemon::automation_effect::session_reflector_run_request(
                    run_id,
                    &session_options,
                )
            },
        )
        .await;
        match session_effect {
            Err(error) => {
                log_scheduler_task_error(project_path, AgentTaskKind::SessionReflector, &error);
                first_error.get_or_insert(error);
            }
            Ok((AutomationEffectAdmission::Conflict, _, _)) => {
                log_scheduler_admission_conflict(project_path, AgentTaskKind::SessionReflector);
            }
            Ok((AutomationEffectAdmission::PreAdmissionProblem(problem), _, _)) => {
                log_scheduler_pre_admission_problem(
                    project_path,
                    AgentTaskKind::SessionReflector,
                    &problem,
                );
            }
            Ok((AutomationEffectAdmission::Replay(terminal), _, _)) => {
                log_scheduler_automation_replay(
                    project_path,
                    AgentTaskKind::SessionReflector,
                    &terminal,
                );
            }
            Ok((AutomationEffectAdmission::Execute(effect), run_id, effect_run_control)) => {
                let retained_run =
                    run_session_reflector_with_backend_and_retrieval_for_retained_settlement(
                        cg,
                        config,
                        &effect_run_control,
                        &configuration.configuration_revision_id,
                        &backend,
                        retrieval.as_ref(),
                        SessionReflectorAutomationOptions {
                            run_id: Some(run_id),
                            ..session_options
                        },
                    )
                    .await;
                match retained_run.into_settlement_disposition() {
                    RetainedAutomationSettlementDisposition::Current {
                        result: Ok(run),
                        settlement_guard,
                    } => {
                        synchronize_scheduler_effect_control(&effect_run_control);
                        let settlement = effect.start_deferred_run_settlement_observed(
                            run.ledger_record,
                            run.committed_receipt,
                            settlement_guard,
                            Some(scheduler_run_observer(engine, &project_id, project_path)),
                        );
                        if let Err(error) = settlement.wait().await {
                            log_scheduler_task_error(
                                project_path,
                                AgentTaskKind::SessionReflector,
                                &error,
                            );
                            first_error.get_or_insert(error);
                        }
                    }
                    RetainedAutomationSettlementDisposition::Current {
                        result: Err(error),
                        settlement_guard,
                    } => {
                        synchronize_scheduler_effect_control(&effect_run_control);
                        let settlement = effect.start_deferred_problem_settlement_observed(
                            error,
                            settlement_guard,
                            Some(scheduler_run_observer(engine, &project_id, project_path)),
                        );
                        if let Some(error) = await_scheduler_automation_problem(
                            project_path,
                            AgentTaskKind::SessionReflector,
                            settlement,
                        )
                        .await
                        {
                            first_error.get_or_insert(error);
                        }
                    }
                    RetainedAutomationSettlementDisposition::ReusedSchedulerSkip {
                        reused,
                        settlement_guard,
                    } => {
                        if let Some(error) = abandon_reused_scheduler_skip(
                            engine,
                            &project_id,
                            project_path,
                            AgentTaskKind::SessionReflector,
                            &effect_run_control,
                            effect,
                            reused,
                            settlement_guard,
                        )
                        .await
                        {
                            first_error.get_or_insert(error);
                        }
                    }
                }
            }
        }
        log_scheduler_task_start(project_path, AgentTaskKind::SkillWriter);
        let skill_options = SkillWriterAutomationOptions {
            trigger: AutomationTrigger::Scheduler,
            profile_root: Some(profile_identity.profile_root().to_path_buf()),
            ..SkillWriterAutomationOptions::default()
        };
        match scheduler_automation_effect(
            engine,
            cg,
            run_control,
            project_path,
            &cg.store_layout().dashboard_root,
            None,
            configuration.configuration_digest.clone(),
            |run_id| {
                crate::daemon::automation_effect::skill_writer_run_request(run_id, &skill_options)
            },
        )
        .await
        {
            Err(error) => {
                log_scheduler_task_error(project_path, AgentTaskKind::SkillWriter, &error);
                first_error.get_or_insert(error);
            }
            Ok((AutomationEffectAdmission::Conflict, _, _)) => {
                log_scheduler_admission_conflict(project_path, AgentTaskKind::SkillWriter);
            }
            Ok((AutomationEffectAdmission::PreAdmissionProblem(problem), _, _)) => {
                log_scheduler_pre_admission_problem(
                    project_path,
                    AgentTaskKind::SkillWriter,
                    &problem,
                );
            }
            Ok((AutomationEffectAdmission::Replay(terminal), _, _)) => {
                log_scheduler_automation_replay(
                    project_path,
                    AgentTaskKind::SkillWriter,
                    &terminal,
                );
            }
            Ok((AutomationEffectAdmission::Execute(effect), run_id, effect_run_control)) => {
                let mut options = skill_options;
                options.run_id = Some(run_id);
                let retained_run =
                    run_skill_writer_with_backend_and_retrieval_for_retained_settlement(
                        cg,
                        config,
                        &configuration.configuration_revision_id,
                        &backend,
                        retrieval.as_ref(),
                        options,
                    )
                    .await;
                match retained_run.into_settlement_disposition() {
                    RetainedAutomationSettlementDisposition::Current {
                        result: Ok(run),
                        settlement_guard,
                    } => {
                        synchronize_scheduler_effect_control(&effect_run_control);
                        let settlement = effect.start_deferred_run_settlement_observed(
                            run.ledger_record,
                            run.committed_receipt,
                            settlement_guard,
                            Some(scheduler_run_observer(engine, &project_id, project_path)),
                        );
                        if let Err(error) = settlement.wait().await {
                            first_error.get_or_insert(error);
                        }
                    }
                    RetainedAutomationSettlementDisposition::Current {
                        result: Err(error),
                        settlement_guard,
                    } => {
                        synchronize_scheduler_effect_control(&effect_run_control);
                        let settlement = effect.start_deferred_problem_settlement_observed(
                            error,
                            settlement_guard,
                            Some(scheduler_run_observer(engine, &project_id, project_path)),
                        );
                        if let Some(error) = await_scheduler_automation_problem(
                            project_path,
                            AgentTaskKind::SkillWriter,
                            settlement,
                        )
                        .await
                        {
                            first_error.get_or_insert(error);
                        }
                    }
                    RetainedAutomationSettlementDisposition::ReusedSchedulerSkip {
                        reused,
                        settlement_guard,
                    } => {
                        if let Some(error) = abandon_reused_scheduler_skip(
                            engine,
                            &project_id,
                            project_path,
                            AgentTaskKind::SkillWriter,
                            &effect_run_control,
                            effect,
                            reused,
                            settlement_guard,
                        )
                        .await
                        {
                            first_error.get_or_insert(error);
                        }
                    }
                }
            }
        }
    }
    run_user_jobs_scheduler_pass(
        engine,
        run_control,
        &project_id,
        project_path,
        &handshake.client_identity.profile_root,
        cg,
        configuration.configuration_digest.clone(),
        config,
        &backend,
        &mut first_error,
    )
    .await;
    match first_error {
        Some(err) => Err(err),
        None => Ok(()),
    }
}

pub(crate) fn scheduler_automation_request_id(
    requested_run_id: Option<&str>,
) -> Result<tracedecay_application::RequestId> {
    match requested_run_id {
        Some(run_id) => {
            let digest = tracedecay_domain::canonical_sha256(&(
                "tracedecay.automation-scheduler.request-id.v1",
                run_id,
            ))
            .map_err(|error| TraceDecayError::Config {
                message: format!("automation scheduler stable request digest is invalid: {error}"),
            })?;
            tracedecay_application::RequestId::new(format!(
                "request.automation-scheduler.{}",
                digest.as_str().trim_start_matches("sha256:")
            ))
            .map_err(|error| TraceDecayError::Config {
                message: format!(
                    "automation scheduler stable request identity is invalid: {error}"
                ),
            })
        }
        None => crate::request_identity::mint_global_request_id(
            crate::request_identity::GlobalRequestSurface::AutomationScheduler,
        )
        .map_err(|error| TraceDecayError::Config {
            message: format!("automation scheduler request identity is unavailable: {error}"),
        }),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use tracedecay_agent_hosts::automation::AutomationRunControl;
    use tracedecay_application::{CancellationSignal, Deadline};
    use tracedecay_domain::UtcMicros;

    use super::scheduler_effect_run_control;

    #[test]
    fn effect_control_propagates_live_scheduler_stop_to_cancellation() {
        let stopped = Arc::new(AtomicBool::new(false));
        let observed = Arc::clone(&stopped);
        let scheduler = AutomationRunControl::from_interrupted(Arc::new(move || {
            observed.load(Ordering::Acquire)
        }));
        let cancellation = CancellationSignal::active("cancel.scheduler-effect-stop")
            .expect("valid cancellation signal");
        let effect_cancellation = cancellation.clone();
        let control = scheduler_effect_run_control(
            &scheduler,
            cancellation,
            Deadline::new(UtcMicros(i64::MAX)).expect("valid scheduler deadline"),
        );

        assert!(!control.read_control().interrupted());
        assert!(!effect_cancellation.is_cancelled());
        stopped.store(true, Ordering::Release);
        assert!(control.read_control().interrupted());
        assert!(effect_cancellation.is_cancelled());
    }

    #[test]
    fn effect_control_observes_deadline_without_fabricating_cancellation() {
        let scheduler = AutomationRunControl::from_interrupted(Arc::new(|| false));
        let cancellation = CancellationSignal::active("cancel.scheduler-effect-deadline")
            .expect("valid cancellation signal");
        let effect_cancellation = cancellation.clone();
        let control = scheduler_effect_run_control(
            &scheduler,
            cancellation,
            Deadline::new(UtcMicros(0)).expect("valid elapsed deadline"),
        );

        assert!(control.read_control().interrupted());
        assert!(!effect_cancellation.is_cancelled());
    }
}
