//! Dependency guard for the root RequestContext convergence slice.

use std::fs;
use std::path::{Path, PathBuf};

fn rust_sources(path: &Path, sources: &mut Vec<PathBuf>) {
    if path.is_file() {
        if path.extension().is_some_and(|extension| extension == "rs") {
            sources.push(path.to_path_buf());
        }
        return;
    }
    for entry in fs::read_dir(path)
        .unwrap_or_else(|error| panic!("read source directory {}: {error}", path.display()))
    {
        let entry = entry.unwrap_or_else(|error| panic!("read source entry: {error}"));
        rust_sources(&entry.path(), sources);
    }
}

fn imports_root_request_context(source: &str) -> bool {
    if source.contains("crate::application::context::RequestContext") {
        return true;
    }
    let mut remaining = source;
    while let Some(start) = remaining.find("use crate::application::context") {
        remaining = &remaining[start..];
        let Some(end) = remaining.find(';') else {
            return true;
        };
        let statement = &remaining[..=end];
        if statement
            .split(|character: char| !character.is_ascii_alphanumeric() && character != '_')
            .any(|token| token == "RequestContext")
        {
            return true;
        }
        remaining = &remaining[end + 1..];
    }
    false
}

#[test]
fn assigned_production_callers_use_application_request_context() {
    let repository = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut sources = Vec::new();
    for path in [
        "src/application/session",
        "src/automation",
        "src/sessions",
        "src/application/code_index.rs",
    ] {
        rust_sources(&repository.join(path), &mut sources);
    }
    assert!(
        !sources.is_empty(),
        "assigned production sources must exist"
    );

    let violations = sources
        .into_iter()
        .filter_map(|path| {
            let source = fs::read_to_string(&path)
                .unwrap_or_else(|error| panic!("read source {}: {error}", path.display()));
            imports_root_request_context(&source).then_some(path)
        })
        .collect::<Vec<_>>();
    assert!(
        violations.is_empty(),
        "assigned production callers must use tracedecay_application::RequestContext with an exact ResolvedScope:\n{}",
        violations
            .iter()
            .map(|path| path.display().to_string())
            .collect::<Vec<_>>()
            .join("\n")
    );
}

#[test]
fn guard_detects_grouped_and_qualified_root_imports() {
    assert!(imports_root_request_context(
        "use crate::application::context::{CancellationToken, RequestContext};"
    ));
    assert!(imports_root_request_context(
        "fn legacy(_: crate::application::context::RequestContext) {}"
    ));
    assert!(!imports_root_request_context(
        "use crate::application::context::CancellationToken;\nuse tracedecay_application::RequestContext;"
    ));
}
