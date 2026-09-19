//! `tracedecay_unsafe_patterns`, risky-construct scan over indexed source.

use std::collections::HashMap;
use std::path::Path;

use serde_json::{Value, json};
use tracedecay_domain::errors::{Result, TraceDecayError};
use tracedecay_graph_query::VerifiedGraphQuery;

use super::{
    VerifiedAnalysisSymbol, path_is_rust, verified_analysis_symbols, verified_analysis_unavailable,
};
use crate::ToolResult;
use crate::handlers::support::{effective_path, rendered_tool_result};
use crate::tools::render;

const UNSAFE_KINDS: &[&str] = &[
    "unwrap",
    "expect",
    "panic",
    "todo",
    "unimplemented",
    "unsafe_block",
];

/// Whether `source` could possibly contain a site of `kind`.
///
/// Deliberately over-approximates: it only has to be true whenever
/// [`line_matches_unsafe_kind`] could be true for some line, so the real
/// per-line matcher stays the single source of truth for what counts.
fn source_may_contain_unsafe_kind(source: &str, kind: &str) -> bool {
    match kind {
        "unwrap" => source.contains(".unwrap"),
        "expect" => source.contains(".expect"),
        "panic" => source.contains("panic!("),
        "todo" => source.contains("todo!("),
        "unimplemented" => source.contains("unimplemented!("),
        "unsafe_block" => source.contains("unsafe"),
        // An unrecognised kind never matches a line either.
        _ => false,
    }
}

/// Byte offset of the risky construct within `line`, when the line has one.
///
/// The offset is what lets a match be attributed to the declaration that
/// actually contains it: two declarations can share a line, so a line number
/// alone cannot say which one a site belongs to.
fn line_matches_unsafe_kind(line: &str, kind: &str) -> Option<usize> {
    let trimmed = line.trim_start();
    if trimmed.starts_with("//") || trimmed.starts_with("///") {
        return None;
    }
    match kind {
        "unwrap" => contains_method_call(line, "unwrap", true),
        "expect" => contains_method_call(line, "expect", false),
        "panic" => line.find("panic!("),
        "todo" => line.find("todo!("),
        "unimplemented" => line.find("unimplemented!("),
        "unsafe_block" => contains_unsafe_block_start(line),
        _ => None,
    }
}

fn contains_method_call(line: &str, method: &str, empty_parens: bool) -> Option<usize> {
    let needle = format!(".{method}");
    let bytes = line.as_bytes();
    let mut start = 0usize;
    while let Some(pos) = line[start..].find(&needle) {
        let abs = start + pos;
        let after = abs + needle.len();
        let next = bytes.get(after).copied();
        let is_word_boundary = !matches!(next, Some(c) if c.is_ascii_alphanumeric() || c == b'_');
        if is_word_boundary && next == Some(b'(') {
            if empty_parens {
                if line[after + 1..].trim_start().starts_with(')') {
                    return Some(abs);
                }
            } else {
                return Some(abs);
            }
        }
        start = abs + needle.len();
    }
    None
}

fn contains_unsafe_block_start(line: &str) -> Option<usize> {
    let bytes = line.as_bytes();
    let mut start = 0usize;
    while let Some(pos) = line[start..].find("unsafe") {
        let abs = start + pos;
        let prev_ok =
            abs == 0 || !matches!(bytes[abs - 1], b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'_');
        let after = abs + "unsafe".len();
        let next = bytes.get(after).copied();
        let next_ok = matches!(next, Some(b' ' | b'\t' | b'{'));
        if prev_ok && next_ok {
            let rest = line[after..].trim_start();
            if rest.starts_with('{')
                || rest.starts_with("fn ")
                || rest.starts_with("impl ")
                || rest.starts_with("trait ")
            {
                return Some(abs);
            }
        }
        start = abs + "unsafe".len();
    }
    None
}

/// Innermost declaration whose source range covers `match_byte`.
///
/// Byte containment, not line containment: an attribute such as `#[test]` and
/// the two functions on `#[test] fn a() {…} fn b() {…}` all sit on one line,
/// and only the byte range says which of them the site is inside. Selecting by
/// line also had no stable order to break ties with, since symbols arrive in
/// occurrence order and occurrence ids are per-project digests.
fn enclosing_declaration(nodes: &[VerifiedAnalysisSymbol], match_byte: u64) -> Option<String> {
    nodes
        .iter()
        .filter_map(|node| node.source_span.map(|span| (node, span)))
        .filter(|(_, span)| span.start_byte <= match_byte && match_byte < span.end_byte)
        .min_by_key(|(_, span)| span.end_byte.saturating_sub(span.start_byte))
        .map(|(node, _)| node.metadata.qualified_name.clone())
}

fn path_looks_like_test(path: &str) -> bool {
    path.starts_with("tests/")
        || path.contains("/tests/")
        || path.ends_with("_test.rs")
        || path.ends_with("_tests.rs")
        || path.ends_with("_test.go")
        || path.contains("/__tests__/")
        || path.ends_with(".test.ts")
        || path.ends_with(".test.tsx")
        || path.ends_with(".test.js")
        || path.ends_with("_test.py")
        || path.ends_with("Test.java")
}

#[hotpath::measure(future = true, label = "mcp.analysis.unsafe_patterns.total")]
pub async fn handle_unsafe_patterns(
    project_root: &Path,
    graph: &VerifiedGraphQuery,
    args: Value,
    scope_prefix: Option<&str>,
) -> Result<ToolResult> {
    let kinds: Vec<String> = args
        .get("kinds")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect::<Vec<_>>()
        })
        .filter(|v: &Vec<String>| !v.is_empty())
        .unwrap_or_else(|| UNSAFE_KINDS.iter().map(|s| (*s).to_string()).collect());

    let path = effective_path(&args, scope_prefix);
    let exclude_tests = args
        .get("exclude_tests")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    let limit = args
        .get("limit")
        .and_then(serde_json::Value::as_u64)
        .map_or(200, |v| v.min(2000) as usize);

    let symbols_by_file = hotpath::measure_block!("mcp.analysis.unsafe_patterns.graph", {
        let mut symbols_by_file = HashMap::<String, Vec<VerifiedAnalysisSymbol>>::new();
        for symbol in verified_analysis_symbols(graph, path)? {
            symbols_by_file
                .entry(symbol.path.clone())
                .or_default()
                .push(symbol);
        }
        symbols_by_file
    });
    // Graph phase is done. The source walk reads and masks candidate files, so
    // it belongs on a blocking worker like the sibling analysis scans.
    let scan_project_root = project_root.to_path_buf();
    let (matches, by_kind, touched) = hotpath::future!(
        tokio::task::spawn_blocking(move || -> Result<_> {
            let mut files = symbols_by_file.keys().cloned().collect::<Vec<_>>();
            files.sort();
            let mut matches: Vec<Value> = Vec::new();
            let mut by_kind: HashMap<String, u64> = HashMap::new();
            let mut touched: Vec<String> = Vec::new();

            'outer: for file in &files {
                let test_file = path_looks_like_test(file);
                if exclude_tests && test_file {
                    continue;
                }
                let abs_path = scan_project_root.join(file);
                let Ok(source) = tracedecay_runtime_core::sync::read_source_file(&abs_path) else {
                    continue;
                };
                // Cheap raw pre-filter before the tree-sitter mask and the per-file
                // store read. Masking only blanks content, so a keyword absent from
                // the raw source cannot appear in the masked copy, skipping here
                // is equivalent, and spares most files in a repository two
                // expensive steps that could never produce a match.
                if !kinds
                    .iter()
                    .any(|kind| source_may_contain_unsafe_kind(&source, kind))
                {
                    continue;
                }
                // Blank comments and string/char literals for Rust files so an
                // `unsafe`/`unwrap`/`panic!` mentioned inside a comment or string
                // is not reported as a real risk site. Detection runs on the
                // masked copy; the original line is kept for the emitted snippet.
                // Non-Rust files are scanned raw (the Rust grammar would
                // mis-tokenise them).
                let masked_owned = path_is_rust(file).then(|| {
                    tracedecay_code_extraction::source_mask::masked_rust_source_with(
                        &source,
                        tracedecay_code_extraction::source_mask::MaskOptions::CODE_SCAN,
                    )
                });
                let masked = masked_owned.as_deref().unwrap_or(&source);
                let test_lines = if path_is_rust(file) {
                    tracedecay_code_extraction::source_mask::rust_test_lines(&source).map_err(
                        |error| {
                            verified_analysis_unavailable(
                                "unsafe-pattern-test-scope",
                                &format!("failed to classify Rust test scopes in {file}: {error}"),
                            )
                        },
                    )?
                } else {
                    Vec::new()
                };
                // Masking can erase every raw hit (all of them in comments or
                // string literals), so the file's nodes are fetched only once a
                // real match survives.
                // Split inclusively so each line keeps its own byte offset;
                // masking preserves byte layout, so the two sides stay aligned.
                let mut line_start = 0usize;
                for (idx, (line, masked_line)) in source
                    .split_inclusive('\n')
                    .zip(masked.split_inclusive('\n'))
                    .enumerate()
                {
                    let line_offset = line_start;
                    line_start += line.len();
                    let line_no = (idx as u32) + 1;
                    // A mixed test/production line is not wholly test scope,
                    // so keep its production risk visible.
                    let in_test = test_file || test_lines.get(idx).copied().unwrap_or(false);
                    if exclude_tests && in_test {
                        continue;
                    }
                    for kind in &kinds {
                        if let Some(column) = line_matches_unsafe_kind(masked_line, kind) {
                            let nodes = symbols_by_file.get(file).map_or(&[][..], Vec::as_slice);
                            let enclosing =
                                enclosing_declaration(nodes, (line_offset + column) as u64);
                            *by_kind.entry(kind.clone()).or_insert(0) += 1;
                            matches.push(json!({
                                "kind": kind,
                                "file": file,
                                "line": line_no,
                                "snippet": line.trim(),
                                "enclosing": enclosing,
                                "in_test": in_test,
                            }));
                            if !touched.contains(file) {
                                touched.push(file.clone());
                            }
                            if matches.len() >= limit {
                                break 'outer;
                            }
                        }
                    }
                }
            }
            Ok((matches, by_kind, touched))
        }),
        label = "mcp.analysis.unsafe_patterns.scan"
    )
    .await
    .map_err(|join_error| TraceDecayError::Config {
        message: format!("tracedecay_unsafe_patterns scan failed to join: {join_error}"),
    })??;

    let payload = hotpath::measure_block!("mcp.analysis.unsafe_patterns.assemble", {
        let counts = serde_json::to_value(&by_kind).map_err(|error| TraceDecayError::Config {
            message: format!("failed to serialize unsafe-pattern counts: {error}"),
        })?;
        json!({
            "match_count": matches.len(),
            "by_kind": counts,
            "matches": matches,
        })
    });
    Ok(rendered_tool_result(
        Some(project_root),
        &args,
        &payload,
        touched,
        || render::risky_patterns_md(&payload),
    ))
}

#[cfg(test)]
mod unsafe_pattern_detection_tests {
    use super::{
        contains_unsafe_block_start, line_matches_unsafe_kind, source_may_contain_unsafe_kind,
    };

    /// The whole-file pre-filter exists only to skip work, so it must never be
    /// narrower than the per-line matcher: any line the matcher accepts has to
    /// keep its file in the scan.
    #[test]
    fn prefilter_never_excludes_a_line_the_matcher_accepts() {
        let lines = [
            "    let x = value.unwrap();",
            "    let x = value.expect(\"why\");",
            "    panic!(\"boom\");",
            "    todo!();",
            "    unimplemented!();",
            "    unsafe { *ptr as usize }",
            "    let y = 1 + 1;",
            "",
        ];
        let kinds = [
            "unwrap",
            "expect",
            "panic",
            "todo",
            "unimplemented",
            "unsafe_block",
        ];

        for line in lines {
            for kind in kinds {
                if line_matches_unsafe_kind(line, kind).is_some() {
                    assert!(
                        source_may_contain_unsafe_kind(line, kind),
                        "prefilter would drop a real {kind} site: {line:?}"
                    );
                }
            }
        }
    }

    /// An unrecognised kind matches nothing, so it must not force a scan.
    #[test]
    fn prefilter_rejects_unknown_kinds() {
        assert!(!source_may_contain_unsafe_kind(
            "let x = value.unwrap();",
            "not_a_kind"
        ));
    }

    #[test]
    fn detects_unsafe_block_inside_safe_fn() {
        // An `unsafe { }` block living inside an otherwise-safe function, the
        // exact shape the audit fixture plants.
        assert!(line_matches_unsafe_kind("    unsafe { *ptr as usize }", "unsafe_block").is_some());
        assert!(contains_unsafe_block_start("    unsafe { *ptr as usize }").is_some());
    }

    #[test]
    fn detects_unsafe_fn_impl_and_trait() {
        assert!(line_matches_unsafe_kind("pub unsafe fn raw(&self) {", "unsafe_block").is_some());
        assert!(line_matches_unsafe_kind("unsafe impl Send for Foo {}", "unsafe_block").is_some());
        assert!(line_matches_unsafe_kind("unsafe trait Zeroable {}", "unsafe_block").is_some());
    }

    #[test]
    fn ignores_safe_code_and_comments() {
        // Plain safe code has no unsafe markers.
        assert!(line_matches_unsafe_kind("let x = total as usize;", "unsafe_block").is_none());
        // The word appears only in a comment/doc line: not a real unsafe site.
        assert!(
            line_matches_unsafe_kind("// this is not unsafe { } really", "unsafe_block").is_none()
        );
        assert!(
            line_matches_unsafe_kind("/// drop the needless unsafe block", "unsafe_block")
                .is_none()
        );
        // A substring of a longer identifier must not trip the word-boundary check.
        assert!(contains_unsafe_block_start("let unsafely = 1;").is_none());
        assert!(contains_unsafe_block_start("let make_unsafe_thing = 2;").is_none());
    }

    /// The reported offset is what attributes a site to a declaration, so it
    /// has to point at the construct itself, not at the start of the line.
    #[test]
    fn reports_where_on_the_line_the_site_is() {
        let line = "#[test] fn a() { Some(5).unwrap(); } pub fn b() { panic!(); }";
        assert_eq!(
            line_matches_unsafe_kind(line, "unwrap"),
            Some(line.find(".unwrap()").expect("unwrap call"))
        );
        assert_eq!(
            line_matches_unsafe_kind(line, "panic"),
            Some(line.find("panic!(").expect("panic call"))
        );
    }
}
