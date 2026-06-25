use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::errors::{Result, TraceDecayError};

use super::hermes_config_projection::{load_hermes_yaml_projection, yaml_bool};

const SKILL_MD: &str = "SKILL.md";
pub(crate) const USAGE_FILE: &str = ".usage.json";
const MAX_BRIDGED_BODY_CHARS: usize = 100_000;
const PROTECTED_BUILTIN_SKILLS: &[&str] = &["plan"];

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HermesSkillSummary {
    pub name: String,
    pub path: PathBuf,
    pub ownership: HermesSkillOwnershipProjection,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body_markdown: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<Value>,
    pub pending_write_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HermesSkillOwnershipProjection {
    pub owner: String,
    pub source: String,
    pub hub_installed: bool,
    pub bundled: bool,
    pub protected_builtin: bool,
    pub curator_suppressed: bool,
    pub curator_eligible: bool,
    pub curator_managed_record: bool,
}

pub(crate) fn load_skill_summaries(
    skills_dir: &Path,
    usage_records: &BTreeMap<String, Value>,
    pending_by_skill: &BTreeMap<String, Vec<String>>,
    ownership: &BTreeMap<String, HermesSkillOwnershipProjection>,
    include_skill_bodies: bool,
) -> Result<Vec<HermesSkillSummary>> {
    let mut skills = Vec::new();
    visit_skill_dirs(skills_dir, &mut |skill_dir, skill_md| {
        let contents = fs::read_to_string(skill_md).map_err(|e| {
            config_error(format!(
                "failed to read Hermes skill '{}': {e}",
                skill_md.display()
            ))
        })?;
        let frontmatter = parse_frontmatter(&contents);
        let name = frontmatter
            .get("name")
            .or_else(|| frontmatter.get("id"))
            .cloned()
            .unwrap_or_else(|| {
                skill_dir
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into()
            });
        let category = skill_dir
            .parent()
            .and_then(|parent| parent.strip_prefix(skills_dir).ok())
            .and_then(|relative| {
                (relative.components().count() > 0).then(|| relative.display().to_string())
            })
            .filter(|value| !value.is_empty());
        let body_markdown =
            include_skill_bodies.then(|| truncate_chars(&contents, MAX_BRIDGED_BODY_CHARS));
        skills.push(HermesSkillSummary {
            usage: usage_records.get(&name).cloned(),
            pending_write_ids: pending_by_skill.get(&name).cloned().unwrap_or_default(),
            ownership: ownership
                .get(&name)
                .cloned()
                .unwrap_or_else(|| HermesSkillOwnershipProjection::agent_local(&name, false)),
            name,
            path: skill_dir.to_path_buf(),
            category,
            description: frontmatter
                .get("description")
                .or_else(|| frontmatter.get("summary"))
                .cloned(),
            body_markdown,
        });
        Ok(())
    })?;
    skills.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(skills)
}

impl HermesSkillOwnershipProjection {
    fn agent_local(name: &str, curator_managed_record: bool) -> Self {
        let protected_builtin = is_protected_builtin(name);
        Self {
            owner: "hermes_local".to_string(),
            source: "local".to_string(),
            hub_installed: false,
            bundled: false,
            protected_builtin,
            curator_suppressed: false,
            curator_eligible: curator_managed_record && !protected_builtin,
            curator_managed_record,
        }
    }
}

pub(crate) fn load_skill_ownership_projection(
    hermes_home: &Path,
    usage_records: &BTreeMap<String, Value>,
) -> Result<BTreeMap<String, HermesSkillOwnershipProjection>> {
    let skills_dir = hermes_home.join("skills");
    let hub_installed = read_hub_installed_names(&skills_dir)?;
    let bundled = read_bundled_manifest_names(&skills_dir)?;
    let suppressed = read_curator_suppressed_names(&skills_dir)?;
    let prune_builtins = read_prune_builtins_enabled(hermes_home)?;
    let mut names = hub_installed
        .iter()
        .chain(bundled.iter())
        .chain(suppressed.iter())
        .cloned()
        .collect::<Vec<_>>();
    names.extend(usage_records.keys().cloned());
    names.sort();
    names.dedup();

    let mut ownership = BTreeMap::new();
    for name in names {
        let hub_installed = hub_installed.contains(&name);
        let bundled = bundled.contains(&name);
        let protected_builtin = is_protected_builtin(&name);
        let curator_suppressed = suppressed.contains(&name);
        let curator_managed_record = usage_records
            .get(&name)
            .is_some_and(is_curator_managed_record);
        let (owner, source) = if hub_installed {
            ("hermes_hub", "hub")
        } else if bundled {
            ("hermes_bundle", "bundled")
        } else {
            ("hermes_local", "local")
        };
        let curator_eligible = !hub_installed
            && !protected_builtin
            && !curator_suppressed
            && ((bundled && prune_builtins) || (!bundled && curator_managed_record));
        ownership.insert(
            name,
            HermesSkillOwnershipProjection {
                owner: owner.to_string(),
                source: source.to_string(),
                hub_installed,
                bundled,
                protected_builtin,
                curator_suppressed,
                curator_eligible,
                curator_managed_record,
            },
        );
    }
    Ok(ownership)
}

fn read_bundled_manifest_names(skills_dir: &Path) -> Result<Vec<String>> {
    let path = skills_dir.join(".bundled_manifest");
    let contents = match fs::read_to_string(&path) {
        Ok(contents) => contents,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => {
            return Err(config_error(format!(
                "failed to read Hermes bundled skill manifest '{}': {e}",
                path.display()
            )));
        }
    };
    Ok(contents
        .lines()
        .filter_map(|line| line.trim().split_once(':').map(|(name, _)| name.trim()))
        .filter(|name| !name.is_empty())
        .map(str::to_string)
        .collect())
}

fn read_curator_suppressed_names(skills_dir: &Path) -> Result<Vec<String>> {
    let path = skills_dir.join(".curator_suppressed");
    let contents = match fs::read_to_string(&path) {
        Ok(contents) => contents,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => {
            return Err(config_error(format!(
                "failed to read Hermes curator suppression list '{}': {e}",
                path.display()
            )));
        }
    };
    Ok(contents
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(str::to_string)
        .collect())
}

fn read_hub_installed_names(skills_dir: &Path) -> Result<Vec<String>> {
    let path = skills_dir.join(".hub").join("lock.json");
    let contents = match fs::read_to_string(&path) {
        Ok(contents) => contents,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => {
            return Err(config_error(format!(
                "failed to read Hermes skill hub lock '{}': {e}",
                path.display()
            )));
        }
    };
    let value: Value = match serde_json::from_str(&contents) {
        Ok(value) => value,
        Err(_) => return Ok(Vec::new()),
    };
    let Some(installed) = value.get("installed").and_then(Value::as_object) else {
        return Ok(Vec::new());
    };
    let mut names = installed.keys().cloned().collect::<Vec<_>>();
    for entry in installed.values().filter_map(Value::as_object) {
        let Some(install_path) = entry.get("install_path").and_then(Value::as_str) else {
            continue;
        };
        let Some(skill_dir) = resolve_hub_skill_path(skills_dir, install_path) else {
            continue;
        };
        if !skill_dir.join(SKILL_MD).is_file() {
            continue;
        }
        if let Ok(contents) = fs::read_to_string(skill_dir.join(SKILL_MD)) {
            let frontmatter = parse_frontmatter(&contents);
            if let Some(name) = frontmatter.get("name").or_else(|| frontmatter.get("id")) {
                names.push(name.clone());
            }
        }
    }
    names.sort();
    names.dedup();
    Ok(names)
}

fn resolve_hub_skill_path(skills_dir: &Path, install_path: &str) -> Option<PathBuf> {
    let path = PathBuf::from(install_path);
    let skill_dir = if path.is_absolute() {
        path
    } else {
        skills_dir.join(path)
    };
    let base = skills_dir.canonicalize().ok()?;
    let resolved = skill_dir.canonicalize().ok()?;
    resolved.starts_with(base).then_some(resolved)
}

fn read_prune_builtins_enabled(hermes_home: &Path) -> Result<bool> {
    let config = load_hermes_yaml_projection(&hermes_home.join("config.yaml"))?;
    Ok(yaml_bool(&config, "curator.prune_builtins").unwrap_or(true))
}

fn is_curator_managed_record(value: &Value) -> bool {
    value.get("created_by").and_then(Value::as_str) == Some("agent")
        || value
            .get("agent_created")
            .and_then(Value::as_bool)
            .unwrap_or(false)
}

fn is_protected_builtin(name: &str) -> bool {
    PROTECTED_BUILTIN_SKILLS.contains(&name)
}

fn visit_skill_dirs(
    root: &Path,
    visitor: &mut impl FnMut(&Path, &Path) -> Result<()>,
) -> Result<()> {
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => {
            return Err(config_error(format!(
                "failed to read Hermes skills directory '{}': {e}",
                root.display()
            )));
        }
    };
    for entry in entries {
        let entry = entry.map_err(|e| {
            config_error(format!(
                "failed to read Hermes skill directory entry '{}': {e}",
                root.display()
            ))
        })?;
        let path = entry.path();
        let name = entry.file_name();
        if name.to_string_lossy().starts_with('.') {
            continue;
        }
        if !path.is_dir() {
            continue;
        }
        let skill_md = path.join(SKILL_MD);
        if skill_md.is_file() {
            visitor(&path, &skill_md)?;
            continue;
        }
        visit_skill_dirs(&path, visitor)?;
    }
    Ok(())
}

pub(crate) fn load_usage_records(skills_dir: &Path) -> Result<BTreeMap<String, Value>> {
    let path = skills_dir.join(USAGE_FILE);
    let contents = match fs::read_to_string(&path) {
        Ok(contents) => contents,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
        Err(e) => {
            return Err(config_error(format!(
                "failed to read Hermes skill usage '{}': {e}",
                path.display()
            )));
        }
    };
    match serde_json::from_str(&contents) {
        Ok(records) => Ok(records),
        Err(_) => Ok(BTreeMap::new()),
    }
}

pub(crate) fn count_archive_entries(archive_dir: &Path) -> Result<usize> {
    let entries = match fs::read_dir(archive_dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(e) => {
            return Err(config_error(format!(
                "failed to read Hermes skill archive '{}': {e}",
                archive_dir.display()
            )));
        }
    };
    Ok(entries.filter_map(std::result::Result::ok).count())
}

fn parse_frontmatter(contents: &str) -> BTreeMap<String, String> {
    let mut lines = contents.lines();
    if lines.next() != Some("---") {
        return BTreeMap::new();
    }
    let mut values = BTreeMap::new();
    for line in lines {
        if line.trim() == "---" {
            break;
        }
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        values.insert(
            key.trim().to_ascii_lowercase(),
            value
                .trim()
                .trim_matches('"')
                .trim_matches('\'')
                .to_string(),
        );
    }
    values
}

fn truncate_chars(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_string();
    }
    value.chars().take(max_chars).collect()
}

fn config_error(message: impl Into<String>) -> TraceDecayError {
    TraceDecayError::Config {
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::{is_curator_managed_record, resolve_hub_skill_path};

    use serde_json::json;

    #[test]
    fn hub_skill_path_resolution_stays_inside_skills_dir(
    ) -> std::result::Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let skills_dir = temp.path().join("skills");
        let skill_dir = skills_dir.join("hub-skill");
        std::fs::create_dir_all(&skill_dir)?;
        std::fs::write(
            skill_dir.join(super::SKILL_MD),
            "---\nname: hub-skill\n---\n",
        )?;
        let outside = temp.path().join("outside");
        std::fs::create_dir_all(&outside)?;

        assert_eq!(
            resolve_hub_skill_path(&skills_dir, "hub-skill"),
            Some(skill_dir.canonicalize()?)
        );
        assert!(resolve_hub_skill_path(&skills_dir, "../outside").is_none());
        assert!(resolve_hub_skill_path(&skills_dir, &outside.to_string_lossy()).is_none());
        Ok(())
    }

    #[test]
    fn curator_record_detection_recognizes_agent_managed_usage() {
        assert!(is_curator_managed_record(&json!({"created_by": "agent"})));
        assert!(is_curator_managed_record(&json!({"agent_created": true})));
        assert!(!is_curator_managed_record(&json!({"created_by": "user"})));
    }
}
