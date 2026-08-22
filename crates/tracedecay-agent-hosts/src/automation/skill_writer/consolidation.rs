use std::collections::BTreeMap;
use std::path::Path;

use serde_json::{Value, json};
use tracedecay_automation::run_labels::SKILL_OVERLAP_REMOVAL_TOMBSTONE;

use super::super::managed_skills::{
    ManagedSkill, ManagedSkillSource, ManagedSkillState, ManagedSkillUpdate,
    apply_managed_skill_overlap_archive, apply_managed_skill_overlap_consolidation,
    preview_managed_skill_update,
};
use super::super::skill_usage::{DEFAULT_SKILL_OVERLAP_LIMIT, skill_overlap_candidates};
use super::{
    SkillProposalAction, optional_proposal_string, optional_proposal_targets,
    required_proposal_string, support_files_from_proposal,
};
use crate::errors::Result;

#[derive(Debug, Clone)]
pub(super) struct SkillArchiveProposal {
    pub(super) skill_id: String,
    pub(super) base_checksum: String,
    pub(super) overlap_skill_id: String,
    pub(super) overlap_base_checksum: String,
}

#[derive(Debug, Clone)]
pub(super) struct SkillMergeProposal {
    pub(super) target_skill_id: String,
    pub(super) base_checksum: String,
    pub(super) source_skill_id: String,
    pub(super) source_base_checksum: String,
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
    Ok(skill)
}

fn required_consolidation_reason(value: Option<&Value>) -> std::result::Result<String, String> {
    let reason = required_proposal_string(value, "reason")?;
    if reason == SKILL_OVERLAP_REMOVAL_TOMBSTONE {
        return Err("reason must not reuse the reserved skill-overlap tombstone label".to_string());
    }
    Ok(reason)
}

fn is_detected_overlap_pair(
    existing_skills: &BTreeMap<String, ManagedSkill>,
    first_skill_id: &str,
    second_skill_id: &str,
) -> bool {
    let skills = existing_skills.values().cloned().collect::<Vec<_>>();
    skill_overlap_candidates(&skills, DEFAULT_SKILL_OVERLAP_LIMIT)
        .iter()
        .any(|candidate| {
            (candidate.skill_a == first_skill_id && candidate.skill_b == second_skill_id)
                || (candidate.skill_a == second_skill_id && candidate.skill_b == first_skill_id)
        })
}

fn detected_overlap_partner(
    existing_skills: &BTreeMap<String, ManagedSkill>,
    skill_id: &str,
) -> Option<String> {
    let skills = existing_skills.values().cloned().collect::<Vec<_>>();
    skill_overlap_candidates(&skills, DEFAULT_SKILL_OVERLAP_LIMIT)
        .into_iter()
        .find_map(|candidate| {
            if candidate.skill_a == skill_id {
                Some(candidate.skill_b)
            } else if candidate.skill_b == skill_id {
                Some(candidate.skill_a)
            } else {
                None
            }
        })
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
    required_consolidation_reason(object.get("reason"))?;
    consolidation_guard(existing_skills, &id, &base_checksum, "archive")?;
    let overlap_skill_id = detected_overlap_partner(existing_skills, &id)
        .ok_or_else(|| format!("managed skill '{id}' is not a detected overlap candidate"))?;
    let overlap_base_checksum = existing_skills
        .get(&overlap_skill_id)
        .ok_or_else(|| {
            format!("detected overlap partner managed skill id '{overlap_skill_id}' does not exist")
        })?
        .metadata
        .checksum
        .clone();
    consolidation_guard(
        existing_skills,
        &overlap_skill_id,
        &overlap_base_checksum,
        "archive overlap partner",
    )?;
    Ok(SkillArchiveProposal {
        skill_id: id,
        base_checksum,
        overlap_skill_id,
        overlap_base_checksum,
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
    required_consolidation_reason(object.get("reason"))?;
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
    if !is_detected_overlap_pair(existing_skills, &target_skill_id, &source_skill_id) {
        return Err(format!(
            "managed skills '{target_skill_id}' and '{source_skill_id}' are not a detected overlap candidate pair"
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
        pinned: None,
    };
    let has_update = update.title.is_some()
        || update.summary.is_some()
        || update.category.is_some()
        || update.targets.is_some()
        || update.body_markdown.is_some()
        || update.support_files.is_some();
    if has_update {
        preview_managed_skill_update(target, &update).map_err(|error| error.to_string())?;
    }
    Ok(SkillMergeProposal {
        target_skill_id,
        base_checksum,
        source_skill_id,
        source_base_checksum,
        update: has_update.then_some(update),
    })
}

/// Applies an archive as one checksum-fenced, crash-recoverable lifecycle
/// transaction whose committed revision durably carries the typed
/// skill-overlap removal tombstone as its archived reason.
pub(super) async fn apply_skill_archive(
    profile_root: &Path,
    archive: &SkillArchiveProposal,
) -> Result<ManagedSkill> {
    apply_managed_skill_overlap_archive(
        profile_root,
        &archive.skill_id,
        &archive.base_checksum,
        &archive.overlap_skill_id,
        &archive.overlap_base_checksum,
    )
    .await
}

/// Applies a merge as one checksum-fenced, crash-recoverable lifecycle
/// transaction. The source stays on disk in `Archived` state, preserving its
/// provenance without leaving an intermediate revision behind.
pub(super) async fn apply_skill_merge(
    profile_root: &Path,
    merge: &SkillMergeProposal,
) -> Result<(ManagedSkill, Option<ManagedSkill>)> {
    let result = apply_managed_skill_overlap_consolidation(
        profile_root,
        &merge.target_skill_id,
        &merge.base_checksum,
        merge.update.clone(),
        &merge.source_skill_id,
        &merge.source_base_checksum,
    )
    .await?;
    Ok((result.source, result.target))
}

pub(super) fn applied_consolidation_record(
    action: SkillProposalAction,
    proposal: &Value,
    applied_source: &ManagedSkill,
    archive_base_checksum: &str,
    merge: Option<&SkillMergeProposal>,
) -> Value {
    let reason = proposal.get("reason").cloned().unwrap_or(Value::Null);
    let mut record = json!({
        "action": action.as_str(),
        "proposal_action": action.as_str(),
        "reason": reason.clone(),
        "proposal_reason": reason,
        "application_status": "applied",
        "resulting_state": "archived",
        "archived_skill_id": applied_source.metadata.id,
        // Canonical typed removal-tombstone label; consumers must key on this
        // constant, not on the free-form proposal reason above.
        "tombstone_label": SKILL_OVERLAP_REMOVAL_TOMBSTONE,
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
                "target_update_applied".to_string(),
                json!(merge.update.is_some()),
            );
        } else {
            object.insert(
                "target_skill_id".to_string(),
                json!(applied_source.metadata.id),
            );
            object.insert("base_checksum".to_string(), json!(archive_base_checksum));
        }
    }
    record
}

#[cfg(test)]
mod tests {
    use super::super::super::managed_skills::{
        ManagedSkillDraft, ManagedSkillProvenance, ManagedSupportFile, apply_managed_skill_update,
        create_managed_skill, default_managed_skill_targets, load_managed_skill,
    };
    #[cfg(unix)]
    use super::super::super::skill_usage::skill_usage_ledger_path;
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

    fn fixture_draft(id: &str, source: ManagedSkillSource) -> ManagedSkillDraft {
        ManagedSkillDraft {
            id: id.to_string(),
            title: format!("{id} guidance"),
            summary: format!("Guidance for {id}."),
            category: "workflow".to_string(),
            targets: default_managed_skill_targets(),
            body_markdown: format!("Follow the {id} workflow before applying changes."),
            support_files: vec![
                ManagedSupportFile::new(
                    format!("references/{id}.md"),
                    format!("Reference material for {id}.").into_bytes(),
                )
                .unwrap(),
            ],
            provenance: ManagedSkillProvenance {
                source,
                actor: format!("{id}-author"),
                run_id: Some(format!("{id}-run")),
            },
        }
    }

    fn fixture_skill(id: &str, source: ManagedSkillSource, pinned: bool) -> ManagedSkill {
        let mut skill = match fixture_draft(id, source).materialize() {
            Ok(skill) => skill,
            Err(err) => panic!("test fixture skill should materialize: {err}"),
        };
        skill.set_state(ManagedSkillState::Active);
        skill.set_pinned(pinned);
        skill
    }

    fn unrelated_automation_skill() -> ManagedSkill {
        let mut draft = fixture_draft("rust-error-handling", ManagedSkillSource::AutomationRun);
        draft.title = "Rust error handling".to_string();
        draft.summary = "Model library failures with explicit error enums.".to_string();
        draft.body_markdown =
            "Convert IO failures at module boundaries and reserve panics for invariants."
                .to_string();
        let mut skill = draft
            .materialize()
            .unwrap_or_else(|err| panic!("unrelated skill should materialize: {err}"));
        skill.set_state(ManagedSkillState::Active);
        skill
    }

    fn overlapping_automation_skill(id: &str, title: &str) -> ManagedSkill {
        let mut draft = fixture_draft(id, ManagedSkillSource::AutomationRun);
        draft.title = title.to_string();
        draft.summary = "Review automatically applied automation run outcomes.".to_string();
        draft.body_markdown = "Check run ledger counts, rejected proposals, and deployment receipts after automatic application.".to_string();
        let mut skill = draft
            .materialize()
            .unwrap_or_else(|err| panic!("overlapping skill should materialize: {err}"));
        skill.set_state(ManagedSkillState::Active);
        skill
    }

    fn consolidation_fixture() -> BTreeMap<String, ManagedSkill> {
        let mut archived =
            fixture_skill("archived-skill", ManagedSkillSource::AutomationRun, false);
        archived.set_state(ManagedSkillState::Archived);
        [
            overlapping_automation_skill("workflow-a", "Automation run review"),
            overlapping_automation_skill("workflow-b", "Review automation runs"),
            fixture_skill("pinned-skill", ManagedSkillSource::AutomationRun, true),
            fixture_skill("user-skill", ManagedSkillSource::User, false),
            fixture_skill("imported-skill", ManagedSkillSource::Import, false),
            archived,
        ]
        .into_iter()
        .map(|skill| (skill.metadata.id.clone(), skill))
        .collect()
    }

    async fn persisted_consolidation_fixture(
        profile_root: &Path,
    ) -> BTreeMap<String, ManagedSkill> {
        let first = create_managed_skill(
            profile_root,
            fixture_draft("workflow-a", ManagedSkillSource::AutomationRun),
        )
        .await
        .unwrap();
        let second = create_managed_skill(
            profile_root,
            fixture_draft("workflow-b", ManagedSkillSource::AutomationRun),
        )
        .await
        .unwrap();
        [
            (first.metadata.id.clone(), first),
            (second.metadata.id.clone(), second),
        ]
        .into_iter()
        .collect()
    }

    fn checksum(skills: &BTreeMap<String, ManagedSkill>, id: &str) -> String {
        skills[id].metadata.checksum.clone()
    }

    #[cfg(unix)]
    fn replace_usage_ledger_with_blocking_fifo(
        profile_root: &Path,
    ) -> (std::fs::File, std::path::PathBuf) {
        let ledger_path = skill_usage_ledger_path(profile_root);
        match std::fs::remove_file(&ledger_path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => panic!("remove usage ledger before FIFO replacement: {error}"),
        }
        assert!(
            std::process::Command::new("mkfifo")
                .arg(&ledger_path)
                .status()
                .expect("run mkfifo")
                .success(),
            "the production usage-ledger read must block on a real FIFO"
        );
        let control = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&ledger_path)
            .expect("open FIFO control handle without blocking");
        (control, ledger_path)
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
        assert_eq!(valid.overlap_skill_id, "workflow-b");
        assert_eq!(valid.overlap_base_checksum, checksum(&skills, "workflow-b"));

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
            "config error: managed skill 'workflow-a' update does not change the active revision",
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
    fn consolidation_proposals_require_a_detected_overlap() {
        let mut skills = consolidation_fixture();
        let unrelated = unrelated_automation_skill();
        let unrelated_id = unrelated.metadata.id.clone();
        skills.insert(unrelated_id.clone(), unrelated);

        assert_err_eq(
            skill_archive_from_proposal(
                &json!({
                    "action": "archive",
                    "id": unrelated_id,
                    "base_checksum": checksum(&skills, "rust-error-handling"),
                    "reason": "retire an unrelated automation skill"
                }),
                &skills,
            ),
            "managed skill 'rust-error-handling' is not a detected overlap candidate",
        );
        assert_err_eq(
            skill_merge_from_proposal(
                &json!({
                    "action": "merge",
                    "id": "workflow-a",
                    "base_checksum": checksum(&skills, "workflow-a"),
                    "source_skill_id": "rust-error-handling",
                    "source_base_checksum": checksum(&skills, "rust-error-handling"),
                    "reason": "merge unrelated automation skills"
                }),
                &skills,
            ),
            "managed skills 'workflow-a' and 'rust-error-handling' are not a detected overlap candidate pair",
        );
    }

    #[test]
    fn consolidation_proposals_reject_the_reserved_tombstone_as_a_reason() {
        let skills = consolidation_fixture();
        assert_err_eq(
            skill_archive_from_proposal(
                &json!({
                    "action": "archive",
                    "id": "workflow-a",
                    "base_checksum": checksum(&skills, "workflow-a"),
                    "reason": SKILL_OVERLAP_REMOVAL_TOMBSTONE
                }),
                &skills,
            ),
            "reason must not reuse the reserved skill-overlap tombstone label",
        );
        assert_err_eq(
            skill_merge_from_proposal(
                &json!({
                    "action": "merge",
                    "id": "workflow-a",
                    "base_checksum": checksum(&skills, "workflow-a"),
                    "source_skill_id": "workflow-b",
                    "source_base_checksum": checksum(&skills, "workflow-b"),
                    "reason": SKILL_OVERLAP_REMOVAL_TOMBSTONE
                }),
                &skills,
            ),
            "reason must not reuse the reserved skill-overlap tombstone label",
        );
    }

    #[test]
    fn applied_consolidation_records_carry_the_canonical_tombstone_label() {
        let skills = consolidation_fixture();
        let archive_record = applied_consolidation_record(
            SkillProposalAction::Archive,
            &json!({
                "action": "archive",
                "id": "workflow-a",
                "base_checksum": checksum(&skills, "workflow-a"),
                "reason": "unused overlap"
            }),
            &skills["workflow-a"],
            &checksum(&skills, "workflow-a"),
            None,
        );
        let merge = assert_ok(skill_merge_from_proposal(
            &json!({
                "action": "merge",
                "id": "workflow-a",
                "base_checksum": checksum(&skills, "workflow-a"),
                "source_skill_id": "workflow-b",
                "source_base_checksum": checksum(&skills, "workflow-b"),
                "reason": "duplicate guidance"
            }),
            &skills,
        ));
        let merge_record = applied_consolidation_record(
            SkillProposalAction::Merge,
            &json!({"reason": "duplicate guidance"}),
            &skills["workflow-b"],
            &checksum(&skills, "workflow-b"),
            Some(&merge),
        );

        for (action, record) in [("archive", &archive_record), ("merge", &merge_record)] {
            assert_eq!(
                record["tombstone_label"],
                json!(SKILL_OVERLAP_REMOVAL_TOMBSTONE),
                "the applied {action} consolidation record must carry the canonical \
                 typed removal-tombstone label"
            );
            assert_ne!(
                record["tombstone_label"], record["reason"],
                "the {action} record's tombstone label must be the typed constant, \
                 not the free-form proposal reason"
            );
        }
    }

    #[tokio::test]
    async fn merge_applies_target_update_and_source_archive_together() {
        let profile = tempfile::tempdir().unwrap();
        let target = create_managed_skill(
            profile.path(),
            fixture_draft("workflow-a", ManagedSkillSource::AutomationRun),
        )
        .await
        .unwrap();
        let source = create_managed_skill(
            profile.path(),
            fixture_draft("workflow-b", ManagedSkillSource::AutomationRun),
        )
        .await
        .unwrap();
        let target_before = target.clone();
        let source_before = source.clone();
        let skills = [
            (target.metadata.id.clone(), target),
            (source.metadata.id.clone(), source),
        ]
        .into_iter()
        .collect::<BTreeMap<_, _>>();
        let merge = skill_merge_from_proposal(
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
        )
        .unwrap();
        let expected_target = preview_managed_skill_update(
            &target_before,
            merge.update.as_ref().expect("merge should update target"),
        )
        .unwrap();

        let (source, target) = apply_skill_merge(profile.path(), &merge).await.unwrap();
        let target = target.expect("merge should return updated target");

        assert_eq!(source.metadata.state, ManagedSkillState::Archived);
        assert_eq!(source.metadata.absorbed_into.as_deref(), Some("workflow-a"));
        assert_eq!(source.metadata.id, source_before.metadata.id);
        assert_eq!(
            source.metadata.provenance,
            source_before.metadata.provenance
        );
        assert_eq!(source.support_files, source_before.support_files);
        assert_eq!(target.metadata.id, expected_target.metadata.id);
        assert_eq!(target.metadata.checksum, expected_target.metadata.checksum);
        assert_eq!(
            target.metadata.provenance,
            target_before.metadata.provenance
        );
        assert_eq!(target.support_files, target_before.support_files);
        assert_eq!(
            target.body_markdown,
            "Merged workflow guidance covering both variants."
        );
    }

    #[tokio::test]
    async fn archive_refuses_a_changed_exact_overlap_partner() {
        let profile = tempfile::tempdir().unwrap();
        let skills = persisted_consolidation_fixture(profile.path()).await;
        let archive = skill_archive_from_proposal(
            &json!({
                "action": "archive",
                "id": "workflow-b",
                "base_checksum": checksum(&skills, "workflow-b"),
                "reason": "duplicate guidance"
            }),
            &skills,
        )
        .unwrap();
        apply_managed_skill_update(
            profile.path(),
            "workflow-a",
            &checksum(&skills, "workflow-a"),
            ManagedSkillUpdate {
                body_markdown: Some(
                    "Model library failures with explicit error enums and no automation guidance."
                        .to_string(),
                ),
                ..ManagedSkillUpdate::default()
            },
        )
        .await
        .unwrap();

        let error = apply_skill_archive(profile.path(), &archive)
            .await
            .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("base_checksum for managed skill id 'workflow-a' is stale")
        );
        let source = load_managed_skill(profile.path(), "workflow-b")
            .await
            .unwrap();
        assert_eq!(source.metadata.state, ManagedSkillState::Active);
        assert_eq!(source.metadata.archived_reason, None);
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    async fn archive_persists_tombstone_before_postcommit_usage_sync_can_be_cancelled() {
        let profile = tempfile::tempdir().unwrap();
        let skills = persisted_consolidation_fixture(profile.path()).await;
        let proposal = json!({
            "action": "archive",
            "id": "workflow-b",
            "base_checksum": checksum(&skills, "workflow-b"),
            "reason": "duplicate guidance"
        });
        let archive = skill_archive_from_proposal(&proposal, &skills).unwrap();
        let (fifo_control, ledger_path) = replace_usage_ledger_with_blocking_fifo(profile.path());
        let profile_root = profile.path().to_path_buf();
        let archive_for_task = archive.clone();
        let mut task =
            tokio::spawn(
                async move { apply_skill_archive(&profile_root, &archive_for_task).await },
            );

        let committed = tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                tokio::select! {
                    result = &mut task => {
                        panic!("archive exited before its committed revision was observable: {result:?}");
                    }
                    loaded = load_managed_skill(profile.path(), "workflow-b") => {
                        let loaded = loaded.expect("load archive candidate through the production store");
                        if loaded.metadata.state == ManagedSkillState::Archived {
                            break loaded;
                        }
                        tokio::task::yield_now().await;
                    }
                }
            }
        })
        .await
        .expect("archive commit becomes observable before usage sync completes");
        assert_eq!(committed.metadata.state, ManagedSkillState::Archived);
        assert_eq!(
            committed.metadata.archived_reason.as_deref(),
            Some(SKILL_OVERLAP_REMOVAL_TOMBSTONE),
            "the archive commit must durably carry its typed tombstone before \
             post-commit usage sync completes"
        );
        task.abort();
        drop(fifo_control);
        assert!(
            tokio::time::timeout(std::time::Duration::from_secs(2), &mut task)
                .await
                .expect("cancelled archive task joins")
                .unwrap_err()
                .is_cancelled()
        );
        std::fs::remove_file(ledger_path).expect("remove usage-ledger FIFO");
        assert_eq!(
            load_managed_skill(profile.path(), "workflow-b")
                .await
                .unwrap()
                .metadata
                .archived_reason
                .as_deref(),
            Some(SKILL_OVERLAP_REMOVAL_TOMBSTONE)
        );
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    async fn merge_persists_tombstone_before_postcommit_usage_sync_can_be_cancelled() {
        let profile = tempfile::tempdir().unwrap();
        let skills = persisted_consolidation_fixture(profile.path()).await;
        let proposal = json!({
            "action": "merge",
            "id": "workflow-a",
            "base_checksum": checksum(&skills, "workflow-a"),
            "source_skill_id": "workflow-b",
            "source_base_checksum": checksum(&skills, "workflow-b"),
            "reason": "duplicate guidance"
        });
        let merge = skill_merge_from_proposal(&proposal, &skills).unwrap();
        let (fifo_control, ledger_path) = replace_usage_ledger_with_blocking_fifo(profile.path());
        let profile_root = profile.path().to_path_buf();
        let merge_for_task = merge.clone();
        let mut task =
            tokio::spawn(async move { apply_skill_merge(&profile_root, &merge_for_task).await });

        let committed_source =
            tokio::time::timeout(std::time::Duration::from_secs(2), async {
                loop {
                    tokio::select! {
                        result = &mut task => {
                            panic!("merge exited before its committed source was observable: {result:?}");
                        }
                        loaded = load_managed_skill(profile.path(), "workflow-b") => {
                            let loaded = loaded.expect("load merge source through the production store");
                            if loaded.metadata.state == ManagedSkillState::Archived {
                                break loaded;
                            }
                            tokio::task::yield_now().await;
                        }
                    }
                }
            })
            .await
            .expect("merge commit becomes observable before usage sync completes");
        assert_eq!(
            committed_source.metadata.state,
            ManagedSkillState::Archived,
            "the cancellation gate must run after the merge commit"
        );
        assert_eq!(
            committed_source.metadata.absorbed_into.as_deref(),
            Some("workflow-a")
        );
        assert_eq!(
            committed_source.metadata.archived_reason.as_deref(),
            Some(SKILL_OVERLAP_REMOVAL_TOMBSTONE),
            "the merge commit must durably carry its typed tombstone before \
             post-commit usage sync completes"
        );
        task.abort();
        drop(fifo_control);
        assert!(
            tokio::time::timeout(std::time::Duration::from_secs(2), &mut task)
                .await
                .expect("cancelled merge task joins")
                .unwrap_err()
                .is_cancelled()
        );
        std::fs::remove_file(ledger_path).expect("remove usage-ledger FIFO");
        assert_eq!(
            load_managed_skill(profile.path(), "workflow-b")
                .await
                .unwrap()
                .metadata
                .archived_reason
                .as_deref(),
            Some(SKILL_OVERLAP_REMOVAL_TOMBSTONE)
        );
    }

    #[tokio::test]
    async fn merge_rejects_a_concurrent_source_update_without_changing_target() {
        let profile = tempfile::tempdir().unwrap();
        let target = create_managed_skill(
            profile.path(),
            fixture_draft("workflow-a", ManagedSkillSource::AutomationRun),
        )
        .await
        .unwrap();
        let source = create_managed_skill(
            profile.path(),
            fixture_draft("workflow-b", ManagedSkillSource::AutomationRun),
        )
        .await
        .unwrap();
        let skills = [
            (target.metadata.id.clone(), target.clone()),
            (source.metadata.id.clone(), source.clone()),
        ]
        .into_iter()
        .collect::<BTreeMap<_, _>>();
        let merge = skill_merge_from_proposal(
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
        )
        .unwrap();
        let concurrent_source = apply_managed_skill_update(
            profile.path(),
            &source.metadata.id,
            &source.metadata.checksum,
            ManagedSkillUpdate {
                body_markdown: Some("Concurrent source revision.".to_string()),
                ..ManagedSkillUpdate::default()
            },
        )
        .await
        .unwrap();

        let error = apply_skill_merge(profile.path(), &merge).await.unwrap_err();

        assert_eq!(
            error.to_string(),
            "config error: base_checksum for managed skill id 'workflow-b' is stale"
        );
        assert_eq!(
            load_managed_skill(profile.path(), "workflow-a")
                .await
                .unwrap(),
            target
        );
        assert_eq!(
            load_managed_skill(profile.path(), "workflow-b")
                .await
                .unwrap(),
            concurrent_source
        );
    }
}
