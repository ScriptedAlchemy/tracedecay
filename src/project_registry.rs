use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::global_db::{
    CodeProjectRecord, ProjectAliasRecord, ProjectRegistryContext, ProjectStoreContext,
};
// The registry data structs (and `PublicCodeProject::from_record`) are the
// canonical copies beside the dashboard read model. The root keeps the superset
// pieces: the `Serialize/Deserialize/JsonSchema`-deriving `ProjectRegistryView`
// (the crate's bare struct can't back the CLI/MCP JSON round-trips), the
// label-disambiguating view builder/renderer, and the alias/store-wired
// `PublicProjectRegistryContext`.
pub use tracedecay_dashboard_api::project_registry::{
    ProjectRegistryEntry, ProjectRegistrySummary, ProjectRepoGroup, PublicCodeProject,
};

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ProjectRegistryView {
    pub summary: ProjectRegistrySummary,
    pub project_tree: Vec<ProjectRepoGroup>,
}

#[derive(Debug, Serialize)]
pub struct PublicProjectRegistryContext<'a> {
    pub project: PublicCodeProject,
    pub aliases: &'a [ProjectAliasRecord],
    pub stores: &'a [ProjectStoreContext],
}

impl<'a> PublicProjectRegistryContext<'a> {
    pub fn new(context: &'a ProjectRegistryContext, active_project_id: Option<&str>) -> Self {
        Self {
            project: PublicCodeProject::from_record(&context.project, active_project_id),
            aliases: &context.aliases,
            stores: &context.stores,
        }
    }
}

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

pub fn render_project_registry_view(title: &str, view: &ProjectRegistryView) -> String {
    if view.summary.project_count == 0 {
        return format!("No {title} found.");
    }
    let mut out = String::new();
    let _ = writeln!(
        out,
        "Found {} {title} across {} repositories.\n\nRepositories:",
        view.summary.project_count, view.summary.repo_count
    );
    for group in &view.project_tree {
        let group_branches = if group.branches.is_empty() {
            "-".to_string()
        } else {
            group.branches.join(", ")
        };
        let _ = writeln!(out, "- {} (branches: {})", group.label, group_branches);
        for project in &group.projects {
            let marker = if project.is_active == Some(true) {
                " *"
            } else {
                ""
            };
            let branches = if project.branches.is_empty() {
                "-".to_string()
            } else {
                project.branches.join(", ")
            };
            let _ = writeln!(
                out,
                "  - `{}`{} [{}] branches: {}; stores: {}; path: {}",
                project.project_id,
                marker,
                project.kind,
                branches,
                project.store_count,
                project.project_root
            );
        }
    }
    if view.summary.truncated {
        out.push_str("\nResult truncated; increase limit for more projects.\n");
    }
    out
}

fn project_entry(
    context: &ProjectRegistryContext,
    active_project_id: Option<&str>,
) -> ProjectRegistryEntry {
    let mut branches = BTreeSet::new();
    if let Some(branch) = &context.project.default_branch {
        branches.insert(branch.clone());
    }
    let mut graph_scope_count = 0usize;
    let mut artifact_count = 0usize;
    for store in &context.stores {
        graph_scope_count += store.graph_scopes.len();
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
        graph_scope_count,
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

fn project_kind(project: &CodeProjectRecord) -> String {
    match &project.git_common_dir {
        Some(git_common_dir)
            if Path::new(&project.canonical_root).join(".git") == Path::new(git_common_dir) =>
        {
            "primary".to_string()
        }
        Some(_) => "worktree".to_string(),
        None => "project".to_string(),
    }
}

/// Resolves the canonical registration root for a project store rooted at
/// `project_root`.
///
/// A tracedecay project id is shared across every linked worktree of a
/// repository: [`crate::global_db::RegisteredGlobalDb::upsert_code_project`] indexes a
/// `git-common-dir:<common dir>` alias back to whichever project id first
/// registered it, so a session opened from *any* linked worktree resolves
/// to the same project id as the primary checkout. Because that same upsert
/// persists whichever `project_root` it is called with as `canonical_root`
/// and `display_root`, registering straight from a worktree's own root lets
/// the last worktree to touch the project pin the shared row to its
/// (often transient) path — silently dropping analytics/path-matching for
/// the primary checkout.
///
/// Given the worktree's own root and its already-resolved
/// [`crate::worktree::git_common_dir`], this returns `Some(primary_root)`
/// when `project_root` is a linked worktree and the primary checkout still
/// exists on disk. It returns `None` — meaning "register `project_root` as
/// given" — when `project_root` already *is* the primary checkout, isn't a
/// git checkout at all, or the primary checkout no longer exists (a
/// worktree-only project is legitimate and must keep registering itself).
pub use tracedecay_runtime_core::project_registry::primary_checkout_root;

/// Registry reap contract: root shim over the canonical copy beside its
/// producer in `tracedecay_global_db::project_registry` (see SEAMS.md).
pub use tracedecay_global_db::{
    GIT_COMMON_DIR_ALIAS_PREFIX, ReapEntryKind, RegistryReapEntry, RegistryReapPlan,
    RetainedRegistryEntry, alias_key_path, ephemeral_root_rejection, is_ephemeral_path,
};

fn path_label(path: &str) -> String {
    Path::new(path)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(path)
        .to_string()
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

    #[test]
    fn primary_checkout_root_redirects_linked_worktree_to_existing_primary() {
        let tmp = tempfile::TempDir::new().unwrap();
        let primary = tmp.path().join("main");
        let worktree = tmp.path().join("main-wt");
        std::fs::create_dir_all(&primary).unwrap();
        std::fs::create_dir_all(&worktree).unwrap();
        // `crate::worktree::git_common_dir` always returns a canonicalized
        // path — mirror that guarantee here rather than a raw join.
        let primary = primary.canonicalize().unwrap();
        let common_dir = primary.join(".git");
        std::fs::create_dir_all(&common_dir).unwrap();

        let redirected = primary_checkout_root(&worktree, Some(&common_dir));

        assert_eq!(
            redirected,
            Some(primary),
            "a linked worktree with a live primary checkout must redirect to it"
        );
    }

    #[test]
    fn primary_checkout_root_is_none_when_project_root_is_already_primary() {
        let tmp = tempfile::TempDir::new().unwrap();
        let primary = tmp.path().join("main");
        std::fs::create_dir_all(&primary).unwrap();
        let primary = primary.canonicalize().unwrap();
        let common_dir = primary.join(".git");
        std::fs::create_dir_all(&common_dir).unwrap();

        assert_eq!(
            primary_checkout_root(&primary, Some(&common_dir)),
            None,
            "the primary checkout must never be redirected to itself"
        );
    }

    #[test]
    fn primary_checkout_root_is_none_without_git_common_dir() {
        let tmp = tempfile::TempDir::new().unwrap();
        let project_root = tmp.path().join("not-a-worktree");
        std::fs::create_dir_all(&project_root).unwrap();

        assert_eq!(
            primary_checkout_root(&project_root, None),
            None,
            "non-git projects must register themselves unchanged"
        );
    }

    #[test]
    fn primary_checkout_root_keeps_worktree_when_primary_checkout_is_missing() {
        // The primary checkout no longer exists on disk (deleted, moved off
        // this machine, ...). A worktree-only project is legitimate and
        // must keep registering its own root rather than redirecting to a
        // path that doesn't exist.
        let tmp = tempfile::TempDir::new().unwrap();
        let missing_primary = tmp.path().join("deleted-main");
        let worktree = tmp.path().join("main-wt");
        std::fs::create_dir_all(&worktree).unwrap();
        let common_dir = missing_primary.join(".git");

        assert_eq!(
            primary_checkout_root(&worktree, Some(&common_dir)),
            None,
            "a missing primary checkout must not be adopted as canonical_root"
        );
    }

    #[test]
    fn primary_checkout_root_ignores_non_dot_git_common_dirs() {
        // Bare repos and submodule gitlinks resolve `git_common_dir` to a
        // path that isn't a plain `<repo>/.git`, so the parent directory
        // isn't reliably a checkout root — leave registration alone rather
        // than risk deriving a bogus "primary".
        let tmp = tempfile::TempDir::new().unwrap();
        let worktree = tmp.path().join("checkout");
        std::fs::create_dir_all(&worktree).unwrap();
        let submodule_common_dir = tmp.path().join("main/.git/modules/sub");
        std::fs::create_dir_all(&submodule_common_dir).unwrap();

        assert_eq!(
            primary_checkout_root(&worktree, Some(&submodule_common_dir)),
            None,
            "non-`.git` common dirs must not redirect registration"
        );
    }
}
