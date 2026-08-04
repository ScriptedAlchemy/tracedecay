use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde_json::{Value, json};

use super::artifacts::sha256_bytes;
use super::config::AutomationConfig;
use super::managed_skills::{
    ManagedSkill, ManagedSkillDraft, ManagedSkillProvenance, ManagedSkillSource,
    ManagedSkillUpdate, ManagedSupportFile, SkillInstallTarget, apply_managed_skill_consolidation,
    approve_managed_skill, create_managed_skill_draft, default_managed_skill_targets,
    list_managed_skills, stage_managed_skill_archive, stage_managed_skill_update,
};
use super::skill_usage::{
    SkillOverlapCandidate, SkillStaleRecommendation, SkillUsageSummary,
    skill_improvement_recommendations as usage_skill_improvement_recommendations,
};
use super::text::truncate_chars_for_prompt;
use crate::analytics::ToolFamilySignal;
use crate::errors::Result;
use crate::sessions::lcm::LcmGrepHit;

mod consolidation;

use consolidation::{
    skill_archive_from_proposal, skill_merge_from_proposal, stage_skill_merge,
    staged_consolidation_record,
};

/// Outcome of validating and applying one batch of `skill_writer` proposals.
/// `consolidations` holds staged or autonomously applied merge/archive records.
#[derive(Debug, Default)]
pub(crate) struct SkillProposalOutcome {
    pub created: Vec<Value>,
    pub updated: Vec<Value>,
    pub consolidations: Vec<Value>,
    pub rejected: Vec<Value>,
}

pub(crate) async fn validate_and_apply_skill_proposals(
    profile_root: &Path,
    run_id: &str,
    proposals: &[Value],
    auto_enable_skills: bool,
) -> Result<SkillProposalOutcome> {
    let mut existing_skills = list_managed_skills(profile_root)
        .await?
        .into_iter()
        .map(|skill| (skill.metadata.id.clone(), skill))
        .collect::<BTreeMap<_, _>>();
    let mut existing_ids = existing_skills.keys().cloned().collect::<BTreeSet<_>>();
    let mut created = Vec::new();
    let mut updated = Vec::new();
    let mut consolidations = Vec::new();
    let mut rejected = Vec::new();
    let mut refresh_exports = false;
    for proposal in proposals {
        match skill_proposal_action(proposal) {
            Ok(SkillProposalAction::Create) => {
                match skill_draft_from_proposal(proposal, run_id, &existing_ids) {
                    Ok(draft) => {
                        existing_ids.insert(draft.id.clone());
                        match create_managed_skill_draft(profile_root, draft).await {
                            Ok(skill) => {
                                let skill = if auto_enable_skills {
                                    let approved =
                                        approve_managed_skill(profile_root, &skill.metadata.id)
                                            .await?;
                                    refresh_exports = true;
                                    approved
                                } else {
                                    skill
                                };
                                existing_skills.insert(skill.metadata.id.clone(), skill.clone());
                                created.push(accepted_skill_proposal_record(
                                    &skill,
                                    proposal,
                                    SkillProposalAction::Create,
                                    None,
                                    auto_enable_skills,
                                ));
                            }
                            Err(err) => rejected.push(rejected_skill(proposal, &err.to_string())),
                        }
                    }
                    Err(reason) => rejected.push(rejected_skill(proposal, &reason)),
                }
            }
            Ok(SkillProposalAction::Update) => {
                match skill_update_from_proposal(proposal, &existing_skills) {
                    Ok((id, base_checksum, update)) => {
                        match stage_managed_skill_update(profile_root, &id, &base_checksum, update)
                            .await
                        {
                            Ok(skill) => {
                                let skill = if auto_enable_skills {
                                    let approved =
                                        approve_managed_skill(profile_root, &skill.metadata.id)
                                            .await?;
                                    refresh_exports = true;
                                    approved
                                } else {
                                    skill
                                };
                                existing_skills.insert(id, skill.clone());
                                updated.push(accepted_skill_proposal_record(
                                    &skill,
                                    proposal,
                                    SkillProposalAction::Update,
                                    Some(&base_checksum),
                                    auto_enable_skills,
                                ));
                            }
                            Err(err) => rejected.push(rejected_skill(proposal, &err.to_string())),
                        }
                    }
                    Err(reason) => rejected.push(rejected_skill(proposal, &reason)),
                }
            }
            Ok(SkillProposalAction::Archive) => {
                match skill_archive_from_proposal(proposal, &existing_skills) {
                    Ok(archive) => {
                        if auto_enable_skills {
                            if let Err(reason) = ensure_skill_not_referenced_by_scheduled_job(
                                profile_root,
                                &archive.skill_id,
                            ) {
                                rejected.push(rejected_skill(proposal, &reason));
                                continue;
                            }
                            match apply_managed_skill_consolidation(
                                profile_root,
                                None,
                                None,
                                None,
                                &archive.skill_id,
                                &archive.base_checksum,
                                &archive.reason,
                            )
                            .await
                            {
                                Ok(result) => {
                                    existing_skills
                                        .insert(archive.skill_id.clone(), result.source.clone());
                                    consolidations.push(applied_consolidation_record(
                                        SkillProposalAction::Archive,
                                        proposal,
                                        &result,
                                    ));
                                    refresh_exports = true;
                                }
                                Err(err) => {
                                    rejected.push(rejected_skill(proposal, &err.to_string()));
                                }
                            }
                            continue;
                        }
                        match stage_managed_skill_archive(
                            profile_root,
                            &archive.skill_id,
                            &archive.base_checksum,
                            Some(archive.reason.clone()),
                        )
                        .await
                        {
                            Ok(skill) => {
                                existing_skills.insert(archive.skill_id.clone(), skill.clone());
                                consolidations.push(staged_consolidation_record(
                                    SkillProposalAction::Archive,
                                    proposal,
                                    &skill,
                                    &archive.base_checksum,
                                    None,
                                ));
                            }
                            Err(err) => rejected.push(rejected_skill(proposal, &err.to_string())),
                        }
                    }
                    Err(reason) => rejected.push(rejected_skill(proposal, &reason)),
                }
            }
            Ok(SkillProposalAction::Merge) => {
                match skill_merge_from_proposal(proposal, &existing_skills) {
                    Ok(merge) if auto_enable_skills => {
                        if let Err(reason) = ensure_skill_not_referenced_by_scheduled_job(
                            profile_root,
                            &merge.source_skill_id,
                        ) {
                            rejected.push(rejected_skill(proposal, &reason));
                            continue;
                        }
                        match apply_managed_skill_consolidation(
                            profile_root,
                            Some(&merge.target_skill_id),
                            Some(&merge.base_checksum),
                            merge.update.clone(),
                            &merge.source_skill_id,
                            &merge.source_base_checksum,
                            &merge.reason,
                        )
                        .await
                        {
                            Ok(result) => {
                                existing_skills
                                    .insert(merge.source_skill_id.clone(), result.source.clone());
                                if let Some(target) = &result.target {
                                    existing_skills
                                        .insert(merge.target_skill_id.clone(), target.clone());
                                }
                                consolidations.push(applied_consolidation_record(
                                    SkillProposalAction::Merge,
                                    proposal,
                                    &result,
                                ));
                                refresh_exports = true;
                            }
                            Err(err) => rejected.push(rejected_skill(proposal, &err.to_string())),
                        }
                    }
                    Ok(merge) => match stage_skill_merge(profile_root, &merge).await {
                        Ok((source_skill, target_skill)) => {
                            existing_skills
                                .insert(merge.source_skill_id.clone(), source_skill.clone());
                            if let Some(target_skill) = &target_skill {
                                existing_skills
                                    .insert(merge.target_skill_id.clone(), target_skill.clone());
                            }
                            consolidations.push(staged_consolidation_record(
                                SkillProposalAction::Merge,
                                proposal,
                                &source_skill,
                                &merge.source_base_checksum,
                                Some(&merge),
                            ));
                        }
                        Err(err) => rejected.push(rejected_skill(proposal, &err.to_string())),
                    },
                    Err(reason) => rejected.push(rejected_skill(proposal, &reason)),
                }
            }
            Err(reason) => rejected.push(rejected_skill(proposal, &reason)),
        }
    }
    if refresh_exports {
        let export_result = refresh_managed_skill_exports_after_auto_enable(profile_root);
        for record in &mut consolidations {
            if record.get("approval_status").and_then(Value::as_str) == Some("auto_applied") {
                record["journal"]["export_result"] = export_result.clone();
            }
        }
    }
    Ok(SkillProposalOutcome {
        created,
        updated,
        consolidations,
        rejected,
    })
}

fn applied_consolidation_record(
    action: SkillProposalAction,
    proposal: &Value,
    result: &super::managed_skills::SkillConsolidationResult,
) -> Value {
    json!({
        "action": action.as_str(),
        "proposal_action": action.as_str(),
        "reason": proposal.get("reason").cloned().unwrap_or(Value::Null),
        "approval_status": "auto_applied",
        "resulting_state": "archived",
        "archived_skill_id": result.source.metadata.id,
        "source_skill_id": result.source.metadata.id,
        "target_skill_id": result.target.as_ref().map(|skill| skill.metadata.id.clone()),
        "absorbed_into": result.source.metadata.absorbed_into,
        "target_update_staged": false,
        "journal": {
            "provenance": result.source.metadata.provenance,
            "target_before_checksum": result.target_before_checksum,
            "target_after_checksum": result.target_after_checksum,
            "source_before_checksum": result.source_before_checksum,
            "source_after_checksum": result.source_after_checksum,
            "export_refresh": "requested_once_after_batch",
        }
    })
}

fn ensure_skill_not_referenced_by_scheduled_job(
    profile_root: &Path,
    skill_id: &str,
) -> std::result::Result<(), String> {
    fn visit(dir: &Path, skill_id: &str, depth: usize) -> std::result::Result<bool, String> {
        if depth > 5 {
            return Ok(false);
        }
        let Ok(entries) = std::fs::read_dir(dir) else {
            return Ok(false);
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if visit(&path, skill_id, depth + 1)? {
                    return Ok(true);
                }
            } else if path
                .file_name()
                .is_some_and(|name| name == "automation_jobs.json")
            {
                let bytes = std::fs::read(&path).map_err(|error| {
                    format!(
                        "cannot verify scheduled job references in '{}': {error}",
                        path.display()
                    )
                })?;
                let value = serde_json::from_slice::<Value>(&bytes).map_err(|error| {
                    format!(
                        "cannot verify scheduled job references in '{}': {error}",
                        path.display()
                    )
                })?;
                if value
                    .get("jobs")
                    .and_then(Value::as_array)
                    .is_some_and(|jobs| {
                        jobs.iter().any(|job| {
                            job.get("skill_ids")
                                .and_then(Value::as_array)
                                .is_some_and(|ids| {
                                    ids.iter().any(|id| id.as_str() == Some(skill_id))
                                })
                        })
                    })
                {
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }
    if visit(profile_root, skill_id, 0)? {
        Err(format!(
            "managed skill '{skill_id}' is referenced by a scheduled job"
        ))
    } else {
        Ok(())
    }
}

pub(crate) fn activation_policy(config: &AutomationConfig) -> &'static str {
    if config.auto_enable_skills {
        "auto_enable_after_validation"
    } else {
        "pending_approval_only"
    }
}

pub(crate) fn support_file_evidence(support: &ManagedSupportFile) -> Value {
    let text = String::from_utf8_lossy(&support.bytes);
    let text_preview = truncate_chars_for_prompt(&text, 1200);
    let text_truncated = text.chars().count() > text_preview.chars().count();
    json!({
        "path": support.path.display().to_string(),
        "bytes": support.bytes.len(),
        "sha256": sha256_bytes(&support.bytes),
        "text_preview": text_preview,
        "text_preview_chars": 1200,
        "text_truncated": text_truncated,
    })
}

pub(crate) fn skill_improvement_recommendations(
    hits: &[LcmGrepHit],
    usage_summaries: &[SkillUsageSummary],
    stale_recommendations: &[SkillStaleRecommendation],
    underused_tool_families: &[ToolFamilySignal],
    overlap_candidates: &[SkillOverlapCandidate],
) -> Vec<Value> {
    let mut recommendations = Vec::new();

    for candidate in overlap_candidates {
        recommendations.push(json!({
            "id": format!("skill_overlap:{}:{}", candidate.skill_a, candidate.skill_b),
            "kind": "managed_skill_consolidation",
            "priority": if candidate.score >= 0.6 { "high" } else { "medium" },
            "skill_a": candidate.skill_a,
            "skill_b": candidate.skill_b,
            "recommendation": candidate.recommendation,
            "reason": candidate.reason,
            "evidence": [
                format!("content_overlap={:.4}", candidate.content_overlap),
                format!("title_overlap={:.4}", candidate.title_overlap),
                format!("shared_tokens={}", candidate.shared_tokens.join(",")),
                format!("pinned={}/{}", candidate.skill_a_pinned, candidate.skill_b_pinned),
            ],
            "source": "skill_overlap_detection",
        }));
    }

    for recommendation in usage_skill_improvement_recommendations(usage_summaries)
        .into_iter()
        .filter(|recommendation| recommendation.improvement)
    {
        recommendations.push(json!({
            "id": format!("skill_usage:{}", recommendation.skill_id),
            "kind": "managed_skill_patch",
            "priority": recommendation.priority,
            "skill_id": recommendation.skill_id,
            "recommendation": recommendation.recommendation,
            "reason": recommendation.reason,
            "evidence": recommendation.evidence,
            "source": "skill_usage_ledger",
        }));
    }

    for recommendation in stale_recommendations
        .iter()
        .filter(|recommendation| recommendation.recommendation == "improve_review")
    {
        recommendations.push(json!({
            "id": format!("stale_scoring:{}", recommendation.skill_id),
            "kind": "managed_skill_patch",
            "priority": "medium",
            "skill_id": recommendation.skill_id,
            "recommendation": recommendation.recommendation,
            "reason": recommendation.reason,
            "evidence": recommendation.evidence,
            "source": "stale_scoring",
        }));
    }

    for family in underused_tool_families
        .iter()
        .filter(|family| family.underused)
    {
        recommendations.push(json!({
            "id": format!("underused_tool_family:{}", family.family),
            "kind": "activation_or_tooling_guidance",
            "priority": if family.missed_events >= 3 { "high" } else { "medium" },
            "tool_family": family.family,
            "recommendation": "add_or_patch_skill_guidance",
            "reason": format!(
                "{} relevant {} event(s) had {} direct use event(s)",
                family.family, family.relevant_events, family.usage_events
            ),
            "evidence": [
                format!("relevant_events={}", family.relevant_events),
                format!("usage_events={}", family.usage_events),
                format!("missed_events={}", family.missed_events),
            ],
            "source": "session_tool_usage",
        }));
    }

    let correction_hits = hits
        .iter()
        .filter(|hit| hit_mentions_correction_or_failure(hit))
        .take(5)
        .collect::<Vec<_>>();
    if correction_hits.len() >= 2 {
        recommendations.push(json!({
            "id": "session_patterns:repeated_corrections",
            "kind": "managed_skill_patch",
            "priority": "high",
            "recommendation": "draft_or_patch_skill_from_repeated_corrections",
            "reason": "multiple bounded LCM hits mention correction, failure, retry, or regression signals",
            "evidence": correction_hits
                .iter()
                .map(|hit| hit_evidence_ref(hit))
                .collect::<Vec<_>>(),
            "source": "lcm_hits",
        }));
    }

    if recommendations.is_empty() {
        recommendations.push(json!({
            "id": "collect_more_skill_improvement_evidence",
            "kind": "evidence_gap",
            "priority": "low",
            "recommendation": "collect_more_evidence",
            "reason": "no repeated correction, failed workflow, underused tool, or patch instability signal was present",
            "evidence": [
                format!("hits={}", hits.len()),
                format!("skills={}", usage_summaries.len()),
                format!("underused_families={}", underused_tool_families.iter().filter(|family| family.underused).count()),
            ],
            "source": "skill_writer_evidence",
        }));
    }

    recommendations
}

fn hit_mentions_correction_or_failure(hit: &LcmGrepHit) -> bool {
    let snippet = hit.snippet.to_ascii_lowercase();
    [
        "correction",
        "corrected",
        "fix",
        "fixed",
        "failed",
        "failure",
        "retry",
        "regression",
        "wrong",
        "underused",
    ]
    .iter()
    .any(|needle| snippet.contains(needle))
}

fn hit_evidence_ref(hit: &LcmGrepHit) -> Value {
    json!({
        "kind": hit.kind,
        "provider": hit.provider,
        "session_id": hit.session_id,
        "message_id": hit.message_id,
        "node_id": hit.node_id,
        "store_id": hit.store_id,
        "snippet": truncate_chars_for_prompt(&hit.snippet, 240),
    })
}

fn accepted_skill_proposal_record(
    skill: &ManagedSkill,
    proposal: &Value,
    action: SkillProposalAction,
    base_checksum: Option<&str>,
    auto_enable_skills: bool,
) -> Value {
    let mut record = serde_json::to_value(skill).unwrap_or_else(|_| json!({}));
    if let Some(object) = record.as_object_mut() {
        let reason = proposal.get("reason").cloned().unwrap_or(Value::Null);
        object.insert("action".to_string(), json!(action.as_str()));
        object.insert("proposal_action".to_string(), json!(action.as_str()));
        object.insert("reason".to_string(), reason.clone());
        object.insert("proposal_reason".to_string(), reason);
        object.insert("target_skill_id".to_string(), json!(skill.metadata.id));
        object.insert(
            "target_checksum".to_string(),
            json!(skill.metadata.checksum),
        );
        object.insert(
            "approval_status".to_string(),
            json!(accepted_skill_approval_status(action, auto_enable_skills)),
        );
        if let Some(base_checksum) = base_checksum {
            object.insert("base_checksum".to_string(), json!(base_checksum));
        }
    }
    record
}

fn refresh_managed_skill_exports_after_auto_enable(profile_root: &Path) -> Value {
    let Some(home) = crate::agents::home_dir() else {
        return json!({"status": "skipped", "reason": "home_unavailable"});
    };
    if !crate::agents::uses_default_user_profile(&home, profile_root) {
        return json!({"status": "skipped", "reason": "non_default_profile_root"});
    }
    let start = std::env::current_dir().unwrap_or_else(|_| home.clone());
    let project_root = crate::automation::skill_materialization::resolve_project_root(&start);
    let reports =
        crate::agents::export_managed_skills_to_agent_hosts(&home, &project_root, profile_root);
    for report in &reports {
        if let Some(error) = &report.error {
            tracing::warn!(
                agent = %report.agent,
                error = %error,
                "failed to refresh managed skill exports after auto-enable"
            );
        }
    }
    // Auto-applied (policy-off) skills also materialize their host-loadable
    // SKILL.md files, matching the manual approve path.
    crate::automation::skill_materialization::reconcile_after_activation(
        profile_root,
        &project_root,
    );
    json!({
        "status": if reports.iter().any(|report| report.error.is_some()) { "partial_failure" } else { "succeeded" },
        "reports": reports,
        "materialization_reconciled": true,
    })
}

fn accepted_skill_approval_status(
    action: SkillProposalAction,
    auto_enable_skills: bool,
) -> &'static str {
    if auto_enable_skills {
        "auto_enabled"
    } else if action == SkillProposalAction::Update {
        "staged_update"
    } else {
        "pending_approval"
    }
}

fn skill_draft_from_proposal(
    proposal: &Value,
    run_id: &str,
    existing_ids: &BTreeSet<String>,
) -> std::result::Result<ManagedSkillDraft, String> {
    let object = proposal
        .as_object()
        .ok_or_else(|| "proposal must be a JSON object".to_string())?;
    let id = required_proposal_string(object.get("id"), "id")?;
    if existing_ids.contains(&id) {
        return Err(format!("managed skill id '{id}' already exists"));
    }
    let title = required_proposal_string(object.get("title"), "title")?;
    let summary = required_proposal_string(object.get("summary"), "summary")?;
    let category = required_proposal_string(object.get("category"), "category")?;
    let targets = proposal_targets_or_default(object.get("targets"))?;
    let body_markdown = object
        .get("body_markdown")
        .or_else(|| object.get("body"))
        .ok_or_else(|| "body_markdown is required".to_string())
        .and_then(|value| required_proposal_string(Some(value), "body_markdown"))?;
    let support_files = support_files_from_proposal(object.get("support_files"))?;
    let draft = ManagedSkillDraft {
        id,
        title,
        summary,
        category,
        targets,
        body_markdown,
        support_files,
        provenance: ManagedSkillProvenance {
            source: ManagedSkillSource::AutomationRun,
            actor: "skill_writer".to_string(),
            run_id: Some(run_id.to_string()),
        },
    };
    draft
        .clone()
        .materialize()
        .map(|_| draft)
        .map_err(|err| err.to_string())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SkillProposalAction {
    Create,
    Update,
    Merge,
    Archive,
}

impl SkillProposalAction {
    fn as_str(self) -> &'static str {
        match self {
            SkillProposalAction::Create => "create",
            SkillProposalAction::Update => "update",
            SkillProposalAction::Merge => "merge",
            SkillProposalAction::Archive => "archive",
        }
    }
}

fn skill_proposal_action(proposal: &Value) -> std::result::Result<SkillProposalAction, String> {
    let object = proposal
        .as_object()
        .ok_or_else(|| "proposal must be a JSON object".to_string())?;
    let Some(action) = object.get("action").or_else(|| object.get("operation")) else {
        return Ok(SkillProposalAction::Create);
    };
    match required_proposal_string(Some(action), "action")?.as_str() {
        "create" | "draft" => Ok(SkillProposalAction::Create),
        "update" | "patch" => Ok(SkillProposalAction::Update),
        "merge" | "consolidate" => Ok(SkillProposalAction::Merge),
        "archive" => Ok(SkillProposalAction::Archive),
        other => Err(format!("unsupported skill proposal action '{other}'")),
    }
}

fn skill_update_from_proposal(
    proposal: &Value,
    existing_skills: &BTreeMap<String, ManagedSkill>,
) -> std::result::Result<(String, String, ManagedSkillUpdate), String> {
    let object = proposal
        .as_object()
        .ok_or_else(|| "proposal must be a JSON object".to_string())?;
    let id = required_proposal_string(object.get("id"), "id")?;
    let existing = existing_skills
        .get(&id)
        .ok_or_else(|| format!("managed skill id '{id}' does not exist"))?;
    if existing.metadata.provenance.source != ManagedSkillSource::AutomationRun {
        return Err(format!("managed skill '{id}' is not automation-owned"));
    }
    let base_checksum = required_proposal_string(object.get("base_checksum"), "base_checksum")?;
    if base_checksum != existing.metadata.checksum {
        return Err(format!(
            "base_checksum for managed skill id '{id}' is stale"
        ));
    }

    let update = ManagedSkillUpdate {
        title: optional_proposal_string(object.get("title"))?,
        summary: optional_proposal_string(object.get("summary"))?,
        category: optional_proposal_string(object.get("category"))?,
        targets: optional_proposal_targets(object.get("targets"))?,
        body_markdown: optional_proposal_string(
            object.get("body_markdown").or_else(|| object.get("body")),
        )?,
        support_files: if object.contains_key("support_files") {
            Some(support_files_from_proposal(object.get("support_files"))?)
        } else {
            None
        },
        pinned: match object.get("pinned") {
            Some(value) => Some(
                value
                    .as_bool()
                    .ok_or_else(|| "pinned must be a boolean".to_string())?,
            ),
            None => None,
        },
    };
    if update.title.is_none()
        && update.summary.is_none()
        && update.category.is_none()
        && update.targets.is_none()
        && update.body_markdown.is_none()
        && update.support_files.is_none()
        && update.pinned.is_none()
    {
        return Err("update proposal must include at least one changed field".to_string());
    }
    let changes_existing = update
        .title
        .as_ref()
        .is_some_and(|title| existing.metadata.title != *title)
        || update
            .summary
            .as_ref()
            .is_some_and(|summary| existing.metadata.summary != *summary)
        || update
            .category
            .as_ref()
            .is_some_and(|category| existing.metadata.category != *category)
        || update
            .targets
            .as_ref()
            .is_some_and(|targets| existing.metadata.targets != *targets)
        || update
            .body_markdown
            .as_ref()
            .is_some_and(|body| existing.body_markdown != *body)
        || update
            .support_files
            .as_ref()
            .is_some_and(|support_files| existing.support_files != *support_files)
        || update
            .pinned
            .is_some_and(|pinned| existing.metadata.pinned != pinned);
    if !changes_existing {
        return Err(format!(
            "update proposal does not change managed skill id '{id}'"
        ));
    }
    Ok((id, base_checksum, update))
}

fn proposal_targets_or_default(
    value: Option<&Value>,
) -> std::result::Result<Vec<SkillInstallTarget>, String> {
    optional_proposal_targets(value)
        .map(|targets| targets.unwrap_or_else(default_managed_skill_targets))
}

fn optional_proposal_targets(
    value: Option<&Value>,
) -> std::result::Result<Option<Vec<SkillInstallTarget>>, String> {
    let Some(value) = value else {
        return Ok(None);
    };
    let array = value
        .as_array()
        .ok_or_else(|| "targets must be an array".to_string())?;
    let mut targets = Vec::new();
    for value in array {
        let target = required_proposal_string(Some(value), "targets[]")?;
        targets.push(parse_skill_install_target(&target)?);
    }
    Ok(Some(targets))
}

fn parse_skill_install_target(value: &str) -> std::result::Result<SkillInstallTarget, String> {
    match value {
        "cursor" => Ok(SkillInstallTarget::Cursor),
        "codex" => Ok(SkillInstallTarget::Codex),
        "claude" => Ok(SkillInstallTarget::Claude),
        "agents" | "prompt_only" | "prompt-only" => Ok(SkillInstallTarget::Agents),
        "opencode" | "open_code" | "open-code" => Ok(SkillInstallTarget::OpenCode),
        "kimi" => Ok(SkillInstallTarget::Kimi),
        "kiro" => Ok(SkillInstallTarget::Kiro),
        "hermes" => Ok(SkillInstallTarget::Hermes),
        other => Err(format!("unsupported managed skill target '{other}'")),
    }
}

fn support_files_from_proposal(
    value: Option<&Value>,
) -> std::result::Result<Vec<ManagedSupportFile>, String> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let array = value
        .as_array()
        .ok_or_else(|| "support_files must be an array".to_string())?;
    if array.len() > 20 {
        return Err("support_files exceeds 20 files".to_string());
    }
    let mut files = Vec::new();
    for item in array {
        let object = item
            .as_object()
            .ok_or_else(|| "support file must be a JSON object".to_string())?;
        let path = required_proposal_string(object.get("path"), "support_files.path")?;
        let text = required_proposal_string(object.get("text"), "support_files.text")?;
        if text.len() > 64 * 1024 {
            return Err(format!("support file '{path}' exceeds 64KiB"));
        }
        files.push(
            ManagedSupportFile::new(Path::new(&path), text.into_bytes())
                .map_err(|err| err.to_string())?,
        );
    }
    Ok(files)
}

fn required_proposal_string(
    value: Option<&Value>,
    field: &str,
) -> std::result::Result<String, String> {
    value
        .and_then(Value::as_str)
        .and_then(normalized_non_empty)
        .ok_or_else(|| format!("{field} is required"))
}

fn optional_proposal_string(value: Option<&Value>) -> std::result::Result<Option<String>, String> {
    value
        .map(|value| required_proposal_string(Some(value), "optional string field"))
        .transpose()
}

fn rejected_skill(proposal: &Value, reason: &str) -> Value {
    json!({
        "proposal": proposal,
        "reason": reason,
    })
}

fn normalized_non_empty(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() {
        None
    } else {
        Some(value.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn managed_skill_exports_only_refresh_for_the_user_profile() {
        let home = Path::new("/home/test-user");
        assert!(crate::agents::uses_default_user_profile(
            home,
            Path::new("/home/test-user/.tracedecay"),
        ));
        assert!(!crate::agents::uses_default_user_profile(
            home,
            Path::new("/tmp/tracedecay-test-profile"),
        ));
    }

    fn assert_err_eq<T>(result: std::result::Result<T, String>, expected: &str) {
        match result {
            Ok(_) => panic!("expected error: {expected}"),
            Err(err) => assert_eq!(err, expected),
        }
    }

    #[test]
    fn proposal_targets_accept_known_aliases() -> std::result::Result<(), String> {
        let targets = optional_proposal_targets(Some(&json!([
            "cursor",
            "prompt-only",
            "open_code",
            "kiro"
        ])))?
        .ok_or_else(|| "targets should be present".to_string())?;

        assert_eq!(
            targets,
            vec![
                SkillInstallTarget::Cursor,
                SkillInstallTarget::Agents,
                SkillInstallTarget::OpenCode,
                SkillInstallTarget::Kiro,
            ]
        );
        Ok(())
    }

    #[test]
    fn proposal_targets_reject_unknown_or_malformed_values() {
        assert_err_eq(
            optional_proposal_targets(Some(&json!("cursor"))),
            "targets must be an array",
        );
        assert_err_eq(
            optional_proposal_targets(Some(&json!(["cursor", "unknown"]))),
            "unsupported managed skill target 'unknown'",
        );
        assert_err_eq(
            optional_proposal_targets(Some(&json!(["  "]))),
            "targets[] is required",
        );
    }

    #[test]
    fn support_files_from_proposal_builds_managed_support_files() -> std::result::Result<(), String>
    {
        let files = support_files_from_proposal(Some(&json!([
            {
                "path": "references/example.md",
                "text": "hello"
            },
            {
                "path": "scripts/check.sh",
                "text": "#!/usr/bin/env bash\n"
            }
        ])))?;

        assert_eq!(files.len(), 2);
        assert_eq!(files[0].path, Path::new("references/example.md"));
        assert_eq!(files[0].bytes, b"hello");
        assert_eq!(files[1].path, Path::new("scripts/check.sh"));
        Ok(())
    }

    #[test]
    fn support_files_from_proposal_rejects_unsafe_or_invalid_entries() {
        assert_err_eq(
            support_files_from_proposal(Some(&json!({}))),
            "support_files must be an array",
        );
        assert_err_eq(
            support_files_from_proposal(Some(&json!([{"path": "../escape.md", "text": "x"}]))),
            "config error: unsafe support path '../escape.md'",
        );
        assert_err_eq(
            support_files_from_proposal(Some(&json!([{"path": "references/missing.md"}]))),
            "support_files.text is required",
        );
    }

    #[test]
    fn scheduled_job_skill_reference_blocks_archive() {
        let temp = tempdir().unwrap();
        let dashboard = temp.path().join("projects/p1/dashboard");
        std::fs::create_dir_all(&dashboard).unwrap();
        std::fs::write(
            dashboard.join("automation_jobs.json"),
            br#"{"schema_version":1,"jobs":[{"skill_ids":["keep-me"]}]}"#,
        )
        .unwrap();

        assert!(
            ensure_skill_not_referenced_by_scheduled_job(temp.path(), "keep-me")
                .unwrap_err()
                .contains("referenced")
        );
        ensure_skill_not_referenced_by_scheduled_job(temp.path(), "other").unwrap();
    }
}
