use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::errors::Result;

use super::managed_skill_format::{frontmatter_string, source_key, state_key, target_key};
use super::managed_skill_validation::{validate_managed_skill, validate_support_file};

pub const MAX_MANAGED_SUPPORT_FILES: usize = 20;
pub const MAX_MANAGED_SUPPORT_FILE_BYTES: usize = 64 * 1024;
pub const MAX_MANAGED_SKILL_BODY_BYTES: usize = 256 * 1024;

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
        matches!(self, Self::Cursor | Self::Codex)
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
    ]
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
                pinned: false,
                checksum: String::new(),
                created_at: now,
                updated_at: now,
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
    pub pinned: bool,
    pub checksum: String,
    #[serde(default)]
    pub created_at: i64,
    #[serde(default)]
    pub updated_at: i64,
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
}

impl ManagedSkillPendingUpdate {
    pub(super) fn into_skill(self) -> ManagedSkill {
        ManagedSkill {
            metadata: self.metadata,
            body_markdown: self.body_markdown,
            support_files: self.support_files,
            pending_update: None,
        }
    }

    pub(super) fn normalize_timestamps(&mut self) {
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
            hasher.update(b"\0file:");
            hasher.update(file.path.to_string_lossy().as_bytes());
            hasher.update(b"\0");
            hasher.update(&file.bytes);
        }
        format!("sha256:{}", hex::encode(hasher.finalize()))
    }
}

pub(super) fn current_metadata_timestamp() -> i64 {
    crate::tracedecay::current_timestamp()
}
