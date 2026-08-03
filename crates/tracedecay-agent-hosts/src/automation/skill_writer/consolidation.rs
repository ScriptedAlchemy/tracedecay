use std::collections::BTreeMap;
use std::path::Path;

use serde_json::{Value, json};

use super::super::managed_skills::{
    ManagedSkill, ManagedSkillSource, ManagedSkillState, ManagedSkillUpdate,
    discard_pending_managed_skill_update, stage_managed_skill_archive, stage_managed_skill_update,
};
use super::{
    SkillProposalAction, optional_proposal_string, optional_proposal_targets,
    required_proposal_string, support_files_from_proposal,
};
use crate::errors::Result;

#[derive(Debug, Clone)]
pub(super) struct SkillArchiveProposal {
    pub(super) skill_id: String,
    pub(super) base_checksum: String,
    pub(super) reason: String,
}

#[derive(Debug, Clone)]
pub(super) struct SkillMergeProposal {
    pub(super) target_skill_id: String,
    pub(super) base_checksum: String,
    pub(super) source_skill_id: String,
    pub(super) source_base_checksum: String,
    pub(super) reason: String,
    pub(super) update: Option<ManagedSkillUpdate>,
}

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
    if skill.metadata.provenance.source != ManagedSkillSource::AutomationRun {
        return Err(format!("managed skill '{id}' is not automation-owned"));
    }
    if skill.metadata.state == ManagedSkillState::Archived {
        return Err(format!("managed skill '{id}' is already archived"));
    }
    if skill.pending_update.is_some() {
        return Err(format!("managed skill '{id}' already has a pending update"));
    }
    Ok(skill)
}

pub(super) fn skill_archive_from_proposal(
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

pub(super) fn skill_merge_from_proposal(
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

pub(super) async fn stage_skill_merge(
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

pub(super) fn staged_consolidation_record(
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

#[cfg(test)]
mod tests {
    use super::super::super::managed_skills::{
        ManagedSkillDraft, ManagedSkillPendingUpdate, ManagedSkillProvenance,
        default_managed_skill_targets,
    };
    use super::super::skill_proposal_action;
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
            fixture_skill("imported-skill", ManagedSkillSource::Import, false),
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
            "managed skill 'user-skill' is not automation-owned",
        );
        assert_err_eq(
            skill_archive_from_proposal(
                &json!({
                    "action": "archive",
                    "id": "imported-skill",
                    "base_checksum": checksum(&skills, "imported-skill"),
                    "reason": "x"
                }),
                &skills,
            ),
            "managed skill 'imported-skill' is not automation-owned",
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
}
