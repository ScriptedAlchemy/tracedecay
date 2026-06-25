use std::collections::BTreeSet;
use std::path::{Component, Path};

use crate::errors::{Result, TraceDecayError};

use super::managed_skill_format::target_key;
use super::managed_skill_model::{
    ManagedSkill, ManagedSkillPendingUpdate, ManagedSkillUpdate, ManagedSupportFile,
    SkillInstallTarget, MAX_MANAGED_SKILL_BODY_BYTES, MAX_MANAGED_SUPPORT_FILES,
    MAX_MANAGED_SUPPORT_FILE_BYTES,
};

const ALLOWED_SUPPORT_ROOTS: &[&str] = &["references", "templates", "scripts", "assets"];

pub(crate) fn validate_skill_id(id: &str) -> Result<()> {
    if id.is_empty()
        || id.starts_with('.')
        || id.contains("..")
        || !id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
    {
        return Err(config_error(format!("unsafe managed skill id '{id}'")));
    }
    Ok(())
}

fn validate_relative_path(path: &Path) -> Result<()> {
    if path.as_os_str().is_empty()
        || path.is_absolute()
        || path.as_os_str().to_string_lossy().contains('\\')
    {
        return Err(config_error(format!(
            "unsafe support path '{}'",
            path.display()
        )));
    }
    for component in path.components() {
        match component {
            Component::Normal(part) if !part.to_string_lossy().contains('\\') => {}
            _ => {
                return Err(config_error(format!(
                    "unsafe support path '{}'",
                    path.display()
                )))
            }
        }
    }
    Ok(())
}

pub fn validate_managed_support_files(support_files: &[ManagedSupportFile]) -> Result<()> {
    if support_files.len() > MAX_MANAGED_SUPPORT_FILES {
        return Err(config_error(format!(
            "managed skill support file count exceeds {MAX_MANAGED_SUPPORT_FILES}"
        )));
    }

    let mut paths = BTreeSet::new();
    for support in support_files {
        validate_support_file(&support.path, &support.bytes)?;
        if !paths.insert(support.path.clone()) {
            return Err(config_error(format!(
                "duplicate managed skill support path '{}'",
                support.path.display()
            )));
        }
    }

    for path in &paths {
        let mut ancestor = path.parent();
        while let Some(parent) = ancestor {
            if parent.as_os_str().is_empty() {
                break;
            }
            if paths.contains(parent) {
                return Err(config_error(format!(
                    "managed skill support path '{}' conflicts with file path '{}'",
                    path.display(),
                    parent.display()
                )));
            }
            ancestor = parent.parent();
        }
    }

    Ok(())
}

pub(crate) fn validate_support_file(path: &Path, bytes: &[u8]) -> Result<()> {
    validate_relative_path(path)?;
    validate_support_root(path)?;
    if bytes.len() > MAX_MANAGED_SUPPORT_FILE_BYTES {
        return Err(config_error(format!(
            "managed skill support file '{}' exceeds {} bytes",
            path.display(),
            MAX_MANAGED_SUPPORT_FILE_BYTES
        )));
    }
    Ok(())
}

fn validate_support_root(path: &Path) -> Result<()> {
    let mut components = path.components();
    let Some(Component::Normal(root)) = components.next() else {
        return Err(config_error(format!(
            "unsafe support path '{}'",
            path.display()
        )));
    };
    let root = root.to_string_lossy();
    if !ALLOWED_SUPPORT_ROOTS.contains(&root.as_ref()) {
        return Err(config_error(format!(
            "managed skill support path '{}' must be under one of: {}",
            path.display(),
            ALLOWED_SUPPORT_ROOTS.join(", ")
        )));
    }
    if components.next().is_none() {
        return Err(config_error(format!(
            "managed skill support path '{}' must name a file under {}",
            path.display(),
            ALLOWED_SUPPORT_ROOTS.join(", ")
        )));
    }
    Ok(())
}

fn validate_non_empty(field: &str, value: &str) -> Result<()> {
    if value.trim().is_empty() {
        Err(config_error(format!(
            "managed skill {field} cannot be empty"
        )))
    } else {
        Ok(())
    }
}

fn validate_body_markdown(body: &str) -> Result<()> {
    validate_non_empty("body_markdown", body)?;
    let trimmed_start = body.trim_start();
    if trimmed_start.starts_with("---\n") || trimmed_start.starts_with("---\r\n") {
        return Err(config_error(
            "managed skill body_markdown cannot include YAML frontmatter".to_string(),
        ));
    }
    if body.len() > MAX_MANAGED_SKILL_BODY_BYTES {
        return Err(config_error(format!(
            "managed skill body_markdown exceeds {MAX_MANAGED_SKILL_BODY_BYTES} bytes"
        )));
    }
    Ok(())
}

fn validate_frontmatter_scalar(field: &str, value: &str) -> Result<()> {
    validate_non_empty(field, value)?;
    if value.trim() != value {
        return Err(config_error(format!(
            "managed skill {field} cannot have leading or trailing whitespace"
        )));
    }
    if value.contains(['\n', '\r']) {
        return Err(config_error(format!(
            "managed skill {field} must be a single line"
        )));
    }
    Ok(())
}

fn validate_skill_category(category: &str) -> Result<()> {
    validate_frontmatter_scalar("category", category)?;
    if category.len() > 64 {
        return Err(config_error(
            "managed skill category cannot exceed 64 characters".to_string(),
        ));
    }
    if category
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
    {
        Ok(())
    } else {
        Err(config_error(
            "managed skill category must use lowercase letters, numbers, '-' or '_'".to_string(),
        ))
    }
}

fn validate_skill_targets(targets: &[SkillInstallTarget]) -> Result<()> {
    if targets.is_empty() {
        return Err(config_error(
            "managed skill targets cannot be empty".to_string(),
        ));
    }
    let mut seen = BTreeSet::new();
    for target in targets {
        if *target == SkillInstallTarget::Hermes {
            return Err(config_error(
                "managed skill targets cannot include Hermes because Hermes owns profile skills"
                    .to_string(),
            ));
        }
        if !seen.insert(*target) {
            return Err(config_error(format!(
                "duplicate managed skill target '{}'",
                target_key(*target)
            )));
        }
    }
    Ok(())
}

pub(crate) fn validate_managed_skill(skill: &ManagedSkill) -> Result<()> {
    validate_skill_id(&skill.metadata.id)?;
    validate_frontmatter_scalar("title", &skill.metadata.title)?;
    validate_frontmatter_scalar("summary", &skill.metadata.summary)?;
    validate_skill_category(&skill.metadata.category)?;
    validate_skill_targets(&skill.metadata.targets)?;
    validate_body_markdown(&skill.body_markdown)?;
    validate_frontmatter_scalar("provenance actor", &skill.metadata.provenance.actor)?;
    if let Some(run_id) = &skill.metadata.provenance.run_id {
        validate_frontmatter_scalar("provenance run_id", run_id)?;
    }
    validate_managed_support_files(&skill.support_files)
}

pub(crate) fn validate_managed_pending_update(
    id: &str,
    pending: &ManagedSkillPendingUpdate,
) -> Result<()> {
    validate_skill_id(id)?;
    if pending.metadata.id != id {
        return Err(config_error(format!(
            "managed skill pending update id '{}' does not match '{id}'",
            pending.metadata.id
        )));
    }
    validate_checksum("base_checksum", &pending.base_checksum)?;
    if pending.staged_at <= 0 {
        return Err(config_error(
            "managed skill staged_at must be a positive timestamp".to_string(),
        ));
    }
    let skill = ManagedSkill {
        metadata: pending.metadata.clone(),
        body_markdown: pending.body_markdown.clone(),
        support_files: pending.support_files.clone(),
        pending_update: None,
    };
    validate_managed_skill(&skill)
}

fn validate_checksum(field: &str, checksum: &str) -> Result<()> {
    validate_non_empty(field, checksum)?;
    let Some(digest) = checksum.strip_prefix("sha256:") else {
        return Err(config_error(format!(
            "managed skill {field} must be a sha256 checksum"
        )));
    };
    if digest.len() == 64 && digest.bytes().all(|b| b.is_ascii_hexdigit()) {
        Ok(())
    } else {
        Err(config_error(format!(
            "managed skill {field} must be a sha256 checksum"
        )))
    }
}

pub(crate) fn validate_managed_skill_update(update: &ManagedSkillUpdate) -> Result<()> {
    if let Some(title) = &update.title {
        validate_frontmatter_scalar("title", title)?;
    }
    if let Some(summary) = &update.summary {
        validate_frontmatter_scalar("summary", summary)?;
    }
    if let Some(category) = &update.category {
        validate_skill_category(category)?;
    }
    if let Some(targets) = &update.targets {
        validate_skill_targets(targets)?;
    }
    if let Some(body_markdown) = &update.body_markdown {
        validate_body_markdown(body_markdown)?;
    }
    if let Some(support_files) = &update.support_files {
        validate_managed_support_files(support_files)?;
    }
    Ok(())
}

fn config_error(message: String) -> TraceDecayError {
    TraceDecayError::Config { message }
}
