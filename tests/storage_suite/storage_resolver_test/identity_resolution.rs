//! Observation-store identity resolution tests.

use super::*;

#[tokio::test]
async fn observation_store_resolver_uses_repository_marker_after_checkout_move() {
    let dir = TempDir::new().unwrap();
    let project = dir.path().join("repo");
    let moved = dir.path().join("repo-moved");
    let profile_root = dir.path().join("profile");
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(project.join("src/lib.rs"), "pub fn marker_only() {}\n").unwrap();
    init_repo_with_commit(&project);
    let git_common_dir = tracedecay::worktree::git_common_dir(&project).unwrap();
    let project_id = "proj_marker_only";
    let db = HostAdmissionTestRuntimeV1::profile(&profile_root)
        .await
        .unwrap();
    let (store_root, database_path) = register_observation_store(
        &db,
        &profile_root,
        project_id,
        &project,
        Some(&git_common_dir),
    )
    .await;
    write_repository_identity_marker(&project, project_id).unwrap();

    fs::rename(&project, &moved).unwrap();
    assert!(
        db.project_registry_context_by_alias(&moved)
            .await
            .unwrap()
            .is_none(),
        "the moved checkout must not already have a canonical path alias"
    );
    assert_ne!(
        tracedecay::worktree::git_common_dir(&moved).unwrap(),
        git_common_dir,
        "moving a primary checkout should also move its git common dir"
    );

    let resolution = db
        .resolve_project_observation_store(&moved)
        .await
        .expect("the durable repository marker should preserve project identity");
    assert_eq!(resolution.project().project_id, project_id);
    assert_eq!(resolution.store_root(), store_root.canonicalize().unwrap());
    assert_eq!(
        resolution.database_path(),
        database_path.canonicalize().unwrap()
    );
}

#[tokio::test]
async fn observation_store_resolver_rejects_conflicting_path_common_dir_and_marker_identities() {
    let dir = TempDir::new().unwrap();
    let project = dir.path().join("repo");
    let common_dir_owner = dir.path().join("common-dir-owner");
    let marker_owner = dir.path().join("marker-owner");
    let profile_root = dir.path().join("profile");
    fs::create_dir_all(project.join("src")).unwrap();
    fs::create_dir_all(&common_dir_owner).unwrap();
    fs::create_dir_all(&marker_owner).unwrap();
    fs::write(project.join("src/lib.rs"), "pub fn ambiguous() {}\n").unwrap();
    init_repo_with_commit(&project);
    let git_common_dir = tracedecay::worktree::git_common_dir(&project).unwrap();
    let db = HostAdmissionTestRuntimeV1::profile(&profile_root)
        .await
        .unwrap();

    db.upsert_code_project("proj_path", &project, None, None, None)
        .await
        .unwrap();
    db.upsert_code_project(
        "proj_common_dir",
        &common_dir_owner,
        Some(&git_common_dir),
        None,
        None,
    )
    .await
    .unwrap();
    db.upsert_code_project("proj_marker", &marker_owner, None, None, None)
        .await
        .unwrap();
    write_repository_identity_marker(&project, "proj_marker").unwrap();

    let error = db
        .resolve_project_observation_store(&project)
        .await
        .unwrap_err();
    let (project_root, mut project_ids) = match error {
        ProjectObservationStoreError::AmbiguousProjectIdentity {
            project_root,
            project_ids,
        } => (project_root, project_ids),
        other => panic!("expected ambiguous project identity, got {other:?}"),
    };
    project_ids.sort();
    assert_eq!(project_root, project.canonicalize().unwrap());
    assert_eq!(
        project_ids,
        vec![
            "proj_common_dir".to_string(),
            "proj_marker".to_string(),
            "proj_path".to_string(),
        ]
    );
}
