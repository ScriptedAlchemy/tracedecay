use std::fmt::Write as _;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
pub use tracedecay_contracts::automation::{
    ManagedSkill, ManagedSkillMaterializationScope, ManagedSkillMetadata, ManagedSkillProvenance,
    ManagedSkillSource, ManagedSkillState, ManagedSupportFile, SkillInstallTarget,
    default_managed_skill_targets,
};
use tracedecay_domain::canonical_text::encode_tagged_lowercase_hex;

use crate::Result;
use crate::managed_skill_format::{frontmatter_string, source_key, state_key, target_key};
use crate::managed_skill_validation::{
    MAX_NATIVE_SKILL_NAME_CHARS, validate_managed_skill, validate_native_skill_markdown,
    validate_support_file,
};

pub const MAX_MANAGED_SUPPORT_FILES: usize = 20;
pub const MAX_MANAGED_SUPPORT_FILE_BYTES: usize = 64 * 1024;
pub const MAX_MANAGED_SKILL_BODY_BYTES: usize = 256 * 1024;

/// Provenance marker written into every host-loadable skill file materialized
/// by TraceDecay automation.
pub const MATERIALIZED_SKILL_MANAGED_BY: &str = "tracedecay-automation";

fn native_skill_name(id: &str) -> String {
    let mut normalized = String::with_capacity(id.len().min(MAX_NATIVE_SKILL_NAME_CHARS));
    for byte in id.bytes() {
        match byte {
            b'a'..=b'z' | b'0'..=b'9' => normalized.push(byte as char),
            b'-' | b'_' if !normalized.ends_with('-') => normalized.push('-'),
            _ => {}
        }
    }

    let trimmed = normalized.trim_matches('-');
    let truncated = truncate_frontmatter_chars(trimmed, MAX_NATIVE_SKILL_NAME_CHARS);
    let name = truncated.trim_end_matches('-');
    if name.is_empty() {
        "skill".to_string()
    } else {
        name.to_string()
    }
}

fn truncate_frontmatter_chars(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_string();
    }

    value
        .chars()
        .take(max_chars)
        .collect::<String>()
        .trim_end()
        .to_string()
}

/// Validated construction of a support file.
pub trait ManagedSupportFileExt: Sized {
    fn new(path: impl AsRef<Path>, bytes: Vec<u8>) -> Result<Self>;
}

impl ManagedSupportFileExt for ManagedSupportFile {
    fn new(path: impl AsRef<Path>, bytes: Vec<u8>) -> Result<Self> {
        let path = path.as_ref();
        validate_support_file(path, &bytes)?;
        Ok(Self {
            path: path.to_path_buf(),
            bytes,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManagedSkillDraft {
    pub id: String,
    pub title: String,
    pub summary: String,
    pub routing_description: String,
    pub category: String,
    #[serde(default = "default_managed_skill_targets")]
    pub targets: Vec<SkillInstallTarget>,
    pub body_markdown: String,
    #[serde(default)]
    pub support_files: Vec<ManagedSupportFile>,
    pub provenance: ManagedSkillProvenance,
}

impl ManagedSkillDraft {
    pub fn materialize(self) -> Result<ManagedSkill> {
        let now = current_metadata_timestamp();
        let mut skill = ManagedSkill {
            metadata: ManagedSkillMetadata {
                id: self.id,
                title: self.title,
                summary: self.summary,
                routing_description: self.routing_description,
                category: self.category,
                targets: self.targets,
                state: ManagedSkillState::Active,
                materialization_scope: ManagedSkillMaterializationScope::default(),
                pinned: false,
                checksum: String::new(),
                created_at: now,
                updated_at: now,
                activated_at: Some(now),
                absorbed_into: None,
                archived_reason: None,
                provenance: self.provenance,
            },
            body_markdown: self.body_markdown,
            support_files: self.support_files,
        };
        validate_managed_skill(&skill)?;
        skill.refresh_checksum();
        Ok(skill)
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManagedSkillUpdate {
    pub title: Option<String>,
    pub summary: Option<String>,
    pub routing_description: Option<String>,
    pub category: Option<String>,
    pub targets: Option<Vec<SkillInstallTarget>>,
    pub body_markdown: Option<String>,
    pub support_files: Option<Vec<ManagedSupportFile>>,
    pub pinned: Option<bool>,
}

/// Lifecycle, checksum, and host rendering of a managed skill package.
pub trait ManagedSkillExt {
    fn set_state(&mut self, state: ManagedSkillState);
    fn set_pinned(&mut self, pinned: bool);
    fn touch(&mut self);
    fn refresh_checksum(&mut self);
    fn render_skill_markdown(&self) -> String;
    fn render_native_skill_markdown(&self) -> Result<String>;
    fn host_skill_slug(&self) -> String;
    fn materialized_package_hash(&self) -> Result<String>;
    fn render_materialized_skill_markdown(&self) -> Result<String>;
}

impl ManagedSkillExt for ManagedSkill {
    fn set_state(&mut self, state: ManagedSkillState) {
        if self.metadata.state != state {
            self.metadata.state = state;
            if state == ManagedSkillState::Active {
                self.metadata.activated_at = Some(current_metadata_timestamp());
            }
            self.touch();
        }
    }

    fn set_pinned(&mut self, pinned: bool) {
        if self.metadata.pinned != pinned {
            self.metadata.pinned = pinned;
            self.touch();
        }
    }

    fn touch(&mut self) {
        self.metadata.updated_at = current_metadata_timestamp();
    }

    fn refresh_checksum(&mut self) {
        self.metadata.checksum = content_checksum(self);
    }

    fn render_skill_markdown(&self) -> String {
        let mut output = String::new();
        output.push_str("---\n");
        let _ = writeln!(output, "id: {}", self.metadata.id);
        let _ = writeln!(
            output,
            "title: {}",
            frontmatter_string(&self.metadata.title)
        );
        let _ = writeln!(
            output,
            "summary: {}",
            frontmatter_string(&self.metadata.summary)
        );
        let _ = writeln!(
            output,
            "routing_description: {}",
            frontmatter_string(&self.metadata.routing_description)
        );
        let _ = writeln!(output, "category: {}", self.metadata.category);
        let target_list = self
            .metadata
            .targets
            .iter()
            .map(|target| target_key(*target))
            .collect::<Vec<_>>()
            .join(", ");
        let _ = writeln!(output, "targets: [{target_list}]");
        let _ = writeln!(output, "state: {}", state_key(self.metadata.state));
        let _ = writeln!(output, "pinned: {}", self.metadata.pinned);
        let _ = writeln!(output, "checksum: {}", self.metadata.checksum);
        let _ = writeln!(output, "created_at: {}", self.metadata.created_at);
        let _ = writeln!(output, "updated_at: {}", self.metadata.updated_at);
        let _ = writeln!(
            output,
            "provenance_source: {}",
            source_key(self.metadata.provenance.source)
        );
        let _ = writeln!(
            output,
            "provenance_actor: {}",
            frontmatter_string(&self.metadata.provenance.actor)
        );
        if let Some(run_id) = &self.metadata.provenance.run_id {
            let _ = writeln!(output, "provenance_run_id: {}", frontmatter_string(run_id));
        }
        output.push_str("---\n\n");
        output.push_str(&self.body_markdown);
        output.push('\n');
        output
    }

    fn render_native_skill_markdown(&self) -> Result<String> {
        let mut output = String::new();
        output.push_str("---\n");
        let _ = writeln!(output, "name: {}", native_skill_name(&self.metadata.id));
        let _ = writeln!(
            output,
            "description: {}",
            frontmatter_string(&self.metadata.routing_description)
        );
        output.push_str("---\n\n");
        output.push_str(&self.body_markdown);
        output.push('\n');
        validate_native_skill_markdown(&output)?;
        Ok(output)
    }

    fn host_skill_slug(&self) -> String {
        native_skill_name(&self.metadata.id)
    }

    fn materialized_package_hash(&self) -> Result<String> {
        let markdown = render_materialized_skill_markdown_with_hash(self, "<package-hash>")?;
        let mut hasher = Sha256::new();
        hasher.update(markdown.as_bytes());
        let mut support_files = self.support_files.iter().collect::<Vec<_>>();
        support_files.sort_by(|left, right| left.path.cmp(&right.path));
        for support in support_files {
            let key = support_file_hash_key(&support.path);
            hasher.update(b"\0file:");
            hasher.update(key.as_bytes());
            hasher.update(b"\0");
            hasher.update(&support.bytes);
        }
        Ok(encode_tagged_lowercase_hex("sha256:", &hasher.finalize()))
    }

    fn render_materialized_skill_markdown(&self) -> Result<String> {
        let package_hash = self.materialized_package_hash()?;
        render_materialized_skill_markdown_with_hash(self, &package_hash)
    }
}

fn render_materialized_skill_markdown_with_hash(
    skill: &ManagedSkill,
    package_hash: &str,
) -> Result<String> {
    let name = native_skill_name(&skill.metadata.id);
    let description = &skill.metadata.routing_description;
    skill.render_native_skill_markdown()?;

    let mut output = String::new();
    output.push_str("---\n");
    let _ = writeln!(output, "name: {name}");
    let _ = writeln!(output, "description: {}", frontmatter_string(description));
    let _ = writeln!(output, "managed-by: {MATERIALIZED_SKILL_MANAGED_BY}");
    let _ = writeln!(
        output,
        "skill-id: {}",
        frontmatter_string(&skill.metadata.id)
    );
    let _ = writeln!(output, "content-hash: {package_hash}");
    let _ = writeln!(output, "skill-version: {}", skill.metadata.updated_at);
    output.push_str("---\n\n");
    output.push_str(&skill.body_markdown);
    output.push('\n');
    Ok(output)
}

fn content_checksum(skill: &ManagedSkill) -> String {
    let mut hasher = Sha256::new();
    hasher.update(skill.metadata.id.as_bytes());
    hasher.update(b"\0");
    hasher.update(skill.metadata.title.as_bytes());
    hasher.update(b"\0");
    hasher.update(skill.metadata.summary.as_bytes());
    hasher.update(b"\0");
    hasher.update(skill.metadata.routing_description.as_bytes());
    hasher.update(b"\0");
    hasher.update(skill.metadata.category.as_bytes());
    hasher.update(b"\0");
    for target in &skill.metadata.targets {
        hasher.update(b"\0target:");
        hasher.update(target_key(*target).as_bytes());
    }
    hasher.update(b"\0");
    hasher.update(skill.body_markdown.as_bytes());
    for file in &skill.support_files {
        let key = support_file_hash_key(&file.path);
        hasher.update(b"\0file:");
        hasher.update(key.as_bytes());
        hasher.update(b"\0");
        hasher.update(&file.bytes);
    }
    encode_tagged_lowercase_hex("sha256:", &hasher.finalize())
}

fn support_file_hash_key(path: &Path) -> String {
    path.components()
        .filter_map(|component| match component {
            std::path::Component::Normal(part) => Some(part.to_string_lossy()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/")
}

#[doc(hidden)]
pub fn current_metadata_timestamp() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_secs()).ok())
        .unwrap_or(0)
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use crate::skill_frontmatter::parse_skill_frontmatter;

    use super::*;

    #[test]
    fn native_skill_markdown_round_trips_escaped_description() {
        let skill = ManagedSkillDraft {
            id: "native-escape".to_string(),
            title: "Native escape".to_string(),
            summary: "Check path quoting.".to_string(),
            routing_description: r#"Diagnose "quoted" paths like C:\tmp"#.to_string(),
            category: "testing".to_string(),
            targets: vec![SkillInstallTarget::Codex],
            body_markdown: "# Native escape\n".to_string(),
            support_files: Vec::new(),
            provenance: ManagedSkillProvenance {
                source: ManagedSkillSource::User,
                actor: "tester".to_string(),
                run_id: None,
            },
        }
        .materialize()
        .unwrap();

        for markdown in [
            skill.render_native_skill_markdown().unwrap(),
            skill.render_materialized_skill_markdown().unwrap(),
        ] {
            let frontmatter = parse_skill_frontmatter(&markdown).unwrap();
            assert_eq!(
                frontmatter["description"].as_scalar(),
                Some(r#"Diagnose "quoted" paths like C:\tmp"#)
            );
        }
        let metadata_markdown = skill.render_skill_markdown();
        let metadata = parse_skill_frontmatter(&metadata_markdown).unwrap();
        assert_eq!(
            metadata["routing_description"].as_scalar(),
            Some(skill.metadata.routing_description.as_str())
        );

        let mut updated = skill.clone();
        updated.metadata.routing_description = "Investigate Windows path escaping.".to_string();
        updated.refresh_checksum();
        assert_ne!(updated.metadata.checksum, skill.metadata.checksum);
        assert_ne!(
            updated.materialized_package_hash().unwrap(),
            skill.materialized_package_hash().unwrap()
        );
    }
}
