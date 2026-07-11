use std::collections::BTreeSet;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use super::config_error;
use crate::errors::Result;
use fs2::FileExt;
use serde::{Deserialize, Serialize};

use super::managed_skill_model::current_metadata_timestamp;
pub use super::managed_skill_model::{
    MAX_MANAGED_SKILL_BODY_BYTES, MAX_MANAGED_SUPPORT_FILE_BYTES, MAX_MANAGED_SUPPORT_FILES,
    ManagedSkill, ManagedSkillDraft, ManagedSkillMaterializationScope, ManagedSkillMetadata,
    ManagedSkillPendingUpdate, ManagedSkillProvenance, ManagedSkillSource, ManagedSkillState,
    ManagedSkillUpdate, ManagedSupportFile, SkillInstallTarget, default_managed_skill_targets,
};
pub use super::managed_skill_validation::validate_managed_support_files;
use super::managed_skill_validation::{
    validate_managed_pending_update, validate_managed_skill, validate_managed_skill_update,
    validate_skill_id,
};

pub fn managed_skill_root(profile_root: &Path) -> PathBuf {
    profile_root.join("agent_managed").join("skills")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillConsolidationResult {
    pub target: Option<ManagedSkill>,
    pub source: ManagedSkill,
    pub target_before_checksum: Option<String>,
    pub target_after_checksum: Option<String>,
    pub source_before_checksum: String,
    pub source_after_checksum: String,
}

struct SkillStoreLock(File);

impl Drop for SkillStoreLock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.0);
    }
}

fn lock_skill_store(profile_root: &Path) -> Result<SkillStoreLock> {
    let root = managed_skill_root(profile_root);
    std::fs::create_dir_all(&root).map_err(|e| {
        config_error(format!(
            "failed to create managed skill root '{}': {e}",
            root.display()
        ))
    })?;
    let path = root.join(".consolidation.lock");
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&path)
        .map_err(|e| {
            config_error(format!(
                "failed to open skill lock '{}': {e}",
                path.display()
            ))
        })?;
    file.lock_exclusive().map_err(|e| {
        config_error(format!(
            "failed to lock skill store '{}': {e}",
            path.display()
        ))
    })?;
    recover_skill_transaction(&root)?;
    Ok(SkillStoreLock(file))
}

async fn lock_skill_store_async(profile_root: &Path) -> Result<SkillStoreLock> {
    let profile_root = profile_root.to_path_buf();
    tokio::task::spawn_blocking(move || lock_skill_store(&profile_root))
        .await
        .map_err(|error| config_error(format!("managed skill lock task failed: {error}")))?
}

const SKILL_TRANSACTION_JOURNAL: &str = ".skill-transaction.json";

#[derive(Debug, Serialize, Deserialize)]
struct SkillTransactionEntry {
    id: String,
    stage: PathBuf,
    backup: PathBuf,
}

#[derive(Debug, Serialize, Deserialize)]
struct SkillTransactionJournal {
    entries: Vec<SkillTransactionEntry>,
}

fn write_synced(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| {
            config_error(format!("failed to create '{}': {error}", parent.display()))
        })?;
    }
    let mut file = File::create(path)
        .map_err(|error| config_error(format!("failed to create '{}': {error}", path.display())))?;
    file.write_all(bytes)
        .map_err(|error| config_error(format!("failed to write '{}': {error}", path.display())))?;
    file.sync_all()
        .map_err(|error| config_error(format!("failed to sync '{}': {error}", path.display())))
}

fn remove_owned_dir(path: &Path) {
    if path.is_dir() {
        let _ = std::fs::remove_dir_all(path);
    }
}

fn clean_staged_entries(entries: &[SkillTransactionEntry]) {
    for entry in entries {
        remove_owned_dir(&entry.stage);
        remove_owned_dir(&entry.backup);
    }
}

fn sync_directory(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        File::open(path)
            .and_then(|dir| dir.sync_all())
            .map_err(|error| config_error(format!("failed to sync '{}': {error}", path.display())))
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(())
    }
}

fn validate_transaction_entry(root: &Path, entry: &SkillTransactionEntry) -> Result<()> {
    validate_skill_id(&entry.id)?;
    let valid_owned_path = |path: &Path, prefix: &str| {
        path.parent() == Some(root)
            && path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with(prefix))
    };
    if !valid_owned_path(&entry.stage, &format!(".stage-{}-", entry.id))
        || !valid_owned_path(&entry.backup, &format!(".backup-{}-", entry.id))
    {
        return Err(config_error(format!(
            "skill transaction entry '{}' escapes its managed root",
            entry.id
        )));
    }
    Ok(())
}

fn recover_skill_transaction(root: &Path) -> Result<()> {
    let journal_path = root.join(SKILL_TRANSACTION_JOURNAL);
    let bytes = match std::fs::read(&journal_path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(config_error(format!(
                "failed to read skill transaction journal '{}': {error}",
                journal_path.display()
            )));
        }
    };
    let journal: SkillTransactionJournal = serde_json::from_slice(&bytes).map_err(|error| {
        config_error(format!(
            "failed to parse skill transaction journal '{}': {error}",
            journal_path.display()
        ))
    })?;
    for entry in &journal.entries {
        validate_transaction_entry(root, entry)?;
        let destination = root.join(&entry.id);
        if entry.stage.exists() {
            if destination.exists() && !entry.backup.exists() {
                std::fs::rename(&destination, &entry.backup).map_err(|error| {
                    config_error(format!(
                        "failed to back up '{}' during skill transaction recovery: {error}",
                        destination.display()
                    ))
                })?;
            }
            if destination.exists() {
                remove_owned_dir(&destination);
            }
            std::fs::rename(&entry.stage, &destination).map_err(|error| {
                config_error(format!(
                    "failed to publish '{}' during skill transaction recovery: {error}",
                    destination.display()
                ))
            })?;
        } else if !destination.exists() && entry.backup.exists() {
            std::fs::rename(&entry.backup, &destination).map_err(|error| {
                config_error(format!(
                    "failed to restore '{}' during skill transaction recovery: {error}",
                    destination.display()
                ))
            })?;
        }
    }
    sync_directory(root)?;
    clean_staged_entries(&journal.entries);
    std::fs::remove_file(&journal_path).map_err(|error| {
        config_error(format!(
            "failed to clear skill transaction journal '{}': {error}",
            journal_path.display()
        ))
    })?;
    sync_directory(root)?;
    Ok(())
}

fn stage_skill_directory(
    root: &Path,
    skill: &ManagedSkill,
    nonce: u128,
) -> Result<SkillTransactionEntry> {
    validate_managed_skill(skill)?;
    let id = skill.metadata.id.clone();
    let stage = root.join(format!(".stage-{id}-{}-{nonce}", std::process::id()));
    let backup = root.join(format!(".backup-{id}-{}-{nonce}", std::process::id()));
    remove_owned_dir(&stage);
    remove_owned_dir(&backup);
    let entry = SkillTransactionEntry { id, stage, backup };
    let write_stage = || -> Result<()> {
        std::fs::create_dir_all(&entry.stage).map_err(|error| {
            config_error(format!(
                "failed to create '{}': {error}",
                entry.stage.display()
            ))
        })?;
        let mut persisted = skill.clone();
        persisted.pending_update = None;
        write_synced(
            &entry.stage.join("skill.json"),
            &serde_json::to_vec_pretty(&persisted)?,
        )?;
        write_synced(
            &entry.stage.join("SKILL.md"),
            skill.render_skill_markdown().as_bytes(),
        )?;
        if let Some(pending) = &skill.pending_update {
            write_synced(
                &entry.stage.join("pending_update.json"),
                &serde_json::to_vec_pretty(pending)?,
            )?;
        }
        for support in &skill.support_files {
            write_synced(&entry.stage.join(&support.path), &support.bytes)?;
        }
        sync_directory(&entry.stage)
    };
    if let Err(error) = write_stage() {
        clean_staged_entries(std::slice::from_ref(&entry));
        return Err(error);
    }
    Ok(entry)
}

fn persist_skill_transaction_unlocked(profile_root: &Path, skills: &[&ManagedSkill]) -> Result<()> {
    let root = managed_skill_root(profile_root);
    let mut ids = BTreeSet::new();
    for skill in skills {
        if !ids.insert(skill.metadata.id.as_str()) {
            return Err(config_error(format!(
                "managed skill transaction contains duplicate id '{}'",
                skill.metadata.id
            )));
        }
    }
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let mut entries = Vec::with_capacity(skills.len());
    for skill in skills {
        match stage_skill_directory(&root, skill, nonce) {
            Ok(entry) => entries.push(entry),
            Err(error) => {
                clean_staged_entries(&entries);
                return Err(error);
            }
        }
    }
    let journal = SkillTransactionJournal { entries };
    let journal_path = root.join(SKILL_TRANSACTION_JOURNAL);
    let journal_temp = root.join(format!("{SKILL_TRANSACTION_JOURNAL}.tmp"));
    let publish_journal = || -> Result<()> {
        write_synced(&journal_temp, &serde_json::to_vec_pretty(&journal)?)?;
        std::fs::rename(&journal_temp, &journal_path).map_err(|error| {
            config_error(format!(
                "failed to publish skill transaction journal: {error}"
            ))
        })
    };
    if let Err(error) = publish_journal() {
        let _ = std::fs::remove_file(&journal_temp);
        clean_staged_entries(&journal.entries);
        return Err(error);
    }
    sync_directory(&root)?;
    recover_skill_transaction(&root)
}

/// Re-loads and checksum-validates both revisions under one store lock, then
/// publishes target and archived source through one crash-recoverable directory
/// transaction. Source content is never deleted.
pub async fn apply_managed_skill_consolidation(
    profile_root: &Path,
    target_id: Option<&str>,
    target_checksum: Option<&str>,
    target_update: Option<ManagedSkillUpdate>,
    source_id: &str,
    source_checksum: &str,
    reason: &str,
) -> Result<SkillConsolidationResult> {
    if target_id == Some(source_id) {
        return Err(config_error(
            "managed skill consolidation source and target must differ",
        ));
    }
    let lock = lock_skill_store_async(profile_root).await?;
    let mut source = load_managed_skill_unlocked(profile_root, source_id)?;
    validate_autonomous_consolidation_skill(&source, source_checksum)?;
    let source_before_checksum = source.metadata.checksum.clone();

    let mut original_target = None;
    let mut target = match target_id {
        Some(id) => {
            let loaded = load_managed_skill_unlocked(profile_root, id)?;
            let checksum =
                target_checksum.ok_or_else(|| config_error("merge target checksum is required"))?;
            validate_autonomous_consolidation_skill(&loaded, checksum)?;
            original_target = Some(loaded.clone());
            Some(loaded)
        }
        None => None,
    };

    if let (Some(target), Some(update)) = (&mut target, target_update) {
        if apply_managed_skill_update(target, update)? {
            target.touch();
            target.refresh_checksum();
        }
    }

    source.metadata.absorbed_into = target_id.map(ToOwned::to_owned);
    source.metadata.archived_reason = Some(reason.to_string());
    source.set_state(ManagedSkillState::Archived);
    source.pending_update = None;
    let mut revisions: Vec<&ManagedSkill> = target.iter().collect();
    revisions.push(&source);
    persist_skill_transaction_unlocked(profile_root, &revisions)?;
    drop(lock);
    for skill in &revisions {
        if let Err(error) = super::skill_usage::sync_skill_usage_metadata(profile_root, skill).await
        {
            tracing::warn!(skill_id = %skill.metadata.id, error = %error, "skill usage metadata reconciliation failed after committed consolidation");
        }
    }

    Ok(SkillConsolidationResult {
        target_before_checksum: original_target
            .as_ref()
            .map(|skill| skill.metadata.checksum.clone()),
        target_after_checksum: target.as_ref().map(|skill| skill.metadata.checksum.clone()),
        source_before_checksum,
        source_after_checksum: source.metadata.checksum.clone(),
        target,
        source,
    })
}

fn validate_autonomous_consolidation_skill(skill: &ManagedSkill, checksum: &str) -> Result<()> {
    let id = &skill.metadata.id;
    if skill.metadata.checksum != checksum {
        return Err(config_error(format!(
            "base_checksum for managed skill id '{id}' is stale"
        )));
    }
    if skill.metadata.provenance.source != ManagedSkillSource::AutomationRun {
        return Err(config_error(format!(
            "managed skill '{id}' is not automation-owned"
        )));
    }
    if skill.metadata.pinned {
        return Err(config_error(format!(
            "managed skill '{id}' is pinned and exempt from consolidation"
        )));
    }
    if skill.metadata.state == ManagedSkillState::Archived {
        return Err(config_error(format!(
            "managed skill '{id}' is already archived"
        )));
    }
    if skill.pending_update.is_some() {
        return Err(config_error(format!(
            "managed skill '{id}' already has a pending update"
        )));
    }
    Ok(())
}

pub fn managed_skill_dir(profile_root: &Path, id: &str) -> Result<PathBuf> {
    validate_skill_id(id)?;
    Ok(managed_skill_root(profile_root).join(id))
}

fn pending_update_path(profile_root: &Path, id: &str) -> Result<PathBuf> {
    Ok(managed_skill_dir(profile_root, id)?.join("pending_update.json"))
}

pub async fn save_managed_skill(profile_root: &Path, skill: &ManagedSkill) -> Result<()> {
    let lock = lock_skill_store_async(profile_root).await?;
    let destination = managed_skill_dir(profile_root, &skill.metadata.id)?;
    if destination.exists() {
        return Err(config_error(format!(
            "managed skill '{}' already exists; use a lifecycle update operation",
            skill.metadata.id
        )));
    }
    persist_skill_transaction_unlocked(profile_root, &[skill])?;
    drop(lock);
    if let Err(error) = super::skill_usage::sync_skill_usage_metadata(profile_root, skill).await {
        tracing::warn!(skill_id = %skill.metadata.id, error = %error, "skill usage metadata reconciliation failed after committed skill save");
    }
    Ok(())
}

fn load_pending_update(profile_root: &Path, id: &str) -> Result<Option<ManagedSkillPendingUpdate>> {
    let path = pending_update_path(profile_root, id)?;
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => {
            return Err(config_error(format!(
                "failed to read managed skill pending update '{}': {e}",
                path.display()
            )));
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

pub async fn create_managed_skill_draft(
    profile_root: &Path,
    draft: ManagedSkillDraft,
) -> Result<ManagedSkill> {
    let skill = draft.materialize()?;
    let lock = lock_skill_store_async(profile_root).await?;
    let destination = managed_skill_dir(profile_root, &skill.metadata.id)?;
    if destination.exists() {
        return Err(config_error(format!(
            "managed skill '{}' already exists",
            skill.metadata.id
        )));
    }
    persist_skill_transaction_unlocked(profile_root, &[&skill])?;
    drop(lock);
    if let Err(error) = super::skill_usage::sync_skill_usage_metadata(profile_root, &skill).await {
        tracing::warn!(skill_id = %skill.metadata.id, error = %error, "skill usage metadata reconciliation failed after committed skill create");
    }
    Ok(skill)
}

pub async fn load_managed_skill(profile_root: &Path, id: &str) -> Result<ManagedSkill> {
    let _lock = lock_skill_store_async(profile_root).await?;
    load_managed_skill_unlocked(profile_root, id)
}

fn load_managed_skill_unlocked(profile_root: &Path, id: &str) -> Result<ManagedSkill> {
    let dir = managed_skill_dir(profile_root, id)?;
    let path = dir.join("skill.json");
    let bytes = std::fs::read(&path).map_err(|e| {
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
    skill.pending_update = load_pending_update(profile_root, id)?;
    Ok(skill)
}

pub async fn list_managed_skills(profile_root: &Path) -> Result<Vec<ManagedSkill>> {
    let _lock = lock_skill_store_async(profile_root).await?;
    list_managed_skills_unlocked(profile_root)
}

pub(crate) fn load_active_managed_skills_snapshot(
    profile_root: &Path,
) -> Result<Vec<ManagedSkill>> {
    let _lock = lock_skill_store(profile_root)?;
    Ok(list_managed_skills_unlocked(profile_root)?
        .into_iter()
        .filter(|skill| skill.metadata.state == ManagedSkillState::Active)
        .collect())
}

fn list_managed_skills_unlocked(profile_root: &Path) -> Result<Vec<ManagedSkill>> {
    let root = managed_skill_root(profile_root);
    let entries = match std::fs::read_dir(&root) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(config_error(format!("failed to read managed skills: {e}"))),
    };
    let mut skills = Vec::new();
    for entry in entries {
        let entry =
            entry.map_err(|e| config_error(format!("failed to read managed skill entry: {e}")))?;
        let path = entry.path().join("skill.json");
        if !path.is_file() {
            continue;
        }
        let bytes = std::fs::read(&path).map_err(|e| {
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
        skill.pending_update = load_pending_update(profile_root, &skill.metadata.id)?;
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
    let lock = lock_skill_store_async(profile_root).await?;
    let skill = set_managed_skill_state_unlocked(profile_root, id, state)?;
    drop(lock);
    record_skill_patch_best_effort(profile_root, &skill, "lifecycle").await;
    Ok(skill)
}

fn set_managed_skill_state_unlocked(
    profile_root: &Path,
    id: &str,
    state: ManagedSkillState,
) -> Result<ManagedSkill> {
    let mut skill = load_managed_skill_unlocked(profile_root, id)?;
    skill.set_state(state);
    persist_skill_transaction_unlocked(profile_root, &[&skill])?;
    Ok(skill)
}

pub async fn update_managed_skill(
    profile_root: &Path,
    id: &str,
    update: ManagedSkillUpdate,
) -> Result<ManagedSkill> {
    let lock = lock_skill_store_async(profile_root).await?;
    let mut skill = load_managed_skill_unlocked(profile_root, id)?;
    if skill.metadata.state != ManagedSkillState::PendingApproval {
        let checksum = skill.metadata.checksum.clone();
        let skill = stage_managed_skill_update_unlocked(profile_root, id, &checksum, update)?;
        drop(lock);
        record_skill_patch_best_effort(profile_root, &skill, "staged_update").await;
        return Ok(skill);
    }
    let content_changed = apply_managed_skill_update(&mut skill, update)?;
    if content_changed {
        skill.set_state(ManagedSkillState::PendingApproval);
        skill.touch();
        skill.refresh_checksum();
    }
    persist_skill_transaction_unlocked(profile_root, &[&skill])?;
    drop(lock);
    record_skill_patch_best_effort(profile_root, &skill, "update").await;
    Ok(skill)
}

pub async fn set_managed_skill_pinned(
    profile_root: &Path,
    id: &str,
    pinned: bool,
) -> Result<ManagedSkill> {
    let lock = lock_skill_store_async(profile_root).await?;
    let mut skill = load_managed_skill_unlocked(profile_root, id)?;
    skill.set_pinned(pinned);
    persist_skill_transaction_unlocked(profile_root, &[&skill])?;
    drop(lock);
    record_skill_patch_best_effort(profile_root, &skill, "pin").await;
    Ok(skill)
}

pub async fn stage_managed_skill_update(
    profile_root: &Path,
    id: &str,
    base_checksum: &str,
    update: ManagedSkillUpdate,
) -> Result<ManagedSkill> {
    let lock = lock_skill_store_async(profile_root).await?;
    let skill = stage_managed_skill_update_unlocked(profile_root, id, base_checksum, update)?;
    drop(lock);
    record_skill_patch_best_effort(profile_root, &skill, "staged_update").await;
    Ok(skill)
}

fn stage_managed_skill_update_unlocked(
    profile_root: &Path,
    id: &str,
    base_checksum: &str,
    update: ManagedSkillUpdate,
) -> Result<ManagedSkill> {
    let skill = load_managed_skill_unlocked(profile_root, id)?;
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
        resulting_state: None,
        staged_reason: None,
    };
    let mut persisted = skill;
    persisted.pending_update = Some(pending.clone());
    persist_skill_transaction_unlocked(profile_root, &[&persisted])?;
    Ok(pending.into_skill())
}

/// Stages an archive transition for a managed skill as a pending update that
/// must be approved (or discarded) through the normal review lifecycle.
/// Skill content is untouched: approving only flips the state to `Archived`,
/// keeping the body and support files recoverable on disk. Pinned skills are
/// exempt, matching the Hermes curator.
pub async fn stage_managed_skill_archive(
    profile_root: &Path,
    id: &str,
    base_checksum: &str,
    reason: Option<String>,
) -> Result<ManagedSkill> {
    let lock = lock_skill_store_async(profile_root).await?;
    let skill = stage_managed_skill_archive_unlocked(profile_root, id, base_checksum, reason)?;
    drop(lock);
    record_skill_patch_best_effort(profile_root, &skill, "staged_archive").await;
    Ok(skill)
}

fn stage_managed_skill_archive_unlocked(
    profile_root: &Path,
    id: &str,
    base_checksum: &str,
    reason: Option<String>,
) -> Result<ManagedSkill> {
    let skill = load_managed_skill_unlocked(profile_root, id)?;
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
    if skill.metadata.pinned {
        return Err(config_error(format!(
            "managed skill '{id}' is pinned and exempt from staged archive"
        )));
    }
    if skill.metadata.state == ManagedSkillState::Archived {
        return Err(config_error(format!(
            "managed skill '{id}' is already archived"
        )));
    }

    let mut staged = skill.clone();
    staged.pending_update = None;
    staged.set_state(ManagedSkillState::PendingApproval);
    staged.touch();
    let pending = ManagedSkillPendingUpdate {
        base_checksum: base_checksum.to_string(),
        staged_at: current_metadata_timestamp(),
        metadata: staged.metadata.clone(),
        body_markdown: staged.body_markdown.clone(),
        support_files: staged.support_files.clone(),
        resulting_state: Some(ManagedSkillState::Archived),
        staged_reason: reason,
    };
    let mut persisted = skill;
    persisted.pending_update = Some(pending.clone());
    persist_skill_transaction_unlocked(profile_root, &[&persisted])?;
    Ok(pending.into_skill())
}

pub async fn discard_pending_managed_skill_update(
    profile_root: &Path,
    id: &str,
) -> Result<ManagedSkill> {
    let _lock = lock_skill_store_async(profile_root).await?;
    let mut skill = load_managed_skill_unlocked(profile_root, id)?;
    skill.pending_update = None;
    persist_skill_transaction_unlocked(profile_root, &[&skill])?;
    Ok(skill)
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

async fn record_skill_patch_best_effort(profile_root: &Path, skill: &ManagedSkill, target: &str) {
    if let Err(error) = super::skill_usage::record_skill_usage_event(
        profile_root,
        super::skill_usage::SkillUsageEvent {
            skill_name: skill.metadata.id.clone(),
            action: super::skill_usage::SkillUsageAction::Patch,
            timestamp: crate::tracedecay::current_timestamp(),
            target: Some(target.to_string()),
        },
        Some(skill),
    )
    .await
    {
        tracing::warn!(skill_id = %skill.metadata.id, target, error = %error, "skill usage patch recording failed after committed skill change");
    }
}

pub async fn approve_managed_skill(profile_root: &Path, id: &str) -> Result<ManagedSkill> {
    let lock = lock_skill_store_async(profile_root).await?;
    let skill = load_managed_skill_unlocked(profile_root, id)?;
    let (approved, patch_target) = match skill.pending_update {
        None => {
            let mut active = skill;
            active.set_state(ManagedSkillState::Active);
            persist_skill_transaction_unlocked(profile_root, &[&active])?;
            (active, "lifecycle")
        }
        Some(pending) => {
            let resulting_state = pending.resulting_state.unwrap_or(ManagedSkillState::Active);
            let patch_target = match resulting_state {
                ManagedSkillState::Archived => "approve_staged_archive",
                _ => "approve_staged_update",
            };
            let mut promoted = pending.into_skill();
            promoted.set_state(resulting_state);
            promoted.refresh_checksum();
            persist_skill_transaction_unlocked(profile_root, &[&promoted])?;
            (promoted, patch_target)
        }
    };
    drop(lock);
    record_skill_patch_best_effort(profile_root, &approved, patch_target).await;
    if let Err(error) = super::skill_usage::record_skill_approval(profile_root, &approved).await {
        tracing::warn!(skill_id = %approved.metadata.id, error = %error, "skill approval recording failed after committed skill approval");
    }
    Ok(approved)
}

pub async fn disable_managed_skill(profile_root: &Path, id: &str) -> Result<ManagedSkill> {
    set_managed_skill_state(profile_root, id, ManagedSkillState::Disabled).await
}

pub async fn archive_managed_skill(profile_root: &Path, id: &str) -> Result<ManagedSkill> {
    set_managed_skill_state(profile_root, id, ManagedSkillState::Archived).await
}

pub async fn restore_managed_skill(profile_root: &Path, id: &str) -> Result<ManagedSkill> {
    let lock = lock_skill_store_async(profile_root).await?;
    let mut skill = load_managed_skill_unlocked(profile_root, id)?;
    skill.metadata.absorbed_into = None;
    skill.metadata.archived_reason = None;
    skill.set_state(ManagedSkillState::PendingApproval);
    persist_skill_transaction_unlocked(profile_root, &[&skill])?;
    drop(lock);
    record_skill_patch_best_effort(profile_root, &skill, "restore").await;
    Ok(skill)
}

#[cfg(test)]
mod transaction_tests {
    use super::*;
    use std::sync::{Arc, Barrier};
    use std::time::Duration;

    fn skill(id: &str, body: &str) -> ManagedSkill {
        ManagedSkillDraft {
            id: id.to_string(),
            title: id.to_string(),
            summary: format!("Reusable workflow for {id}."),
            category: "testing".to_string(),
            targets: vec![SkillInstallTarget::Codex],
            body_markdown: body.to_string(),
            support_files: Vec::new(),
            provenance: ManagedSkillProvenance {
                source: ManagedSkillSource::AutomationRun,
                actor: "test".to_string(),
                run_id: Some("run-1".to_string()),
            },
        }
        .materialize()
        .unwrap()
    }

    #[tokio::test]
    async fn recovery_rolls_forward_a_partially_published_multi_skill_transaction() {
        let temp = tempfile::tempdir().unwrap();
        let profile = temp.path();
        let original_a = skill("skill-a", "# A\nold");
        let original_b = skill("skill-b", "# B\nold");
        {
            let _lock = lock_skill_store(profile).unwrap();
            persist_skill_transaction_unlocked(profile, &[&original_a, &original_b]).unwrap();
        }

        let mut next_a = original_a.clone();
        next_a.body_markdown = "# A\nnew".to_string();
        next_a.refresh_checksum();
        let mut next_b = original_b.clone();
        next_b.set_state(ManagedSkillState::Archived);
        let root = managed_skill_root(profile);
        let nonce = 7;
        let entry_a = stage_skill_directory(&root, &next_a, nonce).unwrap();
        let entry_b = stage_skill_directory(&root, &next_b, nonce).unwrap();
        let journal = SkillTransactionJournal {
            entries: vec![entry_a, entry_b],
        };
        write_synced(
            &root.join(SKILL_TRANSACTION_JOURNAL),
            &serde_json::to_vec_pretty(&journal).unwrap(),
        )
        .unwrap();
        let first = &journal.entries[0];
        std::fs::rename(root.join(&first.id), &first.backup).unwrap();
        std::fs::rename(&first.stage, root.join(&first.id)).unwrap();

        drop(lock_skill_store(profile).unwrap());

        let loaded_a = load_managed_skill(profile, "skill-a").await.unwrap();
        let loaded_b = load_managed_skill(profile, "skill-b").await.unwrap();
        assert_eq!(loaded_a.body_markdown, "# A\nnew");
        assert_eq!(loaded_b.metadata.state, ManagedSkillState::Archived);
        assert!(!root.join(SKILL_TRANSACTION_JOURNAL).exists());
        assert!(
            journal
                .entries
                .iter()
                .all(|entry| !entry.stage.exists() && !entry.backup.exists())
        );
    }

    #[test]
    fn transaction_rejects_duplicate_skill_ids() {
        let temp = tempfile::tempdir().unwrap();
        let profile = temp.path();
        let first = skill("same-skill", "# First");
        let second = skill("same-skill", "# Second");
        let _lock = lock_skill_store(profile).unwrap();

        let error = persist_skill_transaction_unlocked(profile, &[&first, &second]).unwrap_err();

        assert!(error.to_string().contains("duplicate id 'same-skill'"));
    }

    #[test]
    fn failed_journal_write_cleans_staged_directories() {
        let temp = tempfile::tempdir().unwrap();
        let profile = temp.path();
        let skill = skill("staged-skill", "# Staged");
        let root = managed_skill_root(profile);
        let _lock = lock_skill_store(profile).unwrap();
        std::fs::create_dir(root.join(format!("{SKILL_TRANSACTION_JOURNAL}.tmp"))).unwrap();

        persist_skill_transaction_unlocked(profile, &[&skill]).unwrap_err();

        assert!(std::fs::read_dir(root).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".stage-")
        }));
    }

    #[tokio::test]
    async fn consolidation_rejects_the_same_source_and_target() {
        let temp = tempfile::tempdir().unwrap();
        let error = apply_managed_skill_consolidation(
            temp.path(),
            Some("same-skill"),
            Some("checksum"),
            None,
            "same-skill",
            "checksum",
            "duplicate",
        )
        .await
        .unwrap_err();

        assert!(error.to_string().contains("source and target must differ"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn async_store_wait_does_not_block_the_runtime_thread() {
        let temp = tempfile::tempdir().unwrap();
        let profile = temp.path().to_path_buf();
        create_managed_skill_draft(
            &profile,
            ManagedSkillDraft {
                id: "waiting-skill".to_string(),
                title: "Waiting skill".to_string(),
                summary: "Exercises asynchronous lock acquisition.".to_string(),
                category: "testing".to_string(),
                targets: vec![SkillInstallTarget::Codex],
                body_markdown: "# Waiting".to_string(),
                support_files: Vec::new(),
                provenance: ManagedSkillProvenance {
                    source: ManagedSkillSource::AutomationRun,
                    actor: "test".to_string(),
                    run_id: Some("run-1".to_string()),
                },
            },
        )
        .await
        .unwrap();
        let lock = lock_skill_store(&profile).unwrap();
        let waiter_profile = profile.clone();
        let waiter =
            tokio::spawn(async move { load_managed_skill(&waiter_profile, "waiting-skill").await });

        tokio::time::sleep(Duration::from_millis(20)).await;
        drop(lock);

        let loaded = tokio::time::timeout(Duration::from_secs(2), waiter)
            .await
            .expect("lock waiter should not block the runtime")
            .unwrap()
            .unwrap();
        assert_eq!(loaded.metadata.id, "waiting-skill");
    }

    #[tokio::test]
    async fn public_save_rejects_stale_replacement() {
        let temp = tempfile::tempdir().unwrap();
        let profile = temp.path();
        let original = skill("existing-skill", "# Original");
        save_managed_skill(profile, &original).await.unwrap();
        let mut stale = original.clone();
        stale.body_markdown = "# Stale replacement".to_string();
        stale.refresh_checksum();

        let error = save_managed_skill(profile, &stale).await.unwrap_err();

        assert!(error.to_string().contains("already exists"));
        assert_eq!(
            load_managed_skill(profile, "existing-skill")
                .await
                .unwrap()
                .body_markdown,
            "# Original"
        );
    }

    #[tokio::test]
    async fn committed_change_survives_usage_ledger_failure() {
        let temp = tempfile::tempdir().unwrap();
        let profile = temp.path();
        let original = skill("ledger-skill", "# Ledger");
        save_managed_skill(profile, &original).await.unwrap();
        let ledger_path = super::super::skill_usage::skill_usage_ledger_path(profile);
        std::fs::remove_file(&ledger_path).unwrap();
        std::fs::create_dir(&ledger_path).unwrap();

        let pinned = set_managed_skill_pinned(profile, "ledger-skill", true)
            .await
            .unwrap();

        assert!(pinned.metadata.pinned);
        assert!(
            load_managed_skill(profile, "ledger-skill")
                .await
                .unwrap()
                .metadata
                .pinned
        );
    }

    #[test]
    fn concurrent_export_recovers_before_reading_a_partial_publish() {
        let temp = tempfile::tempdir().unwrap();
        let profile = temp.path().to_path_buf();
        let mut original_a = skill("skill-a", "# A\nold");
        original_a.set_state(ManagedSkillState::Active);
        let mut original_b = skill("skill-b", "# B\nold");
        original_b.set_state(ManagedSkillState::Active);
        {
            let _lock = lock_skill_store(&profile).unwrap();
            persist_skill_transaction_unlocked(&profile, &[&original_a, &original_b]).unwrap();
        }

        let lock = lock_skill_store(&profile).unwrap();
        let mut next_a = original_a.clone();
        next_a.body_markdown = "# A\nnew".to_string();
        next_a.refresh_checksum();
        let mut next_b = original_b.clone();
        next_b.set_state(ManagedSkillState::Archived);
        let root = managed_skill_root(&profile);
        let entry_a = stage_skill_directory(&root, &next_a, 11).unwrap();
        let entry_b = stage_skill_directory(&root, &next_b, 11).unwrap();
        let journal = SkillTransactionJournal {
            entries: vec![entry_a, entry_b],
        };
        write_synced(
            &root.join(SKILL_TRANSACTION_JOURNAL),
            &serde_json::to_vec_pretty(&journal).unwrap(),
        )
        .unwrap();
        let first = &journal.entries[0];
        std::fs::rename(root.join(&first.id), &first.backup).unwrap();
        std::fs::rename(&first.stage, root.join(&first.id)).unwrap();

        let barrier = Arc::new(Barrier::new(2));
        let thread_barrier = Arc::clone(&barrier);
        let thread_profile = profile.clone();
        let exporter = std::thread::spawn(move || {
            thread_barrier.wait();
            crate::automation::skill_targets::load_active_managed_skills(&thread_profile).unwrap()
        });
        barrier.wait();
        drop(lock);

        let exported = exporter.join().unwrap();
        assert_eq!(exported.len(), 1);
        assert_eq!(exported[0].metadata.id, "skill-a");
        assert_eq!(exported[0].body_markdown, "# A\nnew");
    }
}
