use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::Result;

use super::managed_skill_format::{frontmatter_string, source_key, state_key, target_key};
use super::managed_skill_validation::{
    MAX_NATIVE_SKILL_DESCRIPTION_CHARS, MAX_NATIVE_SKILL_NAME_CHARS, validate_managed_skill,
    validate_native_skill_markdown, validate_support_file,
};

pub const MAX_MANAGED_SUPPORT_FILES: usize = 20;
pub const MAX_MANAGED_SUPPORT_FILE_BYTES: usize = 64 * 1024;
pub const MAX_MANAGED_SKILL_BODY_BYTES: usize = 256 * 1024;

/// Provenance marker written into the frontmatter of every host-loadable
/// skill file that `TraceDecay` automation materializes. The materialization
/// reconciler owns (updates/removes) only files carrying this exact marker, so
/// user-authored and repo-local dev skills are never touched.
pub const MATERIALIZED_SKILL_MANAGED_BY: &str = "tracedecay-automation";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkillInstallTarget {
    Cursor,
    Codex,
    Claude,
    Agents,
    #[serde(rename = "opencode")]
    OpenCode,
    Kimi,
    Kiro,
    Hermes,
}

impl SkillInstallTarget {
    pub fn is_native_overlay(self) -> bool {
        matches!(self, Self::Cursor | Self::Codex | Self::Hermes)
    }

    /// True for hosts that reconcile their managed-skill listing as a
    /// marker-gated block inside a prompt file. Native-overlay hosts
    /// (Cursor/Codex) deploy a skills directory instead, and Hermes owns its
    /// own curation — neither writes a prompt-index block.
    pub fn writes_prompt_index(self) -> bool {
        !self.is_native_overlay() && self != Self::Hermes
    }

    pub fn prompt_label(self) -> &'static str {
        match self {
            Self::Cursor => "Cursor",
            Self::Codex => "Codex",
            Self::Claude => "Claude",
            Self::Agents => "AGENTS.md",
            Self::OpenCode => "OpenCode",
            Self::Kimi => "Kimi",
            Self::Kiro => "Kiro",
            Self::Hermes => "Hermes",
        }
    }
}

pub fn default_managed_skill_targets() -> Vec<SkillInstallTarget> {
    vec![
        SkillInstallTarget::Cursor,
        SkillInstallTarget::Codex,
        SkillInstallTarget::Claude,
        SkillInstallTarget::Agents,
        SkillInstallTarget::OpenCode,
        SkillInstallTarget::Kimi,
        SkillInstallTarget::Kiro,
        SkillInstallTarget::Hermes,
    ]
}

fn managed_skill_description(summary: &str) -> String {
    let trimmed = summary.trim();
    let description = if trimmed
        .get(..8)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("use when"))
        || trimmed
            .get(..19)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("use this skill when"))
    {
        trimmed.to_string()
    } else {
        format!("Use when {trimmed}")
    };
    truncate_frontmatter_chars(&description, MAX_NATIVE_SKILL_DESCRIPTION_CHARS)
}

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ManagedSkillSource {
    AutomationRun,
    UserDraft,
    Import,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ManagedSkillState {
    PendingApproval,
    Active,
    Disabled,
    Archived,
}

/// Where an active managed skill is host-materialized as a real `SKILL.md`.
///
/// Defaults to [`Self::Global`]: a skill is written only into the user's global
/// host dirs (`~/.claude`, `~/.codex`) and is never poured into every project
/// checkout, so repos are not polluted with untracked `.claude/skills/**` files
/// and hosts do not load duplicate copies. Skills whose evidence is genuinely
/// project-local opt into [`Self::Project`] to also materialize into the
/// enclosing project root.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ManagedSkillMaterializationScope {
    #[default]
    Global,
    Project,
}

impl ManagedSkillMaterializationScope {
    /// Whether this skill should also be materialized into project checkouts.
    pub fn materializes_into_projects(self) -> bool {
        matches!(self, Self::Project)
    }

    /// Whether this is the default (global-only) scope. Used to keep the
    /// serialized record additive: the field is omitted for global skills, so
    /// existing `skill.json` payloads are byte-for-byte unchanged.
    // `serde(skip_serializing_if)` requires a `&self` predicate.
    #[allow(clippy::trivially_copy_pass_by_ref)]
    fn is_global(&self) -> bool {
        matches!(self, Self::Global)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManagedSkillProvenance {
    pub source: ManagedSkillSource,
    pub actor: String,
    pub run_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManagedSupportFile {
    pub path: PathBuf,
    pub bytes: Vec<u8>,
}

impl ManagedSupportFile {
    pub fn new(path: impl AsRef<Path>, bytes: Vec<u8>) -> Result<Self> {
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
                category: self.category,
                targets: self.targets,
                state: ManagedSkillState::PendingApproval,
                materialization_scope: ManagedSkillMaterializationScope::default(),
                pinned: false,
                checksum: String::new(),
                created_at: now,
                updated_at: now,
                approved_at: None,
                absorbed_into: None,
                archived_reason: None,
                provenance: self.provenance,
            },
            body_markdown: self.body_markdown,
            support_files: self.support_files,
            pending_update: None,
        };
        validate_managed_skill(&skill)?;
        skill.refresh_checksum();
        Ok(skill)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManagedSkillMetadata {
    pub id: String,
    pub title: String,
    pub summary: String,
    pub category: String,
    #[serde(default = "default_managed_skill_targets")]
    pub targets: Vec<SkillInstallTarget>,
    pub state: ManagedSkillState,
    /// Host-materialization reach. Defaults to global-only so records written
    /// before this field existed (and every automation-authored skill) never
    /// spray materialized files into project checkouts.
    #[serde(
        default,
        skip_serializing_if = "ManagedSkillMaterializationScope::is_global"
    )]
    pub materialization_scope: ManagedSkillMaterializationScope,
    pub pinned: bool,
    pub checksum: String,
    #[serde(default)]
    pub created_at: i64,
    #[serde(default)]
    pub updated_at: i64,
    /// When the skill last transitioned into `Active` (human approval).
    /// Anchors post-approval outcome tracking; absent for never-approved
    /// skills and records written before this field existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approved_at: Option<i64>,
    /// Canonical managed-skill id that absorbed this archived skill.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub absorbed_into: Option<String>,
    /// Structured lifecycle reason retained when a skill is archived.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archived_reason: Option<String>,
    pub provenance: ManagedSkillProvenance,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManagedSkill {
    pub metadata: ManagedSkillMetadata,
    pub body_markdown: String,
    pub support_files: Vec<ManagedSupportFile>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_update: Option<ManagedSkillPendingUpdate>,
}

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManagedSkillUpdate {
    pub title: Option<String>,
    pub summary: Option<String>,
    pub category: Option<String>,
    pub targets: Option<Vec<SkillInstallTarget>>,
    pub body_markdown: Option<String>,
    pub support_files: Option<Vec<ManagedSupportFile>>,
    pub pinned: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManagedSkillPendingUpdate {
    pub base_checksum: String,
    pub staged_at: i64,
    pub metadata: ManagedSkillMetadata,
    pub body_markdown: String,
    #[serde(default)]
    pub support_files: Vec<ManagedSupportFile>,
    /// Lifecycle state the skill transitions to when this staged change is
    /// approved. `None` keeps the historical behavior (promote to `Active`).
    /// Staged consolidations set `Some(Archived)`; skill content is always
    /// preserved on disk (archive, never delete).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resulting_state: Option<ManagedSkillState>,
    /// Reviewer-facing reason recorded when the change was staged (used by
    /// consolidation proposals).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub staged_reason: Option<String>,
}

impl ManagedSkillPendingUpdate {
    pub fn into_skill(self) -> ManagedSkill {
        ManagedSkill {
            metadata: self.metadata,
            body_markdown: self.body_markdown,
            support_files: self.support_files,
            pending_update: None,
        }
    }

    pub fn normalize_timestamps(&mut self) {
        let mut skill = ManagedSkill {
            metadata: self.metadata.clone(),
            body_markdown: self.body_markdown.clone(),
            support_files: self.support_files.clone(),
            pending_update: None,
        };
        skill.normalize_timestamps();
        self.metadata = skill.metadata;
    }
}

impl ManagedSkill {
    pub fn set_state(&mut self, state: ManagedSkillState) {
        if self.metadata.state != state {
            self.metadata.state = state;
            if state == ManagedSkillState::Active {
                self.metadata.approved_at = Some(current_metadata_timestamp());
            }
            self.touch();
        }
    }

    pub fn set_pinned(&mut self, pinned: bool) {
        if self.metadata.pinned != pinned {
            self.metadata.pinned = pinned;
            self.touch();
        }
    }

    pub fn touch(&mut self) {
        self.metadata.updated_at = current_metadata_timestamp();
    }

    pub fn normalize_timestamps(&mut self) {
        let now = current_metadata_timestamp();
        match (self.metadata.created_at, self.metadata.updated_at) {
            (0, 0) => {
                self.metadata.created_at = now;
                self.metadata.updated_at = now;
            }
            (0, updated_at) => {
                self.metadata.created_at = updated_at;
            }
            (created_at, 0) => {
                self.metadata.updated_at = created_at;
            }
            (created_at, updated_at) if updated_at < created_at => {
                self.metadata.updated_at = created_at;
            }
            _ => {}
        }
    }

    pub fn refresh_checksum(&mut self) {
        self.metadata.checksum = self.content_checksum();
    }

    pub fn render_skill_markdown(&self) -> String {
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

    pub fn render_native_skill_markdown(&self) -> Result<String> {
        let mut output = String::new();
        output.push_str("---\n");
        let _ = writeln!(output, "name: {}", native_skill_name(&self.metadata.id));
        let _ = writeln!(
            output,
            "description: {}",
            frontmatter_string(&managed_skill_description(&self.metadata.summary))
        );
        output.push_str("---\n\n");
        output.push_str(&self.body_markdown);
        output.push('\n');
        validate_native_skill_markdown(&output)?;
        Ok(output)
    }

    /// Kebab-case slug used as the directory name for the host-loadable
    /// materialized skill (`<skills_dir>/<slug>/SKILL.md`). Derived from the
    /// managed-skill id the same way the native overlay derives its `name:`.
    pub fn host_skill_slug(&self) -> String {
        native_skill_name(&self.metadata.id)
    }

    /// Stable identity of the complete host-loadable package contract: the
    /// rendered `SKILL.md` fields/body plus every support path and payload.
    pub fn materialized_package_hash(&self) -> Result<String> {
        let markdown = self.render_materialized_skill_markdown_with_hash("<package-hash>")?;
        let mut hasher = Sha256::new();
        hasher.update(markdown.as_bytes());
        let mut support_files = self.support_files.iter().collect::<Vec<_>>();
        support_files.sort_by(|left, right| left.path.cmp(&right.path));
        for support in support_files {
            // Keep package-hash path bytes slash-normalized so Windows
            // on-disk recompute (which joins Path components with `/`) matches.
            let key = support
                .path
                .components()
                .filter_map(|component| match component {
                    std::path::Component::Normal(part) => Some(part.to_string_lossy()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("/");
            hasher.update(b"\0file:");
            hasher.update(key.as_bytes());
            hasher.update(b"\0");
            hasher.update(&support.bytes);
        }
        Ok(format!("sha256:{}", hex::encode(hasher.finalize())))
    }

    /// Renders a host-loadable `SKILL.md` with provenance frontmatter marking
    /// the file as owned by `TraceDecay` automation. Hosts (Claude Code, Codex)
    /// read `name`/`description`; the extra `managed-by`/`skill-id`/
    /// `content-hash`/`skill-version` keys are ignored by the host but let the
    /// reconciler own exactly its own files and detect drift.
    pub fn render_materialized_skill_markdown(&self) -> Result<String> {
        let package_hash = self.materialized_package_hash()?;
        self.render_materialized_skill_markdown_with_hash(&package_hash)
    }

    fn render_materialized_skill_markdown_with_hash(&self, package_hash: &str) -> Result<String> {
        // Reuse the native name/description derivation + bounds so the host
        // frontmatter shape matches the overlay exactly.
        let name = native_skill_name(&self.metadata.id);
        let description = managed_skill_description(&self.metadata.summary);
        {
            // Validate the host-facing fields via the native validator by
            // rendering a name/description-only document first.
            let native_only = format!(
                "---\nname: {name}\ndescription: {}\n---\n\n{}\n",
                frontmatter_string(&description),
                self.body_markdown
            );
            validate_native_skill_markdown(&native_only)?;
        }

        let mut output = String::new();
        output.push_str("---\n");
        let _ = writeln!(output, "name: {name}");
        let _ = writeln!(output, "description: {}", frontmatter_string(&description));
        let _ = writeln!(output, "managed-by: {MATERIALIZED_SKILL_MANAGED_BY}");
        let _ = writeln!(
            output,
            "skill-id: {}",
            frontmatter_string(&self.metadata.id)
        );
        let _ = writeln!(output, "content-hash: {package_hash}");
        let _ = writeln!(output, "skill-version: {}", self.metadata.updated_at);
        output.push_str("---\n\n");
        output.push_str(&self.body_markdown);
        output.push('\n');
        Ok(output)
    }

    fn content_checksum(&self) -> String {
        let mut hasher = Sha256::new();
        hasher.update(self.metadata.id.as_bytes());
        hasher.update(b"\0");
        hasher.update(self.metadata.title.as_bytes());
        hasher.update(b"\0");
        hasher.update(self.metadata.summary.as_bytes());
        hasher.update(b"\0");
        hasher.update(self.metadata.category.as_bytes());
        hasher.update(b"\0");
        for target in &self.metadata.targets {
            hasher.update(b"\0target:");
            hasher.update(target_key(*target).as_bytes());
        }
        hasher.update(b"\0");
        hasher.update(self.body_markdown.as_bytes());
        for file in &self.support_files {
            let key = file
                .path
                .components()
                .filter_map(|component| match component {
                    std::path::Component::Normal(part) => Some(part.to_string_lossy()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("/");
            hasher.update(b"\0file:");
            hasher.update(key.as_bytes());
            hasher.update(b"\0");
            hasher.update(&file.bytes);
        }
        format!("sha256:{}", hex::encode(hasher.finalize()))
    }
}

pub fn current_metadata_timestamp() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::skill_frontmatter::parse_skill_frontmatter;

    #[test]
    fn native_skill_markdown_round_trips_escaped_description() {
        let skill = ManagedSkillDraft {
            id: "native-escape".to_string(),
            title: "Native escape".to_string(),
            summary: r#"Use when checking "quoted" paths like C:\tmp"#.to_string(),
            category: "testing".to_string(),
            targets: vec![SkillInstallTarget::Codex],
            body_markdown: "# Native escape\n".to_string(),
            support_files: Vec::new(),
            provenance: ManagedSkillProvenance {
                source: ManagedSkillSource::UserDraft,
                actor: "tester".to_string(),
                run_id: None,
            },
        }
        .materialize()
        .unwrap();

        let markdown = skill.render_native_skill_markdown().unwrap();
        let frontmatter = parse_skill_frontmatter(&markdown).unwrap();

        assert_eq!(
            frontmatter["description"].as_scalar(),
            Some(r#"Use when checking "quoted" paths like C:\tmp"#)
        );
    }

    #[test]
    fn legacy_skill_without_consolidation_metadata_deserializes() {
        let skill = ManagedSkillDraft {
            id: "legacy-skill".to_string(),
            title: "Legacy skill".to_string(),
            summary: "Read records written before consolidation metadata.".to_string(),
            category: "testing".to_string(),
            targets: vec![SkillInstallTarget::Codex],
            body_markdown: "# Legacy\n".to_string(),
            support_files: Vec::new(),
            provenance: ManagedSkillProvenance {
                source: ManagedSkillSource::AutomationRun,
                actor: "legacy".to_string(),
                run_id: None,
            },
        }
        .materialize()
        .unwrap();
        let mut value = serde_json::to_value(skill).unwrap();
        let metadata = value["metadata"].as_object_mut().unwrap();
        metadata.remove("absorbed_into");
        metadata.remove("archived_reason");

        let decoded: ManagedSkill = serde_json::from_value(value).unwrap();
        assert_eq!(decoded.metadata.absorbed_into, None);
        assert_eq!(decoded.metadata.archived_reason, None);
    }
}
