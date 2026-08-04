#![deny(clippy::all)]
#![warn(clippy::pedantic)]
#![cfg_attr(not(test), deny(clippy::unwrap_used))]
#![cfg_attr(not(test), deny(clippy::expect_used))]
#![allow(clippy::module_name_repetitions)]
#![allow(clippy::missing_errors_doc)]
#![allow(clippy::missing_panics_doc)]
#![allow(clippy::cast_possible_truncation)]
#![allow(clippy::cast_sign_loss)]
#![allow(clippy::cast_precision_loss)]
#![allow(clippy::cast_possible_wrap)]
#![allow(clippy::too_many_lines)]
#![allow(clippy::must_use_candidate)]
#![allow(clippy::struct_excessive_bools)]
#![allow(clippy::similar_names)]
#![allow(clippy::wildcard_imports)]
#![allow(clippy::collapsible_if)]
#![allow(clippy::unnecessary_wraps)]
#![allow(clippy::single_match)]
#![allow(clippy::needless_borrow)]
#![allow(clippy::map_unwrap_or)]
#![allow(clippy::redundant_closure)]
#![allow(clippy::redundant_closure_for_method_calls)]
#![allow(clippy::format_push_string)]

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
