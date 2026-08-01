//! Exact application scope for the dashboard HTTP surface.
//!
//! The dashboard is an entry point: it resolves its project scope ONCE into
//! the transport-neutral [`tracedecay_application::ResolvedScope`] when the
//! state is constructed, and every handler consumes that pinned scope instead
//! of re-deriving repository/worktree identity from paths per request.
//!
//! Every failure stays explicit: an unregistered root, an invalid project id,
//! or a failed exact-root resolution yields `None`, and each handler reports
//! its own typed unavailable state. No path, CWD, or sibling-root fallback
//! exists here.

use std::path::Path;

use tracedecay_domain::ProjectId;

/// Resolves the exact application scope for one dashboard project root.
///
/// `project_id` must be the registered identity validated from the store
/// layout; `None` (missing registry) or a non-canonical id fails closed. The
/// resolution itself is the single consolidated path behind the root façade
/// (`crate::application::context::resolve_exact_root_scope`), so the
/// dashboard never re-derives repository/worktree identity from paths.
pub fn resolve_dashboard_scope(
    project_root: &Path,
    project_id: Option<&str>,
) -> Option<tracedecay_application::ResolvedScope> {
    let project_id = ProjectId::new(project_id?).ok()?;
    #[allow(deprecated)]
    // the dashboard crosses through the deprecated root façade until the
    // application boundary owns scope resolution
    let scope = crate::application::context::resolve_exact_root_scope(project_root, &project_id);
    scope.ok()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::resolve_dashboard_scope;
    use tracedecay_domain::ProjectId;

    #[test]
    fn dashboard_scope_fails_closed_without_registered_project_id() {
        let root = tempfile::tempdir().expect("root tempdir");

        // Missing registry: there is no path or CWD fallback that could
        // fabricate an exact scope for an unregistered root.
        assert!(resolve_dashboard_scope(root.path(), None).is_none());
    }

    #[test]
    fn dashboard_scope_fails_closed_for_invalid_project_id() {
        let root = tempfile::tempdir().expect("root tempdir");

        assert!(resolve_dashboard_scope(root.path(), Some("")).is_none());
        assert!(resolve_dashboard_scope(root.path(), Some(" project.bad")).is_none());
        assert!(resolve_dashboard_scope(root.path(), Some("project.bad\n")).is_none());
    }

    #[test]
    fn dashboard_scope_resolves_the_exact_root_through_the_application_type() {
        let root = tempfile::tempdir().expect("root tempdir");
        let project_id = ProjectId::new("project.dashboard-scope").expect("project id");

        let scope = resolve_dashboard_scope(root.path(), Some(project_id.as_str()))
            .expect("exact resolved scope");

        scope.validate().expect("resolved scope validates");
        assert_eq!(scope.project_id, project_id);
        // The canonical application authority supplies a digest bound to every
        // resolved identity field; the dashboard does not re-derive it.
        assert_eq!(scope.scope_digest, scope.compute_digest().expect("digest"));

        // Resolved once: repeated resolution is byte-identical, digest included.
        let again = resolve_dashboard_scope(root.path(), Some(project_id.as_str()))
            .expect("exact resolved scope");
        assert_eq!(scope, again);
    }

    #[test]
    fn dashboard_scope_distinguishes_sibling_roots() {
        let first = tempfile::tempdir().expect("first root tempdir");
        let second = tempfile::tempdir().expect("second root tempdir");
        let project_id = "project.dashboard-scope";

        let first_scope =
            resolve_dashboard_scope(first.path(), Some(project_id)).expect("first scope");
        let second_scope =
            resolve_dashboard_scope(second.path(), Some(project_id)).expect("second scope");

        // An unauthorized sibling root can never collide with this root's
        // exact scope: worktree identity and the scope digest both differ.
        assert_ne!(first_scope, second_scope);
        assert_ne!(first_scope.worktree_id, second_scope.worktree_id);
        assert_ne!(first_scope.scope_digest, second_scope.scope_digest);
    }
}
