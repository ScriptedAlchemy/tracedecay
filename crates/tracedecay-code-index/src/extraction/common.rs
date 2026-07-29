//! Helpers shared verbatim by multiple language extractors.
//!
//! Each extractor keeps its own private `ExtractionState`, so these helpers
//! take the individual pieces of state they need (source bytes, file path,
//! unresolved-ref sink) instead of the state struct itself. Bodies are moved
//! here unchanged from the per-language copies so extraction output stays
//! byte-identical.

use tree_sitter::Node as TsNode;

use crate::types::{EdgeKind, UnresolvedRef};

/// Gets the text of a tree-sitter node from the source.
fn node_text(source: &[u8], node: TsNode<'_>) -> String {
    node.utf8_text(source)
        .unwrap_or("<invalid utf8>")
        .to_string()
}

/// Strip comment markers from a single C-style comment text
/// (`//` line comments and `/* ... */` block comments).
#[allow(dead_code)]
pub(crate) fn clean_c_comment(comment: &str) -> String {
    let trimmed = comment.trim();
    if let Some(stripped) = trimmed.strip_prefix("//") {
        stripped.strip_prefix(' ').unwrap_or(stripped).to_string()
    } else if trimmed.starts_with("/*") && trimmed.ends_with("*/") {
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

/// Strip comment markers from a single C-style comment text, including
/// `///` doc comments.
#[allow(dead_code)]
pub(crate) fn clean_c_doc_comment(comment: &str) -> String {
    let trimmed = comment.trim();
    if let Some(stripped) = trimmed.strip_prefix("///") {
        stripped.strip_prefix(' ').unwrap_or(stripped).to_string()
    } else if let Some(stripped) = trimmed.strip_prefix("//") {
        stripped.strip_prefix(' ').unwrap_or(stripped).to_string()
    } else if trimmed.starts_with("/*") && trimmed.ends_with("*/") {
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
#[allow(dead_code)]
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
#[allow(dead_code)]
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
#[allow(dead_code)]
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
            if child.kind() == "call_expression" {
                // Get the callee: the first named child (usually an identifier)
                if let Some(callee) = child.named_child(0) {
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
            }
            // Recurse for nested calls.
            extract_call_expression_sites(source, file_path, unresolved_refs, child, fn_node_id);
            if !cursor.goto_next_sibling() {
                break;
            }
        }
    }
}
