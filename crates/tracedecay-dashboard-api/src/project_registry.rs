use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::Serialize;
use tracedecay_global_db::{
    CodeProjectRecord, ProjectAliasRecord, ProjectRegistryContext, ProjectStoreContext,
};
use tracedecay_runtime_core::branch::{CheckoutHead, checkout_head};

pub use tracedecay_contracts::{
    ProjectRegistryEntry, ProjectRegistrySummary, ProjectRegistryView, ProjectRepoGroup,
    PublicCodeProject,
};

pub fn public_code_project_from_record(
    project: &CodeProjectRecord,
    active_project_id: Option<&str>,
) -> PublicCodeProject {
    public_code_project_for_checkout(project, &[], active_project_id, None)
}

/// Public project row whose `head_branch` is the live HEAD of `preferred` when
/// that path is one of this project's checkouts.
///
/// `preferred` is the checkout the caller is asking about: the active
/// worktree, or the path a context read resolved. A linked worktree does not
/// inherit the primary checkout's branch.
pub fn public_code_project_for_checkout(
    project: &CodeProjectRecord,
    aliases: &[ProjectAliasRecord],
    active_project_id: Option<&str>,
    preferred: Option<&Path>,
) -> PublicCodeProject {
    PublicCodeProject {
        project_id: project.project_id.clone(),
        label: path_label(&project.display_root),
        project_root: project.display_root.clone(),
        display_root: project.display_root.clone(),
        canonical_root: project.canonical_root.clone(),
        git_common_dir: project.git_common_dir.clone(),
        default_branch: project.default_branch.clone(),
        head_branch: observe_checkouts(project, aliases, preferred)
            .and_then(|observed| observed.head_branch),
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
        Self::at_checkout(context, active_project_id, None)
    }

    pub fn at_checkout(
        context: &'a ProjectRegistryContext,
        active_project_id: Option<&str>,
        preferred: Option<&Path>,
    ) -> Self {
        Self {
            project: public_code_project_for_checkout(
                &context.project,
                &context.aliases,
                active_project_id,
                preferred,
            ),
            aliases: &context.aliases,
            stores: &context.stores,
        }
    }
}

#[hotpath::measure(label = "dashboard_api.project_registry.build_view")]
pub fn build_project_registry_view(
    contexts: &[ProjectRegistryContext],
    active_project_id: Option<&str>,
    active_checkout: Option<&Path>,
    truncated: bool,
) -> ProjectRegistryView {
    let mut groups: BTreeMap<String, ProjectRepoGroup> = BTreeMap::new();
    for context in contexts {
        let entry = project_entry(context, active_project_id, active_checkout);
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

/// Copies each registry row's live `head_branch` onto the matching public
/// project. Listing payloads carry both shapes, and they have to name the
/// same HEAD.
pub fn align_public_checkout_branches(
    projects: &mut [PublicCodeProject],
    view: &ProjectRegistryView,
) {
    for project in projects {
        if let Some(entry) = view
            .project_tree
            .iter()
            .flat_map(|group| group.projects.iter())
            .find(|entry| entry.project_id == project.project_id)
        {
            project.head_branch.clone_from(&entry.head_branch);
        }
    }
}

fn project_entry(
    context: &ProjectRegistryContext,
    active_project_id: Option<&str>,
    active_checkout: Option<&Path>,
) -> ProjectRegistryEntry {
    let is_active = active_project_id.is_some_and(|id| id == context.project.project_id);
    let preferred = is_active.then_some(active_checkout).flatten();
    let observed = observe_checkouts(&context.project, &context.aliases, preferred);
    let mut branches = BTreeSet::new();
    let head_branch = if let Some(observed) = observed {
        branches = observed.branches;
        observed.head_branch
    } else {
        // No checkout could be read. Keep the enrolled branch names so a
        // non-git project, or a root that is not on disk in this process,
        // still renders the registry row it already has.
        if let Some(branch) = &context.project.default_branch {
            branches.insert(branch.clone());
        }
        for store in &context.stores {
            for scope in &store.graph_scopes {
                branches.insert(scope.branch_name.clone());
            }
        }
        None
    };
    let mut artifact_count = 0usize;
    for store in &context.stores {
        artifact_count += store.artifacts.len();
    }

    ProjectRegistryEntry {
        project_id: context.project.project_id.clone(),
        label: path_label(&context.project.display_root),
        project_root: context.project.display_root.clone(),
        canonical_root: context.project.canonical_root.clone(),
        kind: project_kind(&context.project),
        default_branch: context.project.default_branch.clone(),
        head_branch,
        branches: branches.into_iter().collect(),
        store_count: context.stores.len(),
        artifact_count,
        alias_count: context.aliases.len(),
        last_seen_at: context.project.last_seen_at,
        is_active: active_project_id.map(|id| id == context.project.project_id),
    }
}

struct ObservedCheckouts {
    head_branch: Option<String>,
    branches: BTreeSet<String>,
}

/// Live HEAD of every checkout this project row names.
///
/// The enrolled `default_branch` and graph-scope names are the branches
/// recorded when the project was registered. They do not move when the
/// checkout switches, detaches, or a linked worktree points somewhere else.
/// `None` means no checkout was readable, so the caller keeps that record.
fn observe_checkouts(
    project: &CodeProjectRecord,
    aliases: &[ProjectAliasRecord],
    preferred: Option<&Path>,
) -> Option<ObservedCheckouts> {
    let mut roots = Vec::new();
    push_checkout(&mut roots, PathBuf::from(&project.canonical_root));
    push_checkout(&mut roots, PathBuf::from(&project.display_root));
    for alias in aliases {
        push_checkout(&mut roots, PathBuf::from(&alias.alias_path));
    }
    let preferred = preferred.filter(|path| {
        roots.iter().any(|root| same_checkout(root, path))
            || shares_repository(path, project.git_common_dir.as_deref())
    });
    if let Some(path) = preferred {
        push_checkout(&mut roots, path.to_path_buf());
    }

    let mut observations = Vec::new();
    for root in &roots {
        if let Some(head) = checkout_head(root) {
            observations.push((root.as_path(), head));
        }
    }
    if observations.is_empty() {
        return None;
    }

    let mut branches = BTreeSet::new();
    for (_, head) in &observations {
        if let CheckoutHead::Branch(branch) = head {
            branches.insert(branch.clone());
        }
    }
    let chosen = preferred
        .and_then(|path| {
            observations
                .iter()
                .find(|(root, _)| same_checkout(root, path))
                .map(|(_, head)| head)
        })
        .or_else(|| observations.first().map(|(_, head)| head));
    let head_branch = match chosen {
        Some(CheckoutHead::Branch(branch)) => Some(branch.clone()),
        Some(CheckoutHead::Detached) | None => None,
    };
    Some(ObservedCheckouts {
        head_branch,
        branches,
    })
}

fn push_checkout(roots: &mut Vec<PathBuf>, path: PathBuf) {
    if roots.iter().any(|root| same_checkout(root, &path)) {
        return;
    }
    roots.push(path);
}

fn same_checkout(left: &Path, right: &Path) -> bool {
    match (left.canonicalize(), right.canonicalize()) {
        (Ok(left), Ok(right)) => left == right,
        _ => left == right,
    }
}

fn shares_repository(path: &Path, git_common_dir: Option<&str>) -> bool {
    let Some(stored) = git_common_dir else {
        return false;
    };
    tracedecay_runtime_core::worktree::git_common_dir(path).is_some_and(|live| {
        live.as_path() == Path::new(stored) || same_checkout(&live, Path::new(stored))
    })
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
    use std::path::Path;
    use std::process::Command;

    use tracedecay_contracts::render_project_registry_view;
    use tracedecay_global_db::{
        GraphScopeRecord, ProjectAliasRecord, ProjectStoreContext, StoreInstanceRecord,
    };

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

        let view = build_project_registry_view(&[primary, worktree], None, None, false);

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
    fn registry_reports_checkout_head_after_branch_switch() {
        let tmp = tempfile::tempdir().expect("checkout tempdir");
        let repo = tmp.path().join("checkout");
        std::fs::create_dir(&repo).expect("checkout dir");
        git(&repo, &["init", "-q", "-b", "main"]);
        std::fs::write(repo.join("README.md"), "hi").expect("readme");
        git(&repo, &["add", "README.md"]);
        git(
            &repo,
            &[
                "-c",
                "user.email=t@t",
                "-c",
                "user.name=t",
                "commit",
                "-q",
                "-m",
                "init",
            ],
        );
        let git_common_dir = tracedecay_runtime_core::worktree::git_common_dir(&repo)
            .expect("primary git common dir");
        let mut context = enrolled_checkout(&repo, &git_common_dir);

        git(&repo, &["checkout", "-q", "-b", "switched-head"]);
        let (branch, branches, rendered) = reported(&context, Some(repo.as_path()));
        assert_eq!(branch.as_deref(), Some("switched-head"));
        assert!(branches.iter().any(|name| name == "switched-head"));
        assert!(branches.iter().all(|name| {
            name != "recorded-default" && name != "indexed-master" && name != "main"
        }));
        assert!(rendered.contains("switched-head"));
        assert!(!rendered.contains("recorded-default"));
        assert!(!rendered.contains("indexed-master"));

        git(&repo, &["checkout", "-q", "--detach"]);
        let (branch, branches, rendered) = reported(&context, Some(repo.as_path()));
        assert_eq!(branch, None);
        assert!(branches.iter().all(|name| name != "switched-head"));
        assert!(!rendered.contains("switched-head"));
        assert!(!rendered.contains("recorded-default"));

        let worktree = tmp.path().join("linked");
        git(
            &repo,
            &[
                "worktree",
                "add",
                "-b",
                "linked-feature",
                worktree.to_str().expect("worktree path"),
            ],
        );
        git(&worktree, &["checkout", "-q", "-b", "worktree-switched"]);
        context.aliases.push(ProjectAliasRecord {
            alias_path: worktree.to_string_lossy().into_owned(),
            project_id: context.project.project_id.clone(),
            last_seen_at: 0,
        });

        let (branch, branches, rendered) = reported(&context, Some(worktree.as_path()));
        assert_eq!(branch.as_deref(), Some("worktree-switched"));
        assert!(branches.iter().any(|name| name == "worktree-switched"));
        assert!(branches.iter().all(|name| {
            name != "linked-feature" && name != "recorded-default" && name != "main"
        }));
        assert!(rendered.contains("worktree-switched"));
        assert!(!rendered.contains("linked-feature"));

        let (branch, branches, _) = reported(&context, None);
        assert_eq!(branch, None);
        assert!(branches.iter().any(|name| name == "worktree-switched"));
        assert!(branches.iter().all(|name| name != "recorded-default"));
    }

    fn enrolled_checkout(root: &Path, git_common_dir: &Path) -> ProjectRegistryContext {
        let mut project = project_record(
            "project.checkout",
            root.to_string_lossy().as_ref(),
            Some(git_common_dir.to_string_lossy().as_ref()),
        );
        project.default_branch = Some("recorded-default".to_owned());
        ProjectRegistryContext {
            project,
            aliases: Vec::new(),
            stores: vec![ProjectStoreContext {
                store: StoreInstanceRecord {
                    store_id: "store.checkout".to_owned(),
                    project_id: "project.checkout".to_owned(),
                    store_kind: "graph".to_owned(),
                    storage_mode: "project".to_owned(),
                    store_relpath: "graph.db".to_owned(),
                    manifest_relpath: None,
                    created_at: 0,
                    last_verified_at: None,
                    last_write_at: None,
                },
                graph_scopes: vec![GraphScopeRecord {
                    graph_scope_id: "scope.checkout".to_owned(),
                    project_id: "project.checkout".to_owned(),
                    store_id: "store.checkout".to_owned(),
                    branch_name: "indexed-master".to_owned(),
                    db_relpath: "graph.db".to_owned(),
                    parent_scope_id: None,
                    last_synced_at: None,
                    writable: true,
                }],
                artifacts: Vec::new(),
            }],
        }
    }

    /// Tree row, aligned listing row, and path context all name the same HEAD
    /// and keep the recorded repository default as the default branch.
    fn reported(
        context: &ProjectRegistryContext,
        checkout: Option<&Path>,
    ) -> (Option<String>, Vec<String>, String) {
        let active_id = checkout.map(|_| context.project.project_id.as_str());
        let view =
            build_project_registry_view(std::slice::from_ref(context), active_id, checkout, false);
        let entry = view
            .project_tree
            .iter()
            .flat_map(|group| group.projects.iter())
            .find(|project| project.project_id == context.project.project_id)
            .expect("enrolled project");
        let mut rows = vec![public_code_project_from_record(&context.project, active_id)];
        align_public_checkout_branches(&mut rows, &view);
        let listed = rows
            .iter()
            .find(|project| project.project_id == context.project.project_id)
            .expect("listed project");
        let context_row = PublicProjectRegistryContext::at_checkout(context, active_id, checkout);
        for default_branch in [
            &entry.default_branch,
            &listed.default_branch,
            &context_row.project.default_branch,
        ] {
            assert_eq!(default_branch.as_deref(), Some("recorded-default"));
        }
        assert_eq!(listed.head_branch, entry.head_branch);
        assert_eq!(context_row.project.head_branch, entry.head_branch);
        (
            entry.head_branch.clone(),
            entry.branches.clone(),
            render_project_registry_view("projects", &view),
        )
    }

    fn git(cwd: &Path, args: &[&str]) {
        let output = Command::new("git")
            .args(["-c", "core.hooksPath=.git/no-hooks"])
            .args(args)
            .current_dir(cwd)
            .output()
            .unwrap_or_else(|error| panic!("git {args:?}: {error}"));
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
