//! In-process structural code search.

mod ast_grep_search;

pub use ast_grep_search::{
    AstGrepSearchError, AstGrepSearchMatch, AstGrepSearchResult, search_tree,
};

#[doc(hidden)]
pub use ast_grep_search::{search_tree_scoped, search_tree_scoped_with_cancel};

/// Returns `true` if the file path looks like a test file.
pub fn is_test_file(path: &str) -> bool {
    let test_segments = [
        "test/",
        "tests/",
        "__tests__/",
        "spec/",
        "e2e/",
        ".test.",
        ".spec.",
        "_test.",
        "_spec.",
    ];
    let lower = path.to_ascii_lowercase();
    test_segments.iter().any(|segment| lower.contains(segment))
}
