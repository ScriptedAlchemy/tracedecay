use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::agents::safe_write_text_file;
use crate::errors::Result;

const MANIFEST_FILE: &str = ".tracedecay-managed-agents.json";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManagedAgentExportEntry {
    pub id: String,
    pub path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManagedAgentInstallSummary {
    pub output: PathBuf,
    pub exported_count: usize,
    pub exported: Vec<ManagedAgentExportEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct ManagedAgentManifest {
    version: u32,
    exported: Vec<ManagedAgentExportEntry>,
}

fn agents() -> &'static [crate::agents::plugin_bundle::PluginFile] {
    crate::agents::plugin_bundle::codex_agent_files()
}

fn generated_agent_id(relative: &'static str) -> &'static str {
    relative
        .strip_prefix("tracedecay-")
        .and_then(|name| name.strip_suffix(".toml"))
        .unwrap_or_else(|| panic!("invalid generated Codex agent path: {relative}"))
}

pub fn install_codex_managed_agents(home: &Path) -> Result<ManagedAgentInstallSummary> {
    let agents_dir = agents_dir(home);
    fs::create_dir_all(&agents_dir)?;
    remove_stale_managed_agents(&agents_dir)?;

    let mut exported = Vec::with_capacity(agents().len());
    for agent in agents() {
        let id = generated_agent_id(agent.relative);
        let path = agents_dir.join(agent.relative);
        safe_write_text_file(&path, agent.contents, None)?;
        exported.push(ManagedAgentExportEntry {
            id: id.to_string(),
            path,
        });
    }

    let manifest = ManagedAgentManifest {
        version: 1,
        exported: exported.clone(),
    };
    safe_write_text_file(
        &agents_dir.join(MANIFEST_FILE),
        &format!("{}\n", serde_json::to_string_pretty(&manifest)?),
        None,
    )?;

    Ok(ManagedAgentInstallSummary {
        output: agents_dir,
        exported_count: exported.len(),
        exported,
    })
}

pub fn remove_managed_agents(agents_dir: &Path) -> Result<()> {
    let manifest_path = agents_dir.join(MANIFEST_FILE);
    let exported = match fs::read_to_string(&manifest_path) {
        Ok(contents) => serde_json::from_str::<ManagedAgentManifest>(&contents)?.exported,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(err) => return Err(err.into()),
    };

    for entry in exported {
        if path_is_direct_child(&entry.path, agents_dir) {
            remove_managed_agent_file(&entry.path);
        }
    }
    remove_managed_agent_file(&manifest_path);
    fs::remove_dir(agents_dir).ok();
    Ok(())
}

/// Delete a generated export through the host-config write-intent recorder.
///
/// `~/.codex/agents` is declared registration surface, so rollback compares
/// every path's observed state against the backup and accepts a difference
/// only when a recorded intent attributes it. A raw `remove_file` leaves the
/// deletion unattributable, which made rollback refuse with `StalePreview` and
/// strand the journal.
fn remove_managed_agent_file(path: &Path) {
    crate::agents::safe_remove_host_file(path).ok();
}

/// Every generated-agent path that a Codex lifecycle operation may mutate.
///
/// This includes both the current bundle's exports and safe direct-child
/// entries from the previous ownership manifest so aggregate transactions can
/// restore stale exports removed during an update.
pub fn managed_agent_transaction_paths(home: &Path) -> Vec<PathBuf> {
    let agents_dir = agents_dir(home);
    let mut paths = agents()
        .iter()
        .map(|agent| agents_dir.join(agent.relative))
        .chain([agents_dir.join(MANIFEST_FILE)])
        .collect::<BTreeSet<_>>();
    if let Ok(previous_paths) = manifest_paths(&agents_dir) {
        paths.extend(previous_paths);
    }
    paths.into_iter().collect()
}

pub fn managed_agent_label(agent_id: &str) -> Option<&'static str> {
    let normalized = agent_id.strip_prefix("tracedecay-").unwrap_or(agent_id);
    agents()
        .iter()
        .map(|agent| generated_agent_id(agent.relative))
        .find(|id| *id == normalized)
}

fn agents_dir(home: &Path) -> PathBuf {
    home.join(".codex/agents")
}

fn remove_stale_managed_agents(agents_dir: &Path) -> Result<()> {
    let keep: BTreeSet<PathBuf> = agents()
        .iter()
        .map(|agent| agents_dir.join(agent.relative))
        .chain([agents_dir.join(MANIFEST_FILE)])
        .collect();

    for path in manifest_paths(agents_dir)? {
        if !keep.contains(&path) {
            remove_managed_agent_file(&path);
        }
    }
    Ok(())
}

fn manifest_paths(agents_dir: &Path) -> Result<Vec<PathBuf>> {
    let manifest_path = agents_dir.join(MANIFEST_FILE);
    let contents = match fs::read_to_string(&manifest_path) {
        Ok(contents) => contents,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(err.into()),
    };
    let manifest: ManagedAgentManifest = serde_json::from_str(&contents)?;
    Ok(manifest
        .exported
        .into_iter()
        .filter_map(|entry| path_is_direct_child(&entry.path, agents_dir).then_some(entry.path))
        .collect())
}

fn path_is_direct_child(path: &Path, parent: &Path) -> bool {
    path.parent() == Some(parent)
}
