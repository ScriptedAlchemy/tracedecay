#[test]
fn test_discover_project_root_finds_parent() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path();
    tracedecay_runtime_core::storage::pin_fixture_repository_identity(root, "proj_discover_parent")
        .unwrap();
    let child = root.join("src/mcp");
    std::fs::create_dir_all(&child).unwrap();

    let found = tracedecay_runtime_core::config::discover_project_root(&child);
    assert_eq!(found, Some(root.to_path_buf()));
}

#[test]
fn test_discover_project_root_returns_none() {
    let dir = tempfile::TempDir::new().unwrap();
    let found = tracedecay_runtime_core::config::discover_project_root(dir.path());
    assert!(found.is_none());
}
