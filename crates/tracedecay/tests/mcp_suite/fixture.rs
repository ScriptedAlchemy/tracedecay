//! Source fixtures shared by MCP suite tests.

use std::fs;
use std::path::Path;

/// Writes the shared production-composition fixture sources: cross-file calls,
/// structs, impls, a test file, and doc comments.
pub fn write_indexed_fixture_sources(project: &Path) {
    fs::create_dir_all(project.join("src")).unwrap();

    fs::write(
        project.join("src").join("main.rs"),
        r#"
use crate::utils::helper;
mod utils;

fn main() {
    let result = helper();
    println!("{}", result);
}
"#,
    )
    .unwrap();

    fs::write(
        project.join("src").join("utils.rs"),
        r#"
/// Returns a greeting string.
pub fn helper() -> String {
    format_greeting("world")
}

fn format_greeting(name: &str) -> String {
    format!("Hello, {}!", name)
}
"#,
    )
    .unwrap();

    // Test file so affected-tests can find something
    fs::create_dir_all(project.join("tests")).unwrap();
    fs::write(
        project.join("tests").join("test_utils.rs"),
        r#"
use crate::utils::helper;

#[test]
fn test_helper() { assert!(!helper().is_empty()); }
"#,
    )
    .unwrap();
}
