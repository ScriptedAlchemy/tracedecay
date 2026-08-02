//! `tracedecay_unsafe_patterns` — risky-construct scan over indexed source.

use super::*;

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

fn line_matches_unsafe_kind(line: &str, kind: &str) -> bool {
    let trimmed = line.trim_start();
    if trimmed.starts_with("//") || trimmed.starts_with("///") {
        return false;
    }
    match kind {
        "unwrap" => contains_method_call(line, "unwrap", true),
        "expect" => contains_method_call(line, "expect", false),
        "panic" => line.contains("panic!("),
        "todo" => line.contains("todo!("),
        "unimplemented" => line.contains("unimplemented!(") || line.contains("unimplemented!()"),
        "unsafe_block" => contains_unsafe_block_start(line),
        _ => false,
    }
}

fn contains_method_call(line: &str, method: &str, empty_parens: bool) -> bool {
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
                    return true;
                }
            } else {
                return true;
            }
        }
        start = abs + needle.len();
    }
    false
}

fn contains_unsafe_block_start(line: &str) -> bool {
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
                return true;
            }
        }
        start = abs + "unsafe".len();
    }
    false
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

pub(crate) async fn handle_unsafe_patterns(
    cg: &TraceDecay,
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

    let project_root = cg.project_root();
    let files = cg.get_all_files().await?;
    let mut matches: Vec<Value> = Vec::new();
    let mut by_kind: HashMap<String, u64> = HashMap::new();
    let mut touched: Vec<String> = Vec::new();

    'outer: for file in &files {
        if !path_matches_optional_scope(&file.path, path) {
            continue;
        }
        let in_test = path_looks_like_test(&file.path);
        if exclude_tests && in_test {
            continue;
        }
        let abs_path = project_root.join(&file.path);
        let Ok(source) = crate::sync::read_source_file(&abs_path) else {
            continue;
        };
        // Cheap raw pre-filter before the tree-sitter mask and the per-file
        // store read. Masking only blanks content, so a keyword absent from the
        // raw source cannot appear in the masked copy — skipping here is
        // equivalent, and spares most files in a repository two expensive
        // steps that could never produce a match.
        if !kinds
            .iter()
            .any(|kind| source_may_contain_unsafe_kind(&source, kind))
        {
            continue;
        }
        // Blank comments and string/char literals for Rust files so an
        // `unsafe`/`unwrap`/`panic!` mentioned inside a comment or string is not
        // reported as a real risk site. Detection runs on the masked copy;
        // the original line is kept for the emitted snippet. Non-Rust files are
        // scanned raw (the Rust grammar would mis-tokenise them).
        let masked = if path_is_rust(&file.path) {
            tracedecay_code_extraction::source_mask::masked_rust_source_with(
                &source,
                tracedecay_code_extraction::source_mask::MaskOptions::CODE_SCAN,
            )
        } else {
            source.clone()
        };
        // Masking can erase every raw hit (all of them in comments or string
        // literals), so the file's nodes are fetched only once a real match
        // survives.
        let mut nodes: Option<Vec<crate::types::Node>> = None;

        for (idx, (line, masked_line)) in source.lines().zip(masked.lines()).enumerate() {
            let line_no = (idx as u32) + 1;
            for kind in &kinds {
                if line_matches_unsafe_kind(masked_line, kind) {
                    let nodes = match nodes {
                        Some(ref nodes) => nodes,
                        None => nodes.insert(cg.get_nodes_by_file(&file.path).await?),
                    };
                    let enclosing = nodes
                        .iter()
                        .filter(|n| n.start_line <= line_no && line_no <= n.end_line)
                        .min_by_key(|n| n.end_line.saturating_sub(n.start_line))
                        .map(|n| n.qualified_name.clone());
                    *by_kind.entry(kind.clone()).or_insert(0) += 1;
                    matches.push(json!({
                        "kind": kind,
                        "file": file.path,
                        "line": line_no,
                        "snippet": line.trim(),
                        "enclosing": enclosing,
                        "in_test": in_test,
                    }));
                    if !touched.contains(&file.path) {
                        touched.push(file.path.clone());
                    }
                    if matches.len() >= limit {
                        break 'outer;
                    }
                }
            }
        }
    }

    let counts = serde_json::to_value(&by_kind).unwrap_or(json!({}));
    let payload = json!({
        "match_count": matches.len(),
        "by_kind": counts,
        "matches": matches,
    });
    Ok(rendered_tool_result(
        Some(cg.project_root()),
        &args,
        &payload,
        touched,
        || render::risky_patterns_md(&payload),
    ))
}

// ---------------------------------------------------------------------------
// tracedecay_diagnostics
// ---------------------------------------------------------------------------
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
                if line_matches_unsafe_kind(line, kind) {
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
        // An `unsafe { }` block living inside an otherwise-safe function — the
        // exact shape the audit fixture plants.
        assert!(line_matches_unsafe_kind(
            "    unsafe { *ptr as usize }",
            "unsafe_block"
        ));
        assert!(contains_unsafe_block_start("    unsafe { *ptr as usize }"));
    }

    #[test]
    fn detects_unsafe_fn_impl_and_trait() {
        assert!(line_matches_unsafe_kind(
            "pub unsafe fn raw(&self) {",
            "unsafe_block"
        ));
        assert!(line_matches_unsafe_kind(
            "unsafe impl Send for Foo {}",
            "unsafe_block"
        ));
        assert!(line_matches_unsafe_kind(
            "unsafe trait Zeroable {}",
            "unsafe_block"
        ));
    }

    #[test]
    fn ignores_safe_code_and_comments() {
        // Plain safe code has no unsafe markers.
        assert!(!line_matches_unsafe_kind(
            "let x = total as usize;",
            "unsafe_block"
        ));
        // The word appears only in a comment/doc line: not a real unsafe site.
        assert!(!line_matches_unsafe_kind(
            "// this is not unsafe { } really",
            "unsafe_block"
        ));
        assert!(!line_matches_unsafe_kind(
            "/// drop the needless unsafe block",
            "unsafe_block"
        ));
        // A substring of a longer identifier must not trip the word-boundary check.
        assert!(!contains_unsafe_block_start("let unsafely = 1;"));
        assert!(!contains_unsafe_block_start("let make_unsafe_thing = 2;"));
    }
}
