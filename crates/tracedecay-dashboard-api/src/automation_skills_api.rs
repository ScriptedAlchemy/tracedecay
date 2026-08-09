//! Dashboard endpoints for profile-owned managed automation skills.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use serde::Deserialize;
use serde_json::{Value, json};

use super::util::{JsonError, http_detail, internal_error};
use super::{
    DashboardAutomationAuthorityErrorV1, DashboardManagedSkillCommandOutcomeV1,
    DashboardManagedSkillCommandV1, DashboardState, automation_authority_error_response,
    exact_automation_authority,
};
use tracedecay_agent_hosts::automation::managed_skills::{
    ManagedSkill, ManagedSkillDraft, ManagedSkillProvenance, ManagedSkillSource,
    ManagedSkillUpdate, ManagedSupportFile, SkillInstallTarget, list_managed_skills,
    load_managed_skill, managed_skill_dir, managed_skill_root,
};
use tracedecay_agent_hosts::automation::skill_usage::{
    skill_improvement_recommendations, stale_skill_recommendations, summarize_skill_usage,
    summarize_skill_usage_for,
};
use tracedecay_agent_hosts::automation::skill_writer::ManagedSkillDeploymentReceipt;
use tracedecay_runtime_core::tracedecay::current_timestamp;

type ApiResult = std::result::Result<Json<Value>, JsonError>;

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
    let profile_root = profile_root(&state)?;
    let skills = list_managed_skills(profile_root)
        .await
        .map_err(|err| internal_error(&err))?;
    let skill_metadata = skills
        .iter()
        .map(|skill| skill.metadata.clone())
        .collect::<Vec<_>>();
    let usage_summaries = summarize_skill_usage(profile_root, &skills)
        .await
        .map_err(|err| internal_error(&err))?;
    let stale_recommendations =
        stale_skill_recommendations(&usage_summaries, current_timestamp(), 60 * 60 * 24 * 90);
    let improvement_recommendations = skill_improvement_recommendations(&usage_summaries);
    Ok(Json(json!({
        "profile_root": profile_root.display().to_string(),
        "skills_root": managed_skill_root(profile_root).display().to_string(),
        "count": skills.len(),
        "skills": skills,
        "skill_metadata": skill_metadata,
        "usage_summaries": usage_summaries,
        "stale_recommendations": stale_recommendations,
        "improvement_recommendations": improvement_recommendations,
    })))
}

pub async fn view(State(state): State<DashboardState>, Path(id): Path<String>) -> ApiResult {
    let profile_root = profile_root(&state)?;
    let skill = load_managed_skill(profile_root, &id)
        .await
        .map_err(|err| not_found_or_internal(&err))?;
    skill_payload(profile_root, skill).await
}

pub async fn create(
    State(state): State<DashboardState>,
    Json(request): Json<ManagedSkillCreateRequest>,
) -> ApiResult {
    execute_skill_command(&state, request.into_create_command()).await
}

pub async fn update(
    State(state): State<DashboardState>,
    Path(id): Path<String>,
    Json(request): Json<ManagedSkillUpdateRequest>,
) -> ApiResult {
    execute_skill_command(
        &state,
        DashboardManagedSkillCommandV1::Update {
            id,
            base_checksum: request.base_checksum,
            update: request.update,
        },
    )
    .await
}

pub async fn disable(State(state): State<DashboardState>, Path(id): Path<String>) -> ApiResult {
    execute_skill_command(&state, DashboardManagedSkillCommandV1::Disable { id }).await
}

pub async fn archive(State(state): State<DashboardState>, Path(id): Path<String>) -> ApiResult {
    execute_skill_command(&state, DashboardManagedSkillCommandV1::Archive { id }).await
}

pub async fn restore(State(state): State<DashboardState>, Path(id): Path<String>) -> ApiResult {
    execute_skill_command(&state, DashboardManagedSkillCommandV1::Restore { id }).await
}

impl ManagedSkillCreateRequest {
    fn into_create_command(self) -> DashboardManagedSkillCommandV1 {
        DashboardManagedSkillCommandV1::Create {
            pinned: self.pinned,
            draft: ManagedSkillDraft {
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
            },
        }
    }
}

async fn execute_skill_command(
    state: &DashboardState,
    command: DashboardManagedSkillCommandV1,
) -> ApiResult {
    let authority = automation_authority(state)?;
    let DashboardManagedSkillCommandOutcomeV1 { skill, deployment } = authority
        .execute_managed_skill_command(&state.project_root, command)
        .await
        .map_err(automation_authority_error_response)?;
    skill_payload_with_deployment(authority.profile_root(), skill, Some(deployment)).await
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

fn automation_authority(
    state: &DashboardState,
) -> std::result::Result<&super::DashboardAutomationAuthorityV1, JsonError> {
    exact_automation_authority(state).map_err(automation_authority_error_response)
}

fn profile_root(state: &DashboardState) -> std::result::Result<&std::path::Path, JsonError> {
    Ok(automation_authority(state)?.profile_root())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn managed_skill_checksum_conflicts_remain_http_conflicts() {
        let (status, Json(payload)) =
            automation_authority_error_response(DashboardAutomationAuthorityErrorV1::Conflict {
                detail: "managed skill base_checksum is stale".to_owned(),
            });

        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(
            payload["detail"],
            json!("managed skill base_checksum is stale")
        );
    }

    #[test]
    fn absent_managed_skill_authority_is_typed_unavailable() {
        let (status, Json(payload)) =
            automation_authority_error_response(DashboardAutomationAuthorityErrorV1::unavailable(
                "dashboard automation authority is not mounted",
            ));

        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(
            payload["detail"],
            json!("dashboard automation authority is not mounted")
        );
    }
}
