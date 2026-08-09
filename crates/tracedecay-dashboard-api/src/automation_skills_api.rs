//! Dashboard endpoints for profile-owned managed automation skills.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use serde::Deserialize;
use serde_json::{Value, json};

use super::DashboardState;
use super::util::{JsonError, http_detail, internal_error};
use tracedecay_agent_hosts::automation::managed_skills::{
    ManagedSkill, ManagedSkillDraft, ManagedSkillProvenance, ManagedSkillSource,
    ManagedSkillUpdate, ManagedSupportFile, SkillInstallTarget, apply_managed_skill_update,
    archive_managed_skill, create_managed_skill, disable_managed_skill, list_managed_skills,
    load_managed_skill, managed_skill_dir, managed_skill_root, restore_managed_skill,
    set_managed_skill_pinned,
};
use tracedecay_agent_hosts::automation::skill_usage::{
    SkillUsageAction, ingest_project_analytics_events, record_skill_usage,
    skill_improvement_recommendations, stale_skill_recommendations, summarize_skill_usage,
    summarize_skill_usage_for,
};
use tracedecay_agent_hosts::automation::skill_writer::{
    ManagedSkillDeploymentReceipt, deploy_managed_skills_to_project,
};
use tracedecay_agent_hosts::ports::session_store::AutomationSessionStore;
use tracedecay_runtime_core::tracedecay::current_timestamp;

type ApiResult = std::result::Result<Json<Value>, JsonError>;
const SKILL_ANALYTICS_IMPORT_LIMIT: usize = 10_000;

#[derive(Debug, Deserialize)]
pub struct ManagedSkillCreateRequest {
    id: String,
    title: String,
    summary: String,
    category: String,
    #[serde(
        default = "tracedecay_agent_hosts::automation::managed_skills::default_managed_skill_targets"
    )]
    targets: Vec<SkillInstallTarget>,
    body_markdown: String,
    #[serde(default)]
    support_files: Vec<ManagedSupportFile>,
    #[serde(default)]
    provenance: Option<ManagedSkillProvenance>,
    #[serde(default)]
    pinned: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct ManagedSkillUpdateRequest {
    base_checksum: String,
    #[serde(flatten)]
    update: ManagedSkillUpdate,
}

pub async fn list(State(state): State<DashboardState>) -> ApiResult {
    let profile_root = profile_root_or_error()?;
    sync_project_skill_analytics(&profile_root, &state).await?;
    let skills = list_managed_skills(&profile_root)
        .await
        .map_err(|err| internal_error(&err))?;
    let skill_metadata = skills
        .iter()
        .map(|skill| skill.metadata.clone())
        .collect::<Vec<_>>();
    let usage_summaries = summarize_skill_usage(&profile_root, &skills)
        .await
        .map_err(|err| internal_error(&err))?;
    let stale_recommendations =
        stale_skill_recommendations(&usage_summaries, current_timestamp(), 60 * 60 * 24 * 90);
    let improvement_recommendations = skill_improvement_recommendations(&usage_summaries);
    Ok(Json(json!({
        "profile_root": profile_root.display().to_string(),
        "skills_root": managed_skill_root(&profile_root).display().to_string(),
        "count": skills.len(),
        "skills": skills,
        "skill_metadata": skill_metadata,
        "usage_summaries": usage_summaries,
        "stale_recommendations": stale_recommendations,
        "improvement_recommendations": improvement_recommendations,
    })))
}

pub async fn view(State(state): State<DashboardState>, Path(id): Path<String>) -> ApiResult {
    let profile_root = profile_root_or_error()?;
    let skill = load_managed_skill(&profile_root, &id)
        .await
        .map_err(|err| not_found_or_internal(&err))?;
    record_skill_usage(
        &profile_root,
        &skill,
        SkillUsageAction::View,
        "dashboard",
        vec!["dashboard".to_string()],
        Some("dashboard".to_string()),
        None,
    )
    .await
    .map_err(|err| internal_error(&err))?;
    sync_project_skill_analytics(&profile_root, &state).await?;
    skill_payload(&profile_root, skill).await
}

pub async fn create(
    State(state): State<DashboardState>,
    Json(request): Json<ManagedSkillCreateRequest>,
) -> ApiResult {
    let profile_root = profile_root_or_error()?;
    reject_existing_managed_skill(&profile_root, &request.id).await?;
    let pinned = request.pinned;
    let skill = create_managed_skill(&profile_root, request.into_managed_skill())
        .await
        .map_err(|err| bad_request_or_internal(&err))?;
    let skill = if let Some(pinned) = pinned {
        set_managed_skill_pinned(&profile_root, &skill.metadata.id, pinned)
            .await
            .map_err(|err| internal_error(&err))?
    } else {
        skill
    };
    let deployment = deploy_skills_to_project(&profile_root, &state.project_root).await?;
    skill_payload_with_deployment(&profile_root, skill, Some(deployment)).await
}

async fn reject_existing_managed_skill(
    profile_root: &std::path::Path,
    id: &str,
) -> std::result::Result<(), JsonError> {
    match load_managed_skill(profile_root, id).await {
        Ok(_) => Err(conflict(&format!(
            "managed skill '{id}' already exists; use PATCH to update it"
        ))),
        Err(err) => {
            let message = err.to_string();
            if is_not_found(&message) {
                Ok(())
            } else {
                Err(not_found_bad_request_or_internal(&message))
            }
        }
    }
}

pub async fn update(
    State(state): State<DashboardState>,
    Path(id): Path<String>,
    Json(request): Json<ManagedSkillUpdateRequest>,
) -> ApiResult {
    let profile_root = profile_root_or_error()?;
    let skill =
        apply_managed_skill_update(&profile_root, &id, &request.base_checksum, request.update)
            .await
            .map_err(|err| not_found_bad_request_or_internal(&err))?;
    let deployment = deploy_skills_to_project(&profile_root, &state.project_root).await?;
    skill_payload_with_deployment(&profile_root, skill, Some(deployment)).await
}

pub async fn disable(State(state): State<DashboardState>, Path(id): Path<String>) -> ApiResult {
    let profile_root = profile_root_or_error()?;
    let skill = disable_managed_skill(&profile_root, &id)
        .await
        .map_err(|err| not_found_or_internal(&err))?;
    let deployment = deploy_skills_to_project(&profile_root, &state.project_root).await?;
    skill_payload_with_deployment(&profile_root, skill, Some(deployment)).await
}

pub async fn archive(State(state): State<DashboardState>, Path(id): Path<String>) -> ApiResult {
    let profile_root = profile_root_or_error()?;
    let skill = archive_managed_skill(&profile_root, &id)
        .await
        .map_err(|err| not_found_or_internal(&err))?;
    let deployment = deploy_skills_to_project(&profile_root, &state.project_root).await?;
    skill_payload_with_deployment(&profile_root, skill, Some(deployment)).await
}

pub async fn restore(State(state): State<DashboardState>, Path(id): Path<String>) -> ApiResult {
    let profile_root = profile_root_or_error()?;
    let skill = restore_managed_skill(&profile_root, &id)
        .await
        .map_err(|err| not_found_or_internal(&err))?;
    let deployment = deploy_skills_to_project(&profile_root, &state.project_root).await?;
    skill_payload_with_deployment(&profile_root, skill, Some(deployment)).await
}

impl ManagedSkillCreateRequest {
    fn into_managed_skill(self) -> ManagedSkillDraft {
        ManagedSkillDraft {
            id: self.id,
            title: self.title,
            summary: self.summary,
            category: self.category,
            targets: self.targets,
            body_markdown: self.body_markdown,
            support_files: self.support_files,
            provenance: self.provenance.unwrap_or(ManagedSkillProvenance {
                source: ManagedSkillSource::User,
                actor: "dashboard".to_string(),
                run_id: None,
            }),
        }
    }
}

/// Runs the canonical project-scoped deployment after a deliberate operator
/// lifecycle override. Deployment failures remain in the receipt with an
/// explicit retry requirement; a join failure is an adapter failure, not a
/// fabricated deployment result.
async fn deploy_skills_to_project(
    profile_root: &std::path::Path,
    project_root: &std::path::Path,
) -> std::result::Result<ManagedSkillDeploymentReceipt, JsonError> {
    let profile_root = profile_root.to_path_buf();
    let project_root = project_root.to_path_buf();
    tokio::task::spawn_blocking(move || {
        deploy_managed_skills_to_project(&profile_root, &project_root)
    })
    .await
    .map_err(|error| internal_error(&error))
}

async fn skill_payload(profile_root: &std::path::Path, skill: ManagedSkill) -> ApiResult {
    skill_payload_with_deployment(profile_root, skill, None).await
}

async fn skill_payload_with_deployment(
    profile_root: &std::path::Path,
    skill: ManagedSkill,
    deployment: Option<ManagedSkillDeploymentReceipt>,
) -> ApiResult {
    let skill_dir = managed_skill_dir(profile_root, &skill.metadata.id)
        .map_err(|err| bad_request_or_internal(&err))?;
    let usage_summary = summarize_skill_usage_for(profile_root, &skill)
        .await
        .map_err(|err| internal_error(&err))?;
    let stale_recommendation = stale_skill_recommendations(
        std::slice::from_ref(&usage_summary),
        current_timestamp(),
        60 * 60 * 24 * 90,
    )
    .into_iter()
    .next();
    let improvement_recommendation =
        skill_improvement_recommendations(std::slice::from_ref(&usage_summary))
            .into_iter()
            .next();
    let mut payload = json!({
        "profile_root": profile_root.display().to_string(),
        "skills_root": managed_skill_root(profile_root).display().to_string(),
        "skill_dir": skill_dir.display().to_string(),
        "skill": skill,
        "usage_summary": usage_summary,
        "stale_recommendation": stale_recommendation,
        "improvement_recommendation": improvement_recommendation,
    });
    if let Some(deployment) = deployment {
        payload["deployment"] =
            serde_json::to_value(deployment).map_err(|error| internal_error(&error))?;
    }
    Ok(Json(payload))
}

async fn sync_project_skill_analytics(
    profile_root: &std::path::Path,
    state: &DashboardState,
) -> std::result::Result<(), JsonError> {
    let analytics_db = state
        .savings_db
        .as_deref()
        .map(|database| database as &dyn AutomationSessionStore);
    ingest_project_analytics_events(
        profile_root,
        &state.project_root,
        analytics_db,
        SKILL_ANALYTICS_IMPORT_LIMIT,
    )
    .await
    .map(|_| ())
    .map_err(|err| internal_error(&err))
}

fn profile_root_or_error() -> std::result::Result<std::path::PathBuf, JsonError> {
    tracedecay_runtime_core::storage::default_profile_root().map_err(|err| internal_error(&err))
}

fn bad_request(err: &impl ToString) -> JsonError {
    (StatusCode::BAD_REQUEST, Json(http_detail(&err.to_string())))
}

fn bad_request_or_internal(err: &impl ToString) -> JsonError {
    client_error_or_internal(err, false, true)
}

fn not_found_or_internal(err: &impl ToString) -> JsonError {
    client_error_or_internal(err, true, false)
}

fn not_found_bad_request_or_internal(err: &impl ToString) -> JsonError {
    client_error_or_internal(err, true, true)
}

fn client_error_or_internal(
    err: &impl ToString,
    allow_not_found: bool,
    allow_bad_request: bool,
) -> JsonError {
    let message = err.to_string();
    if allow_not_found && is_not_found(&message) {
        not_found(&message)
    } else if allow_bad_request && is_bad_request(&message) {
        bad_request(&message)
    } else {
        internal_error(&message)
    }
}

fn is_not_found(message: &str) -> bool {
    message.contains("No such file") || message.contains("not found")
}

fn not_found(message: &str) -> JsonError {
    (StatusCode::NOT_FOUND, Json(http_detail(message)))
}

fn conflict(message: &str) -> JsonError {
    (StatusCode::CONFLICT, Json(http_detail(message)))
}

fn is_bad_request(message: &str) -> bool {
    message.contains("unsafe")
        || message.contains("cannot be empty")
        || message.contains("duplicate")
        || message.contains("conflicts with")
        || message.contains("exceeds")
        || message.contains("must be under")
        || message.contains("must name a file")
        || message.contains("failed to parse")
        || message.contains("base_checksum")
        || message.contains("stale")
        || message.contains("does not change")
}
