use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde::Serialize;
use tracedecay_global_db::{
    CodeProjectRecord, ProjectAliasRecord, ProjectRegistryContext, ProjectStoreContext,
};

pub use tracedecay_application::{
    ProjectRegistryEntry, ProjectRegistrySummary, ProjectRegistryView, ProjectRepoGroup,
    PublicCodeProject,
};

pub fn public_code_project_from_record(
    project: &CodeProjectRecord,
    active_project_id: Option<&str>,
) -> PublicCodeProject {
    PublicCodeProject {
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

/// Serialized project-registry context for one project: the public project
/// row plus its borrowed alias and store rows.
#[derive(Debug, Serialize)]
pub struct PublicProjectRegistryContext<'a> {
    pub project: PublicCodeProject,
    pub aliases: &'a [ProjectAliasRecord],
    pub stores: &'a [ProjectStoreContext],
}

impl<'a> PublicProjectRegistryContext<'a> {
    pub fn new(context: &'a ProjectRegistryContext, active_project_id: Option<&str>) -> Self {
        Self {
            project: public_code_project_from_record(&context.project, active_project_id),
            aliases: &context.aliases,
            stores: &context.stores,
        }
    }
}

#[hotpath::measure(label = "dashboard_api.project_registry.build_view")]
pub fn build_project_registry_view(
    contexts: &[ProjectRegistryContext],
    active_project_id: Option<&str>,
    truncated: bool,
) -> ProjectRegistryView {
    let mut groups: BTreeMap<String, ProjectRepoGroup> = BTreeMap::new();
    for context in contexts {
        let entry = project_entry(context, active_project_id);
        let group_key = context
            .project
            .git_common_dir
            .clone()
            .unwrap_or_else(|| context.project.canonical_root.clone());
        let group = groups.entry(group_key).or_insert_with(|| ProjectRepoGroup {
            label: repo_label(&context.project),
            git_common_dir: context.project.git_common_dir.clone(),
            project_count: 0,
            branches: Vec::new(),
            projects: Vec::new(),
        });
        group.projects.push(entry);
    }

    let mut project_tree = groups.into_values().collect::<Vec<_>>();
    let mut label_counts = BTreeMap::<String, usize>::new();
    for group in &project_tree {
        *label_counts.entry(group.label.clone()).or_default() += 1;
    }
    for group in &mut project_tree {
        if label_counts.get(&group.label).copied().unwrap_or_default() > 1 {
            group.label = repo_label_with_parent(group);
        }
        group.projects.sort_by(|a, b| {
            a.label
                .cmp(&b.label)
                .then_with(|| a.project_id.cmp(&b.project_id))
        });
        group.project_count = group.projects.len();
        let mut branches = BTreeSet::new();
        for project in &group.projects {
            for branch in &project.branches {
                branches.insert(branch.clone());
            }
        }
        group.branches = branches.into_iter().collect();
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
    let mut artifact_count = 0usize;
    for store in &context.stores {
        artifact_count += store.artifacts.len();
        for scope in &store.graph_scopes {
            branches.insert(scope.branch_name.clone());
        }
    }

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
    if let Some(git_common_dir) = &project.git_common_dir {
        let path = Path::new(git_common_dir);
        if path.file_name().and_then(|name| name.to_str()) == Some(".git")
            && let Some(parent) = path.parent()
        {
            return path_label(parent.to_string_lossy().as_ref());
        }
    }
    path_label(&project.display_root)
}

fn repo_label_with_parent(group: &ProjectRepoGroup) -> String {
    let path = group.git_common_dir.as_deref().or_else(|| {
        group
            .projects
            .first()
            .map(|project| project.canonical_root.as_str())
    });
    let Some(path) = path else {
        return group.label.clone();
    };
    let path = Path::new(path);
    let repo_path = if path.file_name().and_then(|name| name.to_str()) == Some(".git") {
        path.parent().unwrap_or(path)
    } else {
        path
    };
    let parent = repo_path
        .parent()
        .and_then(|parent| parent.file_name())
        .and_then(|name| name.to_str());
    match parent {
        Some(parent) => format!("{} ({parent})", group.label),
        None => group.label.clone(),
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn project_record(
        project_id: &str,
        canonical_root: &str,
        git_common_dir: Option<&str>,
    ) -> CodeProjectRecord {
        CodeProjectRecord {
            project_id: project_id.to_string(),
            canonical_root: canonical_root.to_string(),
            display_root: canonical_root.to_string(),
            git_common_dir: git_common_dir.map(ToString::to_string),
            git_remote_url: None,
            default_branch: None,
            created_at: 0,
            last_seen_at: 0,
        }
    }

    fn registry_context(project: CodeProjectRecord) -> ProjectRegistryContext {
        ProjectRegistryContext {
            project,
            aliases: Vec::new(),
            stores: Vec::new(),
        }
    }

    #[test]
    fn repo_label_with_parent_disambiguates_identical_repo_labels() {
        // Two distinct repos that both happen to be named "app" under
        // different parent directories should end up with distinct,
        // parent-qualified labels instead of colliding.
        let ctx_a = registry_context(project_record(
            "a1",
            "/work/teamA/app",
            Some("/work/teamA/app/.git"),
        ));
        let ctx_b = registry_context(project_record(
            "b1",
            "/work/teamB/app",
            Some("/work/teamB/app/.git"),
        ));

        let view = build_project_registry_view(&[ctx_a, ctx_b], None, false);

        assert_eq!(view.summary.repo_count, 2);
        let labels: BTreeSet<String> = view.project_tree.iter().map(|g| g.label.clone()).collect();
        assert!(
            labels.contains("app (teamA)"),
            "expected disambiguated label for teamA, got {labels:?}"
        );
        assert!(
            labels.contains("app (teamB)"),
            "expected disambiguated label for teamB, got {labels:?}"
        );
        // The plain, colliding label must not survive disambiguation.
        assert!(!labels.contains("app"));
    }

    #[test]
    fn worktree_groups_under_parent_repo_git_common_dir() {
        // A worktree's git_common_dir points back at the primary repo's
        // .git directory, so both entries should be grouped together
        // under a single repo group.
        let primary = registry_context(project_record(
            "main",
            "/repo/main",
            Some("/repo/main/.git"),
        ));
        let worktree = registry_context(project_record(
            "wt",
            "/repo/main-wt",
            Some("/repo/main/.git"),
        ));

        let view = build_project_registry_view(&[primary, worktree], None, false);

        assert_eq!(view.summary.project_count, 2);
        assert_eq!(view.summary.repo_count, 1);
        let group = &view.project_tree[0];
        assert_eq!(group.project_count, 2);

        let mut kinds: BTreeMap<&str, &str> = BTreeMap::new();
        for project in &group.projects {
            kinds.insert(project.project_id.as_str(), project.kind.as_str());
        }
        assert_eq!(kinds.get("main"), Some(&"primary"));
        assert_eq!(kinds.get("wt"), Some(&"worktree"));
    }

    #[test]
    fn repo_label_with_parent_leaves_label_unchanged_for_root_path() {
        // Degenerate case: a project rooted at "/" has no parent
        // directory to qualify the label with, so the label must be
        // returned unchanged rather than panicking or producing garbage.
        let entry = project_entry(&registry_context(project_record("root", "/", None)), None);
        let group = ProjectRepoGroup {
            label: "root".to_string(),
            git_common_dir: None,
            project_count: 1,
            branches: Vec::new(),
            projects: vec![entry],
        };

        assert_eq!(repo_label_with_parent(&group), "root");
    }
}
