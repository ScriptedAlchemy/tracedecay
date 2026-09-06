//! Host-install surface that used to live on `tracedecay-agent-hosts::agents`.
//!
//! Pure helpers (`home_dir`, `uses_default_user_profile`, the skill-index
//! marker) are implemented here. Host-config writes, PATH resolution, plugin
//! bundle files, and managed-skill export sweeps stay in agent-hosts and are
//! registered at process start so this crate does not depend on it.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use serde::Serialize;
use serde_json::Value;

use super::skill_targets::SkillInstallSummary;
use crate::errors::{Result, TraceDecayError};

/// The unslugged managed-skill start marker. Same literal the agent-hosts
/// prompt-rules block-splicer stops at.
pub const SKILL_INDEX_START: &str = "<!-- TRACEDECAY MANAGED SKILLS START -->";

/// Per-agent outcome of a managed-skill export refresh.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
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

type ExportToAgents = fn(&Path, &Path) -> Vec<ManagedSkillExportReport>;
type ExportToAgentHosts = fn(&Path, &Path, &Path) -> Vec<ManagedSkillExportReport>;
type WriteText = fn(&Path, &str, Option<&Path>) -> Result<()>;
type WriteJson = fn(&Path, &Value, Option<&Path>) -> Result<()>;
type RemoveHostFile = fn(&Path) -> std::io::Result<()>;
type ResolveOnPath = fn(&str, Option<&OsStr>) -> Result<Option<PathBuf>>;
type CodexAgentFiles = fn() -> &'static [PluginFile];
type WithWriteIntents = fn(PathBuf, &mut dyn FnMut());

static HOST_IO: OnceLock<HostIoRegistration> = OnceLock::new();

/// Registered host-install implementations. First registration wins.
///
/// This is one capability value, not eight independent slots: the whole
/// bundle is installed under a single [`OnceLock`], so a later or concurrent
/// registration can never replace part of an earlier one. Every reader below
/// therefore sees callbacks from exactly one registration.
pub struct HostIoRegistration {
    pub export_to_agents: ExportToAgents,
    pub export_to_agent_hosts: ExportToAgentHosts,
    pub write_text: WriteText,
    pub write_json: WriteJson,
    pub remove_host_file: RemoveHostFile,
    pub resolve_on_path: ResolveOnPath,
    pub codex_agent_files: CodexAgentFiles,
    pub with_write_intents: WithWriteIntents,
}

/// Installs the agent-hosts host-install surface. Idempotent and atomic:
/// the first complete bundle wins and no later registration is mixed into it.
pub fn register(registration: HostIoRegistration) {
    let _ = HOST_IO.set(registration);
}

/// The one registered bundle, or `None` when no root ever registered.
#[must_use]
pub fn registered() -> Option<&'static HostIoRegistration> {
    HOST_IO.get()
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

#[hotpath::measure(label = "automation.host_io.export_agents")]
pub fn export_managed_skills_to_agents(
    home: &Path,
    profile_root: &Path,
) -> Result<Vec<ManagedSkillExportReport>> {
    match registered().map(|io| io.export_to_agents) {
        Some(export) => Ok(export(home, profile_root)),
        None => Err(unregistered("export_managed_skills_to_agents")),
    }
}

#[hotpath::measure(label = "automation.host_io.export_hosts")]
pub fn export_managed_skills_to_agent_hosts(
    home: &Path,
    project_root: &Path,
    profile_root: &Path,
) -> Result<Vec<ManagedSkillExportReport>> {
    match registered().map(|io| io.export_to_agent_hosts) {
        Some(export) => Ok(export(home, project_root, profile_root)),
        None => Err(unregistered("export_managed_skills_to_agent_hosts")),
    }
}

pub fn safe_write_text_file(path: &Path, contents: &str, backup: Option<&Path>) -> Result<()> {
    match registered().map(|io| io.write_text) {
        Some(write) => write(path, contents, backup),
        None => Err(unregistered("safe_write_text_file")),
    }
}

pub fn safe_write_json_file(path: &Path, value: &Value, backup: Option<&Path>) -> Result<()> {
    match registered().map(|io| io.write_json) {
        Some(write) => write(path, value, backup),
        None => Err(unregistered("safe_write_json_file")),
    }
}

pub fn safe_remove_host_file(path: &Path) -> std::io::Result<()> {
    match registered().map(|io| io.remove_host_file) {
        Some(remove) => remove(path),
        None => Err(std::io::Error::other(
            "host-config write surface is unavailable: no host I/O is registered",
        )),
    }
}

pub fn resolve_on_path(program: &str, path_var: Option<&OsStr>) -> Result<Option<PathBuf>> {
    match registered().map(|io| io.resolve_on_path) {
        Some(resolve) => resolve(program, path_var),
        None => Err(unregistered("resolve_on_path")),
    }
}

pub fn codex_agent_files() -> Result<&'static [PluginFile]> {
    match registered().map(|io| io.codex_agent_files) {
        Some(files) => Ok(files()),
        None => Err(unregistered("codex_agent_files")),
    }
}

pub fn with_host_config_write_intents<T>(root: PathBuf, effect: impl FnOnce() -> T) -> Result<T> {
    let Some(with_intents) = registered().map(|io| io.with_write_intents) else {
        return Err(unregistered("with_host_config_write_intents"));
    };
    let mut effect = Some(effect);
    let mut slot = None;
    with_intents(root, &mut || {
        if let Some(effect) = effect.take() {
            slot = Some(effect());
        }
    });
    match slot {
        Some(value) => Ok(value),
        None => Err(TraceDecayError::Config {
            message: "host-config write surface is unavailable: with_host_config_write_intents adapter did not invoke the effect".to_string(),
        }),
    }
}

fn unregistered(name: &str) -> TraceDecayError {
    TraceDecayError::Config {
        message: format!("host-config write surface is unavailable: {name} is not registered"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn export_agents(_: &Path, _: &Path) -> Vec<ManagedSkillExportReport> {
        Vec::new()
    }

    fn export_hosts(_: &Path, _: &Path, _: &Path) -> Vec<ManagedSkillExportReport> {
        Vec::new()
    }

    fn write_text(_: &Path, _: &str, _: Option<&Path>) -> Result<()> {
        Ok(())
    }

    fn write_json(_: &Path, _: &Value, _: Option<&Path>) -> Result<()> {
        Ok(())
    }

    fn remove_file(_: &Path) -> std::io::Result<()> {
        Ok(())
    }

    fn resolve(_: &str, _: Option<&OsStr>) -> Result<Option<PathBuf>> {
        Ok(None)
    }

    fn files() -> &'static [PluginFile] {
        &[]
    }

    fn intents(_: PathBuf, effect: &mut dyn FnMut()) {
        effect();
    }

    fn bundle() -> HostIoRegistration {
        HostIoRegistration {
            export_to_agents: export_agents,
            export_to_agent_hosts: export_hosts,
            write_text,
            write_json,
            remove_host_file: remove_file,
            resolve_on_path: resolve,
            codex_agent_files: files,
            with_write_intents: intents,
        }
    }

    /// Every callback must come from one registration. A second `register`
    /// call replaces nothing, so no reader can observe a bundle spliced from
    /// two roots — the failure mode eight independent `OnceLock` slots had.
    #[test]
    fn a_second_registration_cannot_replace_part_of_the_first() {
        register(bundle());
        let installed = registered().expect("a bundle is registered");
        register(bundle());
        assert!(
            std::ptr::eq(
                installed,
                registered().expect("the bundle survives a second register")
            ),
            "a second register must not replace any part of the first bundle"
        );
    }
}
