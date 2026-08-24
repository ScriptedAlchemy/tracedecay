//! Structural-search tool handler: `tracedecay_ast_grep_search`.
//!
//! Runs an ast-grep structural pattern over the project working tree *in
//! process* (via [`crate::ast_grep_search`], which wires the repo's bundled
//! tree-sitter grammars into the `ast-grep-core` pattern engine — no external
//! `ast-grep` binary required). Each hit is graph-enriched with its enclosing
//! symbol, exactly like `tracedecay_grep`, so the natural follow-up is
//! `tracedecay_body`.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use serde_json::{Value, json};

use crate::ast_grep_search::{AstGrepSearchMatch, search_tree_scoped_with_cancel};
use crate::errors::{Result, TraceDecayError};
use crate::tracedecay::TraceDecay;

use super::super::ToolResult;
use super::super::render::{self, Md};
use super::support::{CancelSearchOnDrop, unique_file_paths};

/// Hard cap on `max_results` regardless of what the caller requests.
const MAX_RESULTS_CAP: usize = 200;
/// Default `max_results` when the caller omits it.
const DEFAULT_MAX_RESULTS: usize = 50;

async fn search_tree_off_thread(
    project_root: std::path::PathBuf,
    pattern: String,
    lang: Option<String>,
    path_glob: Option<String>,
    max_results: usize,
    scope_prefix: Option<String>,
) -> Result<crate::ast_grep_search::AstGrepSearchResult> {
    let query = pattern.clone();
    let cancelled = Arc::new(AtomicBool::new(false));
    let cancel_on_drop = CancelSearchOnDrop::new(cancelled.clone());
    let result = tokio::task::spawn_blocking(move || {
        search_tree_scoped_with_cancel(
            &project_root,
            &pattern,
            lang.as_deref(),
            path_glob.as_deref(),
            max_results,
            scope_prefix.as_deref(),
            || cancelled.load(Ordering::Acquire),
        )
    })
    .await
    .map_err(|err| TraceDecayError::Search {
        message: format!("structural search worker failed: {err}"),
        query,
    })?;
    drop(cancel_on_drop);
    result.map_err(|err| TraceDecayError::Config {
        message: err.to_string(),
    })
}

/// Handles `tracedecay_ast_grep_search` tool calls.
pub(super) async fn handle_ast_grep_search(
    cg: &TraceDecay,
    args: Value,
    scope_prefix: Option<&str>,
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

    let project_root = cg.project_root().to_path_buf();
    let mut search = search_tree_off_thread(
        project_root,
        pattern.to_string(),
        lang.map(str::to_owned),
        path_glob.map(str::to_owned),
        max_results,
        scope_prefix.map(str::to_owned),
    )
    .await?;

    // Enrich each hit with the smallest graph node that contains it (mirrors
    // tracedecay_grep so the model can jump straight to tracedecay_body).
    let mut hits: Vec<EnrichedHit> = Vec::with_capacity(search.matches.len());
    for m in search.matches.drain(..) {
        let mut hit = EnrichedHit::new(m);
        if let Ok(Some(node)) = cg.node_at_location(&hit.m.file, hit.m.line).await {
            hit.symbol_name = Some(node.name);
            hit.symbol_id = Some(node.id);
            hit.symbol_kind = Some(node.kind.as_str().to_string());
        }
        hits.push(hit);
    }

    let touched_files = unique_file_paths(hits.iter().map(|h| h.m.file.as_str()));
    let output_value = build_output_value(&hits, search.truncated, search.files_scanned);

    let text = render::finalize(Some(cg.project_root()), &args, &output_value, || {
        render_md(&hits, search.truncated, search.files_scanned)
    });
    Ok(ToolResult::new(
        json!({ "content": [{ "type": "text", "text": text }] }),
        touched_files,
    ))
}

/// A structural match plus its resolved enclosing graph symbol.
struct EnrichedHit {
    m: AstGrepSearchMatch,
    symbol_name: Option<String>,
    symbol_id: Option<String>,
    symbol_kind: Option<String>,
}

impl EnrichedHit {
    fn new(m: AstGrepSearchMatch) -> Self {
        Self {
            m,
            symbol_name: None,
            symbol_id: None,
            symbol_kind: None,
        }
    }
}

fn build_output_value(hits: &[EnrichedHit], truncated: bool, files_scanned: usize) -> Value {
    let items: Vec<Value> = hits
        .iter()
        .map(|hit| {
            let mut item = json!({
                "file": hit.m.file,
                "line": hit.m.line,
                "column": hit.m.column,
                "lang": hit.m.lang,
                "match": hit.m.matched_text,
                "line_text": hit.m.line_text,
            });
            if let Some(name) = &hit.symbol_name {
                item["symbol"] = json!(name);
            }
            if let Some(id) = &hit.symbol_id {
                item["node_id"] = json!(id);
            }
            if let Some(kind) = &hit.symbol_kind {
                item["kind"] = json!(kind);
            }
            item
        })
        .collect();

    json!({
        "results": items,
        "match_count": hits.len(),
        "files_scanned": files_scanned,
        "truncated": truncated,
    })
}

fn render_md(hits: &[EnrichedHit], truncated: bool, files_scanned: usize) -> String {
    let mut md = Md::new();
    md.heading(2, "Structural Search Results");
    if hits.is_empty() {
        md.empty_note("No structural matches.");
        md.line(&format!("_Scanned {files_scanned} files._"));
        return md.render();
    }

    for hit in hits {
        let location = match (&hit.symbol_name, &hit.symbol_kind) {
            (Some(name), Some(kind)) => {
                format!("{}:{} — **{name}** ({kind})", hit.m.file, hit.m.line)
            }
            _ => format!("{}:{}", hit.m.file, hit.m.line),
        };
        md.bullet(&location);
        md.line(&format!("  > {}", hit.m.matched_text));
        if let Some(id) = &hit.symbol_id {
            md.line(&format!(
                "  `{id}` · call `tracedecay_body` to read the symbol"
            ));
        }
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
            let _guard = CancelSearchOnDrop::new(cancelled.clone());
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
        )
        .await
        .expect("structural search");

        assert_eq!(result.matches.len(), 1);
    }
}
