use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::global_db::{
    CodeProjectRecord, ProjectAliasRecord, ProjectRegistryContext, ProjectStoreContext,
};

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ProjectRegistryView {
    pub summary: ProjectRegistrySummary,
    pub project_tree: Vec<ProjectRepoGroup>,
}

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
    pub graph_scope_count: usize,
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

/// Prefix marking a `project_aliases` row that keys a repository's git common
/// directory rather than a checkout path.
pub const GIT_COMMON_DIR_ALIAS_PREFIX: &str = "git-common-dir:";

/// Which registry table a reap candidate belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReapEntryKind {
    /// A `projects` row: the cross-project savings ledger, which despite the
    /// table name is an accounting record and not a project registry.
    SavingsLedgerPath,
    /// A `project_aliases` row keyed by a filesystem path.
    ProjectAlias,
    /// A `code_projects` row: the V2 canonical identity authority.
    CodeProject,
}

impl ReapEntryKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::SavingsLedgerPath => "savings-ledger path",
            Self::ProjectAlias => "project alias",
            Self::CodeProject => "project authority",
        }
    }
}

/// One registry row whose referenced path is gone from disk.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegistryReapEntry {
    pub kind: ReapEntryKind,
    /// Primary key of the row: ledger path, alias key, or project id.
    pub key: String,
    /// The filesystem path that no longer exists.
    pub missing_path: String,
    pub project_id: Option<String>,
}

/// A dead-looking row that reaping deliberately leaves alone, with the reason.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetainedRegistryEntry {
    pub entry: RegistryReapEntry,
    pub reason: String,
}

/// The outcome of classifying the registry: what may be removed and what is
/// deliberately kept. Reaping only ever deletes rows; no store directory,
/// database, or session artifact is touched by any part of this plan.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegistryReapPlan {
    pub reapable: Vec<RegistryReapEntry>,
    pub retained: Vec<RetainedRegistryEntry>,
}

impl RegistryReapPlan {
    pub fn is_empty(&self) -> bool {
        self.reapable.is_empty()
    }

    /// One line per row, for a dry-run report.
    pub fn summary(&self) -> String {
        let mut out = format!(
            "{} reapable, {} retained\n",
            self.reapable.len(),
            self.retained.len()
        );
        for entry in &self.reapable {
            let _ = writeln!(
                out,
                "  reap    {} {} (missing {})",
                entry.kind.label(),
                entry.key,
                entry.missing_path
            );
        }
        for retained in &self.retained {
            let _ = writeln!(
                out,
                "  retain  {} {} — {}",
                retained.entry.kind.label(),
                retained.entry.key,
                retained.reason
            );
        }
        out
    }
}

/// The path a `project_aliases` key refers to, or `None` when the key is not
/// path-shaped (a `git-remote-name:` search alias, say) and therefore can
/// never be judged dead by checking the filesystem.
pub fn alias_key_path(alias: &str) -> Option<&Path> {
    let candidate = alias
        .strip_prefix(GIT_COMMON_DIR_ALIAS_PREFIX)
        .unwrap_or(alias);
    let path = Path::new(candidate);
    path.is_absolute().then_some(path)
}

/// Whether `path` lives under the OS temporary directory.
///
/// Canonicalizes both sides where possible so a `/tmp` symlinked to
/// `/private/tmp` (macOS) still matches.
pub fn is_ephemeral_path(path: &Path) -> bool {
    let temp_root = std::env::temp_dir();
    let temp_root = temp_root.canonicalize().unwrap_or(temp_root);
    let path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    path.starts_with(&temp_root)
}

/// Registry admission policy for a project root, returning the refusal reason
/// when the root must not become a durable project authority.
///
/// A checkout under the OS temporary directory is throwaway by construction —
/// `mktemp -d` fixtures, extracted archives, scratch clones — yet registering
/// one writes a `code_projects` row and a shard that outlive it by years,
/// which is how a literal `mktemp` path became a permanent authority in a real
/// profile. The comparison is against the *profile*, not absolute: a hermetic
/// profile that itself lives under the temp directory is equally throwaway, so
/// test fixtures and sandboxed runs keep working. Only a durable profile
/// refuses an ephemeral root.
pub fn ephemeral_root_rejection(project_root: &Path, profile_root: &Path) -> Option<String> {
    (is_ephemeral_path(project_root) && !is_ephemeral_path(profile_root)).then(|| {
        format!(
            "project root '{}' is under the OS temporary directory and cannot be \
             registered as a durable authority in profile '{}'",
            project_root.display(),
            profile_root.display()
        )
    })
}

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
