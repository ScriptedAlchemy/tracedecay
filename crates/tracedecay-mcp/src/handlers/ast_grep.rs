//! Structural-search tool handler: `tracedecay_ast_grep_search`.
//!
//! Runs an ast-grep structural pattern over the project working tree *in
//! process* (via [`tracedecay_code_index::ast_grep_search`], which wires the
//! repo's bundled tree-sitter grammars into the `ast-grep-core` pattern
//! engine — no external `ast-grep` binary required).

use std::path::Path;

use serde_json::{Value, json};
use tracedecay_code_index::ast_grep_search::{
    AstGrepSearchMatch, AstGrepSearchResult, search_tree_scoped_with_cancel,
};
use tracedecay_domain::errors::{Result, TraceDecayError};

use crate::ToolResult;
use crate::handlers::run_bounded_search;
use crate::tools::render::{self, Md};
use crate::unique_file_paths;

/// Hard cap on `max_results` regardless of what the caller requests.
const MAX_RESULTS_CAP: usize = 200;
/// Default `max_results` when the caller omits it.
const DEFAULT_MAX_RESULTS: usize = 50;

#[hotpath::measure(future = true, label = "mcp.search.ast_grep.scan")]
async fn search_tree_off_thread(
    project_root: std::path::PathBuf,
    pattern: String,
    lang: Option<String>,
    path_glob: Option<String>,
    max_results: usize,
    scope_prefix: Option<String>,
    deadline: Option<tracedecay_application::Deadline>,
    cancellation: Option<tracedecay_application::CancellationSignal>,
) -> Result<AstGrepSearchResult> {
    let query = pattern.clone();
    run_bounded_search(
        "tracedecay_ast_grep_search",
        query,
        deadline,
        cancellation,
        move |cancelled, transport_cancellation| {
            search_tree_scoped_with_cancel(
                &project_root,
                &pattern,
                lang.as_deref(),
                path_glob.as_deref(),
                max_results,
                scope_prefix.as_deref(),
                || {
                    cancelled.load(std::sync::atomic::Ordering::Acquire)
                        || transport_cancellation
                            .as_ref()
                            .is_some_and(tracedecay_application::CancellationSignal::is_cancelled)
                },
            )
        },
    )
    .await
}

#[hotpath::measure(future = true, label = "mcp.search.ast_grep.total")]
pub async fn handle_ast_grep_search(
    project_root: &Path,
    args: Value,
    scope_prefix: Option<&str>,
    deadline: Option<tracedecay_application::Deadline>,
    cancellation: Option<tracedecay_application::CancellationSignal>,
) -> Result<ToolResult> {
    let pattern =
        args.get("pattern")
            .and_then(Value::as_str)
            .ok_or_else(|| TraceDecayError::Config {
                message: "missing required parameter: pattern".to_string(),
            })?;
    let lang = args
        .get("lang")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let path_glob = args
        .get("path_glob")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let max_results = args
        .get("max_results")
        .and_then(Value::as_u64)
        .map_or(DEFAULT_MAX_RESULTS, |v| (v as usize).min(MAX_RESULTS_CAP))
        .max(1);

    let search = search_tree_off_thread(
        project_root.to_path_buf(),
        pattern.to_string(),
        lang.map(str::to_owned),
        path_glob.map(str::to_owned),
        max_results,
        scope_prefix.map(str::to_owned),
        deadline,
        cancellation,
    )
    .await?;

    let hits = search.matches;

    let touched_files = unique_file_paths(hits.iter().map(|hit| hit.file.as_str()));
    let output_value = build_output_value(&hits, search.truncated, search.files_scanned);

    let text = render::finalize(Some(project_root), &args, &output_value, || {
        render_md(&hits, search.truncated, search.files_scanned)
    });
    Ok(ToolResult::new(
        json!({ "content": [{ "type": "text", "text": text }] }),
        touched_files,
    ))
}

fn build_output_value(hits: &[AstGrepSearchMatch], truncated: bool, files_scanned: usize) -> Value {
    let items: Vec<Value> = hits
        .iter()
        .map(|hit| {
            json!({
                "file": hit.file,
                "line": hit.line,
                "column": hit.column,
                "lang": hit.lang,
                "match": hit.matched_text,
                "line_text": hit.line_text,
            })
        })
        .collect();

    json!({
        "results": items,
        "match_count": hits.len(),
        "files_scanned": files_scanned,
        "truncated": truncated,
    })
}

fn render_md(hits: &[AstGrepSearchMatch], truncated: bool, files_scanned: usize) -> String {
    let mut md = Md::new();
    md.heading(2, "Structural Search Results");
    if hits.is_empty() {
        md.empty_note("No structural matches.");
        md.line(&format!("_Scanned {files_scanned} files._"));
        return md.render();
    }

    for hit in hits {
        let location = format!("{}:{}", hit.file, hit.line);
        md.bullet(&location);
        md.line(&format!("  > {}", hit.matched_text));
    }

    md.blank();
    let mut summary = format!("_{} matches across {files_scanned} files._", hits.len());
    if truncated {
        summary.push_str(" Results capped — narrow with `path_glob` or `max_results`.");
    }
    md.line(&summary);
    md.render()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancellation_guard_signals_worker_on_drop() {
        let cancelled = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        {
            let _guard =
                crate::handlers::bounded_search::CancelSearchOnDrop::new(cancelled.clone());
        }
        assert!(cancelled.load(std::sync::atomic::Ordering::Acquire));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn blocking_search_wrapper_finds_match() {
        let temp = tempfile::tempdir().expect("temp project");
        std::fs::write(temp.path().join("lib.rs"), "fn f() { target(1); }\n")
            .expect("write fixture");

        let result = search_tree_off_thread(
            temp.path().to_path_buf(),
            "target($A)".to_string(),
            Some("rust".to_string()),
            None,
            10,
            None,
            None,
            None,
        )
        .await
        .expect("structural search");

        assert_eq!(result.matches.len(), 1);
    }
}
