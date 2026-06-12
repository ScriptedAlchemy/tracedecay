use tempfile::TempDir;
use tracedecay::config::*;

#[test]
fn test_default_config_has_exclude_patterns() {
    let config = TraceDecayConfig::default();
    assert!(config.exclude.iter().any(|p| p == "target/**"));
    assert!(config.exclude.iter().any(|p| p == ".git/**"));
}

#[test]
fn test_save_and_load_config() {
    let dir = TempDir::new().unwrap();
    let config = TraceDecayConfig::default();
    save_config(dir.path(), &config).unwrap();
    let loaded = load_config(dir.path()).unwrap();
    assert_eq!(config.version, loaded.version);
    assert_eq!(config.exclude, loaded.exclude);
}

#[test]
fn test_is_excluded() {
    let config = TraceDecayConfig::default();
    assert!(!is_excluded("src/main.rs", &config));
    assert!(is_excluded("target/debug/foo", &config));
    assert!(is_excluded("node_modules/foo.rs", &config));
    assert!(is_excluded("build/classes/App.class", &config));
}

#[test]
fn test_tracedecay_dir_creation() {
    let dir = TempDir::new().unwrap();
    let cg_dir = get_tracedecay_dir(dir.path());
    assert!(cg_dir.ends_with(".tracedecay"));
}

#[test]
fn test_config_serde_roundtrip() {
    let config = TraceDecayConfig::default();
    let json = serde_json::to_string_pretty(&config).unwrap();
    let deserialized: TraceDecayConfig = serde_json::from_str(&json).unwrap();
    assert_eq!(config.version, deserialized.version);
    assert_eq!(config.max_file_size, deserialized.max_file_size);
}

#[test]
fn test_legacy_config_with_include_field_still_loads() {
    let dir = TempDir::new().unwrap();
    let tracedecay_dir = dir.path().join(".tracedecay");
    std::fs::create_dir_all(&tracedecay_dir).unwrap();
    // Simulate an old config that still has an "include" field
    let legacy_json = r#"{
        "version": 1,
        "root_dir": ".",
        "include": ["**/*.rs"],
        "exclude": ["target/**", ".git/**", ".tracedecay/**"],
        "max_file_size": 1048576,
        "extract_docstrings": true,
        "track_call_sites": true,
        "enable_embeddings": false
    }"#;
    std::fs::write(tracedecay_dir.join("config.json"), legacy_json).unwrap();
    let loaded = load_config(dir.path()).unwrap();
    assert_eq!(loaded.version, 1);
    assert!(loaded.exclude.contains(&"target/**".to_string()));
}

// ── is_in_gitignore ─────────────────────────────────────────────────────────

#[test]
fn test_is_in_gitignore_present() {
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join(".gitignore"), ".tracedecay\n").unwrap();
    assert!(is_in_gitignore(dir.path()));
}

#[test]
fn test_is_in_gitignore_with_slash() {
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join(".gitignore"), ".tracedecay/\n").unwrap();
    assert!(is_in_gitignore(dir.path()));
}

#[test]
fn test_is_in_gitignore_with_leading_slash() {
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join(".gitignore"), "/.tracedecay\n").unwrap();
    assert!(is_in_gitignore(dir.path()));
}

#[test]
fn test_is_in_gitignore_absent() {
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join(".gitignore"), "target/\n*.o\n").unwrap();
    assert!(!is_in_gitignore(dir.path()));
}

#[test]
fn test_is_in_gitignore_no_file() {
    let dir = TempDir::new().unwrap();
    assert!(!is_in_gitignore(dir.path()));
}

#[test]
fn test_is_in_gitignore_among_other_entries() {
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join(".gitignore"), "target/\n.tracedecay\n*.o\n").unwrap();
    assert!(is_in_gitignore(dir.path()));
}

// ── add_to_gitignore ────────────────────────────────────────────────────────

#[test]
fn test_add_to_gitignore_creates_file() {
    let dir = TempDir::new().unwrap();
    add_to_gitignore(dir.path());
    let content = std::fs::read_to_string(dir.path().join(".gitignore")).unwrap();
    assert!(content.contains(".tracedecay"));
    assert!(content.ends_with('\n'));
}

#[test]
fn test_add_to_gitignore_appends() {
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join(".gitignore"), "target/\n").unwrap();
    add_to_gitignore(dir.path());
    let content = std::fs::read_to_string(dir.path().join(".gitignore")).unwrap();
    assert!(content.contains("target/"));
    assert!(content.contains(".tracedecay"));
}

#[test]
fn test_add_to_gitignore_adds_newline_if_missing() {
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join(".gitignore"), "target/").unwrap();
    add_to_gitignore(dir.path());
    let content = std::fs::read_to_string(dir.path().join(".gitignore")).unwrap();
    assert!(content.contains("target/\n.tracedecay\n"));
}

// ── resolve_path ────────────────────────────────────────────────────────────

#[test]
fn test_resolve_path_with_value() {
    let result = resolve_path(Some("/tmp/myproject".to_string()));
    assert_eq!(result, std::path::PathBuf::from("/tmp/myproject"));
}

#[test]
fn test_resolve_path_none_uses_cwd() {
    let result = resolve_path(None);
    assert!(!result.as_os_str().is_empty());
}

#[test]
fn test_discover_project_root_finds_parent() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path();
    std::fs::create_dir_all(root.join(".tracedecay")).unwrap();
    std::fs::write(root.join(".tracedecay/tracedecay.db"), b"fake").unwrap();
    let child = root.join("src/mcp");
    std::fs::create_dir_all(&child).unwrap();

    let found = tracedecay::config::discover_project_root(&child);
    assert_eq!(found, Some(root.to_path_buf()));
}

#[test]
fn test_discover_project_root_returns_none() {
    let dir = tempfile::TempDir::new().unwrap();
    let found = tracedecay::config::discover_project_root(dir.path());
    assert!(found.is_none());
}

#[test]
fn test_discover_project_root_at_root_itself() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path();
    std::fs::create_dir_all(root.join(".tracedecay")).unwrap();
    std::fs::write(root.join(".tracedecay/tracedecay.db"), b"fake").unwrap();

    let found = tracedecay::config::discover_project_root(root);
    assert_eq!(found, Some(root.to_path_buf()));
}
