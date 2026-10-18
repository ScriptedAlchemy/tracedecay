use std::path::Path;
use std::process::Command;

use tracedecay_domain::ProjectId;
use tracedecay_session_memory::context::RegisteredScopeResolver;

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

#[test]
fn registered_scope_route_admits_a_clone_whose_marker_names_the_project() {
    let repository = repository();
    let registered_root = repository.path().canonicalize().expect("registered root");
    let project_id = ProjectId::new("project.registered-route").expect("project id");
    let clone_parent = tempfile::TempDir::new().expect("clone parent");
    let clone = clone_parent.path().join("clone");
    git(
        clone_parent.path(),
        &[
            "clone",
            "--quiet",
            registered_root.to_str().expect("registered path"),
            clone.to_str().expect("clone path"),
        ],
    );
    for root in [registered_root.as_path(), clone.as_path()] {
        assert!(
            tracedecay_runtime_core::storage::write_repository_identity_marker(
                root,
                project_id.as_str(),
            )
            .expect("identity marker")
        );
    }

    let registered_scope =
        RegisteredScopeResolver::resolve(&registered_root, &registered_root, &project_id)
            .expect("registered scope");
    let clone_scope = RegisteredScopeResolver::resolve(&registered_root, &clone, &project_id)
        .expect("a clone whose own marker names the project resolves");

    assert_eq!(clone_scope.project_id, project_id);
    assert_ne!(clone_scope.worktree_id, registered_scope.worktree_id);
}
