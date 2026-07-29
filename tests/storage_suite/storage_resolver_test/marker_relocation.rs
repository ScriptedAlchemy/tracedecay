//! Repository marker survival across moves, renames, and symlink aliases.

use super::*;

#[cfg(unix)]
#[tokio::test]
async fn repository_marker_resolves_through_symlinked_root() {
    let dir = TempDir::new().unwrap();
    let project = dir.path().join("repo");
    let link = dir.path().join("repo-link");
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(project.join("src/lib.rs"), "pub fn linked() {}\n").unwrap();
    init_repo_with_commit(&project);
    let git_common_dir = tracedecay::worktree::git_common_dir(&project).unwrap();
    let project_id = "proj_symlink_root";
    write_repository_identity_marker(&project, project_id).unwrap();

    symlink(&project, &link).unwrap();
    // A symlinked root canonicalizes to the same git common dir, so the marker
    // is accepted with no conflict and the same identity.
    assert_eq!(
        read_repository_identity_marker(&link)
            .unwrap()
            .unwrap()
            .project_id,
        project_id
    );

    let db = HostAdmissionTestRuntimeV1::profile(dir.path())
        .await
        .unwrap();
    register_identity_store(&db, project_id, &project, &git_common_dir).await;
    let via_link_common_dir = tracedecay::worktree::git_common_dir(&link).unwrap();
    let resolution = db
        .resolve_project_store_by_identity(&link, Some(&via_link_common_dir))
        .await
        .unwrap()
        .expect("symlinked root should resolve to the enrolled store");
    assert_eq!(resolution.project.project_id, project_id);
    assert_eq!(resolution.store.store_id, format!("store_{project_id}"));
}

#[tokio::test]
async fn repository_marker_survives_rename_within_parent() {
    let dir = TempDir::new().unwrap();
    let project = dir.path().join("repo");
    let renamed = dir.path().join("repo-renamed");
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(project.join("src/lib.rs"), "pub fn renamed() {}\n").unwrap();
    init_repo_with_commit(&project);
    let git_common_dir = tracedecay::worktree::git_common_dir(&project).unwrap();
    let project_id = "proj_rename";
    write_repository_identity_marker(&project, project_id).unwrap();

    let db = HostAdmissionTestRuntimeV1::profile(dir.path())
        .await
        .unwrap();
    register_identity_store(&db, project_id, &project, &git_common_dir).await;

    fs::rename(&project, &renamed).unwrap();
    // The marker travels inside `.git`; its stored git common dir no longer
    // exists, so the moved checkout keeps its identity.
    assert_eq!(
        read_repository_identity_marker(&renamed)
            .unwrap()
            .unwrap()
            .project_id,
        project_id
    );
    let renamed_common_dir = tracedecay::worktree::git_common_dir(&renamed).unwrap();
    let resolution = db
        .resolve_project_store_by_identity(&renamed, Some(&renamed_common_dir))
        .await
        .unwrap()
        .expect("renamed checkout should resolve to its registered store");
    assert_eq!(resolution.project.project_id, project_id);
}

#[tokio::test]
async fn repository_marker_survives_move_across_parents() {
    let dir = TempDir::new().unwrap();
    let old_parent = dir.path().join("old");
    let new_parent = dir.path().join("new");
    let project = old_parent.join("repo");
    let moved = new_parent.join("repo");
    fs::create_dir_all(project.join("src")).unwrap();
    fs::create_dir_all(&new_parent).unwrap();
    fs::write(project.join("src/lib.rs"), "pub fn moved() {}\n").unwrap();
    init_repo_with_commit(&project);
    let git_common_dir = tracedecay::worktree::git_common_dir(&project).unwrap();
    let project_id = "proj_move";
    write_repository_identity_marker(&project, project_id).unwrap();

    let db = HostAdmissionTestRuntimeV1::profile(dir.path())
        .await
        .unwrap();
    register_identity_store(&db, project_id, &project, &git_common_dir).await;

    fs::rename(&project, &moved).unwrap();
    assert_eq!(
        read_repository_identity_marker(&moved)
            .unwrap()
            .unwrap()
            .project_id,
        project_id
    );
    let moved_common_dir = tracedecay::worktree::git_common_dir(&moved).unwrap();
    let resolution = db
        .resolve_project_store_by_identity(&moved, Some(&moved_common_dir))
        .await
        .unwrap()
        .expect("moved checkout should resolve to its registered store");
    assert_eq!(resolution.project.project_id, project_id);
}

#[cfg(unix)]
#[tokio::test]
async fn repository_marker_resolves_through_two_symlink_aliases() {
    let dir = TempDir::new().unwrap();
    let project = dir.path().join("repo");
    let alias_a = dir.path().join("alias-a");
    let alias_b = dir.path().join("alias-b");
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(project.join("src/lib.rs"), "pub fn aliased() {}\n").unwrap();
    init_repo_with_commit(&project);
    let git_common_dir = tracedecay::worktree::git_common_dir(&project).unwrap();
    let project_id = "proj_two_aliases";
    write_repository_identity_marker(&project, project_id).unwrap();
    symlink(&project, &alias_a).unwrap();
    symlink(&project, &alias_b).unwrap();

    let db = HostAdmissionTestRuntimeV1::profile(dir.path())
        .await
        .unwrap();
    register_identity_store(&db, project_id, &project, &git_common_dir).await;

    // Both symlinks canonicalize to one root: one identity, one store, no flap.
    for alias in [&alias_a, &alias_b] {
        assert_eq!(
            read_repository_identity_marker(alias)
                .unwrap()
                .unwrap()
                .project_id,
            project_id
        );
        let alias_common_dir = tracedecay::worktree::git_common_dir(alias).unwrap();
        let resolution = db
            .resolve_project_store_by_identity(alias, Some(&alias_common_dir))
            .await
            .unwrap()
            .expect("each symlink alias should resolve to the one enrolled store");
        assert_eq!(resolution.project.project_id, project_id);
        assert_eq!(resolution.store.store_id, format!("store_{project_id}"));
    }
}

#[tokio::test]
async fn moved_repo_with_reused_old_path_accepts_marker_and_self_heals() {
    let dir = TempDir::new().unwrap();
    let old_path = dir.path().join("repo");
    let new_path = dir.path().join("repo-moved");
    fs::create_dir_all(old_path.join("src")).unwrap();
    fs::write(old_path.join("src/lib.rs"), "pub fn repo_x() {}\n").unwrap();
    init_repo_with_commit(&old_path);
    let x_id = "proj_repo_x";
    write_repository_identity_marker(&old_path, x_id).unwrap();

    // Repo X moves old -> new; the marker travels with it (git_common_dir still
    // records the original old path).
    fs::rename(&old_path, &new_path).unwrap();

    // An unrelated repo Y is created at the reused old path, with its own marker.
    fs::create_dir_all(old_path.join("src")).unwrap();
    fs::write(old_path.join("src/lib.rs"), "pub fn repo_y() {}\n").unwrap();
    init_repo_with_commit(&old_path);
    let y_id = "proj_repo_y";
    write_repository_identity_marker(&old_path, y_id).unwrap();

    // Opening X at its new path succeeds with X's id: the old path is now live
    // but hosts Y's (different) marker, so this is a move, not a true copy.
    assert_eq!(
        read_repository_identity_marker(&new_path)
            .unwrap()
            .unwrap()
            .project_id,
        x_id
    );
    // Opening Y at the old path uses Y's own id, never X's.
    assert_eq!(
        read_repository_identity_marker(&old_path)
            .unwrap()
            .unwrap()
            .project_id,
        y_id
    );

    // A writable open rewrites the marker's git_common_dir to the current dir,
    // self-healing so subsequent reads no longer take the disambiguation path.
    write_repository_identity_marker(&new_path, x_id).unwrap();
    let new_common_dir = tracedecay::worktree::git_common_dir(&new_path).unwrap();
    let healed = read_repository_identity_marker(&new_path).unwrap().unwrap();
    assert_eq!(healed.project_id, x_id);
    assert_eq!(Path::new(&healed.git_common_dir), new_common_dir.as_path());
}

#[tokio::test]
async fn true_repository_copy_still_fails_closed_as_conflict() {
    let dir = TempDir::new().unwrap();
    let original = dir.path().join("repo");
    let copy = dir.path().join("repo-copy");
    fs::create_dir_all(original.join("src")).unwrap();
    fs::write(original.join("src/lib.rs"), "pub fn copied() {}\n").unwrap();
    init_repo_with_commit(&original);
    let project_id = "proj_copy";
    write_repository_identity_marker(&original, project_id).unwrap();

    // `cp -a` style duplicate keeps the marker verbatim (its git_common_dir
    // still points at the original, which is still live and names the same id).
    copy_dir_all(&original, &copy);

    let error = read_repository_identity_marker(&copy).unwrap_err();
    assert!(
        error.to_string().contains("repository identity conflict"),
        "a true copy must fail closed, got: {error}"
    );
    // The genuine original stays authoritative and keeps resolving.
    assert_eq!(
        read_repository_identity_marker(&original)
            .unwrap()
            .unwrap()
            .project_id,
        project_id
    );
}

#[tokio::test]
async fn resolve_project_store_by_identity_propagates_marker_conflict() {
    let dir = TempDir::new().unwrap();
    let original = dir.path().join("repo");
    let copy = dir.path().join("repo-copy");
    fs::create_dir_all(original.join("src")).unwrap();
    fs::write(original.join("src/lib.rs"), "pub fn conflicting() {}\n").unwrap();
    init_repo_with_commit(&original);
    let git_common_dir = tracedecay::worktree::git_common_dir(&original).unwrap();
    let project_id = "proj_conflict";
    write_repository_identity_marker(&original, project_id).unwrap();

    let db = HostAdmissionTestRuntimeV1::profile(dir.path())
        .await
        .unwrap();
    register_identity_store(&db, project_id, &original, &git_common_dir).await;

    copy_dir_all(&original, &copy);
    let copy_common_dir = tracedecay::worktree::git_common_dir(&copy).unwrap();

    // The conflict must surface as a typed error (fail closed) rather than being
    // swallowed into `None` and minting a fresh path-hash identity.
    let error = db
        .resolve_project_store_by_identity(&copy, Some(&copy_common_dir))
        .await
        .unwrap_err();
    assert!(
        error.to_string().contains("repository identity conflict"),
        "expected a propagated identity conflict, got: {error}"
    );
    // No fresh path-hash project row was minted for the copy.
    assert!(
        db.get_code_project(&default_profile_project_id(&copy))
            .await
            .is_none(),
        "a swallowed conflict must not have minted a new project row"
    );
}
