use std::path::Path;

use tracedecay_agent_hosts::automation::AutomationRunControl;
use tracedecay_agent_hosts::automation::backend::AgentTaskKind;

use super::super::{DaemonEngine, DaemonHandshake, log_daemon_event};
use super::{
    automation_scheduler_has_work, effective_automation_config_for_project,
    log_scheduler_automation_replay, log_scheduler_task_error, log_scheduler_task_start,
    maybe_run_global_retention, run_user_jobs_scheduler_pass, scheduler_run_observer,
    settle_scheduler_retained_automation,
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
    let run_id = requested_run_id.map_or_else(|| request_id.as_str().to_owned(), str::to_owned);
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

/// Poll period for the scheduler cancellation bridge.
///
/// Retained settlement retries back off to at most five seconds, so a
/// sub-second poll guarantees an in-flight settlement observes cancellation
/// within one retry iteration of scheduler retirement or daemon draining.
const SCHEDULER_CANCELLATION_BRIDGE_POLL: std::time::Duration =
    std::time::Duration::from_millis(500);

/// Hard ceiling on one bridge's lifetime.
///
/// A bridge normally ends as soon as its effect run control is dropped or the
/// signal is cancelled. This bound is the backstop that stops a leaked run
/// control clone from polling for the remaining life of the daemon.
const SCHEDULER_CANCELLATION_BRIDGE_MAX_LIFETIME: std::time::Duration =
    std::time::Duration::from_mins(30);

/// Keeps one scheduler run's cancellation signal live while its settlement
/// blocks, and aborts that polling task as soon as the owning effect run
/// control is dropped.
struct SchedulerCancellationBridge {
    task: Option<tokio::task::JoinHandle<()>>,
}

impl Drop for SchedulerCancellationBridge {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

/// Flips one scheduler run's cancellation signal when the scheduler's own
/// interruption predicate turns true, without waiting for the next evaluation
/// of the effect run control.
///
/// The scheduler's shutdown sources (`DaemonLifecycle` draining and
/// `AutomationSchedulerStop`) are synchronous atomics with no shutdown future
/// to await, and the effect run control only sees them through the parent's
/// opaque predicate, so this polls that predicate rather than bridging a
/// channel.
fn spawn_scheduler_cancellation_bridge<Interrupted>(
    interrupted: Interrupted,
    cancellation: tracedecay_application::CancellationSignal,
) -> SchedulerCancellationBridge
where
    Interrupted: Fn() -> bool + Send + 'static,
{
    let Ok(runtime) = tokio::runtime::Handle::try_current() else {
        return SchedulerCancellationBridge { task: None };
    };
    let task = runtime.spawn(async move {
        let started = tokio::time::Instant::now();
        loop {
            tokio::time::sleep(SCHEDULER_CANCELLATION_BRIDGE_POLL).await;
            if cancellation.is_cancelled() {
                return;
            }
            if interrupted() {
                let _ = cancellation.cancel(tracedecay_application::now_micros());
                return;
            }
            if started.elapsed() >= SCHEDULER_CANCELLATION_BRIDGE_MAX_LIFETIME {
                return;
            }
        }
    });
    SchedulerCancellationBridge { task: Some(task) }
}

fn scheduler_effect_run_control(
    run_control: &AutomationRunControl,
    cancellation: tracedecay_application::CancellationSignal,
    deadline: tracedecay_application::Deadline,
) -> AutomationRunControl {
    let parent = run_control.read_control().clone();
    let bridge = spawn_scheduler_cancellation_bridge(
        {
            let parent = parent.clone();
            move || parent.interrupted()
        },
        cancellation.clone(),
    );
    AutomationRunControl::from_interrupted(std::sync::Arc::new(move || {
        // Load-bearing capture: the bridge is owned by this predicate, so it
        // is aborted exactly when the effect run control is dropped.
        let _bridge = &bridge;
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
        SessionReflectorAutomationOptions, SkillWriterAutomationOptions,
        registered_project_automation_retrieval,
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
        maybe_run_global_retention(
            &engine.store_administration,
            profile_database.as_ref(),
            &cg.get_config().sync.retention,
        )
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
                if let Some(error) = settle_scheduler_retained_automation(
                    engine,
                    &project_id,
                    project_path,
                    AgentTaskKind::MemoryCurator,
                    &effect_run_control,
                    *effect,
                    retained_run,
                    |run| (run.ledger_record, run.committed_receipt),
                )
                .await
                {
                    first_error.get_or_insert(error);
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
                if let Some(error) = settle_scheduler_retained_automation(
                    engine,
                    &project_id,
                    project_path,
                    AgentTaskKind::SessionReflector,
                    &effect_run_control,
                    *effect,
                    retained_run,
                    |run| (run.ledger_record, run.committed_receipt),
                )
                .await
                {
                    first_error.get_or_insert(error);
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
                if let Some(error) = settle_scheduler_retained_automation(
                    engine,
                    &project_id,
                    project_path,
                    AgentTaskKind::SkillWriter,
                    &effect_run_control,
                    *effect,
                    retained_run,
                    |run| (run.ledger_record, run.committed_receipt),
                )
                .await
                {
                    first_error.get_or_insert(error);
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
    use std::time::Duration;

    use tracedecay_agent_hosts::automation::AutomationRunControl;
    use tracedecay_application::{CancellationSignal, Deadline};
    use tracedecay_domain::UtcMicros;

    use super::{scheduler_effect_run_control, synchronize_scheduler_effect_control};

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

    #[tokio::test(start_paused = true)]
    async fn effect_control_bridge_cancels_a_blocked_settlement_on_scheduler_stop() {
        let stopped = Arc::new(AtomicBool::new(false));
        let observed = Arc::clone(&stopped);
        let scheduler = AutomationRunControl::from_interrupted(Arc::new(move || {
            observed.load(Ordering::Acquire)
        }));
        let cancellation = CancellationSignal::active("cancel.scheduler-effect-bridge")
            .expect("valid cancellation signal");
        let effect_cancellation = cancellation.clone();
        let control = scheduler_effect_run_control(
            &scheduler,
            cancellation,
            Deadline::new(UtcMicros(i64::MAX)).expect("valid scheduler deadline"),
        );
        // The one production evaluation that happens before settlement starts.
        synchronize_scheduler_effect_control(&control);
        assert!(!effect_cancellation.is_cancelled());

        stopped.store(true, Ordering::Release);
        // Nothing evaluates the run control again while settlement blocks, so
        // only the bridge can flip the signal the retry loop polls.
        tokio::time::sleep(Duration::from_secs(2)).await;

        assert!(
            effect_cancellation.is_cancelled(),
            "scheduler retirement must reach an in-flight settlement within one retry iteration"
        );
        drop(control);
    }

    #[tokio::test(start_paused = true)]
    async fn effect_control_bridge_stops_when_the_run_control_is_dropped() {
        let stopped = Arc::new(AtomicBool::new(false));
        let observed = Arc::clone(&stopped);
        let scheduler = AutomationRunControl::from_interrupted(Arc::new(move || {
            observed.load(Ordering::Acquire)
        }));
        let cancellation = CancellationSignal::active("cancel.scheduler-effect-bridge-drop")
            .expect("valid cancellation signal");
        let effect_cancellation = cancellation.clone();
        let control = scheduler_effect_run_control(
            &scheduler,
            cancellation,
            Deadline::new(UtcMicros(i64::MAX)).expect("valid scheduler deadline"),
        );

        drop(control);
        stopped.store(true, Ordering::Release);
        tokio::time::sleep(Duration::from_mins(1)).await;

        assert!(
            !effect_cancellation.is_cancelled(),
            "a settled run's bridge must not outlive its effect run control"
        );
    }
}
