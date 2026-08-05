use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde::{Deserialize, Serialize};
use tracedecay_global_db::{CodeProjectRecord, ProjectRegistryContext};

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ProjectRegistrySummary {
    pub project_count: usize,
    pub repo_count: usize,
    pub truncated: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ProjectRepoGroup {
    pub label: String,
    pub git_common_dir: Option<String>,
    pub project_count: usize,
    pub branches: Vec<String>,
    pub projects: Vec<ProjectRegistryEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ProjectRegistryEntry {
    pub project_id: String,
    pub label: String,
    pub project_root: String,
    pub canonical_root: String,
    pub kind: String,
    pub default_branch: Option<String>,
    pub branches: Vec<String>,
    pub store_count: usize,
    pub artifact_count: usize,
    pub alias_count: usize,
    pub last_seen_at: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_active: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PublicCodeProject {
    pub project_id: String,
    pub label: String,
    pub project_root: String,
    pub display_root: String,
    pub canonical_root: String,
    pub git_common_dir: Option<String>,
    pub default_branch: Option<String>,
    pub created_at: i64,
    pub last_seen_at: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_active: Option<bool>,
}

impl PublicCodeProject {
    pub fn from_record(project: &CodeProjectRecord, active_project_id: Option<&str>) -> Self {
        Self {
            project_id: project.project_id.clone(),
            label: path_label(&project.display_root),
            project_root: project.display_root.clone(),
            display_root: project.display_root.clone(),
            canonical_root: project.canonical_root.clone(),
            git_common_dir: project.git_common_dir.clone(),
            default_branch: project.default_branch.clone(),
            created_at: project.created_at,
            last_seen_at: project.last_seen_at,
            is_active: active_project_id.map(|id| id == project.project_id),
        }
    }
}

pub struct ProjectRegistryView {
    pub summary: ProjectRegistrySummary,
    pub project_tree: Vec<ProjectRepoGroup>,
}

pub fn build_project_registry_view(
    contexts: &[ProjectRegistryContext],
    active_project_id: Option<&str>,
    truncated: bool,
) -> ProjectRegistryView {
    let mut groups = BTreeMap::<String, ProjectRepoGroup>::new();
    for context in contexts {
        let key = context
            .project
            .git_common_dir
            .clone()
            .unwrap_or_else(|| context.project.canonical_root.clone());
        groups
            .entry(key)
            .or_insert_with(|| ProjectRepoGroup {
                label: repo_label(&context.project),
                git_common_dir: context.project.git_common_dir.clone(),
                project_count: 0,
                branches: Vec::new(),
                projects: Vec::new(),
            })
            .projects
            .push(project_entry(context, active_project_id));
    }
    let mut project_tree = groups.into_values().collect::<Vec<_>>();
    for group in &mut project_tree {
        group.projects.sort_by(|a, b| {
            a.label
                .cmp(&b.label)
                .then_with(|| a.project_id.cmp(&b.project_id))
        });
        group.project_count = group.projects.len();
        group.branches = group
            .projects
            .iter()
            .flat_map(|project| project.branches.iter().cloned())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
    }
    project_tree.sort_by(|a, b| a.label.cmp(&b.label));
    ProjectRegistryView {
        summary: ProjectRegistrySummary {
            project_count: contexts.len(),
            repo_count: project_tree.len(),
            truncated,
        },
        project_tree,
    }
}

fn project_entry(
    context: &ProjectRegistryContext,
    active_project_id: Option<&str>,
) -> ProjectRegistryEntry {
    let mut branches = BTreeSet::new();
    if let Some(branch) = &context.project.default_branch {
        branches.insert(branch.clone());
    }
    let artifact_count = context
        .stores
        .iter()
        .map(|store| store.artifacts.len())
        .sum();
    ProjectRegistryEntry {
        project_id: context.project.project_id.clone(),
        label: path_label(&context.project.display_root),
        project_root: context.project.display_root.clone(),
        canonical_root: context.project.canonical_root.clone(),
        kind: project_kind(&context.project),
        default_branch: context.project.default_branch.clone(),
        branches: branches.into_iter().collect(),
        store_count: context.stores.len(),
        artifact_count,
        alias_count: context.aliases.len(),
        last_seen_at: context.project.last_seen_at,
        is_active: active_project_id.map(|id| id == context.project.project_id),
    }
}

fn repo_label(project: &CodeProjectRecord) -> String {
    project
        .git_common_dir
        .as_deref()
        .and_then(|path| Path::new(path).parent())
        .map_or_else(
            || path_label(&project.display_root),
            |path| path_label(path.to_string_lossy().as_ref()),
        )
}

fn project_kind(project: &CodeProjectRecord) -> String {
    match &project.git_common_dir {
        Some(git_common_dir)
            if Path::new(&project.canonical_root).join(".git") == Path::new(git_common_dir) =>
        {
            "primary".to_owned()
        }
        Some(_) => "worktree".to_owned(),
        None => "project".to_owned(),
    }
}

fn path_label(path: &str) -> String {
    Path::new(path)
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or(path)
        .to_owned()
}
