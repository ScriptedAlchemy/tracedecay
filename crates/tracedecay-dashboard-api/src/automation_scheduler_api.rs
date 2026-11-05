//! Dashboard endpoints for automation scheduler state and coarse controls.

use axum::Json;
use axum::extract::State;
use schemars::JsonSchema;
use serde::Serialize;

use super::DashboardState;
use super::automation_config_api::effective_automation_config;
use super::util::{JsonError, internal_error};
use tracedecay_automation_runtime::automation::backend::{AgentTaskKind, task_key};
use tracedecay_automation_runtime::automation::config::{
    AdmittedAutomationConfig, AutomationSettingsV1,
};
use tracedecay_automation_runtime::automation::run_ledger::{
    AutomationRunLedgerRecord, AutomationRunLedgerTaskSummary, load_run_ledger_task_summary,
};
use tracedecay_automation_runtime::automation::scheduler::{
    AutomationSchedulerControl, SessionActivity, backend_executable_unconfigured,
    configured_backend_schedule_decision, load_scheduler_control, load_session_activity,
    save_scheduler_control, scheduler_control_path,
};
use tracedecay_contracts::retained_surfaces::AutomationSkipReasonV1;
use tracedecay_domain::configuration::LcmSummarizerExecutableV1;
use tracedecay_domain::errors::{StoreResetRequiredV1, TraceDecayError};
use tracedecay_runtime_core::tracedecay::current_timestamp;

type ApiResult = std::result::Result<Json<AutomationSchedulerStatusV1>, JsonError>;

/// The scheduler reading served by `status`, `pause`, and `resume`.
///
/// Automation is autonomous: the status has no pending-review counters. The
/// last run record carries the validation, repair, application, quarantine,
/// and deployment receipts that describe what actually happened.
#[derive(Clone, Debug, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct AutomationSchedulerStatusV1 {
    pub status: AutomationSchedulerAvailabilityV1,
    pub paused: bool,
    pub enabled: bool,
    pub scheduler_tick_secs: u64,
    pub now: i64,
    pub last_session_activity: Option<i64>,
    pub configuration_revision_id: String,
    pub control_path: String,
    pub tasks: Vec<AutomationTaskStatusV1>,
    pub reset_required_stores: Vec<StoreResetRequiredV1>,
}

/// Closed availability vocabulary for the scheduler status reading.
///
/// Wire tokens stay the historical labels. A new state is a compile error in
/// [`scheduler_status_label`] until this enum gains a variant.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub(super) enum AutomationSchedulerAvailabilityV1 {
    Paused,
    AutomationDisabled,
    DelegatedHost,
    BackendDisabled,
    BackendUnconfigured,
    BlockedResetRequired,
    Configured,
}

#[derive(Clone, Debug, Serialize, JsonSchema)]
pub(super) struct AutomationTaskStatusV1 {
    pub task: String,
    pub due: bool,
    pub skip_reason: Option<AutomationSkipReasonV1>,
    /// The most recent scheduler-triggered ledger record. Its run artifacts
    /// remain the canonical detailed receipt surface.
    pub last_scheduler_run: Option<AutomationRunLedgerRecord>,
}

#[tracing::instrument(name = "dashboard_api.scheduler.status", level = "trace", skip_all)]
pub async fn status(State(state): State<DashboardState>) -> ApiResult {
    scheduler_status_payload(&state).await
}

#[tracing::instrument(name = "dashboard_api.scheduler.pause", level = "trace", skip_all)]
pub async fn pause(State(state): State<DashboardState>) -> ApiResult {
    set_scheduler_paused(&state, true).await?;
    scheduler_status_payload(&state).await
}

#[tracing::instrument(name = "dashboard_api.scheduler.resume", level = "trace", skip_all)]
pub async fn resume(State(state): State<DashboardState>) -> ApiResult {
    set_scheduler_paused(&state, false).await?;
    scheduler_status_payload(&state).await
}

async fn set_scheduler_paused(
    state: &DashboardState,
    paused: bool,
) -> std::result::Result<(), JsonError> {
    effective_automation_config(state).map_err(|err| internal_error(&err))?;
    save_scheduler_control(
        &state.dashboard_root,
        &AutomationSchedulerControl { paused },
    )
    .await
    .map_err(|err| internal_error(&err))
}

async fn scheduler_status_payload(state: &DashboardState) -> ApiResult {
    let (configuration_revision_id, effective, codex) =
        effective_automation_config(state).map_err(|err| internal_error(&err))?;
    let control = load_scheduler_control(&state.dashboard_root)
        .await
        .map_err(|err| internal_error(&err))?;
    let mut reset_required_stores = state
        .scheduler_reset_census_reader
        .as_ref()
        .map(|read| read())
        .unwrap_or_default();
    let memory_summary = scheduler_summary_or_refusal(
        load_run_ledger_task_summary(
            &state.dashboard_root,
            AgentTaskKind::MemoryCurator,
            task_key(AgentTaskKind::MemoryCurator),
        )
        .await,
        &mut reset_required_stores,
    )?;
    let session_summary = scheduler_summary_or_refusal(
        load_run_ledger_task_summary(
            &state.dashboard_root,
            AgentTaskKind::SessionReflector,
            task_key(AgentTaskKind::SessionReflector),
        )
        .await,
        &mut reset_required_stores,
    )?;
    let skill_summary = scheduler_summary_or_refusal(
        load_run_ledger_task_summary(
            &state.dashboard_root,
            AgentTaskKind::SkillWriter,
            task_key(AgentTaskKind::SkillWriter),
        )
        .await,
        &mut reset_required_stores,
    )?;
    let now = current_timestamp();
    let activity = match state.lcm_db.as_deref() {
        Some(sessions_db) => load_session_activity(sessions_db).await,
        None => SessionActivity::none(),
    };
    let blocked = !reset_required_stores.is_empty();
    Ok(Json(AutomationSchedulerStatusV1 {
        status: if blocked {
            AutomationSchedulerAvailabilityV1::BlockedResetRequired
        } else {
            scheduler_status_label(&effective, &codex, control.paused)
        },
        paused: control.paused,
        enabled: effective.enabled,
        scheduler_tick_secs: effective.scheduler_tick_secs,
        now,
        last_session_activity: activity.last_activity_secs,
        configuration_revision_id: configuration_revision_id.as_str().to_owned(),
        control_path: scheduler_control_path(&state.dashboard_root)
            .display()
            .to_string(),
        // A refused ledger has no authoritative schedule or last-run reading.
        tasks: if let (false, Some(memory_summary), Some(session_summary), Some(skill_summary)) =
            (blocked, memory_summary, session_summary, skill_summary)
        {
            vec![
                task_status(
                    &effective,
                    &codex,
                    control.paused,
                    &memory_summary,
                    activity,
                    now,
                    AgentTaskKind::MemoryCurator,
                ),
                task_status(
                    &effective,
                    &codex,
                    control.paused,
                    &session_summary,
                    activity,
                    now,
                    AgentTaskKind::SessionReflector,
                ),
                task_status(
                    &effective,
                    &codex,
                    control.paused,
                    &skill_summary,
                    activity,
                    now,
                    AgentTaskKind::SkillWriter,
                ),
            ]
        } else {
            Vec::new()
        },
        reset_required_stores,
    }))
}

fn scheduler_summary_or_refusal(
    result: Result<AutomationRunLedgerTaskSummary, TraceDecayError>,
    refusals: &mut Vec<StoreResetRequiredV1>,
) -> Result<Option<AutomationRunLedgerTaskSummary>, JsonError> {
    match result {
        Ok(summary) => Ok(Some(summary)),
        Err(error) => {
            let Some(refusal) = error.store_reset_required(
                "automation",
                tracedecay_domain::errors::PROFILE_RESET_COMMAND,
            ) else {
                return Err(internal_error(&error));
            };
            if !refusals.contains(&refusal) {
                refusals.push(refusal);
            }
            Ok(None)
        }
    }
}

fn task_status(
    config: &AdmittedAutomationConfig,
    codex: &LcmSummarizerExecutableV1,
    paused: bool,
    summary: &AutomationRunLedgerTaskSummary,
    activity: SessionActivity,
    now: i64,
    task: AgentTaskKind,
) -> AutomationTaskStatusV1 {
    let decision = if paused {
        tracedecay_automation_runtime::automation::scheduler::AutomationScheduleDecision::skipped(
            AutomationSkipReasonV1::SchedulerPaused,
        )
    } else {
        configured_backend_schedule_decision(config, codex, task, summary.records(), activity, now)
    };
    AutomationTaskStatusV1 {
        task: task_key(task).to_string(),
        due: decision.is_due(),
        skip_reason: decision.skip_reason(),
        last_scheduler_run: summary.latest_scheduler_activity().cloned(),
    }
}

fn scheduler_status_label(
    config: &AutomationSettingsV1,
    codex: &LcmSummarizerExecutableV1,
    paused: bool,
) -> AutomationSchedulerAvailabilityV1 {
    if paused {
        return AutomationSchedulerAvailabilityV1::Paused;
    }
    if !config.enabled {
        return AutomationSchedulerAvailabilityV1::AutomationDisabled;
    }
    if config.host_mode
        == tracedecay_automation_runtime::automation::config::AutomationHostModeV1::DelegatedHost
    {
        return AutomationSchedulerAvailabilityV1::DelegatedHost;
    }
    if config.backend
        == tracedecay_automation_runtime::automation::config::AutomationBackendV1::Disabled
    {
        return AutomationSchedulerAvailabilityV1::BackendDisabled;
    }
    if backend_executable_unconfigured(config, codex) {
        return AutomationSchedulerAvailabilityV1::BackendUnconfigured;
    }
    AutomationSchedulerAvailabilityV1::Configured
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use tracedecay_automation_runtime::automation::backend::AgentTaskFailureClass;
    use tracedecay_automation_runtime::automation::config::{
        AutomationBackendV1, AutomationTaskSettingsV1,
    };
    use tracedecay_automation_runtime::automation::run_ledger::{
        AutomationRunLedgerRecord, AutomationRunStatus, AutomationTrigger, run_ledger_path,
    };

    #[tokio::test]
    async fn refused_ledger_status_keeps_the_reset_census_and_recovers_without_fake_history() {
        let temp = tempfile::TempDir::new().unwrap();
        // A complete retired row: the ledger refuses it as a reset, which is a
        // different state from a malformed row (a config error).
        std::fs::write(
            run_ledger_path(temp.path()),
            "{\"schema_version\":1,\"run_id\":\"retired\",\"trigger\":\"scheduler\",\
             \"task\":\"memory_curator\",\"task_key\":\"memory_curator\",\
             \"backend\":\"codex_app_server\",\"status\":\"succeeded\",\
             \"accepted_count\":0,\"rejected_count\":0,\"started_at\":\"1970-01-01T00:00:01Z\",\
             \"completed_at\":\"1970-01-01T00:00:02Z\"}\n",
        )
        .unwrap();
        let mut census = Vec::new();
        for task in [
            AgentTaskKind::MemoryCurator,
            AgentTaskKind::SessionReflector,
        ] {
            scheduler_summary_or_refusal(
                load_run_ledger_task_summary(temp.path(), task, task_key(task)).await,
                &mut census,
            )
            .unwrap();
        }
        assert_eq!(census.len(), 1);
        assert!(!census[0].reason.is_empty());
        assert_eq!(census[0].store, "automation");
        std::fs::remove_file(run_ledger_path(temp.path())).unwrap();
        let mut recovered_census = Vec::new();
        let summary = scheduler_summary_or_refusal(
            load_run_ledger_task_summary(
                temp.path(),
                AgentTaskKind::MemoryCurator,
                "memory_curator",
            )
            .await,
            &mut recovered_census,
        )
        .unwrap();
        assert!(recovered_census.is_empty());
        assert!(summary.unwrap().records().is_empty());
        let isolated = tempfile::TempDir::new().unwrap();
        assert!(
            load_run_ledger_task_summary(
                isolated.path(),
                AgentTaskKind::MemoryCurator,
                "memory_curator"
            )
            .await
            .unwrap()
            .records()
            .is_empty()
        );
    }

    fn record(
        run_id: &str,
        task: AgentTaskKind,
        status: AutomationRunStatus,
        completed_at: i64,
        completed_at_micros: i64,
    ) -> AutomationRunLedgerRecord {
        serde_json::from_value(serde_json::json!({
            "schema_version": 2,
            "run_id": run_id,
            "trigger": AutomationTrigger::Scheduler,
            "task": task,
            "task_key": task_key(task),
            "backend": "codex_app_server",
            "status": status,
            "accepted_count": 0,
            "rejected_count": 0,
            "started_at": completed_at.to_string(),
            "completed_at": completed_at.to_string(),
            "completed_at_micros": completed_at_micros,
        }))
        .unwrap()
    }

    #[test]
    fn status_reports_an_unconfigured_backend_until_an_executable_is_bound() {
        let mut config = AutomationSettingsV1 {
            enabled: true,
            backend: AutomationBackendV1::CodexAppServer,
            ..AutomationSettingsV1::default()
        };
        config.tasks.memory_curator = AutomationTaskSettingsV1 {
            enabled: true,
            schedule: Some("every:1m".to_string()),
            ..AutomationTaskSettingsV1::default()
        };
        let admitted = tracedecay_automation::config::admit_config(&config).unwrap();
        let empty = AutomationRunLedgerTaskSummary::default();
        let read = |codex: &LcmSummarizerExecutableV1| {
            task_status(
                &admitted,
                codex,
                false,
                &empty,
                SessionActivity::none(),
                150,
                AgentTaskKind::MemoryCurator,
            )
        };

        let unconfigured = LcmSummarizerExecutableV1::Unconfigured;
        let blocked = read(&unconfigured);
        assert!(!blocked.due);
        assert_eq!(
            blocked.skip_reason,
            Some(AutomationSkipReasonV1::BackendUnconfigured)
        );
        assert_eq!(
            scheduler_status_label(&config, &unconfigured, false),
            AutomationSchedulerAvailabilityV1::BackendUnconfigured
        );
        // An operator's pause stays what it is.
        assert_eq!(
            scheduler_status_label(&config, &unconfigured, true),
            AutomationSchedulerAvailabilityV1::Paused
        );

        let executable = tempfile::NamedTempFile::new().unwrap();
        let configured =
            LcmSummarizerExecutableV1::configured(executable.path().to_path_buf()).unwrap();
        assert!(read(&configured).due);
        assert_eq!(
            scheduler_status_label(&config, &configured, false),
            AutomationSchedulerAvailabilityV1::Configured
        );
    }

    #[tokio::test]
    async fn status_uses_full_canonical_summary_for_decision_and_last_scheduler_run() {
        let temp = tempfile::TempDir::new().unwrap();
        let mut older_success = record(
            "a-success",
            AgentTaskKind::MemoryCurator,
            AutomationRunStatus::Succeeded,
            100,
            100_000_100,
        );
        let mut later_failure = record(
            "z-failure",
            AgentTaskKind::MemoryCurator,
            AutomationRunStatus::Failed,
            100,
            100_000_900,
        );
        later_failure.error = Some("the request is permanently invalid".to_string());
        later_failure.error_classification = Some(AgentTaskFailureClass::Permanent);
        later_failure.error_retryable = Some(false);
        older_success.model = Some("older".to_string());
        let mut rows = vec![older_success, later_failure];
        rows.extend((0..201).map(|index| {
            record(
                &format!("unrelated-{index}"),
                AgentTaskKind::SkillWriter,
                AutomationRunStatus::Succeeded,
                101 + index,
                (101 + index) * 1_000_000,
            )
        }));
        let body = rows
            .iter()
            .map(serde_json::to_string)
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
            .join("\n")
            + "\n";
        std::fs::write(run_ledger_path(temp.path()), body).unwrap();

        let summary = load_run_ledger_task_summary(
            temp.path(),
            AgentTaskKind::MemoryCurator,
            task_key(AgentTaskKind::MemoryCurator),
        )
        .await
        .unwrap();
        let mut config = AutomationSettingsV1 {
            enabled: true,
            backend: AutomationBackendV1::CodexAppServer,
            ..AutomationSettingsV1::default()
        };
        config.tasks.memory_curator = AutomationTaskSettingsV1 {
            enabled: true,
            schedule: Some("daily".to_string()),
            ..AutomationTaskSettingsV1::default()
        };

        let status = task_status(
            &tracedecay_automation::config::admit_config(&config).unwrap(),
            &LcmSummarizerExecutableV1::Unconfigured,
            false,
            &summary,
            SessionActivity::none(),
            150,
            AgentTaskKind::MemoryCurator,
        );

        assert!(!status.due);
        assert_eq!(
            status.skip_reason,
            Some(AutomationSkipReasonV1::SchedulerNonRetryableFailure)
        );
        assert_eq!(status.last_scheduler_run.unwrap().run_id, "z-failure");
    }
}
