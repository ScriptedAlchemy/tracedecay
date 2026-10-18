#[test]
fn test_discover_project_root_finds_parent() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path();
    tracedecay_runtime_core::storage::pin_fixture_repository_identity(root, "proj_discover_parent")
        .unwrap();
    let child = root.join("src/mcp");
    std::fs::create_dir_all(&child).unwrap();

    let profile = tempfile::TempDir::new().unwrap();
    let found = tracedecay_runtime_core::config::ProfileRoot::new(profile.path())
        .discover_project_root(&child);
    assert_eq!(found, Some(root.to_path_buf()));
}

#[test]
fn test_discover_project_root_returns_none() {
    let dir = tempfile::TempDir::new().unwrap();
    let profile = tempfile::TempDir::new().unwrap();
    let profile = tracedecay_runtime_core::config::ProfileRoot::new(profile.path());
    assert!(profile.discover_project_root(dir.path()).is_none());
    tracedecay_runtime_core::storage::pin_fixture_repository_identity(
        dir.path(),
        "proj_discover_none",
    )
    .unwrap();
    assert_eq!(
        profile.discover_project_root(dir.path()),
        Some(dir.path().to_path_buf())
    );
}
