use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde_json::{json, Value};

use super::artifacts::sha256_bytes;
use super::config::AutomationConfig;
use super::managed_skills::{
    approve_managed_skill, create_managed_skill_draft, default_managed_skill_targets,
    discard_pending_managed_skill_update, list_managed_skills, stage_managed_skill_archive,
    stage_managed_skill_update, ManagedSkill, ManagedSkillDraft, ManagedSkillProvenance,
    ManagedSkillSource, ManagedSkillState, ManagedSkillUpdate, ManagedSupportFile,
    SkillInstallTarget,
};
use super::skill_usage::{
    skill_improvement_recommendations as usage_skill_improvement_recommendations,
    SkillOverlapCandidate, SkillStaleRecommendation, SkillUsageSummary,
};
use super::text::truncate_chars_for_prompt;
use crate::analytics::ToolFamilySignal;
use crate::errors::Result;
use crate::sessions::lcm::LcmGrepHit;

/// Outcome of validating and applying one batch of `skill_writer` proposals.
/// `consolidations` holds staged merge/archive records; consolidations are
/// always staged for dashboard approval and never auto-applied, even when
/// `auto_enable_skills` is set.
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
    for proposal in proposals {
        match skill_proposal_action(proposal) {
            Ok(SkillProposalAction::Create) => {
                match skill_draft_from_proposal(proposal, run_id, &existing_ids) {
                    Ok(draft) => {
                        existing_ids.insert(draft.id.clone());
                        match create_managed_skill_draft(profile_root, draft).await {
                            Ok(skill) => {
                                let skill = if auto_enable_skills {
                                    approve_managed_skill(profile_root, &skill.metadata.id).await?
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
                                    approve_managed_skill(profile_root, &skill.metadata.id).await?
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
    Ok(SkillProposalOutcome {
        created,
        updated,
        consolidations,
        rejected,
    })
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

#[derive(Debug, Clone)]
struct SkillArchiveProposal {
    skill_id: String,
    base_checksum: String,
    reason: String,
}

#[derive(Debug, Clone)]
struct SkillMergeProposal {
    target_skill_id: String,
    base_checksum: String,
    source_skill_id: String,
    source_base_checksum: String,
    reason: String,
    update: Option<ManagedSkillUpdate>,
}

/// Shared safety gate for consolidation (merge/archive) targets: the skill
/// must exist, its checksum must match, it must not be pinned (Hermes-parity
/// pinned exemption), must not be user-authored, must not already be
/// archived, and must not already carry a staged update.
fn consolidation_guard<'a>(
    existing_skills: &'a BTreeMap<String, ManagedSkill>,
    id: &str,
    base_checksum: &str,
    role: &str,
) -> std::result::Result<&'a ManagedSkill, String> {
    let skill = existing_skills
        .get(id)
        .ok_or_else(|| format!("{role} managed skill id '{id}' does not exist"))?;
    if base_checksum != skill.metadata.checksum {
        return Err(format!(
            "base_checksum for managed skill id '{id}' is stale"
        ));
    }
    if skill.metadata.pinned {
        return Err(format!(
            "managed skill '{id}' is pinned and exempt from consolidation"
        ));
    }
    if skill.metadata.provenance.source == ManagedSkillSource::UserDraft {
        return Err(format!(
            "managed skill '{id}' is user-authored and exempt from consolidation"
        ));
    }
    if skill.metadata.state == ManagedSkillState::Archived {
        return Err(format!("managed skill '{id}' is already archived"));
    }
    if skill.pending_update.is_some() {
        return Err(format!("managed skill '{id}' already has a pending update"));
    }
    Ok(skill)
}

fn skill_archive_from_proposal(
    proposal: &Value,
    existing_skills: &BTreeMap<String, ManagedSkill>,
) -> std::result::Result<SkillArchiveProposal, String> {
    let object = proposal
        .as_object()
        .ok_or_else(|| "proposal must be a JSON object".to_string())?;
    let id = required_proposal_string(object.get("id"), "id")?;
    let base_checksum = required_proposal_string(object.get("base_checksum"), "base_checksum")?;
    let reason = required_proposal_string(object.get("reason"), "reason")?;
    consolidation_guard(existing_skills, &id, &base_checksum, "archive")?;
    Ok(SkillArchiveProposal {
        skill_id: id,
        base_checksum,
        reason,
    })
}

fn skill_merge_from_proposal(
    proposal: &Value,
    existing_skills: &BTreeMap<String, ManagedSkill>,
) -> std::result::Result<SkillMergeProposal, String> {
    let object = proposal
        .as_object()
        .ok_or_else(|| "proposal must be a JSON object".to_string())?;
    let target_skill_id = required_proposal_string(object.get("id"), "id")?;
    let base_checksum = required_proposal_string(object.get("base_checksum"), "base_checksum")?;
    let source_skill_id =
        required_proposal_string(object.get("source_skill_id"), "source_skill_id")?;
    let source_base_checksum =
        required_proposal_string(object.get("source_base_checksum"), "source_base_checksum")?;
    let reason = required_proposal_string(object.get("reason"), "reason")?;
    if source_skill_id == target_skill_id {
        return Err("merge proposal source_skill_id must differ from id".to_string());
    }
    let target = consolidation_guard(existing_skills, &target_skill_id, &base_checksum, "merge")?;
    consolidation_guard(
        existing_skills,
        &source_skill_id,
        &source_base_checksum,
        "merge source",
    )?;

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
        pinned: None,
    };
    let has_update = update.title.is_some()
        || update.summary.is_some()
        || update.category.is_some()
        || update.targets.is_some()
        || update.body_markdown.is_some()
        || update.support_files.is_some();
    if has_update && !merge_update_changes_target(&update, target) {
        return Err(format!(
            "merge proposal does not change managed skill id '{target_skill_id}'"
        ));
    }
    Ok(SkillMergeProposal {
        target_skill_id,
        base_checksum,
        source_skill_id,
        source_base_checksum,
        reason,
        update: has_update.then_some(update),
    })
}

fn merge_update_changes_target(update: &ManagedSkillUpdate, target: &ManagedSkill) -> bool {
    update
        .title
        .as_ref()
        .is_some_and(|title| target.metadata.title != *title)
        || update
            .summary
            .as_ref()
            .is_some_and(|summary| target.metadata.summary != *summary)
        || update
            .category
            .as_ref()
            .is_some_and(|category| target.metadata.category != *category)
        || update
            .targets
            .as_ref()
            .is_some_and(|targets| target.metadata.targets != *targets)
        || update
            .body_markdown
            .as_ref()
            .is_some_and(|body| target.body_markdown != *body)
        || update
            .support_files
            .as_ref()
            .is_some_and(|support_files| target.support_files != *support_files)
}

/// Stages a merge: an optional content update on the surviving skill plus an
/// archive on the absorbed skill. Both land as pending updates for dashboard
/// approval; nothing is auto-applied and no skill content is deleted. If the
/// archive staging fails after the target update was staged, the target
/// staging is rolled back so a merge never half-lands.
async fn stage_skill_merge(
    profile_root: &Path,
    merge: &SkillMergeProposal,
) -> Result<(ManagedSkill, Option<ManagedSkill>)> {
    let staged_target = match &merge.update {
        Some(update) => Some(
            stage_managed_skill_update(
                profile_root,
                &merge.target_skill_id,
                &merge.base_checksum,
                update.clone(),
            )
            .await?,
        ),
        None => None,
    };
    let archive_reason = format!("merged into '{}': {}", merge.target_skill_id, merge.reason);
    let staged_source = match stage_managed_skill_archive(
        profile_root,
        &merge.source_skill_id,
        &merge.source_base_checksum,
        Some(archive_reason),
    )
    .await
    {
        Ok(skill) => skill,
        Err(err) => {
            if staged_target.is_some() {
                discard_pending_managed_skill_update(profile_root, &merge.target_skill_id).await?;
            }
            return Err(err);
        }
    };
    Ok((staged_source, staged_target))
}

fn staged_consolidation_record(
    action: SkillProposalAction,
    proposal: &Value,
    staged_source: &ManagedSkill,
    archive_base_checksum: &str,
    merge: Option<&SkillMergeProposal>,
) -> Value {
    let reason = proposal.get("reason").cloned().unwrap_or(Value::Null);
    let mut record = json!({
        "action": action.as_str(),
        "proposal_action": action.as_str(),
        "reason": reason.clone(),
        "proposal_reason": reason,
        "approval_status": "staged_consolidation",
        "resulting_state": "archived",
        "archived_skill_id": staged_source.metadata.id,
    });
    if let Some(object) = record.as_object_mut() {
        if let Some(merge) = merge {
            object.insert("target_skill_id".to_string(), json!(merge.target_skill_id));
            object.insert("base_checksum".to_string(), json!(merge.base_checksum));
            object.insert("source_skill_id".to_string(), json!(merge.source_skill_id));
            object.insert(
                "source_base_checksum".to_string(),
                json!(merge.source_base_checksum),
            );
            object.insert(
                "target_update_staged".to_string(),
                json!(merge.update.is_some()),
            );
        } else {
            object.insert(
                "target_skill_id".to_string(),
                json!(staged_source.metadata.id),
            );
            object.insert("base_checksum".to_string(), json!(archive_base_checksum));
        }
    }
    record
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
    use super::super::managed_skills::ManagedSkillPendingUpdate;
    use super::*;

    fn assert_err_eq<T>(result: std::result::Result<T, String>, expected: &str) {
        match result {
            Ok(_) => panic!("expected error: {expected}"),
            Err(err) => assert_eq!(err, expected),
        }
    }

    fn assert_ok<T>(result: std::result::Result<T, String>) -> T {
        match result {
            Ok(value) => value,
            Err(err) => panic!("expected ok, got error: {err}"),
        }
    }

    fn fixture_skill(id: &str, source: ManagedSkillSource, pinned: bool) -> ManagedSkill {
        let draft = ManagedSkillDraft {
            id: id.to_string(),
            title: format!("{id} guidance"),
            summary: format!("Guidance for {id}."),
            category: "workflow".to_string(),
            targets: default_managed_skill_targets(),
            body_markdown: format!("Follow the {id} workflow before applying changes."),
            support_files: Vec::new(),
            provenance: ManagedSkillProvenance {
                source,
                actor: "test".to_string(),
                run_id: None,
            },
        };
        let mut skill = match draft.materialize() {
            Ok(skill) => skill,
            Err(err) => panic!("test fixture skill should materialize: {err}"),
        };
        skill.set_state(ManagedSkillState::Active);
        skill.set_pinned(pinned);
        skill
    }

    fn consolidation_fixture() -> BTreeMap<String, ManagedSkill> {
        let mut archived =
            fixture_skill("archived-skill", ManagedSkillSource::AutomationRun, false);
        archived.set_state(ManagedSkillState::Archived);
        [
            fixture_skill("workflow-a", ManagedSkillSource::AutomationRun, false),
            fixture_skill("workflow-b", ManagedSkillSource::AutomationRun, false),
            fixture_skill("pinned-skill", ManagedSkillSource::AutomationRun, true),
            fixture_skill("user-skill", ManagedSkillSource::UserDraft, false),
            archived,
        ]
        .into_iter()
        .map(|skill| (skill.metadata.id.clone(), skill))
        .collect()
    }

    fn checksum(skills: &BTreeMap<String, ManagedSkill>, id: &str) -> String {
        skills[id].metadata.checksum.clone()
    }

    #[test]
    fn archive_proposals_validate_ids_checksums_and_exemptions() {
        let skills = consolidation_fixture();
        let valid = assert_ok(skill_archive_from_proposal(
            &json!({
                "action": "archive",
                "id": "workflow-a",
                "base_checksum": checksum(&skills, "workflow-a"),
                "reason": "unused overlap"
            }),
            &skills,
        ));
        assert_eq!(valid.skill_id, "workflow-a");
        assert_eq!(valid.reason, "unused overlap");

        assert_err_eq(
            skill_archive_from_proposal(
                &json!({"action": "archive", "id": "workflow-a", "reason": "x"}),
                &skills,
            ),
            "base_checksum is required",
        );
        assert_err_eq(
            skill_archive_from_proposal(
                &json!({
                    "action": "archive",
                    "id": "workflow-a",
                    "base_checksum": checksum(&skills, "workflow-a")
                }),
                &skills,
            ),
            "reason is required",
        );
        assert_err_eq(
            skill_archive_from_proposal(
                &json!({
                    "action": "archive",
                    "id": "missing",
                    "base_checksum": "sha256:0000",
                    "reason": "x"
                }),
                &skills,
            ),
            "archive managed skill id 'missing' does not exist",
        );
        assert_err_eq(
            skill_archive_from_proposal(
                &json!({
                    "action": "archive",
                    "id": "workflow-a",
                    "base_checksum": "sha256:stale",
                    "reason": "x"
                }),
                &skills,
            ),
            "base_checksum for managed skill id 'workflow-a' is stale",
        );
        assert_err_eq(
            skill_archive_from_proposal(
                &json!({
                    "action": "archive",
                    "id": "pinned-skill",
                    "base_checksum": checksum(&skills, "pinned-skill"),
                    "reason": "x"
                }),
                &skills,
            ),
            "managed skill 'pinned-skill' is pinned and exempt from consolidation",
        );
        assert_err_eq(
            skill_archive_from_proposal(
                &json!({
                    "action": "archive",
                    "id": "user-skill",
                    "base_checksum": checksum(&skills, "user-skill"),
                    "reason": "x"
                }),
                &skills,
            ),
            "managed skill 'user-skill' is user-authored and exempt from consolidation",
        );
        assert_err_eq(
            skill_archive_from_proposal(
                &json!({
                    "action": "archive",
                    "id": "archived-skill",
                    "base_checksum": checksum(&skills, "archived-skill"),
                    "reason": "x"
                }),
                &skills,
            ),
            "managed skill 'archived-skill' is already archived",
        );
    }

    #[test]
    fn archive_proposals_reject_targets_with_pending_updates() {
        let mut skills = consolidation_fixture();
        let pending_metadata = skills["workflow-a"].metadata.clone();
        if let Some(skill) = skills.get_mut("workflow-a") {
            skill.pending_update = Some(ManagedSkillPendingUpdate {
                base_checksum: pending_metadata.checksum.clone(),
                staged_at: 1,
                metadata: pending_metadata,
                body_markdown: "staged body".to_string(),
                support_files: Vec::new(),
                resulting_state: None,
                staged_reason: None,
            });
        }
        assert_err_eq(
            skill_archive_from_proposal(
                &json!({
                    "action": "archive",
                    "id": "workflow-a",
                    "base_checksum": checksum(&skills, "workflow-a"),
                    "reason": "x"
                }),
                &skills,
            ),
            "managed skill 'workflow-a' already has a pending update",
        );
    }

    #[test]
    fn merge_proposals_validate_source_target_and_checksums() {
        let skills = consolidation_fixture();
        let merge = assert_ok(skill_merge_from_proposal(
            &json!({
                "action": "merge",
                "id": "workflow-a",
                "base_checksum": checksum(&skills, "workflow-a"),
                "source_skill_id": "workflow-b",
                "source_base_checksum": checksum(&skills, "workflow-b"),
                "body_markdown": "Merged workflow guidance covering both variants.",
                "reason": "duplicate guidance"
            }),
            &skills,
        ));
        assert_eq!(merge.target_skill_id, "workflow-a");
        assert_eq!(merge.source_skill_id, "workflow-b");
        assert!(merge.update.is_some());

        let archive_only = assert_ok(skill_merge_from_proposal(
            &json!({
                "action": "merge",
                "id": "workflow-a",
                "base_checksum": checksum(&skills, "workflow-a"),
                "source_skill_id": "workflow-b",
                "source_base_checksum": checksum(&skills, "workflow-b"),
                "reason": "target already covers the source"
            }),
            &skills,
        ));
        assert!(archive_only.update.is_none());

        assert_err_eq(
            skill_merge_from_proposal(
                &json!({
                    "action": "merge",
                    "id": "workflow-a",
                    "base_checksum": checksum(&skills, "workflow-a"),
                    "source_skill_id": "workflow-a",
                    "source_base_checksum": checksum(&skills, "workflow-a"),
                    "reason": "x"
                }),
                &skills,
            ),
            "merge proposal source_skill_id must differ from id",
        );
        assert_err_eq(
            skill_merge_from_proposal(
                &json!({
                    "action": "merge",
                    "id": "workflow-a",
                    "base_checksum": checksum(&skills, "workflow-a"),
                    "source_skill_id": "workflow-b",
                    "source_base_checksum": "sha256:stale",
                    "reason": "x"
                }),
                &skills,
            ),
            "base_checksum for managed skill id 'workflow-b' is stale",
        );
        assert_err_eq(
            skill_merge_from_proposal(
                &json!({
                    "action": "merge",
                    "id": "pinned-skill",
                    "base_checksum": checksum(&skills, "pinned-skill"),
                    "source_skill_id": "workflow-b",
                    "source_base_checksum": checksum(&skills, "workflow-b"),
                    "reason": "x"
                }),
                &skills,
            ),
            "managed skill 'pinned-skill' is pinned and exempt from consolidation",
        );
        assert_err_eq(
            skill_merge_from_proposal(
                &json!({
                    "action": "merge",
                    "id": "workflow-a",
                    "base_checksum": checksum(&skills, "workflow-a"),
                    "source_skill_id": "pinned-skill",
                    "source_base_checksum": checksum(&skills, "pinned-skill"),
                    "reason": "x"
                }),
                &skills,
            ),
            "managed skill 'pinned-skill' is pinned and exempt from consolidation",
        );
        assert_err_eq(
            skill_merge_from_proposal(
                &json!({
                    "action": "merge",
                    "id": "workflow-a",
                    "base_checksum": checksum(&skills, "workflow-a"),
                    "source_skill_id": "workflow-b",
                    "source_base_checksum": checksum(&skills, "workflow-b"),
                    "body_markdown": skills["workflow-a"].body_markdown,
                    "reason": "x"
                }),
                &skills,
            ),
            "merge proposal does not change managed skill id 'workflow-a'",
        );
    }

    #[test]
    fn consolidation_actions_parse_from_proposals() {
        assert_eq!(
            assert_ok(skill_proposal_action(&json!({"action": "merge"}))),
            SkillProposalAction::Merge
        );
        assert_eq!(
            assert_ok(skill_proposal_action(&json!({"action": "consolidate"}))),
            SkillProposalAction::Merge
        );
        assert_eq!(
            assert_ok(skill_proposal_action(&json!({"action": "archive"}))),
            SkillProposalAction::Archive
        );
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
}
