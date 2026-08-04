//! Dashboard CRUD + run endpoints for user-defined scheduled jobs
//! (Hermes cron parity, audit R9). Routes live beside the automation config
//! endpoints:
//!
//! - `GET/POST /api/automation/jobs`
//! - `GET/PATCH/DELETE /api/automation/jobs/{id}`
//! - `POST /api/automation/jobs/{id}/run`

use std::collections::BTreeMap;

use axum::Json;
use axum::extract::{Path as AxumPath, State};
use axum::http::StatusCode;
use serde::Deserialize;
use serde_json::{Value, json};

use super::DashboardState;
use super::util::{JsonError, http_detail};
use crate::automation::backend::CodexAppServerBackend;
use crate::automation::config::{AutomationConfig, effective_config, load_project_config};
use crate::automation::jobs::{
    AutomationJob, JobDelivery, UserJobRunOptions, find_job, job_task_key, load_jobs,
    run_user_job_with_backend, save_jobs, validate_job, validate_job_id,
};
use crate::automation::run_ledger::AutomationTrigger;
use crate::tracedecay::current_timestamp;

type ApiResult = std::result::Result<Json<Value>, JsonError>;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateJobBody {
    #[serde(default)]
    id: Option<String>,
    name: String,
    prompt: String,
    #[serde(default)]
    schedule: Option<String>,
    #[serde(default = "default_true")]
    enabled: bool,
    #[serde(default)]
    interval_secs: Option<u64>,
    #[serde(default)]
    cooldown_secs: Option<u64>,
    #[serde(default)]
    skill_ids: Vec<String>,
    #[serde(default)]
    pre_run_command: Option<String>,
    #[serde(default)]
    delivery: Option<JobDelivery>,
}

#[allow(clippy::option_option)]
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PatchJobBody {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    prompt: Option<String>,
    #[serde(default, deserialize_with = "clearable")]
    schedule: Option<Option<String>>,
    #[serde(default)]
    enabled: Option<bool>,
    #[serde(default, deserialize_with = "clearable")]
    interval_secs: Option<Option<u64>>,
    #[serde(default, deserialize_with = "clearable")]
    cooldown_secs: Option<Option<u64>>,
    #[serde(default)]
    skill_ids: Option<Vec<String>>,
    #[serde(default, deserialize_with = "clearable")]
    pre_run_command: Option<Option<String>>,
    #[serde(default)]
    delivery: Option<JobDelivery>,
}

#[allow(clippy::option_option)]
fn clearable<'de, D, T>(deserializer: D) -> std::result::Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer).map(Some)
}

fn default_true() -> bool {
    true
}

pub async fn list(State(state): State<DashboardState>) -> ApiResult {
    let jobs = load_jobs(&state.dashboard_root)
        .await
        .map_err(|err| internal_error(&err))?;
    Ok(Json(json!({ "jobs": jobs, "count": jobs.len() })))
}

pub async fn create(State(state): State<DashboardState>, Json(body): Json<Value>) -> ApiResult {
    let body = serde_json::from_value::<CreateJobBody>(body)
        .map_err(|err| bad_request(&format!("invalid job: {err}")))?;
    let now = current_timestamp();
    let job = AutomationJob {
        id: match body.id {
            Some(id) => id,
            None => generated_job_id(&body.name),
        },
        name: body.name,
        prompt: body.prompt,
        schedule: body.schedule,
        enabled: body.enabled,
        interval_secs: body.interval_secs,
        cooldown_secs: body.cooldown_secs,
        skill_ids: body.skill_ids,
        pre_run_command: body.pre_run_command,
        delivery: body.delivery.unwrap_or_default(),
        created_at: now,
        updated_at: now,
        extra: BTreeMap::new(),
    };
    validate_job(&job).map_err(|err| bad_request(&err.to_string()))?;
    let mut jobs = load_jobs(&state.dashboard_root)
        .await
        .map_err(|err| internal_error(&err))?;
    if jobs.iter().any(|existing| existing.id == job.id) {
        return Err(bad_request(&format!("job '{}' already exists", job.id)));
    }
    jobs.push(job.clone());
    save_jobs(&state.dashboard_root, &jobs)
        .await
        .map_err(|err| internal_error(&err))?;
    state.reconcile_automation_scheduler();
    Ok(Json(json!({ "job": job })))
}

pub async fn view(
    State(state): State<DashboardState>,
    AxumPath(job_id): AxumPath<String>,
) -> ApiResult {
    let job = load_job_or_404(&state, &job_id).await?;
    Ok(Json(json!({ "job": job })))
}

pub async fn update(
    State(state): State<DashboardState>,
    AxumPath(job_id): AxumPath<String>,
    Json(body): Json<Value>,
) -> ApiResult {
    let patch = serde_json::from_value::<PatchJobBody>(body)
        .map_err(|err| bad_request(&format!("invalid job patch: {err}")))?;
    let mut jobs = load_jobs(&state.dashboard_root)
        .await
        .map_err(|err| internal_error(&err))?;
    let Some(job) = jobs.iter_mut().find(|job| job.id == job_id) else {
        return Err(not_found(&job_id));
    };
    if let Some(name) = patch.name {
        job.name = name;
    }
    if let Some(prompt) = patch.prompt {
        job.prompt = prompt;
    }
    if let Some(schedule) = patch.schedule {
        job.schedule = schedule;
    }
    if let Some(enabled) = patch.enabled {
        job.enabled = enabled;
    }
    if let Some(interval_secs) = patch.interval_secs {
        job.interval_secs = interval_secs;
    }
    if let Some(cooldown_secs) = patch.cooldown_secs {
        job.cooldown_secs = cooldown_secs;
    }
    if let Some(skill_ids) = patch.skill_ids {
        job.skill_ids = skill_ids;
    }
    if let Some(pre_run_command) = patch.pre_run_command {
        job.pre_run_command = pre_run_command;
    }
    if let Some(delivery) = patch.delivery {
        job.delivery = delivery;
    }
    job.updated_at = current_timestamp();
    validate_job(job).map_err(|err| bad_request(&err.to_string()))?;
    let updated = job.clone();
    save_jobs(&state.dashboard_root, &jobs)
        .await
        .map_err(|err| internal_error(&err))?;
    state.reconcile_automation_scheduler();
    Ok(Json(json!({ "job": updated })))
}

pub async fn delete(
    State(state): State<DashboardState>,
    AxumPath(job_id): AxumPath<String>,
) -> ApiResult {
    let mut jobs = load_jobs(&state.dashboard_root)
        .await
        .map_err(|err| internal_error(&err))?;
    let before = jobs.len();
    jobs.retain(|job| job.id != job_id);
    if jobs.len() == before {
        return Err(not_found(&job_id));
    }
    save_jobs(&state.dashboard_root, &jobs)
        .await
        .map_err(|err| internal_error(&err))?;
    state.reconcile_automation_scheduler();
    Ok(Json(json!({ "deleted": job_id })))
}

pub async fn run(
    State(state): State<DashboardState>,
    AxumPath(job_id): AxumPath<String>,
) -> std::result::Result<(StatusCode, Json<Value>), JsonError> {
    let job = load_job_or_404(&state, &job_id).await?;
    let config = load_effective_config(&state).await?;
    let run_id = format!("dashboard_user_job_{}_{}", job.id, micros_now());
    let payload = json!({
        "run_id": run_id,
        "job_id": job.id,
        "task": job_task_key(&job.id),
        "status": "accepted",
    });
    let dashboard_root = state.dashboard_root.clone();
    let project_root = state.project_root.clone();
    tokio::spawn(async move {
        let backend = CodexAppServerBackend::from_automation_config(&config);
        let options = UserJobRunOptions {
            trigger: AutomationTrigger::Dashboard,
            run_id: Some(run_id),
            profile_root: None,
            project_root: Some(project_root),
        };
        if let Err(err) =
            run_user_job_with_backend(&dashboard_root, &config, &backend, &job, options).await
        {
            eprintln!("[tracedecay] dashboard user job '{}' failed: {err}", job.id);
        }
    });
    Ok((StatusCode::ACCEPTED, Json(payload)))
}

async fn load_job_or_404(
    state: &DashboardState,
    job_id: &str,
) -> std::result::Result<AutomationJob, JsonError> {
    validate_job_id(job_id).map_err(|err| bad_request(&err.to_string()))?;
    match find_job(&state.dashboard_root, job_id).await {
        Ok(Some(job)) => Ok(job),
        Ok(None) => Err(not_found(job_id)),
        Err(err) => Err(internal_error(&err)),
    }
}

async fn load_effective_config(
    state: &DashboardState,
) -> std::result::Result<AutomationConfig, JsonError> {
    let global = crate::user_config::UserConfig::load().automation;
    let project = load_project_config(&state.dashboard_root)
        .await
        .map_err(|err| internal_error(&err))?;
    effective_config(&global, project.as_ref()).map_err(|err| internal_error(&err))
}

fn micros_now() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_micros())
        .unwrap_or_default()
}

fn generated_job_id(name: &str) -> String {
    let slug: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .split('-')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    let slug = if slug.is_empty() {
        "job".to_string()
    } else {
        slug.chars().take(40).collect()
    };
    format!("{slug}-{}", micros_now() % 1_000_000)
}

fn bad_request(message: &str) -> JsonError {
    (StatusCode::BAD_REQUEST, Json(http_detail(message)))
}

fn not_found(job_id: &str) -> JsonError {
    (
        StatusCode::NOT_FOUND,
        Json(http_detail(&format!("automation job '{job_id}' not found"))),
    )
}

fn internal_error(err: &impl ToString) -> JsonError {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(http_detail(&err.to_string())),
    )
}
