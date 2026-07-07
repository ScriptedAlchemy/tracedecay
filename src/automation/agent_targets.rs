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

struct AgentTemplate {
    id: &'static str,
    codex: &'static str,
}

const AGENTS: &[AgentTemplate] = &[
    AgentTemplate {
        id: "code-explorer",
        codex: include_str!("../agents/codex_agents/code-explorer.toml"),
    },
    AgentTemplate {
        id: "code-health-auditor",
        codex: include_str!("../agents/codex_agents/code-health-auditor.toml"),
    },
    AgentTemplate {
        id: "session-historian",
        codex: include_str!("../agents/codex_agents/session-historian.toml"),
    },
];

pub fn install_codex_managed_agents(home: &Path) -> Result<ManagedAgentInstallSummary> {
    let agents_dir = agents_dir(home);
    fs::create_dir_all(&agents_dir)?;
    remove_stale_managed_agents(&agents_dir)?;

    let mut exported = Vec::with_capacity(AGENTS.len());
    for agent in AGENTS {
        let path = agents_dir.join(agent_file_name(agent.id));
        safe_write_text_file(&path, agent.codex, None)?;
        exported.push(ManagedAgentExportEntry {
            id: agent.id.to_string(),
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
            fs::remove_file(&entry.path).ok();
        }
    }
    fs::remove_file(&manifest_path).ok();
    fs::remove_dir(agents_dir).ok();
    Ok(())
}

pub fn managed_agent_label(agent_id: &str) -> Option<&'static str> {
    let normalized = agent_id.strip_prefix("tracedecay-").unwrap_or(agent_id);
    AGENTS
        .iter()
        .find(|agent| agent.id == normalized)
        .map(|agent| agent.id)
}

fn agents_dir(home: &Path) -> PathBuf {
    home.join(".codex/agents")
}

fn agent_file_name(id: &str) -> String {
    format!("tracedecay-{id}.toml")
}

fn remove_stale_managed_agents(agents_dir: &Path) -> Result<()> {
    let keep: BTreeSet<PathBuf> = AGENTS
        .iter()
        .map(|agent| agents_dir.join(agent_file_name(agent.id)))
        .chain([agents_dir.join(MANIFEST_FILE)])
        .collect();

    for path in manifest_paths(agents_dir)? {
        if !keep.contains(&path) {
            fs::remove_file(path).ok();
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
