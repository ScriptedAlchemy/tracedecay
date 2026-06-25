use std::fmt::Write as _;
use std::fs;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::automation::managed_skills::{
    managed_skill_root, validate_managed_support_files, ManagedSkill, ManagedSkillState,
    ManagedSupportFile,
};
use crate::config::{TRACEDECAY_DIR, USER_DATA_DIR_ENV};
use crate::errors::{Result, TraceDecayError};

const NATIVE_NAMESPACE_DIR: &str = "agent-managed";
const NATIVE_MANIFEST_FILE: &str = ".tracedecay-managed-skills.json";
const PROMPT_INDEX_START: &str = "<!-- TRACEDECAY MANAGED SKILLS START -->";
const PROMPT_INDEX_END: &str = "<!-- TRACEDECAY MANAGED SKILLS END -->";

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
    if target == SkillInstallTarget::Hermes {
        return Err(hermes_host_owned_error());
    }
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
    if overlay_root.exists() {
        fs::remove_dir_all(&overlay_root)?;
    }
    if skills.is_empty() {
        return Ok(SkillInstallSummary {
            target,
            output: plugin_root.to_path_buf(),
            exported_count: 0,
            exported: Vec::new(),
        });
    }
    fs::create_dir_all(&overlay_root)?;

    let mut exported = Vec::new();
    for skill in &skills {
        validate_managed_support_files(&skill.support_files)?;
        let package_dir = overlay_root.join(&skill.metadata.id);
        fs::create_dir_all(&package_dir)?;
        let skill_path = package_dir.join("SKILL.md");
        fs::write(&skill_path, skill.render_skill_markdown())?;
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

    let manifest = NativeSkillManifest {
        version: 1,
        target,
        exported: exported.clone(),
    };
    fs::write(
        overlay_root.join(NATIVE_MANIFEST_FILE),
        serde_json::to_vec_pretty(&manifest)?,
    )?;

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
        remove_marked_block(&existing)?
    } else {
        let block = render_prompt_index_block(target, &skills);
        replace_or_append_marked_block(&existing, &block)?
    };

    if updated != existing {
        if let Some(parent) = prompt_path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(prompt_path, updated)?;
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
    let existing = match fs::read_to_string(prompt_path) {
        Ok(contents) => contents,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(err.into()),
    };
    let updated = remove_marked_block(&existing)?;
    if updated == existing {
        return Ok(());
    }
    if updated.trim().is_empty() {
        fs::remove_file(prompt_path)?;
    } else {
        fs::write(prompt_path, updated)?;
    }
    Ok(())
}

pub fn load_active_managed_skills(profile_root: &Path) -> Result<Vec<ManagedSkill>> {
    let root = managed_skill_root(profile_root);
    if !root.exists() {
        return Ok(Vec::new());
    }

    let mut skills = Vec::new();
    for entry in fs::read_dir(&root)? {
        let entry = entry?;
        let path = entry.path().join("skill.json");
        if !path.is_file() {
            continue;
        }
        let skill: ManagedSkill = serde_json::from_slice(&fs::read(&path)?)?;
        if skill.metadata.state == ManagedSkillState::Active {
            skills.push(skill);
        }
    }
    skills.sort_by(|left, right| left.metadata.id.cmp(&right.metadata.id));
    Ok(skills)
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
    block.push_str(PROMPT_INDEX_START);
    block.push('\n');
    block.push_str("## TraceDecay managed skills\n\n");
    let _ = write!(
        block,
        "This {} index lists approved profile-managed skills. For full instructions, call MCP tool `tracedecay_skill_view` with the listed `id`.\n\n",
        target.prompt_label()
    );

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

    block.push_str(PROMPT_INDEX_END);
    block.push('\n');
    block
}

fn replace_or_append_marked_block(existing: &str, block: &str) -> Result<String> {
    match (
        existing.find(PROMPT_INDEX_START),
        existing.find(PROMPT_INDEX_END),
    ) {
        (Some(start), Some(end)) if start <= end => {
            let end = end + PROMPT_INDEX_END.len();
            let mut updated = String::new();
            updated.push_str(existing[..start].trim_end());
            updated.push_str("\n\n");
            updated.push_str(block.trim_end());
            updated.push_str("\n\n");
            updated.push_str(existing[end..].trim_start());
            if !updated.ends_with('\n') {
                updated.push('\n');
            }
            Ok(updated)
        }
        (None, None) => {
            let mut updated = String::new();
            updated.push_str(existing.trim_end());
            if !updated.is_empty() {
                updated.push_str("\n\n");
            }
            updated.push_str(block);
            Ok(updated)
        }
        _ => Err(config_error(
            "managed skill prompt index markers are unbalanced".to_string(),
        )),
    }
}

fn remove_marked_block(existing: &str) -> Result<String> {
    match (
        existing.find(PROMPT_INDEX_START),
        existing.find(PROMPT_INDEX_END),
    ) {
        (Some(start), Some(end)) if start <= end => {
            let end = end + PROMPT_INDEX_END.len();
            let mut updated = String::new();
            updated.push_str(existing[..start].trim_end());
            updated.push_str("\n\n");
            updated.push_str(existing[end..].trim_start());
            if !updated.trim().is_empty() && !updated.ends_with('\n') {
                updated.push('\n');
            }
            Ok(updated)
        }
        (None, None) => Ok(existing.to_string()),
        _ => Err(config_error(
            "managed skill prompt index markers are unbalanced".to_string(),
        )),
    }
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

fn config_error(message: impl Into<String>) -> TraceDecayError {
    TraceDecayError::Config {
        message: message.into(),
    }
}

fn hermes_host_owned_error() -> TraceDecayError {
    config_error(
        "Hermes owns profile skills, pending approvals, usage telemetry, and curator state; use the read-only Hermes skill bridge instead of exporting TraceDecay managed skills into Hermes",
    )
}
