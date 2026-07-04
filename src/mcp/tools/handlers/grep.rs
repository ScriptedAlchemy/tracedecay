//! Content-search tool handler: `tracedecay_grep`.
//!
//! Literal/regex search over the project working tree (respecting
//! `.gitignore`), graph-enriched: each hit resolves the enclosing symbol from
//! the code graph so the natural follow-up is `tracedecay_body`. This closes
//! the gap that made agents fall back to raw `rg` — `tracedecay_search` only
//! matches symbol *names*, not file *content*.

use std::fmt::Write as _;
use std::path::Path;

use ignore::overrides::{Override, OverrideBuilder};
use ignore::WalkBuilder;
use regex::{Regex, RegexBuilder};
use serde_json::{json, Value};

use crate::errors::{Result, TraceDecayError};
use crate::tracedecay::TraceDecay;

use super::super::render::{self, Md};
use super::super::ToolResult;
use super::support::{filter_by_scope, unique_file_paths};

/// Hard cap on `max_results` regardless of what the caller requests.
const MAX_RESULTS_CAP: usize = 200;
/// Default `max_results` when the caller omits it.
const DEFAULT_MAX_RESULTS: usize = 50;
/// Hard cap on `context_lines`.
const MAX_CONTEXT_LINES: usize = 3;
/// Per-file hit cap, so one noisy file cannot crowd out the rest of the tree.
const MAX_HITS_PER_FILE: usize = 20;
/// Bytes sniffed from the head of each file to classify it as binary.
const BINARY_SNIFF_BYTES: usize = 8_192;
/// Skip individual lines longer than this (minified bundles, embedded blobs).
const MAX_LINE_BYTES: usize = 4_096;

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

/// Handles `tracedecay_grep` tool calls.
pub(super) async fn handle_grep(
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
    let path_glob = args.get("path_glob").and_then(Value::as_str);
    let max_results = args
        .get("max_results")
        .and_then(Value::as_u64)
        .map_or(DEFAULT_MAX_RESULTS, |v| (v as usize).min(MAX_RESULTS_CAP))
        .max(1);
    let context_lines = args
        .get("context_lines")
        .and_then(Value::as_u64)
        .map_or(0, |v| (v as usize).min(MAX_CONTEXT_LINES));

    let matcher = build_matcher(pattern, fixed_strings, case_sensitive)?;

    let project_root = cg.project_root().to_path_buf();

    // Optional path filter. A caller-supplied glob whitelists candidate files
    // via the `ignore` crate's override mechanism (same glob semantics as a
    // `.gitignore` line), so it prunes at the walker level.
    let overrides = match path_glob {
        Some(raw) if !raw.trim().is_empty() => {
            let mut builder = OverrideBuilder::new(&project_root);
            builder.add(raw).map_err(|err| TraceDecayError::Config {
                message: format!("invalid path_glob '{raw}': {err}"),
            })?;
            Some(builder.build().map_err(|err| TraceDecayError::Config {
                message: format!("invalid path_glob '{raw}': {err}"),
            })?)
        }
        _ => None,
    };

    // Collect one extra past the cap so we can honestly report truncation.
    let scan = scan_tree(
        &project_root,
        &matcher,
        overrides,
        context_lines,
        max_results,
    );

    // Scope filtering mirrors `tracedecay_search`: when the client pins a
    // subtree, only hits under it are returned.
    let mut hits = filter_by_scope(scan.hits, scope_prefix, |hit| hit.file.as_str());
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

/// Builds the line matcher. Fixed-string search escapes the pattern so regex
/// metacharacters are treated literally; case-insensitivity is the default.
fn build_matcher(pattern: &str, fixed_strings: bool, case_sensitive: bool) -> Result<Regex> {
    let source = if fixed_strings {
        regex::escape(pattern)
    } else {
        pattern.to_string()
    };
    RegexBuilder::new(&source)
        .case_insensitive(!case_sensitive)
        .build()
        .map_err(|err| TraceDecayError::Config {
            message: format!("invalid regex pattern '{pattern}': {err}"),
        })
}

struct ScanResult {
    hits: Vec<GrepHit>,
    files_scanned: usize,
    truncated: bool,
}

/// Walks the working tree respecting `.gitignore`, skipping binary files, and
/// collects matching lines. Stops early once `max_results` + 1 hits are found
/// so the caller can report truncation without scanning the whole tree.
fn scan_tree(
    project_root: &Path,
    matcher: &Regex,
    overrides: Option<Override>,
    context_lines: usize,
    max_results: usize,
) -> ScanResult {
    let mut builder = WalkBuilder::new(project_root);
    builder
        .follow_links(false)
        .hidden(false)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .add_custom_ignore_filename(".gitignore");
    if let Some(overrides) = overrides {
        builder.overrides(overrides);
    }
    let walker = builder.build();

    let mut hits: Vec<GrepHit> = Vec::new();
    let mut files_scanned = 0usize;
    let mut truncated = false;

    for entry in walker {
        let Ok(entry) = entry else { continue };
        let Some(ft) = entry.file_type() else {
            continue;
        };
        if !ft.is_file() {
            continue;
        }
        let path = entry.path();
        let Ok(rel) = path.strip_prefix(project_root) else {
            continue;
        };
        let rel_str = rel.to_string_lossy().replace('\\', "/");

        let Ok(bytes) = std::fs::read(path) else {
            continue;
        };
        if looks_binary(&bytes) {
            continue;
        }
        let Ok(content) = String::from_utf8(bytes) else {
            continue;
        };
        files_scanned += 1;

        let lines: Vec<&str> = content.lines().collect();
        let mut file_hits = 0usize;
        for (idx, line) in lines.iter().enumerate() {
            if line.len() > MAX_LINE_BYTES {
                continue;
            }
            if !matcher.is_match(line) {
                continue;
            }
            if file_hits >= MAX_HITS_PER_FILE {
                truncated = true;
                break;
            }
            file_hits += 1;

            let before = context_slice(&lines, idx.saturating_sub(context_lines), idx);
            let after = context_slice(&lines, idx + 1, (idx + 1 + context_lines).min(lines.len()));
            hits.push(GrepHit {
                file: rel_str.clone(),
                line: (idx as u32) + 1,
                text: (*line).to_string(),
                before,
                after,
                symbol_name: None,
                symbol_id: None,
                symbol_kind: None,
            });

            // Collect one past the cap so truncation is honest without paying
            // for a full-tree scan on high-frequency patterns.
            if hits.len() > max_results {
                truncated = true;
                return ScanResult {
                    hits,
                    files_scanned,
                    truncated,
                };
            }
        }
    }

    ScanResult {
        hits,
        files_scanned,
        truncated,
    }
}

fn context_slice(lines: &[&str], start: usize, end: usize) -> Vec<String> {
    if start >= end {
        return Vec::new();
    }
    lines[start..end].iter().map(|l| (*l).to_string()).collect()
}

/// Classifies a byte buffer as binary when a NUL byte appears in the head.
/// This is the same heuristic `git` and `ripgrep` use for text detection.
fn looks_binary(bytes: &[u8]) -> bool {
    let head = &bytes[..bytes.len().min(BINARY_SNIFF_BYTES)];
    head.contains(&0)
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
