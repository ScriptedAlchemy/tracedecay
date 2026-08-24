use tempfile::TempDir;
use tracedecay::config::*;

#[test]
fn default_config_excludes_generated_vendor_cache_trees_and_gitignore_on() {
    let config = TraceDecayConfig::default();
    assert!(config.git_ignore);
    assert!(config.include.is_empty());
    for pattern in [
        "target/**",
        ".git/**",
        ".tracedecay/**",
        "**/node_modules/**",
        "vendor/**",
        "**/vendor/**",
        "build/**",
        "**/build/**",
        "dist/**",
        "**/dist/**",
        "out/**",
        "**/out/**",
        "coverage/**",
        "**/coverage/**",
        ".cache/**",
        "**/.cache/**",
        ".next/**",
        "**/.next/**",
        ".turbo/**",
        "**/.turbo/**",
        ".gradle/**",
        "**/.gradle/**",
        ".venv/**",
        "**/.venv/**",
        "venv/**",
        "**/venv/**",
        "**/__pycache__/**",
    ] {
        assert!(
            config.exclude.iter().any(|p| p == pattern),
            "missing default exclude pattern {pattern}"
        );
    }
}

#[test]
fn legacy_config_fixture_load_does_not_rewrite_input() {
    let dir = TempDir::new().unwrap();
    let config = TraceDecayConfig::default();
    let config_path = get_config_path(dir.path());
    std::fs::create_dir_all(config_path.parent().unwrap()).unwrap();
    let source = serde_json::to_string_pretty(&config).unwrap();
    std::fs::write(&config_path, &source).unwrap();
    let loaded = load_config(dir.path()).unwrap();
    assert_eq!(config.version, loaded.version);
    assert_eq!(config.exclude, loaded.exclude);
    assert_eq!(std::fs::read_to_string(config_path).unwrap(), source);
}

#[test]
fn test_is_excluded() {
    let config = TraceDecayConfig::default();
    assert!(!is_excluded("src/main.rs", &config));
    assert!(is_excluded("target/debug/foo", &config));
    assert!(is_excluded("node_modules/foo.rs", &config));
    assert!(is_excluded("build/classes/App.class", &config));
    assert!(is_excluded("packages/web/dist/main.js", &config));
    assert!(is_excluded("packages/web/coverage/lcov.js", &config));
    assert!(is_excluded("packages/web/.next/server/app.js", &config));
    assert!(is_excluded("tools/.cache/generated.py", &config));
}

#[test]
fn default_generated_excludes_prune_nested_dirs() {
    let config = TraceDecayConfig::default();
    for path in [
        "packages/web/dist",
        "packages/web/coverage",
        "packages/web/.next",
        "packages/web/.turbo",
        "tools/.cache",
        "backend/.venv",
        "backend/__pycache__",
    ] {
        assert!(
            is_excluded_dir(path, &config),
            "expected default excludes to prune {path}"
        );
    }
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
    assert!(loaded.git_ignore);
}

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

// ── resolve_path ────────────────────────────────────────────────────────────

#[test]
fn test_resolve_path_with_value() {
    let path = std::env::temp_dir().join("myproject");
    let result = resolve_path(Some(path.to_string_lossy().into_owned()));
    assert_eq!(result, path);
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
