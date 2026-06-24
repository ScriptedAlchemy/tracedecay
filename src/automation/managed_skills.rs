use std::fmt::Write as _;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::errors::{Result, TraceDecayError};

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
        validate_relative_path(path)?;
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
    pub body_markdown: String,
    pub support_files: Vec<ManagedSupportFile>,
    pub provenance: ManagedSkillProvenance,
}

impl ManagedSkillDraft {
    pub fn materialize(self) -> Result<ManagedSkill> {
        validate_skill_id(&self.id)?;
        validate_non_empty("title", &self.title)?;
        validate_non_empty("summary", &self.summary)?;
        validate_non_empty("category", &self.category)?;
        validate_non_empty("body_markdown", &self.body_markdown)?;
        validate_non_empty("provenance actor", &self.provenance.actor)?;

        let mut skill = ManagedSkill {
            metadata: ManagedSkillMetadata {
                id: self.id,
                title: self.title,
                summary: self.summary,
                category: self.category,
                state: ManagedSkillState::PendingApproval,
                pinned: false,
                checksum: String::new(),
                provenance: self.provenance,
            },
            body_markdown: self.body_markdown,
            support_files: self.support_files,
        };
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
    pub state: ManagedSkillState,
    pub pinned: bool,
    pub checksum: String,
    pub provenance: ManagedSkillProvenance,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManagedSkill {
    pub metadata: ManagedSkillMetadata,
    pub body_markdown: String,
    pub support_files: Vec<ManagedSupportFile>,
}

impl ManagedSkill {
    pub fn set_state(&mut self, state: ManagedSkillState) {
        self.metadata.state = state;
    }

    pub fn set_pinned(&mut self, pinned: bool) {
        self.metadata.pinned = pinned;
    }

    pub fn refresh_checksum(&mut self) {
        self.metadata.checksum = self.content_checksum();
    }

    pub fn render_skill_markdown(&self) -> String {
        let mut output = String::new();
        output.push_str("---\n");
        let _ = writeln!(output, "id: {}", self.metadata.id);
        let _ = writeln!(output, "title: {}", self.metadata.title);
        let _ = writeln!(output, "summary: {}", self.metadata.summary);
        let _ = writeln!(output, "category: {}", self.metadata.category);
        let _ = writeln!(output, "state: {}", state_key(self.metadata.state));
        let _ = writeln!(output, "pinned: {}", self.metadata.pinned);
        let _ = writeln!(output, "checksum: {}", self.metadata.checksum);
        let _ = writeln!(
            output,
            "provenance_source: {}",
            source_key(self.metadata.provenance.source)
        );
        let _ = writeln!(
            output,
            "provenance_actor: {}",
            self.metadata.provenance.actor
        );
        if let Some(run_id) = &self.metadata.provenance.run_id {
            let _ = writeln!(output, "provenance_run_id: {run_id}");
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

fn validate_skill_id(id: &str) -> Result<()> {
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
    if path.as_os_str().is_empty() || path.is_absolute() {
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

fn validate_non_empty(field: &str, value: &str) -> Result<()> {
    if value.trim().is_empty() {
        Err(config_error(format!(
            "managed skill {field} cannot be empty"
        )))
    } else {
        Ok(())
    }
}

fn config_error(message: String) -> TraceDecayError {
    TraceDecayError::Config { message }
}

fn source_key(source: ManagedSkillSource) -> &'static str {
    match source {
        ManagedSkillSource::AutomationRun => "automation_run",
        ManagedSkillSource::UserDraft => "user_draft",
        ManagedSkillSource::Import => "import",
    }
}

fn state_key(state: ManagedSkillState) -> &'static str {
    match state {
        ManagedSkillState::PendingApproval => "pending_approval",
        ManagedSkillState::Active => "active",
        ManagedSkillState::Disabled => "disabled",
    }
}
