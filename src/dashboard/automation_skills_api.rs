//! Dashboard endpoints for profile-owned managed automation skills.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

use super::util::{http_detail, JsonError};
use super::DashboardState;
use crate::automation::managed_skills::{
    approve_managed_skill, create_managed_skill_draft, discard_pending_managed_skill_update,
    list_managed_skills, load_managed_skill, managed_skill_dir, managed_skill_root,
    save_managed_skill, set_managed_skill_state, stage_managed_skill_update, update_managed_skill,
    ManagedSkill, ManagedSkillDraft, ManagedSkillProvenance, ManagedSkillSource, ManagedSkillState,
    ManagedSkillUpdate, ManagedSupportFile, SkillInstallTarget,
};
use crate::automation::skill_usage::{
    ingest_project_analytics_events, record_skill_usage, skill_improvement_recommendations,
    stale_skill_recommendations, summarize_skill_usage, summarize_skill_usage_for,
    SkillUsageAction,
};
use crate::tracedecay::current_timestamp;

type ApiResult = std::result::Result<Json<Value>, JsonError>;
const SKILL_ANALYTICS_IMPORT_LIMIT: usize = 10_000;

#[derive(Debug, Deserialize)]
pub(crate) struct ManagedSkillDraftRequest {
    id: String,
    title: String,
    summary: String,
    category: String,
    #[serde(default = "crate::automation::managed_skills::default_managed_skill_targets")]
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
pub(crate) struct ManagedSkillUpdateRequest {
    #[serde(default)]
    base_checksum: Option<String>,
    #[serde(flatten)]
    update: ManagedSkillUpdate,
}

pub(crate) async fn list(State(state): State<DashboardState>) -> ApiResult {
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

pub(crate) async fn view(State(state): State<DashboardState>, Path(id): Path<String>) -> ApiResult {
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

pub(crate) async fn draft(
    State(_state): State<DashboardState>,
    Json(request): Json<ManagedSkillDraftRequest>,
) -> ApiResult {
    let profile_root = profile_root_or_error()?;
    reject_existing_managed_skill(&profile_root, &request.id).await?;
    let pinned = request.pinned;
    let mut skill = create_managed_skill_draft(&profile_root, request.into_draft())
        .await
        .map_err(|err| bad_request_or_internal(&err))?;
    if let Some(pinned) = pinned {
        skill.set_pinned(pinned);
        save_managed_skill(&profile_root, &skill)
            .await
            .map_err(|err| internal_error(&err))?;
    }
    skill_payload(&profile_root, skill).await
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

pub(crate) async fn update(
    State(_state): State<DashboardState>,
    Path(id): Path<String>,
    Json(request): Json<ManagedSkillUpdateRequest>,
) -> ApiResult {
    let profile_root = profile_root_or_error()?;
    let current = load_managed_skill(&profile_root, &id)
        .await
        .map_err(|err| not_found_or_internal(&err))?;
    let skill = (if current.metadata.state == ManagedSkillState::PendingApproval
        && current.pending_update.is_none()
    {
        update_managed_skill(&profile_root, &id, request.update).await
    } else {
        let base_checksum = request.base_checksum.as_deref().ok_or_else(|| {
            bad_request(&format!(
                "base_checksum is required to stage managed skill update for '{id}'"
            ))
        })?;
        match stage_managed_skill_update(&profile_root, &id, base_checksum, request.update).await {
            Ok(_) => load_managed_skill(&profile_root, &id).await,
            Err(err) => Err(err),
        }
    })
    .map_err(|err| not_found_bad_request_or_internal(&err))?;
    skill_payload(&profile_root, skill).await
}

pub(crate) async fn approve(
    State(_state): State<DashboardState>,
    Path(id): Path<String>,
) -> ApiResult {
    let profile_root = profile_root_or_error()?;
    let skill = approve_managed_skill(&profile_root, &id)
        .await
        .map_err(|err| not_found_or_internal(&err))?;
    skill_payload(&profile_root, skill).await
}

pub(crate) async fn discard_update(
    State(_state): State<DashboardState>,
    Path(id): Path<String>,
) -> ApiResult {
    let profile_root = profile_root_or_error()?;
    let skill = discard_pending_managed_skill_update(&profile_root, &id)
        .await
        .map_err(|err| not_found_or_internal(&err))?;
    skill_payload(&profile_root, skill).await
}

pub(crate) async fn disable(
    State(_state): State<DashboardState>,
    Path(id): Path<String>,
) -> ApiResult {
    set_state(&id, ManagedSkillState::Disabled).await
}

pub(crate) async fn archive(
    State(_state): State<DashboardState>,
    Path(id): Path<String>,
) -> ApiResult {
    set_state(&id, ManagedSkillState::Archived).await
}

pub(crate) async fn restore(
    State(_state): State<DashboardState>,
    Path(id): Path<String>,
) -> ApiResult {
    set_state(&id, ManagedSkillState::PendingApproval).await
}

async fn set_state(id: &str, state: ManagedSkillState) -> ApiResult {
    let profile_root = profile_root_or_error()?;
    let skill = set_managed_skill_state(&profile_root, id, state)
        .await
        .map_err(|err| not_found_or_internal(&err))?;
    skill_payload(&profile_root, skill).await
}

impl ManagedSkillDraftRequest {
    fn into_draft(self) -> ManagedSkillDraft {
        ManagedSkillDraft {
            id: self.id,
            title: self.title,
            summary: self.summary,
            category: self.category,
            targets: self.targets,
            body_markdown: self.body_markdown,
            support_files: self.support_files,
            provenance: self.provenance.unwrap_or(ManagedSkillProvenance {
                source: ManagedSkillSource::UserDraft,
                actor: "dashboard".to_string(),
                run_id: None,
            }),
        }
    }
}

async fn skill_payload(profile_root: &std::path::Path, skill: ManagedSkill) -> ApiResult {
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
    Ok(Json(json!({
        "profile_root": profile_root.display().to_string(),
        "skills_root": managed_skill_root(profile_root).display().to_string(),
        "skill_dir": skill_dir.display().to_string(),
        "skill": skill,
        "usage_summary": usage_summary,
        "stale_recommendation": stale_recommendation,
        "improvement_recommendation": improvement_recommendation,
    })))
}

async fn sync_project_skill_analytics(
    profile_root: &std::path::Path,
    state: &DashboardState,
) -> std::result::Result<(), JsonError> {
    ingest_project_analytics_events(
        profile_root,
        &state.project_root,
        state.savings_db.as_deref(),
        SKILL_ANALYTICS_IMPORT_LIMIT,
    )
    .await
    .map(|_| ())
    .map_err(|err| internal_error(&err))
}

fn profile_root_or_error() -> std::result::Result<std::path::PathBuf, JsonError> {
    crate::storage::default_profile_root().map_err(|err| internal_error(&err))
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
        || message.contains("pending update")
        || message.contains("does not change")
}

fn internal_error(err: &impl ToString) -> JsonError {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(http_detail(&err.to_string())),
    )
}
