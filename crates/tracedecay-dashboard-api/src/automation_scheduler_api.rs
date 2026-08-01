//! Dashboard endpoints for automation scheduler state and coarse controls.

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use schemars::JsonSchema;
use serde::Serialize;
use serde_json::Value;

use super::DashboardState;
use super::util::{JsonError, http_detail};
use crate::user_config::UserConfig;
use tracedecay_agent_hosts::automation::backend::{AgentTaskKind, task_key};
use tracedecay_agent_hosts::automation::config::{
    AutomationConfig, effective_config, load_project_config,
};
use tracedecay_agent_hosts::automation::run_ledger::{AutomationRunLedgerRecord, load_run_records};
use tracedecay_agent_hosts::automation::scheduler::{
    AutomationSchedulerControl, SessionActivity, load_scheduler_control, load_session_activity,
    save_scheduler_control, schedule_decision, scheduler_control_path,
};
use tracedecay_agent_hosts::automation::staged_notice::{
    AutomationPendingCounts, PendingReviewCount, count_pending_fact_proposals,
    count_pending_managed_skills,
};
use tracedecay_runtime_core::tracedecay::current_timestamp;

type ApiResult = std::result::Result<Json<AutomationSchedulerStatusV1>, JsonError>;

/// The scheduler reading served by all three of `status`, `pause`, and
/// `resume`.
///
/// The two control routes return this same payload rather than an
/// acknowledgement because the pause flag is the thing a caller is actually
/// asking about, and a bare `{"ok":true}` would leave the dashboard to assume
/// the new state instead of observing it. Returning the re-read means a client
/// never has to optimistically flip a control it has not seen take effect.
///
/// Typed here, rather than assembled with `json!`, so the shape reaches
/// `contract_schema.rs` and the dashboard consumes a generated contract. The
/// field set is deliberately identical to the `json!` literal it replaces: this
/// is the same wire, described, not a new one.
#[derive(Clone, Debug, Serialize, JsonSchema)]
pub(super) struct AutomationSchedulerStatusV1 {
    /// Coarse label: `paused`, `automation_disabled`, `delegated_host`,
    /// `backend_disabled`, or `configured`.
    pub status: String,
    pub paused: bool,
    /// Null — never zero — when the queue could not be read. `pending_review`
    /// carries which reading each figure actually is; these two flat fields
    /// remain for clients predating that block.
    pub pending_fact_proposals: Option<u64>,
    pub pending_skills: Option<u64>,
    pub pending_review: AutomationPendingReviewV1,
    pub enabled: bool,
    pub scheduler_tick_secs: u64,
    pub now: i64,
    pub last_session_activity: Option<i64>,
    pub project_config_path: String,
    pub control_path: String,
    pub tasks: Vec<AutomationTaskStatusV1>,
}

#[derive(Clone, Debug, Serialize, JsonSchema)]
pub(super) struct AutomationPendingReviewV1 {
    pub fact_proposals: AutomationPendingReviewCountV1,
    pub skills: AutomationPendingReviewCountV1,
}

/// One human-review queue: a count, or the reason there is no count.
///
/// Kept as a tagged reading rather than a bare number because these two queues
/// are the entire human-approval step of the automation pipeline. A queue whose
/// authority is unmounted must not report `0`, which reads as "checked, nothing
/// waiting" — the precise false zero this shape exists to prevent.
#[derive(Clone, Debug, Serialize, JsonSchema)]
#[serde(tag = "state", rename_all = "snake_case")]
pub(super) enum AutomationPendingReviewCountV1 {
    Measured { count: u64, reason: Option<String> },
    Unreadable { count: Option<u64>, reason: String },
}

#[derive(Clone, Debug, Serialize, JsonSchema)]
pub(super) struct AutomationTaskStatusV1 {
    pub task: String,
    pub due: bool,
    pub skip_reason: Option<String>,
    /// The most recent scheduler-triggered ledger record, passed through
    /// verbatim. Left untyped because `AutomationRunLedgerRecord` is an
    /// internal automation-crate record rather than a dashboard wire type, and
    /// projecting it here would change this payload's shape; typing it belongs
    /// with the run-artifact routes, which are the surfaces that read it.
    pub last_scheduler_run: Option<Value>,
}

pub async fn status(State(state): State<DashboardState>) -> ApiResult {
    scheduler_status_payload(&state).await
}

pub async fn pause(State(state): State<DashboardState>) -> ApiResult {
    set_scheduler_paused(&state, true).await?;
    scheduler_status_payload(&state).await
}

pub async fn resume(State(state): State<DashboardState>) -> ApiResult {
    set_scheduler_paused(&state, false).await?;
    scheduler_status_payload(&state).await
}

async fn set_scheduler_paused(
    state: &DashboardState,
    paused: bool,
) -> std::result::Result<(), JsonError> {
    save_scheduler_control(
        &state.dashboard_root,
        &AutomationSchedulerControl { paused },
    )
    .await
    .map_err(|err| internal_error(&err))
}

async fn scheduler_status_payload(state: &DashboardState) -> ApiResult {
    let global = UserConfig::load().automation;
    let project = load_project_config(&state.dashboard_root)
        .await
        .map_err(|err| internal_error(&err))?;
    let effective =
        effective_config(&global, project.as_ref()).map_err(|err| internal_error(&err))?;
    let control = load_scheduler_control(&state.dashboard_root)
        .await
        .map_err(|err| internal_error(&err))?;
    let records = load_run_records(&state.dashboard_root, 200)
        .await
        .map_err(|err| internal_error(&err))?;
    let pending = pending_review_counts(state).await;
    let now = current_timestamp();
    let activity = match state.lcm_db.as_deref() {
        Some(sessions_db) => load_session_activity(sessions_db).await,
        None => SessionActivity::none(),
    };
    Ok(Json(AutomationSchedulerStatusV1 {
        status: scheduler_status_label(&effective, control.paused).to_string(),
        paused: control.paused,
        pending_fact_proposals: pending.fact_proposals.count().map(|count| count as u64),
        pending_skills: pending.skills.count().map(|count| count as u64),
        pending_review: AutomationPendingReviewV1 {
            fact_proposals: pending_review_reading(&pending.fact_proposals),
            skills: pending_review_reading(&pending.skills),
        },
        enabled: effective.enabled,
        scheduler_tick_secs: effective.scheduler_tick_secs,
        now,
        last_session_activity: activity.last_activity_secs,
        project_config_path: tracedecay_agent_hosts::automation::config::project_config_path(
            &state.dashboard_root,
        )
        .display()
        .to_string(),
        control_path: scheduler_control_path(&state.dashboard_root)
            .display()
            .to_string(),
        tasks: vec![
            task_status(
                &effective,
                control.paused,
                &records,
                activity,
                now,
                AgentTaskKind::MemoryCurator,
            ),
            task_status(
                &effective,
                control.paused,
                &records,
                activity,
                now,
                AgentTaskKind::SessionReflector,
            ),
            task_status(
                &effective,
                control.paused,
                &records,
                activity,
                now,
                AgentTaskKind::SkillWriter,
            ),
        ],
    }))
}

/// Reads the two human-review queues, each from its own authority.
///
/// Both reads used to be gated behind one `match` whose failure arm produced
/// zeroes, so an unresolvable profile root or an unmounted fact authority
/// served `pending_fact_proposals: 0, pending_skills: 0` under HTTP 200 — a
/// report that nothing awaits human approval. They are independent reads now,
/// and a queue that cannot be read says so.
async fn pending_review_counts(state: &DashboardState) -> AutomationPendingCounts {
    let fact_proposals = match crate::tracedecay::facts::memory_application_for_db(
        state.memory_owner.clone(),
        state.mem_db.as_ref(),
    ) {
        Ok(memory) => count_pending_fact_proposals(&memory).await,
        Err(error) => PendingReviewCount::unreadable(format!(
            "the project fact authority is not available: {error}"
        )),
    };
    let skills = match tracedecay_runtime_core::storage::default_profile_root() {
        Ok(profile_root) => count_pending_managed_skills(&profile_root).await,
        Err(error) => PendingReviewCount::unreadable(format!(
            "the user profile root could not be resolved: {error}"
        )),
    };
    AutomationPendingCounts {
        fact_proposals,
        skills,
    }
}

/// The per-queue reading, on the dashboard's evidence vocabulary: `measured`
/// carries a real count, `unreadable` carries the reason and no count.
fn pending_review_reading(count: &PendingReviewCount) -> AutomationPendingReviewCountV1 {
    match count {
        PendingReviewCount::Counted(count) => AutomationPendingReviewCountV1::Measured {
            count: *count as u64,
            reason: None,
        },
        PendingReviewCount::Unreadable(reason) => AutomationPendingReviewCountV1::Unreadable {
            count: None,
            reason: reason.clone(),
        },
    }
}

fn task_status(
    config: &AutomationConfig,
    paused: bool,
    records: &[AutomationRunLedgerRecord],
    activity: SessionActivity,
    now: i64,
    task: AgentTaskKind,
) -> AutomationTaskStatusV1 {
    let decision = if paused {
        tracedecay_agent_hosts::automation::scheduler::AutomationScheduleDecision::skipped(
            "scheduler_paused",
        )
    } else {
        schedule_decision(config, task, records, activity, now)
    };
    let latest_scheduler = records
        .iter()
        .filter(|record| {
            record.task == task
                && record.trigger
                    == tracedecay_agent_hosts::automation::run_ledger::AutomationTrigger::Scheduler
        })
        .max_by_key(|record| record.completed_at.parse::<i64>().ok().unwrap_or(0));
    AutomationTaskStatusV1 {
        task: task_key(task).to_string(),
        due: decision.is_due(),
        skip_reason: decision.skip_reason().map(str::to_string),
        last_scheduler_run: latest_scheduler.and_then(|record| serde_json::to_value(record).ok()),
    }
}

fn scheduler_status_label(config: &AutomationConfig, paused: bool) -> &'static str {
    if paused {
        return "paused";
    }
    if !config.enabled {
        return "automation_disabled";
    }
    if config.host_mode
        == tracedecay_agent_hosts::automation::config::AutomationHostMode::DelegatedHost
    {
        return "delegated_host";
    }
    if config.backend == tracedecay_agent_hosts::automation::config::AutomationBackend::Disabled {
        return "backend_disabled";
    }
    "configured"
}

fn internal_error(err: &impl ToString) -> JsonError {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(http_detail(&err.to_string())),
    )
}
