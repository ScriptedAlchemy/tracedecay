//! Structural-search tool handler: `tracedecay_ast_grep_search`.
//!
//! Runs an ast-grep structural pattern over the project working tree *in
//! process* (via [`tracedecay_code_index::ast_grep_search`], which wires the
//! repo's bundled tree-sitter grammars into the `ast-grep-core` pattern
//! engine, no external `ast-grep` binary required).

use std::path::Path;

use serde_json::Value;
use tracedecay_code_index::ast_grep_search::{AstGrepSearchResult, search_tree_scoped_with_cancel};
use tracedecay_contracts::graph_tool::{GraphToolCompletionV1, GraphToolResultV1};
use tracedecay_contracts::retrieval::{
    AstGrepSearchMatchV1, AstGrepSearchResultV1, AstGrepSearchSurfaceRequestV1,
};
use tracedecay_domain::errors::Result;

use crate::ToolResult;
use crate::handlers::graph::graph_tool_completion;
use crate::handlers::run_bounded_search;
use crate::handlers::support::{decode_primitive_request, text_tool_result};
use crate::tools::render::{self, Md};
use crate::unique_file_paths;

/// Hard cap on `max_results` regardless of what the caller requests.
const MAX_RESULTS_CAP: usize = 200;
/// Default `max_results` when the caller omits it.
const DEFAULT_MAX_RESULTS: usize = 50;

#[hotpath::measure(future = true, label = "mcp.search.ast_grep.total")]
pub async fn compute_ast_grep_search(
    project_root: &Path,
    args: Value,
    scope_prefix: Option<&str>,
    deadline: Option<tracedecay_contracts::Deadline>,
    cancellation: Option<tracedecay_contracts::CancellationSignal>,
) -> Result<GraphToolCompletionV1> {
    let request: AstGrepSearchSurfaceRequestV1 =
        decode_primitive_request(&args, "tracedecay_ast_grep_search")?;
    let lang = non_blank(request.lang);
    let path_glob = non_blank(request.path_glob);
    let max_results = request
        .max_results
        .map_or(DEFAULT_MAX_RESULTS, |v| (v as usize).min(MAX_RESULTS_CAP))
        .max(1);

    let project_root_buf = project_root.to_path_buf();
    let query = request.pattern.clone();
    let scope_prefix = scope_prefix.map(str::to_owned);
    let search: AstGrepSearchResult = hotpath::future!(
        run_bounded_search(
            "tracedecay_ast_grep_search",
            request.pattern,
            deadline,
            cancellation,
            move |cancelled, transport_cancellation| {
                search_tree_scoped_with_cancel(
                    &project_root_buf,
                    &query,
                    lang.as_deref(),
                    path_glob.as_deref(),
                    max_results,
                    scope_prefix.as_deref(),
                    || {
                        cancelled.load(std::sync::atomic::Ordering::Acquire)
                            || transport_cancellation
                                .as_ref()
                                .is_some_and(tracedecay_contracts::CancellationSignal::is_cancelled)
                    },
                )
            },
        ),
        label = "mcp.search.ast_grep.scan"
    )
    .await?;

    let touched_files = unique_file_paths(search.matches.iter().map(|hit| hit.file.as_ref()));
    let results = search
        .matches
        .into_iter()
        .map(|hit| AstGrepSearchMatchV1 {
            file: hit.file.to_string(),
            line: hit.line,
            column: hit.column,
            lang: hit.lang,
            matched_text: hit.matched_text,
            line_text: hit.line_text,
        })
        .collect::<Vec<_>>();
    Ok(graph_tool_completion(
        GraphToolResultV1::AstGrepSearch(AstGrepSearchResultV1 {
            match_count: results.len() as u64,
            results,
            files_scanned: search.files_scanned as u64,
            truncated: search.truncated,
        }),
        touched_files,
    ))
}

fn non_blank(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

/// Renders a structural-search result as its tool text.
pub fn render_ast_grep_search(
    response_handle_root: Option<&Path>,
    args: &Value,
    result: &AstGrepSearchResultV1,
) -> Result<ToolResult> {
    let value = serde_json::to_value(result)?;
    let text = render::finalize(response_handle_root, args, &value, || render_md(result));
    Ok(text_tool_result(&text, Vec::new()))
}

fn render_md(result: &AstGrepSearchResultV1) -> String {
    let files_scanned = result.files_scanned;
    let mut md = Md::new();
    md.heading(2, "Structural Search Results");
    if result.results.is_empty() {
        md.empty_note("No structural matches.");
        md.line(&format!("_Scanned {files_scanned} files._"));
        return md.render();
    }

    for hit in &result.results {
        let location = format!("{}:{}", hit.file, hit.line);
        md.bullet(&location);
        md.line(&format!("  > {}", hit.matched_text));
    }

    md.blank();
    let mut summary = format!(
        "_{} matches across {files_scanned} files._",
        result.results.len()
    );
    if result.truncated {
        summary.push_str(" Results capped. Narrow with `path_glob` or `max_results`.");
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
    async fn bounded_search_finds_match() {
        let temp = tempfile::tempdir().expect("temp project");
        std::fs::write(temp.path().join("lib.rs"), "fn f() { target(1); }\n")
            .expect("write fixture");

        let result = compute_ast_grep_search(
            temp.path(),
            serde_json::json!({"pattern": "target($A)", "lang": "rust", "max_results": 10}),
            None,
            None,
            None,
        )
        .await
        .expect("structural search");

        assert_eq!(result.touched_files, vec!["lib.rs".to_owned()]);
    }
}
