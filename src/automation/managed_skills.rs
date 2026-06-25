use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::errors::{Result, TraceDecayError};

use super::managed_skill_model::current_metadata_timestamp;
pub use super::managed_skill_model::{
    default_managed_skill_targets, ManagedSkill, ManagedSkillDraft, ManagedSkillMetadata,
    ManagedSkillPendingUpdate, ManagedSkillProvenance, ManagedSkillSource, ManagedSkillState,
    ManagedSkillUpdate, ManagedSupportFile, SkillInstallTarget, MAX_MANAGED_SKILL_BODY_BYTES,
    MAX_MANAGED_SUPPORT_FILES, MAX_MANAGED_SUPPORT_FILE_BYTES,
};
pub use super::managed_skill_validation::validate_managed_support_files;
use super::managed_skill_validation::{
    validate_managed_pending_update, validate_managed_skill, validate_managed_skill_update,
    validate_skill_id,
};

pub fn managed_skill_root(profile_root: &Path) -> PathBuf {
    profile_root.join("agent_managed").join("skills")
}

pub fn managed_skill_dir(profile_root: &Path, id: &str) -> Result<PathBuf> {
    validate_skill_id(id)?;
    Ok(managed_skill_root(profile_root).join(id))
}

fn pending_update_path(profile_root: &Path, id: &str) -> Result<PathBuf> {
    Ok(managed_skill_dir(profile_root, id)?.join("pending_update.json"))
}

pub async fn save_managed_skill(profile_root: &Path, skill: &ManagedSkill) -> Result<()> {
    validate_managed_skill(skill)?;
    let dir = managed_skill_dir(profile_root, &skill.metadata.id)?;
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|e| config_error(format!("failed to create managed skill dir: {e}")))?;
    let record_path = dir.join("skill.json");
    let mut persisted = skill.clone();
    persisted.pending_update = None;
    let record = serde_json::to_vec_pretty(&persisted).map_err(TraceDecayError::from)?;
    tokio::fs::write(&record_path, record).await.map_err(|e| {
        config_error(format!(
            "failed to write managed skill record '{}': {e}",
            record_path.display()
        ))
    })?;
    let skill_md = dir.join("SKILL.md");
    tokio::fs::write(&skill_md, skill.render_skill_markdown())
        .await
        .map_err(|e| {
            config_error(format!(
                "failed to write managed skill markdown '{}': {e}",
                skill_md.display()
            ))
        })?;
    remove_stale_support_files(&dir, &skill.support_files)?;
    for support in &skill.support_files {
        let path = dir.join(&support.path);
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await.map_err(|e| {
                config_error(format!(
                    "failed to create managed skill support dir '{}': {e}",
                    parent.display()
                ))
            })?;
        }
        tokio::fs::write(&path, &support.bytes).await.map_err(|e| {
            config_error(format!(
                "failed to write managed skill support file '{}': {e}",
                path.display()
            ))
        })?;
    }
    super::skill_usage::sync_skill_usage_metadata(profile_root, skill).await?;
    Ok(())
}

async fn load_pending_update(
    profile_root: &Path,
    id: &str,
) -> Result<Option<ManagedSkillPendingUpdate>> {
    let path = pending_update_path(profile_root, id)?;
    let bytes = match tokio::fs::read(&path).await {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => {
            return Err(config_error(format!(
                "failed to read managed skill pending update '{}': {e}",
                path.display()
            )))
        }
    };
    let mut pending: ManagedSkillPendingUpdate = serde_json::from_slice(&bytes).map_err(|e| {
        config_error(format!(
            "failed to parse managed skill pending update '{}': {e}",
            path.display()
        ))
    })?;
    pending.normalize_timestamps();
    validate_managed_pending_update(id, &pending)?;
    Ok(Some(pending))
}

async fn save_pending_update(
    profile_root: &Path,
    id: &str,
    pending: &ManagedSkillPendingUpdate,
) -> Result<()> {
    validate_managed_pending_update(id, pending)?;
    let path = pending_update_path(profile_root, id)?;
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await.map_err(|e| {
            config_error(format!(
                "failed to create managed skill pending update dir '{}': {e}",
                parent.display()
            ))
        })?;
    }
    let bytes = serde_json::to_vec_pretty(pending).map_err(TraceDecayError::from)?;
    tokio::fs::write(&path, bytes).await.map_err(|e| {
        config_error(format!(
            "failed to write managed skill pending update '{}': {e}",
            path.display()
        ))
    })?;
    Ok(())
}

async fn remove_pending_update(profile_root: &Path, id: &str) -> Result<()> {
    let path = pending_update_path(profile_root, id)?;
    match tokio::fs::remove_file(&path).await {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(config_error(format!(
            "failed to remove managed skill pending update '{}': {e}",
            path.display()
        ))),
    }
}

fn remove_stale_support_files(dir: &Path, support_files: &[ManagedSupportFile]) -> Result<()> {
    let expected: BTreeSet<PathBuf> = support_files
        .iter()
        .map(|support| support.path.clone())
        .collect();
    let existing = existing_support_files(dir)?;
    for relative in existing {
        if expected.contains(&relative) {
            continue;
        }
        let path = dir.join(&relative);
        std::fs::remove_file(&path).map_err(|e| {
            config_error(format!(
                "failed to remove stale managed skill support file '{}': {e}",
                path.display()
            ))
        })?;
    }
    Ok(())
}

fn existing_support_files(dir: &Path) -> Result<Vec<PathBuf>> {
    fn visit(root: &Path, dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
        let entries = match std::fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => {
                return Err(config_error(format!(
                    "failed to read managed skill support files '{}': {e}",
                    dir.display()
                )))
            }
        };
        for entry in entries {
            let entry = entry.map_err(|e| {
                config_error(format!(
                    "failed to read managed skill support file entry '{}': {e}",
                    dir.display()
                ))
            })?;
            let path = entry.path();
            if path.is_dir() {
                visit(root, &path, out)?;
                continue;
            }
            let relative = path.strip_prefix(root).map_err(|e| {
                config_error(format!(
                    "failed to relativize managed skill support file '{}': {e}",
                    path.display()
                ))
            })?;
            if relative == Path::new("skill.json")
                || relative == Path::new("SKILL.md")
                || relative == Path::new("pending_update.json")
            {
                continue;
            }
            out.push(relative.to_path_buf());
        }
        Ok(())
    }

    let mut out = Vec::new();
    visit(dir, dir, &mut out)?;
    Ok(out)
}

pub async fn create_managed_skill_draft(
    profile_root: &Path,
    draft: ManagedSkillDraft,
) -> Result<ManagedSkill> {
    let skill = draft.materialize()?;
    save_managed_skill(profile_root, &skill).await?;
    Ok(skill)
}

pub async fn load_managed_skill(profile_root: &Path, id: &str) -> Result<ManagedSkill> {
    let dir = managed_skill_dir(profile_root, id)?;
    let path = dir.join("skill.json");
    let bytes = tokio::fs::read(&path).await.map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            config_error(format!("managed skill '{id}' not found"))
        } else {
            config_error(format!(
                "failed to read managed skill record '{}': {e}",
                path.display()
            ))
        }
    })?;
    let mut skill: ManagedSkill = serde_json::from_slice(&bytes).map_err(|e| {
        config_error(format!(
            "failed to parse managed skill record '{}': {e}",
            path.display()
        ))
    })?;
    skill.normalize_timestamps();
    validate_managed_skill(&skill)?;
    skill.pending_update = load_pending_update(profile_root, id).await?;
    Ok(skill)
}

pub async fn list_managed_skills(profile_root: &Path) -> Result<Vec<ManagedSkill>> {
    let root = managed_skill_root(profile_root);
    let mut entries = match tokio::fs::read_dir(&root).await {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(config_error(format!("failed to read managed skills: {e}"))),
    };
    let mut skills = Vec::new();
    while let Some(entry) = entries
        .next_entry()
        .await
        .map_err(|e| config_error(format!("failed to read managed skill entry: {e}")))?
    {
        let path = entry.path().join("skill.json");
        if !path.is_file() {
            continue;
        }
        let bytes = tokio::fs::read(&path).await.map_err(|e| {
            config_error(format!(
                "failed to read managed skill record '{}': {e}",
                path.display()
            ))
        })?;
        let mut skill = serde_json::from_slice::<ManagedSkill>(&bytes).map_err(|e| {
            config_error(format!(
                "failed to parse managed skill record '{}': {e}",
                path.display()
            ))
        })?;
        skill.normalize_timestamps();
        validate_managed_skill(&skill)?;
        skill.pending_update = load_pending_update(profile_root, &skill.metadata.id).await?;
        skills.push(skill);
    }
    skills.sort_by(|a, b| a.metadata.id.cmp(&b.metadata.id));
    Ok(skills)
}

pub async fn set_managed_skill_state(
    profile_root: &Path,
    id: &str,
    state: ManagedSkillState,
) -> Result<ManagedSkill> {
    let mut skill = load_managed_skill(profile_root, id).await?;
    skill.set_state(state);
    save_managed_skill(profile_root, &skill).await?;
    record_skill_patch(profile_root, &skill, "lifecycle".to_string()).await?;
    Ok(skill)
}

pub async fn update_managed_skill(
    profile_root: &Path,
    id: &str,
    update: ManagedSkillUpdate,
) -> Result<ManagedSkill> {
    let mut skill = load_managed_skill(profile_root, id).await?;
    if skill.metadata.state != ManagedSkillState::PendingApproval {
        return stage_managed_skill_update(profile_root, id, &skill.metadata.checksum, update)
            .await;
    }
    let content_changed = apply_managed_skill_update(&mut skill, update)?;
    if content_changed {
        skill.set_state(ManagedSkillState::PendingApproval);
        skill.touch();
        skill.refresh_checksum();
    }
    save_managed_skill(profile_root, &skill).await?;
    record_skill_patch(profile_root, &skill, "update".to_string()).await?;
    Ok(skill)
}

pub async fn stage_managed_skill_update(
    profile_root: &Path,
    id: &str,
    base_checksum: &str,
    update: ManagedSkillUpdate,
) -> Result<ManagedSkill> {
    let skill = load_managed_skill(profile_root, id).await?;
    if base_checksum != skill.metadata.checksum {
        return Err(config_error(format!(
            "base_checksum for managed skill id '{id}' is stale"
        )));
    }
    if skill.pending_update.is_some() {
        return Err(config_error(format!(
            "managed skill '{id}' already has a pending update"
        )));
    }

    let mut staged = skill.clone();
    staged.pending_update = None;
    let original_pinned = staged.metadata.pinned;
    let content_changed = apply_managed_skill_update(&mut staged, update)?;
    let metadata_changed = staged.metadata.pinned != original_pinned;
    if !content_changed && !metadata_changed {
        return Err(config_error(format!(
            "managed skill '{id}' update does not change the active revision"
        )));
    }
    if content_changed {
        staged.refresh_checksum();
    }
    staged.set_state(ManagedSkillState::PendingApproval);
    staged.touch();

    let pending = ManagedSkillPendingUpdate {
        base_checksum: base_checksum.to_string(),
        staged_at: current_metadata_timestamp(),
        metadata: staged.metadata.clone(),
        body_markdown: staged.body_markdown.clone(),
        support_files: staged.support_files.clone(),
    };
    save_pending_update(profile_root, id, &pending).await?;
    record_skill_patch(profile_root, &staged, "staged_update".to_string()).await?;
    Ok(pending.into_skill())
}

pub async fn discard_pending_managed_skill_update(
    profile_root: &Path,
    id: &str,
) -> Result<ManagedSkill> {
    let skill = load_managed_skill(profile_root, id).await?;
    remove_pending_update(profile_root, id).await?;
    Ok(ManagedSkill {
        pending_update: None,
        ..skill
    })
}

fn replace_if_changed<T: PartialEq>(slot: &mut T, next: T) -> bool {
    if *slot == next {
        false
    } else {
        *slot = next;
        true
    }
}

fn apply_managed_skill_update(
    skill: &mut ManagedSkill,
    update: ManagedSkillUpdate,
) -> Result<bool> {
    validate_managed_skill_update(&update)?;

    let mut content_changed = false;
    if let Some(title) = update.title {
        content_changed |= replace_if_changed(&mut skill.metadata.title, title);
    }
    if let Some(summary) = update.summary {
        content_changed |= replace_if_changed(&mut skill.metadata.summary, summary);
    }
    if let Some(category) = update.category {
        content_changed |= replace_if_changed(&mut skill.metadata.category, category);
    }
    if let Some(targets) = update.targets {
        content_changed |= replace_if_changed(&mut skill.metadata.targets, targets);
    }
    if let Some(body_markdown) = update.body_markdown {
        content_changed |= replace_if_changed(&mut skill.body_markdown, body_markdown);
    }
    if let Some(support_files) = update.support_files {
        content_changed |= replace_if_changed(&mut skill.support_files, support_files);
    }
    if let Some(pinned) = update.pinned {
        skill.set_pinned(pinned);
    }
    Ok(content_changed)
}

async fn record_skill_patch(
    profile_root: &Path,
    skill: &ManagedSkill,
    target: String,
) -> Result<()> {
    super::skill_usage::record_skill_usage_event(
        profile_root,
        super::skill_usage::SkillUsageEvent {
            skill_name: skill.metadata.id.clone(),
            action: super::skill_usage::SkillUsageAction::Patch,
            timestamp: crate::tracedecay::current_timestamp(),
            target: Some(target),
        },
        Some(skill),
    )
    .await?;
    Ok(())
}

pub async fn approve_managed_skill(profile_root: &Path, id: &str) -> Result<ManagedSkill> {
    let skill = load_managed_skill(profile_root, id).await?;
    let Some(pending) = skill.pending_update else {
        return set_managed_skill_state(profile_root, id, ManagedSkillState::Active).await;
    };

    let mut promoted = pending.into_skill();
    promoted.set_state(ManagedSkillState::Active);
    promoted.refresh_checksum();
    remove_pending_update(profile_root, id).await?;
    save_managed_skill(profile_root, &promoted).await?;
    record_skill_patch(profile_root, &promoted, "approve_staged_update".to_string()).await?;
    Ok(promoted)
}

pub async fn disable_managed_skill(profile_root: &Path, id: &str) -> Result<ManagedSkill> {
    set_managed_skill_state(profile_root, id, ManagedSkillState::Disabled).await
}

pub async fn archive_managed_skill(profile_root: &Path, id: &str) -> Result<ManagedSkill> {
    set_managed_skill_state(profile_root, id, ManagedSkillState::Archived).await
}

pub async fn restore_managed_skill(profile_root: &Path, id: &str) -> Result<ManagedSkill> {
    set_managed_skill_state(profile_root, id, ManagedSkillState::PendingApproval).await
}

fn config_error(message: String) -> TraceDecayError {
    TraceDecayError::Config { message }
}
