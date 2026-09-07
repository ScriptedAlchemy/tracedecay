//! Authoritative repository identity for the evaluator's historical lane.

use std::path::Path;

use tracedecay_application::ResolvedScope;

/// Resolve the authoritative project/repository/worktree identity of one
/// checkout for the evaluator's historical lane.
///
/// Returns `None` when the checkout carries no authoritative repository
/// identity marker, or when the marker does not admit a worktree identity. The
/// evaluator turns that into an explicit contract failure rather than guessing
/// an identity.
pub fn root_admitted_corpus_scope(repo_root: &Path) -> Option<ResolvedScope> {
    let marker = tracedecay_runtime_core::storage::read_repository_identity_marker(repo_root)
        .ok()
        .flatten()?;
    let project_id = tracedecay_domain::ProjectId::new(marker.project_id.clone()).ok()?;
    let (project_id, repository_id, worktree_id) =
        tracedecay_sessions::repository_provenance::RepositoryProvenanceAdmissionContext::
            from_authoritative_project_marker(repo_root, &project_id, &marker)
            .and_then(|context| context.admitted_identity())?;
    ResolvedScope::new(project_id, repository_id, worktree_id, None).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn init_repository(root: &Path) {
        let output = std::process::Command::new("git")
            .arg("init")
            .arg("--quiet")
            .arg(root)
            .output()
            .expect("initialize repository fixture");
        assert!(
            output.status.success(),
            "initialize repository fixture: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn admitted_corpus_scope_requires_the_authoritative_marker() {
        let temp = tempfile::tempdir().expect("temporary repository fixture");
        let root = temp.path().join("repo");
        init_repository(&root);

        assert!(
            root_admitted_corpus_scope(&root).is_none(),
            "a markerless checkout carries no authoritative identity"
        );

        assert!(
            tracedecay_runtime_core::storage::write_repository_identity_marker(
                &root,
                "project.search-eval-fixture"
            )
            .expect("write repository fixture identity")
        );
        let scope = root_admitted_corpus_scope(&root).expect("admitted identity");
        assert_eq!(scope.project_id.as_str(), "project.search-eval-fixture");
        assert!(scope.repository_id.as_str().starts_with("repository."));
        assert!(scope.worktree_id.as_str().starts_with("worktree."));
    }
}
