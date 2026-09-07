//! Host-install surface that used to live on `tracedecay-agent-hosts::agents`.
//!
//! Pure helpers (`home_dir`, `uses_default_user_profile`, the skill-index
//! marker) are implemented here. Host-config writes, plugin bundle files, and
//! managed-skill export sweeps stay in agent-hosts, which sits above this
//! crate: it builds one [`HostIo`] value and passes it to every automation
//! entry point that writes host-owned files. There is no process-global slot,
//! so omitting the bundle is a compile error rather than a runtime fallback.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::skill_targets::SkillInstallSummary;
use crate::errors::Result;

/// The unslugged managed-skill start marker. Same literal the agent-hosts
/// prompt-rules block-splicer stops at.
pub const SKILL_INDEX_START: &str = "<!-- TRACEDECAY MANAGED SKILLS START -->";

/// Per-agent outcome of a managed-skill export refresh.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManagedSkillExportReport {
    pub agent: String,
    pub exports: Vec<SkillInstallSummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// One embedded plugin file: `relative` is its deploy path.
#[derive(Clone, Copy)]
pub struct PluginFile {
    pub relative: &'static str,
    pub contents: &'static str,
}

pub type ExportToAgents = fn(&Path, &Path) -> Vec<ManagedSkillExportReport>;
pub type ExportToAgentHosts = fn(&Path, &Path, &Path) -> Vec<ManagedSkillExportReport>;
pub type WriteText = fn(&Path, &str, Option<&Path>) -> Result<()>;
pub type WriteJson = fn(&Path, &Value, Option<&Path>) -> Result<()>;
pub type RemoveHostFile = fn(&Path) -> std::io::Result<()>;
pub type CodexAgentFiles = fn() -> &'static [PluginFile];

/// The host-install implementations automation borrows from the host
/// installers.
///
/// One capability value, not six independent slots: a caller either has a
/// whole bundle or none, so no reader can observe callbacks spliced from two
/// compositions. Every field is a plain `fn` pointer, which keeps the bundle
/// `Copy` and lets fixtures build an isolated one per test. A bundle is
/// all-or-nothing: a struct literal that leaves any surface out does not
/// compile, and there is no `Default` to fill one in.
#[derive(Clone, Copy)]
pub struct HostIo {
    pub export_to_agents: ExportToAgents,
    pub export_to_agent_hosts: ExportToAgentHosts,
    pub write_text: WriteText,
    pub write_json: WriteJson,
    pub remove_host_file: RemoveHostFile,
    pub codex_agent_files: CodexAgentFiles,
}

impl HostIo {
    #[hotpath::measure(label = "automation.host_io.export_agents")]
    pub fn export_managed_skills_to_agents(
        &self,
        home: &Path,
        profile_root: &Path,
    ) -> Vec<ManagedSkillExportReport> {
        (self.export_to_agents)(home, profile_root)
    }

    #[hotpath::measure(label = "automation.host_io.export_hosts")]
    pub fn export_managed_skills_to_agent_hosts(
        &self,
        home: &Path,
        project_root: &Path,
        profile_root: &Path,
    ) -> Vec<ManagedSkillExportReport> {
        (self.export_to_agent_hosts)(home, project_root, profile_root)
    }

    pub fn safe_write_text_file(
        &self,
        path: &Path,
        contents: &str,
        backup: Option<&Path>,
    ) -> Result<()> {
        (self.write_text)(path, contents, backup)
    }

    pub fn safe_write_json_file(
        &self,
        path: &Path,
        value: &Value,
        backup: Option<&Path>,
    ) -> Result<()> {
        (self.write_json)(path, value, backup)
    }

    pub fn safe_remove_host_file(&self, path: &Path) -> std::io::Result<()> {
        (self.remove_host_file)(path)
    }

    pub fn codex_agent_files(&self) -> &'static [PluginFile] {
        (self.codex_agent_files)()
    }
}

/// Returns the user's home directory, cross-platform.
#[must_use]
pub fn home_dir() -> Option<PathBuf> {
    std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .ok()
        .map(PathBuf::from)
}

#[must_use]
pub fn uses_default_user_profile(home: &Path, profile_root: &Path) -> bool {
    profile_root == home.join(".tracedecay")
}

/// A bundle whose file writes land on disk plainly and whose export sweeps
/// touch no agent host, for tests that exercise automation without a host
/// installer.
#[cfg(test)]
pub(crate) fn plain_file_host_io() -> HostIo {
    fn export_to_agents(_: &Path, _: &Path) -> Vec<ManagedSkillExportReport> {
        Vec::new()
    }

    fn export_to_agent_hosts(_: &Path, _: &Path, _: &Path) -> Vec<ManagedSkillExportReport> {
        Vec::new()
    }

    fn write_text(path: &Path, contents: &str, _: Option<&Path>) -> Result<()> {
        Ok(std::fs::write(path, contents)?)
    }

    fn write_json(path: &Path, value: &Value, _: Option<&Path>) -> Result<()> {
        Ok(std::fs::write(path, serde_json::to_vec_pretty(value)?)?)
    }

    fn remove_host_file(path: &Path) -> std::io::Result<()> {
        std::fs::remove_file(path)
    }

    fn codex_agent_files() -> &'static [PluginFile] {
        &[]
    }

    HostIo {
        export_to_agents,
        export_to_agent_hosts,
        write_text,
        write_json,
        remove_host_file,
        codex_agent_files,
    }
}
