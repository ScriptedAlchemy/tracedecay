//! Query-facing MCP scope resolution through the application boundary types.
//!
//! Authority: `docs/superpowers/plans/v2/01-domain-request-context.md` (this
//! slice converges the query-facing MCP tool-handler surface) and
//! `docs/superpowers/plans/2026-07-28-v2-delivery-root-crate-breakup.md`
//! (global constraints: every partial/unavailable/ambiguous state stays
//! explicit and fails closed).
//!
//! Query-facing tool entry points resolve their project scope ONCE, through
//! the already-authorized registry context, into the transport-neutral
//! `tracedecay_application::ResolvedScope`. The resolution itself is the
//! single consolidated canonical path
//! (`crate::application::context::RegisteredScopeResolver`); this module
//! only adapts the registry context into that path and preserves the MCP
//! error taxonomy. Every failure state stays explicit: a CWD-relative root, a
//! non-canonical registry identity, an unauthorized sibling root, or an
//! inconsistent scope digest fails closed — the MCP surface never substitutes
//! another project.

use std::fmt;
use std::path::Path;

use crate::application::context::ApplicationScopeError;
use crate::global_db::ProjectRegistryContext;
use crate::mcp::project_route::{ProjectRouteFailure, ProjectRouteFailureKind};

/// The explicit failure states when a query-facing MCP entry point resolves
/// its exact application scope. Every variant fails closed: no path, CWD, or
/// sibling-root fallback exists at this boundary.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum QueryScopeError {
    /// The requested root is not absolute; resolving it against the process
    /// CWD would be the CWD fallback the plan forbids.
    RelativeRoot {
        /// The offending requested root.
        requested_root: String,
    },
    /// The registry's project id is not a canonical domain identifier; the
    /// surface never normalizes it into one.
    NonCanonicalProjectId {
        /// The offending registry identity.
        project_id: String,
    },
    /// The requested root lives outside the registered canonical root and
    /// belongs to a different repository (a linked worktree of the same
    /// repository remains authorized; an unrelated sibling root is not).
    UnauthorizedSiblingRoot {
        /// The registered canonical root.
        registered_root: String,
        /// The offending requested root.
        requested_root: String,
    },
    /// The boundary crossing itself (identity derivation or the application
    /// contract) rejected the resolution.
    Resolution(String),
    /// The resolved scope failed its own validation; a stale or tampered
    /// digest must never cross the boundary.
    InconsistentScope(String),
}

impl QueryScopeError {
    /// Maps the failure onto the project-route taxonomy so callers report the
    /// same explicit route failures the transport already distinguishes.
    pub(crate) fn into_route_failure(self) -> ProjectRouteFailure {
        let kind = match self {
            Self::RelativeRoot { .. }
            | Self::NonCanonicalProjectId { .. }
            | Self::UnauthorizedSiblingRoot { .. } => ProjectRouteFailureKind::NotAuthorized,
            Self::Resolution(_) | Self::InconsistentScope(_) => {
                ProjectRouteFailureKind::Unavailable
            }
        };
        ProjectRouteFailure {
            kind,
            detail: self.to_string(),
        }
    }
}

impl fmt::Display for QueryScopeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RelativeRoot { requested_root } => write!(
                formatter,
                "requested root '{requested_root}' is not absolute; query scope resolution fails closed without a CWD fallback"
            ),
            Self::NonCanonicalProjectId { project_id } => write!(
                formatter,
                "registry project id '{project_id}' is not canonical; query scope resolution fails closed without normalization"
            ),
            Self::UnauthorizedSiblingRoot {
                registered_root,
                requested_root,
            } => write!(
                formatter,
                "requested root '{requested_root}' resolves outside registered root '{registered_root}' and names a different repository; refusing to serve a sibling root implicitly"
            ),
            Self::Resolution(message) => {
                write!(formatter, "application scope resolution failed: {message}")
            }
            Self::InconsistentScope(message) => write!(
                formatter,
                "resolved scope failed validation and must not cross the boundary: {message}"
            ),
        }
    }
}

impl std::error::Error for QueryScopeError {}

impl From<ApplicationScopeError> for QueryScopeError {
    fn from(error: ApplicationScopeError) -> Self {
        match error {
            ApplicationScopeError::RelativeRoot { requested_root } => {
                Self::RelativeRoot { requested_root }
            }
            ApplicationScopeError::UnauthorizedSiblingRoot {
                registered_root,
                requested_root,
            } => Self::UnauthorizedSiblingRoot {
                registered_root,
                requested_root,
            },
            ApplicationScopeError::InconsistentScope(message) => Self::InconsistentScope(message),
            other => Self::Resolution(other.to_string()),
        }
    }
}

/// Resolves the exact application scope for one already-authorized registry
/// context and requested root.
///
/// `owner` is the registry authority's context for the selected project;
/// `requested_root` is the worktree root the call will actually serve (the
/// registered root, a path inside it, or a linked worktree of the same
/// repository). The guards, the daemon-owned identity delegation, and the
/// digest revalidation all live in the single root-façade path; the
/// resolution fails closed rather than falling back to the CWD, another
/// registered project, or a sibling repository.
pub(crate) fn resolve_query_scope(
    owner: &ProjectRegistryContext,
    requested_root: &Path,
) -> Result<tracedecay_application::ResolvedScope, QueryScopeError> {
    let project_id =
        tracedecay_domain::ProjectId::new(owner.project.project_id.clone()).map_err(|_| {
            QueryScopeError::NonCanonicalProjectId {
                project_id: owner.project.project_id.clone(),
            }
        })?;
    let resolved = crate::application::context::RegisteredScopeResolver::resolve(
        Path::new(&owner.project.canonical_root),
        requested_root,
        &project_id,
    );
    resolved.map_err(QueryScopeError::from)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use std::path::Path;
    use std::process::Command;

    use tempfile::TempDir;

    use super::{QueryScopeError, resolve_query_scope};
    use crate::global_db::{CodeProjectRecord, ProjectRegistryContext};

    fn git(root: &Path, args: &[&str]) {
        let output = Command::new("git")
            .current_dir(root)
            .args(args)
            .output()
            .expect("git runs");
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn init_repo(root: &Path) {
        git(root, &["init", "-q", "-b", "main"]);
        git(root, &["config", "user.email", "t@t.com"]);
        git(root, &["config", "user.name", "T"]);
        std::fs::write(root.join("lib.rs"), "pub fn a() {}\n").expect("write");
        git(root, &["add", "."]);
        git(root, &["commit", "-q", "-m", "initial"]);
    }

    fn owner_for(canonical_root: &Path, project_id: &str) -> ProjectRegistryContext {
        ProjectRegistryContext {
            project: CodeProjectRecord {
                project_id: project_id.to_string(),
                canonical_root: canonical_root.to_string_lossy().into_owned(),
                display_root: canonical_root.to_string_lossy().into_owned(),
                git_common_dir: None,
                git_remote_url: None,
                default_branch: Some("main".to_string()),
                created_at: 1,
                last_seen_at: 2,
            },
            aliases: Vec::new(),
            stores: Vec::new(),
        }
    }

    #[test]
    fn exact_root_resolves_same_project_and_scope_via_application_type() {
        let temp = TempDir::new().unwrap();
        let root = temp.path().canonicalize().unwrap();
        init_repo(&root);
        let owner = owner_for(&root, "project.mcp-scope-test");

        let first = resolve_query_scope(&owner, &root).unwrap();
        let second = resolve_query_scope(&owner, &root).unwrap();

        assert_eq!(first.project_id.as_str(), "project.mcp-scope-test");
        first.validate().unwrap();
        assert_eq!(
            first, second,
            "the same exact root must resolve the same scope"
        );
        assert_eq!(
            first
                .reference
                .as_ref()
                .map(tracedecay_domain::RefId::as_str),
            Some("refs/heads/main"),
        );
    }

    #[test]
    fn subdirectory_request_converges_to_registered_canonical_root() {
        let temp = TempDir::new().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let subdir = root.join("src/deep");
        std::fs::create_dir_all(&subdir).unwrap();
        let owner = owner_for(&root, "project.mcp-scope-test");

        let from_subdir = resolve_query_scope(&owner, &subdir).unwrap();
        let from_root = resolve_query_scope(&owner, &root).unwrap();

        assert_eq!(
            from_subdir, from_root,
            "a path inside the registered root must resolve the canonical root's scope"
        );
    }

    #[test]
    fn relative_requested_root_fails_closed_without_cwd_fallback() {
        let temp = TempDir::new().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let owner = owner_for(&root, "project.mcp-scope-test");

        let error = resolve_query_scope(&owner, Path::new("relative/root")).unwrap_err();

        assert!(
            matches!(error, QueryScopeError::RelativeRoot { .. }),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn noncanonical_project_id_fails_closed_without_normalization() {
        let temp = TempDir::new().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let owner = owner_for(&root, " project.mcp-scope-test");

        let error = resolve_query_scope(&owner, &root).unwrap_err();

        assert!(
            matches!(error, QueryScopeError::NonCanonicalProjectId { .. }),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn sibling_root_of_another_repository_fails_closed() {
        let temp = TempDir::new().unwrap();
        let registered = temp.path().join("registered");
        let sibling = temp.path().join("sibling");
        std::fs::create_dir_all(&registered).unwrap();
        std::fs::create_dir_all(&sibling).unwrap();
        let registered = registered.canonicalize().unwrap();
        init_repo(&registered);
        init_repo(&sibling);
        let sibling = sibling.canonicalize().unwrap();
        let owner = owner_for(&registered, "project.mcp-scope-test");

        let error = resolve_query_scope(&owner, &sibling).unwrap_err();

        assert!(
            matches!(error, QueryScopeError::UnauthorizedSiblingRoot { .. }),
            "a resolution naming a different repository must fail closed: {error}"
        );
    }

    #[test]
    fn linked_worktree_of_the_same_repository_resolves_its_own_worktree_scope() {
        let temp = TempDir::new().unwrap();
        let root = temp.path().join("main-checkout");
        std::fs::create_dir_all(&root).unwrap();
        init_repo(&root);
        let root = root.canonicalize().unwrap();
        let linked = temp.path().join("linked-feature");
        git(
            &root,
            &[
                "worktree",
                "add",
                linked.to_string_lossy().as_ref(),
                "-b",
                "feature",
            ],
        );
        let linked = linked.canonicalize().unwrap();
        let owner = owner_for(&root, "project.mcp-scope-test");

        let main_scope = resolve_query_scope(&owner, &root).unwrap();
        let linked_scope = resolve_query_scope(&owner, &linked).unwrap();

        assert_eq!(
            main_scope.repository_id, linked_scope.repository_id,
            "a linked worktree shares the repository identity"
        );
        assert_ne!(
            main_scope.worktree_id, linked_scope.worktree_id,
            "a linked worktree is its own exact worktree scope"
        );
        assert_eq!(
            linked_scope
                .reference
                .as_ref()
                .map(tracedecay_domain::RefId::as_str),
            Some("refs/heads/feature"),
        );
        linked_scope.validate().unwrap();
    }

    #[test]
    fn dotdot_request_resolves_the_same_worktree_scope_as_daemon_authority() {
        let temp = TempDir::new().unwrap();
        let root = temp.path().join("main-checkout");
        std::fs::create_dir_all(&root).unwrap();
        init_repo(&root);
        let root = root.canonicalize().unwrap();
        let linked = temp.path().join("linked-feature");
        git(
            &root,
            &[
                "worktree",
                "add",
                linked.to_string_lossy().as_ref(),
                "-b",
                "feature-dotdot",
            ],
        );
        let linked = linked.canonicalize().unwrap();
        let requested = root.join("../linked-feature");
        let owner = owner_for(&root, "project.mcp-scope-test");
        let project_id = tracedecay_domain::ProjectId::new("project.mcp-scope-test").unwrap();

        let scope = resolve_query_scope(&owner, &requested).unwrap();
        let daemon_scope =
            crate::daemon::project_open_owners::resolved_scope_for_project(&linked, &project_id)
                .unwrap();

        assert_eq!(
            scope, daemon_scope,
            "dotdot spelling must not anchor identity to the lexical parent worktree"
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlink_request_resolves_the_same_worktree_scope_as_daemon_authority() {
        use std::os::unix::fs::symlink;

        let temp = TempDir::new().unwrap();
        let root = temp.path().join("main-checkout");
        std::fs::create_dir_all(&root).unwrap();
        init_repo(&root);
        let root = root.canonicalize().unwrap();
        let linked = temp.path().join("linked-feature");
        git(
            &root,
            &[
                "worktree",
                "add",
                linked.to_string_lossy().as_ref(),
                "-b",
                "feature-symlink",
            ],
        );
        let linked = linked.canonicalize().unwrap();
        let requested = root.join("linked-alias");
        symlink(&linked, &requested).unwrap();
        let owner = owner_for(&root, "project.mcp-scope-test");
        let project_id = tracedecay_domain::ProjectId::new("project.mcp-scope-test").unwrap();

        let scope = resolve_query_scope(&owner, &requested).unwrap();
        let daemon_scope =
            crate::daemon::project_open_owners::resolved_scope_for_project(&linked, &project_id)
                .unwrap();

        assert_eq!(
            scope, daemon_scope,
            "symlink spelling must not anchor identity to the lexical parent worktree"
        );
    }

    #[test]
    fn scope_failures_map_onto_explicit_route_failure_kinds() {
        use crate::mcp::project_route::ProjectRouteFailureKind;

        let relative = QueryScopeError::RelativeRoot {
            requested_root: "relative".to_string(),
        }
        .into_route_failure();
        assert_eq!(relative.kind, ProjectRouteFailureKind::NotAuthorized);
        assert!(!relative.kind.retryable());

        let sibling = QueryScopeError::UnauthorizedSiblingRoot {
            registered_root: "/registered".to_string(),
            requested_root: "/sibling".to_string(),
        }
        .into_route_failure();
        assert_eq!(sibling.kind, ProjectRouteFailureKind::NotAuthorized);

        let resolution = QueryScopeError::Resolution("identity".to_string()).into_route_failure();
        assert_eq!(resolution.kind, ProjectRouteFailureKind::Unavailable);
        assert!(resolution.kind.retryable());
    }
}
