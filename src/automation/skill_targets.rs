use std::fmt::Write as _;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use super::config_error;
use crate::agents::safe_write_text_file;
use crate::automation::managed_skills::{
    ManagedSkill, ManagedSupportFile, load_active_managed_skills_snapshot,
    validate_managed_support_files,
};
use crate::config::{TRACEDECAY_DIR, USER_DATA_DIR_ENV};
use crate::errors::{Result, TraceDecayError};

const NATIVE_NAMESPACE_DIR: &str = "agent-managed";
const NATIVE_MANIFEST_FILE: &str = ".tracedecay-managed-skills.json";
/// The unslugged legacy managed-skill start marker. Reuses the same literal the
/// prompt-rules block-splicer stops at, keeping the two in sync.
const PROMPT_INDEX_START: &str = crate::agents::prompt_rules::SKILL_INDEX_START;
const PROMPT_INDEX_END: &str = "<!-- TRACEDECAY MANAGED SKILLS END -->";

const ALL_SKILL_INSTALL_TARGETS: [SkillInstallTarget; 8] = [
    SkillInstallTarget::Cursor,
    SkillInstallTarget::Codex,
    SkillInstallTarget::Claude,
    SkillInstallTarget::Agents,
    SkillInstallTarget::OpenCode,
    SkillInstallTarget::Kimi,
    SkillInstallTarget::Kiro,
    SkillInstallTarget::Hermes,
];

pub use crate::automation::managed_skills::SkillInstallTarget;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillExportEntry {
    pub id: String,
    pub title: String,
    pub checksum: String,
    pub path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillInstallSummary {
    pub target: SkillInstallTarget,
    pub output: PathBuf,
    pub exported_count: usize,
    pub exported: Vec<SkillExportEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct NativeSkillManifest {
    version: u32,
    target: SkillInstallTarget,
    exported: Vec<SkillExportEntry>,
}

pub fn install_managed_skills(
    profile_root: &Path,
    target: SkillInstallTarget,
    output: &Path,
) -> Result<SkillInstallSummary> {
    if target.is_native_overlay() {
        export_native_skill_overlay(profile_root, target, output)
    } else {
        export_prompt_skill_index(profile_root, target, output)
    }
}

pub fn profile_root_for_agent_home(home: &Path) -> PathBuf {
    std::env::var_os(USER_DATA_DIR_ENV)
        .filter(|value| !value.is_empty())
        .map_or_else(|| home.join(TRACEDECAY_DIR), PathBuf::from)
}

pub fn export_native_skill_overlay(
    profile_root: &Path,
    target: SkillInstallTarget,
    plugin_root: &Path,
) -> Result<SkillInstallSummary> {
    if !target.is_native_overlay() {
        return Err(config_error(format!(
            "{target:?} does not support native skill overlays"
        )));
    }

    let skills = load_active_managed_skills_for_target(profile_root, target)?;
    let overlay_root = plugin_root.join("skills").join(NATIVE_NAMESPACE_DIR);
    if skills.is_empty() {
        if overlay_root.exists() {
            fs::remove_dir_all(&overlay_root)?;
        }
        return Ok(SkillInstallSummary {
            target,
            output: plugin_root.to_path_buf(),
            exported_count: 0,
            exported: Vec::new(),
        });
    }
    let mut native_markdown = Vec::with_capacity(skills.len());
    for skill in &skills {
        validate_managed_support_files(&skill.support_files)?;
        native_markdown.push(skill.render_native_skill_markdown()?);
    }

    let stage_root = unique_overlay_sibling(&overlay_root, "tmp");
    if stage_root.exists() {
        fs::remove_dir_all(&stage_root)?;
    }
    fs::create_dir_all(&stage_root)?;
    let mut exported = Vec::new();
    let write_result =
        (|| -> Result<()> {
            for (skill, skill_markdown) in skills.iter().zip(native_markdown.iter()) {
                let package_dir = stage_root.join(&skill.metadata.id);
                fs::create_dir_all(&package_dir)?;
                let skill_path = package_dir.join("SKILL.md");
                fs::write(&skill_path, skill_markdown)?;
                for support in &skill.support_files {
                    write_support_file(&package_dir, support)?;
                }
                exported.push(SkillExportEntry {
                    id: skill.metadata.id.clone(),
                    title: skill.metadata.title.clone(),
                    checksum: skill.metadata.checksum.clone(),
                    path: skill_path,
                });
            }

            let mut manifest_exported = Vec::with_capacity(exported.len());
            for mut entry in exported.iter().cloned() {
                let relative_path = entry.path.strip_prefix(&stage_root).map_err(|err| {
                    TraceDecayError::Config {
                        message: format!(
                            "native skill export path '{}' escaped stage root '{}': {err}",
                            entry.path.display(),
                            stage_root.display()
                        ),
                    }
                })?;
                entry.path = overlay_root.join(relative_path);
                manifest_exported.push(entry);
            }
            let manifest = NativeSkillManifest {
                version: 1,
                target,
                exported: manifest_exported,
            };
            fs::write(
                stage_root.join(NATIVE_MANIFEST_FILE),
                serde_json::to_vec_pretty(&manifest)?,
            )?;
            Ok(())
        })();
    if let Err(err) = write_result {
        fs::remove_dir_all(&stage_root).ok();
        return Err(err);
    }

    swap_overlay_dirs(&overlay_root, &stage_root)?;
    for entry in &mut exported {
        if let Ok(relative) = entry.path.strip_prefix(&stage_root) {
            entry.path = overlay_root.join(relative);
        }
    }

    Ok(SkillInstallSummary {
        target,
        output: plugin_root.to_path_buf(),
        exported_count: exported.len(),
        exported,
    })
}

pub fn export_prompt_skill_index(
    profile_root: &Path,
    target: SkillInstallTarget,
    prompt_path: &Path,
) -> Result<SkillInstallSummary> {
    if target == SkillInstallTarget::Hermes {
        return Err(hermes_host_owned_error());
    }
    let skills = load_active_managed_skills_for_target(profile_root, target)?;
    let existing = match fs::read_to_string(prompt_path) {
        Ok(contents) => contents,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(err) => return Err(err.into()),
    };
    let updated = if skills.is_empty() {
        remove_marked_block_for_target(&existing, target)?
    } else {
        let block = render_prompt_index_block(target, &skills);
        replace_or_append_marked_block(&existing, target, &block)?
    };

    if updated != existing {
        if let Some(parent) = prompt_path.parent() {
            fs::create_dir_all(parent)?;
        }
        safe_write_text_file(prompt_path, &updated, None)?;
    }

    let exported = skills
        .into_iter()
        .map(|skill| SkillExportEntry {
            id: skill.metadata.id,
            title: skill.metadata.title,
            checksum: skill.metadata.checksum,
            path: prompt_path.to_path_buf(),
        })
        .collect::<Vec<_>>();

    Ok(SkillInstallSummary {
        target,
        output: prompt_path.to_path_buf(),
        exported_count: exported.len(),
        exported,
    })
}

pub fn remove_prompt_skill_index(prompt_path: &Path) -> Result<()> {
    remove_prompt_skill_indexes(prompt_path, None)
}

pub fn remove_prompt_skill_index_for_target(
    prompt_path: &Path,
    target: SkillInstallTarget,
) -> Result<()> {
    remove_prompt_skill_indexes(prompt_path, Some(target))
}

fn remove_prompt_skill_indexes(
    prompt_path: &Path,
    target: Option<SkillInstallTarget>,
) -> Result<()> {
    let existing = match fs::read_to_string(prompt_path) {
        Ok(contents) => contents,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(err.into()),
    };
    let updated = match target {
        Some(target) => remove_marked_block_for_target(&existing, target)?,
        None => remove_all_marked_blocks(&existing)?,
    };
    if updated == existing {
        return Ok(());
    }
    if updated.trim().is_empty() {
        fs::remove_file(prompt_path)?;
    } else {
        safe_write_text_file(prompt_path, &updated, None)?;
    }
    Ok(())
}

pub fn load_active_managed_skills(profile_root: &Path) -> Result<Vec<ManagedSkill>> {
    load_active_managed_skills_snapshot(profile_root)
}

pub fn load_active_managed_skills_for_target(
    profile_root: &Path,
    target: SkillInstallTarget,
) -> Result<Vec<ManagedSkill>> {
    Ok(load_active_managed_skills(profile_root)?
        .into_iter()
        .filter(|skill| skill.metadata.targets.contains(&target))
        .collect())
}

fn render_prompt_index_block(target: SkillInstallTarget, skills: &[ManagedSkill]) -> String {
    let mut block = String::new();
    let (start, end) = prompt_index_markers(target);
    block.push_str(&start);
    block.push('\n');
    block.push_str(&prompt_index_preamble(target));

    if skills.is_empty() {
        block.push_str("- No approved managed skills are currently exported.\n");
    } else {
        for skill in skills {
            let _ = writeln!(
                block,
                "- `{}`: {}. Summary: {} Full body: `tracedecay_skill_view` with `id=\"{}\"`.",
                skill.metadata.id, skill.metadata.title, skill.metadata.summary, skill.metadata.id
            );
        }
    }

    block.push_str(&end);
    block.push('\n');
    block
}

fn replace_or_append_marked_block(
    existing: &str,
    target: SkillInstallTarget,
    block: &str,
) -> Result<String> {
    let (start_marker, end_marker) = prompt_index_markers(target);
    // Prefer this target's slugged block; fall back to the legacy unslugged one.
    let existing_range = match managed_block_range(existing, target, &start_marker, &end_marker)? {
        Some(range) => Some(range),
        None => managed_block_range(existing, target, PROMPT_INDEX_START, PROMPT_INDEX_END)?,
    };
    if let Some((start, end)) = existing_range {
        Ok(splice_range(existing, start, end, block))
    } else {
        let mut updated = String::new();
        updated.push_str(existing.trim_end());
        if !updated.is_empty() {
            updated.push_str("\n\n");
        }
        updated.push_str(block);
        Ok(updated)
    }
}

/// Replace `existing[start..end]` with `block`, normalizing surrounding blank
/// lines and guaranteeing a trailing newline.
fn splice_range(existing: &str, start: usize, end: usize, block: &str) -> String {
    let mut updated = String::new();
    updated.push_str(existing[..start].trim_end());
    updated.push_str("\n\n");
    updated.push_str(block.trim_end());
    updated.push_str("\n\n");
    updated.push_str(existing[end..].trim_start());
    if !updated.ends_with('\n') {
        updated.push('\n');
    }
    updated
}

fn remove_marked_block_for_target(existing: &str, target: SkillInstallTarget) -> Result<String> {
    let (start_marker, end_marker) = prompt_index_markers(target);
    if let Some((start, end)) = managed_block_range(existing, target, &start_marker, &end_marker)? {
        return Ok(remove_range(existing, start, end));
    }
    // Legacy fallback: older installs wrote an unslugged block. Only claim it as
    // this target's when NO other target's slugged block is present. On a shared
    // file mid-migration (one host slugged, another still legacy-unslugged),
    // removing the legacy block here would delete the other host's block, so
    // leave it untouched and let the remove-all path handle it instead.
    if !has_other_slugged_block(existing, target) {
        if let Some((start, end)) =
            managed_block_range(existing, target, PROMPT_INDEX_START, PROMPT_INDEX_END)?
        {
            return Ok(remove_range(existing, start, end));
        }
    }
    Ok(existing.to_string())
}

/// True when the file contains a slugged managed-skill block belonging to a
/// target other than `target`.
fn has_other_slugged_block(existing: &str, target: SkillInstallTarget) -> bool {
    ALL_SKILL_INSTALL_TARGETS
        .iter()
        .copied()
        .filter(|candidate| *candidate != target)
        .any(|candidate| existing.contains(&prompt_index_markers(candidate).0))
}

fn remove_all_marked_blocks(existing: &str) -> Result<String> {
    let mut updated = existing.to_string();
    for target in ALL_SKILL_INSTALL_TARGETS
        .into_iter()
        .filter(|target| target.writes_prompt_index())
    {
        let (start_marker, end_marker) = prompt_index_markers(target);
        if let Some((start, end)) =
            managed_block_range(&updated, target, &start_marker, &end_marker)?
        {
            updated = remove_range(&updated, start, end);
        }
    }
    if let Some((start, end)) = legacy_managed_block_range(&updated)? {
        updated = remove_range(&updated, start, end);
    }
    Ok(updated)
}

fn marked_block_range(
    existing: &str,
    start_marker: &str,
    end_marker: &str,
) -> Result<Option<(usize, usize)>> {
    match (existing.find(start_marker), existing.find(end_marker)) {
        (Some(start), Some(end)) if start <= end => {
            if existing.match_indices(start_marker).count() != 1
                || existing.match_indices(end_marker).count() != 1
            {
                return Err(config_error(
                    "managed skill prompt index markers are ambiguous".to_string(),
                ));
            }
            Ok(Some((start, end + end_marker.len())))
        }
        (None, None) => Ok(None),
        _ => Err(config_error(
            "managed skill prompt index markers are unbalanced".to_string(),
        )),
    }
}

/// Finds a normal marker-delimited block, or a generated block whose start
/// marker was lost while its exact preamble and end marker remain. Recovery
/// begins at the preamble, so preceding user-authored text is never claimed.
fn managed_block_range(
    existing: &str,
    target: SkillInstallTarget,
    start_marker: &str,
    end_marker: &str,
) -> Result<Option<(usize, usize)>> {
    match (existing.find(start_marker), existing.find(end_marker)) {
        (Some(start), Some(end)) if start <= end => {
            if existing.match_indices(start_marker).count() != 1
                || existing.match_indices(end_marker).count() != 1
            {
                return Err(config_error(
                    "managed skill prompt index markers are ambiguous".to_string(),
                ));
            }
            Ok(Some((start, end + end_marker.len())))
        }
        (None, None) => Ok(None),
        (None, Some(end)) => orphaned_generated_block_range(existing, target, end, end_marker),
        _ => Err(config_error(
            "managed skill prompt index markers are unbalanced".to_string(),
        )),
    }
}

fn legacy_managed_block_range(existing: &str) -> Result<Option<(usize, usize)>> {
    match (
        existing.find(PROMPT_INDEX_START),
        existing.find(PROMPT_INDEX_END),
    ) {
        (Some(_), Some(_)) => marked_block_range(existing, PROMPT_INDEX_START, PROMPT_INDEX_END),
        (None, None) => Ok(None),
        (None, Some(end)) => {
            if existing.match_indices(PROMPT_INDEX_END).count() != 1 {
                return Err(config_error(
                    "managed skill prompt index markers are ambiguous".to_string(),
                ));
            }
            let mut starts = Vec::new();
            for target in ALL_SKILL_INSTALL_TARGETS
                .into_iter()
                .filter(|target| target.writes_prompt_index())
            {
                let preamble = prompt_index_preamble(target);
                starts.extend(
                    existing[..end]
                        .match_indices(&preamble)
                        .map(|(start, _)| start),
                );
            }
            let Some(start) = starts.first().copied() else {
                return Err(config_error(
                    "managed skill prompt index markers are unbalanced".to_string(),
                ));
            };
            if starts.len() != 1 {
                return Err(config_error(
                    "managed skill prompt index markers are ambiguous".to_string(),
                ));
            }
            Ok(Some((start, end + PROMPT_INDEX_END.len())))
        }
        _ => Err(config_error(
            "managed skill prompt index markers are unbalanced".to_string(),
        )),
    }
}

fn orphaned_generated_block_range(
    existing: &str,
    target: SkillInstallTarget,
    end: usize,
    end_marker: &str,
) -> Result<Option<(usize, usize)>> {
    if existing.match_indices(end_marker).count() != 1 {
        return Err(config_error(
            "managed skill prompt index markers are unbalanced".to_string(),
        ));
    }
    let preamble = prompt_index_preamble(target);
    let mut matches = existing[..end].match_indices(&preamble);
    let Some((start, _)) = matches.next() else {
        return Err(config_error(
            "managed skill prompt index markers are unbalanced".to_string(),
        ));
    };
    if matches.next().is_some() || existing[end + end_marker.len()..].contains(&preamble) {
        return Err(config_error(
            "managed skill prompt index markers are unbalanced".to_string(),
        ));
    }
    Ok(Some((start, end + end_marker.len())))
}

fn prompt_index_preamble(target: SkillInstallTarget) -> String {
    format!(
        "## TraceDecay managed skills\n\nThis {} index lists approved profile-managed skills. For full instructions, call MCP tool `tracedecay_skill_view` with the listed `id`.\n\n",
        target.prompt_label()
    )
}

fn remove_range(existing: &str, start: usize, end: usize) -> String {
    let mut updated = String::new();
    updated.push_str(existing[..start].trim_end());
    updated.push_str("\n\n");
    updated.push_str(existing[end..].trim_start());
    if !updated.trim().is_empty() && !updated.ends_with('\n') {
        updated.push('\n');
    }
    updated
}

fn prompt_index_markers(target: SkillInstallTarget) -> (String, String) {
    let slug = target_marker_slug(target);
    (
        format!("<!-- TRACEDECAY MANAGED SKILLS START {slug} -->"),
        format!("<!-- TRACEDECAY MANAGED SKILLS END {slug} -->"),
    )
}

fn target_marker_slug(target: SkillInstallTarget) -> &'static str {
    match target {
        SkillInstallTarget::Cursor => "cursor",
        SkillInstallTarget::Codex => "codex",
        SkillInstallTarget::Claude => "claude",
        SkillInstallTarget::Agents => "agents",
        SkillInstallTarget::OpenCode => "opencode",
        SkillInstallTarget::Kimi => "kimi",
        SkillInstallTarget::Kiro => "kiro",
        SkillInstallTarget::Hermes => "hermes",
    }
}

fn unique_overlay_sibling(overlay_root: &Path, suffix: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    overlay_root.with_file_name(format!(
        ".{NATIVE_NAMESPACE_DIR}.{suffix}-{}-{nonce}",
        std::process::id()
    ))
}

fn swap_overlay_dirs(overlay_root: &Path, stage_root: &Path) -> Result<()> {
    let backup_root = unique_overlay_sibling(overlay_root, "previous");
    if backup_root.exists() {
        fs::remove_dir_all(&backup_root)?;
    }
    if overlay_root.exists() {
        fs::rename(overlay_root, &backup_root)?;
    }
    if let Err(err) = fs::rename(stage_root, overlay_root) {
        // Remove the staged directory so a failed swap does not orphan a
        // `.tracedecay-managed.tmp-<pid>-<nonce>` sibling on every retry.
        fs::remove_dir_all(stage_root).ok();
        if backup_root.exists() {
            if let Err(restore_err) = fs::rename(&backup_root, overlay_root) {
                tracing::warn!(
                    backup = %backup_root.display(),
                    overlay = %overlay_root.display(),
                    error = %restore_err,
                    "failed to restore managed skill overlay backup; previous content remains at backup path"
                );
            }
        }
        return Err(err.into());
    }
    if backup_root.exists() {
        fs::remove_dir_all(backup_root)?;
    }
    Ok(())
}

fn write_support_file(package_dir: &Path, support: &ManagedSupportFile) -> Result<()> {
    let relative = safe_relative_path(&support.path)?;
    let path = package_dir.join(relative);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, &support.bytes)?;
    Ok(())
}

fn safe_relative_path(path: &Path) -> Result<&Path> {
    if path.as_os_str().is_empty() || path.is_absolute() {
        return Err(config_error(format!(
            "unsafe managed skill support path '{}'",
            path.display()
        )));
    }
    for component in path.components() {
        match component {
            Component::Normal(part) if !part.to_string_lossy().contains('\\') => {}
            _ => {
                return Err(config_error(format!(
                    "unsafe managed skill support path '{}'",
                    path.display()
                )));
            }
        }
    }
    Ok(path)
}

fn hermes_host_owned_error() -> TraceDecayError {
    config_error(
        "Hermes owns profile skills, pending approvals, usage telemetry, and curator state; TraceDecay does not export managed skills into Hermes",
    )
}
