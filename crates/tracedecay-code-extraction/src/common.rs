//! Helpers shared verbatim by multiple language extractors.
//!
//! Each extractor keeps its own private `ExtractionState`, so these helpers
//! take the individual pieces of state they need (source bytes, file path,
//! unresolved-ref sink) instead of the state struct itself. Bodies are moved
//! here unchanged from the per-language copies so extraction output stays
//! byte-identical.

use tree_sitter::Node as TsNode;

use crate::types::{EdgeKind, NodeKind, UnresolvedRef, generate_node_id, generate_node_id_at};

/// Gets the text of a tree-sitter node from the source.
fn node_text(source: &[u8], node: TsNode<'_>) -> String {
    node.utf8_text(source)
        .unwrap_or("<invalid utf8>")
        .to_string()
}

/// Mints the extraction-local ID for the construct rooted at `node`.
///
/// Every edge and unresolved reference an extractor emits while visiting the
/// construct is keyed by this ID, so two constructs must never share one. A
/// construct that begins its line (only blanks precede it) keeps the line-keyed
/// [`generate_node_id`], leaving indentation and one-construct lines
/// identity-neutral; a construct preceded by other source on its line also
/// carries its start column, which distinguishes `impl A { fn run() {} } impl
/// B { fn run() {} }` before relation endpoints are bound.
pub(crate) fn local_node_id(
    file_path: &str,
    source: &[u8],
    kind: &NodeKind,
    name: &str,
    node: TsNode<'_>,
) -> String {
    let start = node.start_position();
    let line = start.row as u32;
    let start_byte = node.start_byte().min(source.len());
    let line_start = source[..start_byte]
        .iter()
        .rposition(|byte| *byte == b'\n')
        .map_or(0, |newline| newline + 1);
    let begins_line = source[line_start..start_byte]
        .iter()
        .all(|byte| matches!(byte, b' ' | b'\t' | b'\r'));
    if begins_line {
        generate_node_id(file_path, kind, name, line)
    } else {
        generate_node_id_at(file_path, kind, name, line, start.column as u32)
    }
}

/// Strip comment markers from a single C-style comment text
/// (`//` line comments and `/* ... */` block comments).
pub(crate) fn clean_c_comment(comment: &str) -> String {
    clean_c_line_or_block_comment(comment, &["//"])
}

/// Strip comment markers from a single C-style comment text, including
/// `///` doc comments.
pub(crate) fn clean_c_doc_comment(comment: &str) -> String {
    // Longer prefixes first so `///` is not stripped as `//`.
    clean_c_line_or_block_comment(comment, &["///", "//"])
}

fn clean_c_line_or_block_comment(comment: &str, line_prefixes: &[&str]) -> String {
    let trimmed = comment.trim();
    for prefix in line_prefixes {
        if let Some(stripped) = trimmed.strip_prefix(prefix) {
            return stripped.strip_prefix(' ').unwrap_or(stripped).to_string();
        }
    }
    if trimmed.starts_with("/*") && trimmed.ends_with("*/") {
        let inner = &trimmed[2..trimmed.len() - 2];
        inner
            .lines()
            .map(|line| {
                let l = line.trim();
                l.strip_prefix("* ")
                    .or_else(|| l.strip_prefix('*'))
                    .unwrap_or(l)
            })
            .collect::<Vec<_>>()
            .join("\n")
            .trim()
            .to_string()
    } else {
        trimmed.to_string()
    }
}

/// Extract a docstring from the run of `comment` siblings immediately
/// preceding `node`, cleaning each comment with `clean`.
pub(crate) fn docstring_from_preceding_comments(
    source: &[u8],
    node: TsNode<'_>,
    clean: fn(&str) -> String,
) -> Option<String> {
    let mut comments = Vec::new();
    let mut current = node.prev_named_sibling();
    while let Some(sibling) = current {
        if sibling.kind() == "comment" {
            comments.push(node_text(source, sibling));
            current = sibling.prev_named_sibling();
        } else {
            break;
        }
    }
    if comments.is_empty() {
        return None;
    }
    // Comments are collected in reverse order (closest first).
    comments.reverse();
    let cleaned: Vec<String> = comments.iter().map(|c| clean(c)).collect();
    let result = cleaned.join("\n").trim().to_string();
    if result.is_empty() {
        None
    } else {
        Some(result)
    }
}

/// Extract a docstring from the run of `#` comment siblings immediately
/// preceding `node`.
#[cfg(any(feature = "lang-bash", feature = "lang-ruby"))]
pub(crate) fn docstring_from_hash_comments(source: &[u8], node: TsNode<'_>) -> Option<String> {
    let mut comments: Vec<String> = Vec::new();
    let mut prev = node.prev_named_sibling();
    while let Some(prev_node) = prev {
        if prev_node.kind() == "comment" {
            let text = node_text(source, prev_node);
            let stripped = text.trim_start_matches('#').trim().to_string();
            comments.push(stripped);
            prev = prev_node.prev_named_sibling();
        } else {
            break;
        }
    }
    if comments.is_empty() {
        return None;
    }
    // Comments were collected in reverse order; reverse them back.
    comments.reverse();
    Some(comments.join("\n"))
}

/// Recursively find `call_expression` nodes and create unresolved Calls
/// references, taking the callee name from the first named child.
pub(crate) fn extract_call_expression_sites(
    source: &[u8],
    file_path: &str,
    unresolved_refs: &mut Vec<UnresolvedRef>,
    node: TsNode<'_>,
    fn_node_id: &str,
) {
    let mut cursor = node.walk();
    if cursor.goto_first_child() {
        loop {
            let child = cursor.node();
            if child.kind() == "call_expression"
                && let Some(callee) = child.named_child(0)
            {
                let callee_name = node_text(source, callee);
                unresolved_refs.push(UnresolvedRef {
                    from_node_id: fn_node_id.to_string(),
                    reference_name: callee_name,
                    reference_kind: EdgeKind::Calls,
                    line: child.start_position().row as u32,
                    column: child.start_position().column as u32,
                    file_path: file_path.to_string(),
                });
            }
            extract_call_expression_sites(source, file_path, unresolved_refs, child, fn_node_id);
            if !cursor.goto_next_sibling() {
                break;
            }
        }
    }
}
