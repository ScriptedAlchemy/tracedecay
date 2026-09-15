use tracedecay_domain::repository_path_matches_scope;

#[test]
fn scope_matches_itself_and_descendants_only() {
    assert!(repository_path_matches_scope("src", Some("src")));
    assert!(repository_path_matches_scope("src/lib.rs", Some("src")));
    assert!(repository_path_matches_scope(
        "src/code/index.rs",
        Some("src")
    ));

    assert!(!repository_path_matches_scope(
        "src-old/lib.rs",
        Some("src")
    ));
    assert!(!repository_path_matches_scope("source/lib.rs", Some("src")));
    assert!(!repository_path_matches_scope(
        "tests/src/lib.rs",
        Some("src")
    ));
}
