use tracedecay_domain::{path_matches_scope, repository_path_matches_scope};

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

#[test]
fn trailing_slash_is_literal_for_repository_scope_and_a_separator_for_path_scope() {
    assert!(repository_path_matches_scope("src/", Some("src/")));
    assert!(repository_path_matches_scope("src//lib.rs", Some("src/")));
    assert!(!repository_path_matches_scope("src/lib.rs", Some("src/")));
    assert!(repository_path_matches_scope("", Some("")));
    assert!(repository_path_matches_scope("/src", Some("")));
    assert!(!repository_path_matches_scope("src", Some("")));
    assert!(!repository_path_matches_scope("/src", Some("/")));

    assert!(path_matches_scope("src/lib.rs", Some("src")));
    assert!(path_matches_scope("src", Some("src")));
    assert!(!path_matches_scope("src2/lib.rs", Some("src")));
    assert!(path_matches_scope("src/lib.rs", None));
    assert!(path_matches_scope("src/lib.rs", Some("src/")));
    assert!(path_matches_scope("src/", Some("src/")));
    assert!(!path_matches_scope("src", Some("src/")));
    assert!(path_matches_scope("", Some("")));
    assert!(path_matches_scope("/a", Some("")));
    assert!(!path_matches_scope("a", Some("")));
    assert!(path_matches_scope("/src", Some("/")));
    assert!(!path_matches_scope("src", Some("/")));
}
