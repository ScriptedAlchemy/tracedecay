use std::path::Path;
use std::process::Command;

use tracedecay_domain::ProjectId;
use tracedecay_usecases::context::RegisteredScopeResolver;

fn git(root: &Path, args: &[&str]) {
    let status = Command::new("git")
        .current_dir(root)
        .args(args)
        .status()
        .expect("git command starts");
    assert!(status.success(), "git command failed: {args:?}");
}

fn repository() -> tempfile::TempDir {
    let root = tempfile::TempDir::new().expect("repository");
    for args in [
        &["init", "-q"][..],
        &["config", "user.name", "TraceDecay Test"][..],
        &["config", "user.email", "test@tracedecay.invalid"][..],
        &["commit", "--allow-empty", "-qm", "initial"][..],
    ] {
        git(root.path(), args);
    }
    root
}

#[test]
fn registered_scope_route_preserves_linked_worktree_identity() {
    let repository = repository();
    let registered_root = repository.path().canonicalize().expect("registered root");
    let project_id = ProjectId::new("project.registered-route").expect("project id");
    assert!(
        tracedecay_runtime_core::storage::write_repository_identity_marker(
            &registered_root,
            project_id.as_str(),
        )
        .expect("identity marker")
    );
    let linked_parent = tempfile::TempDir::new().expect("linked parent");
    let linked = linked_parent.path().join("linked");
    git(
        &registered_root,
        &[
            "worktree",
            "add",
            "--quiet",
            "--detach",
            linked.to_str().expect("linked path"),
        ],
    );

    let registered_scope =
        RegisteredScopeResolver::resolve(&registered_root, &registered_root, &project_id)
            .expect("registered scope");
    let linked_scope = RegisteredScopeResolver::resolve(&registered_root, &linked, &project_id)
        .expect("linked scope");

    assert_eq!(linked_scope.project_id, project_id);
    assert_eq!(linked_scope.repository_id, registered_scope.repository_id);
    assert_ne!(linked_scope.worktree_id, registered_scope.worktree_id);
}

#[test]
fn registered_scope_route_rejects_foreign_repository() {
    let registered = repository();
    let foreign = repository();
    let project_id = ProjectId::new("project.registered-route").expect("project id");
    assert!(
        tracedecay_runtime_core::storage::write_repository_identity_marker(
            registered.path(),
            project_id.as_str(),
        )
        .expect("identity marker")
    );

    assert!(
        RegisteredScopeResolver::resolve(registered.path(), foreign.path(), &project_id).is_err()
    );
}
