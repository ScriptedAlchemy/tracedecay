use super::{normalize_rel_path, normalize_rel_paths};

#[test]
fn normalize_rel_path_converts_backslashes() {
    assert_eq!(normalize_rel_path("src\\foo.py"), "src/foo.py");
    assert_eq!(normalize_rel_path("a\\b\\c\\d.rs"), "a/b/c/d.rs");
}

#[test]
fn normalize_rel_path_leaves_forward_slashes_alone() {
    assert_eq!(normalize_rel_path("src/foo.py"), "src/foo.py");
    assert_eq!(normalize_rel_path("a"), "a");
    assert_eq!(normalize_rel_path(""), "");
}

#[test]
fn normalize_rel_paths_processes_a_mixed_slice() {
    let input = vec![
        "src/a.rs".to_string(),
        "src\\b.rs".to_string(),
        "lib\\nested\\c.rs".to_string(),
    ];
    let out = normalize_rel_paths(&input);
    assert_eq!(out, vec!["src/a.rs", "src/b.rs", "lib/nested/c.rs"]);
}
