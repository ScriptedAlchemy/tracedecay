//! Content-search tool handler: `tracedecay_grep`.
//!
//! Literal/regex search over the project working tree (respecting
//! `.gitignore`), graph-enriched: each hit resolves the enclosing symbol from
//! the code graph so the natural follow-up is `tracedecay_body`. This closes
//! the gap that made agents fall back to raw `rg` — `tracedecay_search` only
//! matches symbol *names*, not file *content*.

use std::fmt::Write as _;

use serde_json::{Value, json};
use tracedecay_code_index::grep_search::{GrepSearchHit, GrepSearchQuery, search_tree_with_cancel};

use crate::errors::{Result, TraceDecayError};
use crate::tracedecay::TraceDecay;

use super::super::ToolResult;
use super::super::render::{self, Md};
use super::support::{filter_by_scope, run_bounded_search, unique_file_paths};

/// Hard cap on `max_results` regardless of what the caller requests.
const MAX_RESULTS_CAP: usize = 200;
/// Default `max_results` when the caller omits it.
const DEFAULT_MAX_RESULTS: usize = 50;
/// Hard cap on `context_lines`.
const MAX_CONTEXT_LINES: usize = 3;
/// A single content-search hit, enriched with the enclosing graph symbol.
struct GrepHit {
    file: String,
    line: u32,
    text: String,
    before: Vec<String>,
    after: Vec<String>,
    symbol_name: Option<String>,
    symbol_id: Option<String>,
    symbol_kind: Option<String>,
}

impl From<GrepSearchHit> for GrepHit {
    fn from(hit: GrepSearchHit) -> Self {
        Self {
            file: hit.file,
            line: hit.line,
            text: hit.text,
            before: hit.before,
            after: hit.after,
            symbol_name: None,
            symbol_id: None,
            symbol_kind: None,
        }
    }
}

/// Handles `tracedecay_grep` tool calls.
pub(super) async fn handle_grep(
    cg: &TraceDecay,
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
    if pattern.is_empty() {
        return Err(TraceDecayError::Config {
            message: "pattern must not be empty".to_string(),
        });
    }

    let fixed_strings = args
        .get("fixed_strings")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let case_sensitive = args
        .get("case_sensitive")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let path_glob = args
        .get("path_glob")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let max_results = args
        .get("max_results")
        .and_then(Value::as_u64)
        .map_or(DEFAULT_MAX_RESULTS, |v| (v as usize).min(MAX_RESULTS_CAP))
        .max(1);
    let context_lines = args
        .get("context_lines")
        .and_then(Value::as_u64)
        .map_or(0, |v| (v as usize).min(MAX_CONTEXT_LINES));

    let project_root = cg.project_root().to_path_buf();
    let query = GrepSearchQuery {
        pattern: pattern.to_owned(),
        fixed_strings,
        case_sensitive,
        path_glob,
        context_lines,
        max_results,
    };
    let scan = run_bounded_search(
        "tracedecay_grep",
        pattern.to_owned(),
        deadline,
        cancellation,
        move |cancelled, transport_cancellation| {
            search_tree_with_cancel(&project_root, &query, || {
                cancelled.load(std::sync::atomic::Ordering::Acquire)
                    || transport_cancellation
                        .as_ref()
                        .is_some_and(|signal| signal.is_cancelled())
            })
        },
    )
    .await?;

    // Scope filtering mirrors `tracedecay_search`: when the client pins a
    // subtree, only hits under it are returned.
    let mut hits = filter_by_scope(
        scan.hits.into_iter().map(GrepHit::from).collect(),
        scope_prefix,
        |hit| hit.file.as_str(),
    );
    let truncated = scan.truncated || hits.len() > max_results;
    hits.truncate(max_results);

    // Enrich each hit with the smallest graph node that contains it.
    for hit in &mut hits {
        if let Ok(Some(node)) = cg.node_at_location(&hit.file, hit.line).await {
            hit.symbol_name = Some(node.name);
            hit.symbol_id = Some(node.id);
            hit.symbol_kind = Some(node.kind.as_str().to_string());
        }
    }

    let touched_files = unique_file_paths(hits.iter().map(|hit| hit.file.as_str()));
    let output_value = build_output_value(&hits, truncated, scan.files_scanned);

    let text = render::finalize(Some(cg.project_root()), &args, &output_value, || {
        render_grep_md(&hits, truncated, scan.files_scanned)
    });
    Ok(ToolResult::new(
        json!({ "content": [{ "type": "text", "text": text }] }),
        touched_files,
    ))
}

fn build_output_value(hits: &[GrepHit], truncated: bool, files_scanned: usize) -> Value {
    let items: Vec<Value> = hits
        .iter()
        .map(|hit| {
            let mut item = json!({
                "file": hit.file,
                "line": hit.line,
                "text": hit.text,
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
            if !hit.before.is_empty() {
                item["before"] = json!(hit.before);
            }
            if !hit.after.is_empty() {
                item["after"] = json!(hit.after);
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

fn render_grep_md(hits: &[GrepHit], truncated: bool, files_scanned: usize) -> String {
    let mut md = Md::new();
    md.heading(2, "Grep Results");
    if hits.is_empty() {
        md.empty_note("No matching lines.");
        md.line(&format!("_Scanned {files_scanned} files._"));
        return md.render();
    }

    for hit in hits {
        let location = match (&hit.symbol_name, &hit.symbol_kind) {
            (Some(name), Some(kind)) => {
                format!("{}:{} — **{name}** ({kind})", hit.file, hit.line)
            }
            _ => format!("{}:{}", hit.file, hit.line),
        };
        md.bullet(&location);
        for line in &hit.before {
            md.line(&format!("    {line}"));
        }
        md.line(&format!("  > {}", hit.text));
        for line in &hit.after {
            md.line(&format!("    {line}"));
        }
        if let Some(id) = &hit.symbol_id {
            md.line(&format!(
                "  `{id}` · call `tracedecay_body` to read the symbol"
            ));
        }
    }

    md.blank();
    let mut summary = format!("_{} matches across {files_scanned} files._", hits.len());
    if truncated {
        let _ = write!(
            summary,
            " Results capped — narrow with `path_glob` or a more specific pattern."
        );
    }
    md.line(&summary);
    md.render()
}
