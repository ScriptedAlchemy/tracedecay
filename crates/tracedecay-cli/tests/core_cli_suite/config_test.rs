use tempfile::TempDir;
use tracedecay_configuration::is_in_gitignore;

// ── is_in_gitignore ─────────────────────────────────────────────────────────

/// Every `.gitignore` spelling the detector must accept or reject. The table is
/// a fixed-size array, so it can never iterate empty. The no-file case is a
/// separate test below because it writes no `.gitignore` at all.
#[test]
fn test_is_in_gitignore_recognizes_tracedecay_entry_spellings() {
    let cases: [(&str, bool); 5] = [
        // present
        (".tracedecay\n", true),
        // with a trailing slash
        (".tracedecay/\n", true),
        // with a leading slash
        ("/.tracedecay\n", true),
        // absent
        ("target/\n*.o\n", false),
        // among other entries
        ("target/\n.tracedecay\n*.o\n", true),
    ];

    for (contents, expected) in cases {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join(".gitignore"), contents).unwrap();
        assert_eq!(
            is_in_gitignore(dir.path()),
            expected,
            "unexpected is_in_gitignore result for .gitignore contents {contents:?}"
        );
    }
}

#[test]
fn test_is_in_gitignore_no_file() {
    let dir = TempDir::new().unwrap();
    assert!(!is_in_gitignore(dir.path()));
}

#[test]
fn test_discover_project_root_finds_parent() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path();
    tracedecay_runtime_core::storage::pin_fixture_repository_identity(root, "proj_discover_parent")
        .unwrap();
    let child = root.join("src/mcp");
    std::fs::create_dir_all(&child).unwrap();

    let found = tracedecay_project::config::discover_project_root(&child);
    assert_eq!(found, Some(root.to_path_buf()));
}

#[test]
fn test_discover_project_root_returns_none() {
    let dir = tempfile::TempDir::new().unwrap();
    let found = tracedecay_project::config::discover_project_root(dir.path());
    assert!(found.is_none());
}
