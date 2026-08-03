// Rust guideline compliant 2025-10-17
//! Mode-aware file reads for `tracedecay_read`.
//!
//! Four modes are implemented in 5.0:
//!
//! - `full` — verbatim file content (parity with the raw `Read` tool)
//! - `lines` — explicit line slice (`A-B`, 1-based, inclusive)
//! - `map` — flat list of every top-level symbol in the file, sourced from
//!   the code graph (cheap; no source bytes touched)
//! - `signatures` — `map` filtered to function/type kinds, with the cached
//!   `signature` column included
//!
//! Each function returns the rendered body as a `String`. Token-counting and
//! cache I/O happen one layer up, in the MCP handler.

use crate::db::Database;
use crate::errors::{Result, TraceDecayError};
use crate::types::{Node, NodeKind};
use serde_json::{Value, json};

pub use tracedecay_usecases::context::read_modes::{
    LineRange, ReadMode, estimate_tokens, render_full, render_lines,
};

const MAX_CONTEXT_SYMBOLS: usize = 12;

/// Renders the `map` mode body — JSON list of every top-level symbol in the
/// file, sourced from the graph. No source bytes are touched.
///
/// `kinds` is an optional case-insensitive filter on `NodeKind::as_str()`
/// values (e.g. `["function", "struct"]`). When `None` or empty, every kind
/// is included.
pub async fn render_map(db: &Database, file_path: &str, kinds: Option<&[String]>) -> Result<Value> {
    let nodes = fetch_nodes(db, file_path).await?;
    let entries: Vec<Value> = nodes
        .iter()
        .filter(|n| kind_matches_filter(&n.kind, kinds))
        .map(map_symbol_entry)
        .collect();
    Ok(json!({
        "file": file_path,
        "symbol_count": entries.len(),
        "symbols": entries,
    }))
}

/// Renders the `signatures` mode body — `map` filtered to function/type kinds
/// with the cached `signature` string. Skips items without a signature so the
/// result stays compact.
pub async fn render_signatures(db: &Database, file_path: &str) -> Result<Value> {
    let nodes = fetch_nodes(db, file_path).await?;
    let entries: Vec<Value> = nodes
        .iter()
        .filter(|n| is_signature_kind(&n.kind))
        .filter_map(signature_symbol_entry)
        .collect();
    Ok(json!({
        "file": file_path,
        "signature_count": entries.len(),
        "signatures": entries,
    }))
}

/// Renders graph context for source reads. For full-file reads, this is a
/// compact signature overview; for line reads, it is the overlapping symbols.
pub async fn render_symbol_context(
    db: &Database,
    file_path: &str,
    range: Option<LineRange>,
) -> Result<Value> {
    let nodes = fetch_nodes(db, file_path).await?;
    let mut entries = Vec::new();
    let mut symbol_count = 0usize;

    for node in nodes
        .iter()
        .filter(|node| is_signature_kind(&node.kind))
        .filter(|node| range.is_none_or(|range| symbol_overlaps_range(node, range)))
        .filter_map(context_symbol_entry)
    {
        symbol_count += 1;
        if entries.len() < MAX_CONTEXT_SYMBOLS {
            entries.push(node);
        }
    }

    Ok(json!({
        "file": file_path,
        "range": range.map(|range| json!({
            "start": range.start,
            "end": range.end,
        })),
        "symbol_count": symbol_count,
        "truncated": symbol_count > entries.len(),
        "symbols": entries,
    }))
}

async fn fetch_nodes(db: &Database, file_path: &str) -> Result<Vec<Node>> {
    db.get_nodes_by_file(file_path)
        .await
        .map_err(|e| TraceDecayError::Database {
            message: format!("read_modes: failed to load nodes for {file_path}: {e}"),
            operation: "read_modes::fetch_nodes".to_string(),
        })
}

fn kind_matches_filter(kind: &NodeKind, kinds: Option<&[String]>) -> bool {
    let Some(filter) = kinds.filter(|k| !k.is_empty()) else {
        return true;
    };
    let kind = kind.as_str();
    filter.iter().any(|want| want.eq_ignore_ascii_case(kind))
}

fn map_symbol_entry(node: &Node) -> Value {
    json!({
        "kind": node.kind.as_str(),
        "name": node.name,
        "line": node.start_line,
        "end_line": node.end_line,
        "visibility": node.visibility.as_str(),
    })
}

fn signature_symbol_entry(node: &Node) -> Option<Value> {
    let signature = node.signature.as_deref()?;
    Some(json!({
        "kind": node.kind.as_str(),
        "name": node.name,
        "qualified_name": node.qualified_name,
        "line": node.start_line,
        "end_line": node.end_line,
        "visibility": node.visibility.as_str(),
        "signature": signature,
        "is_async": node.is_async,
    }))
}

fn context_symbol_entry(node: &Node) -> Option<Value> {
    let signature = node.signature.as_deref()?;
    let (line, end_line) = node_user_line_span(node);
    Some(json!({
        "kind": node.kind.as_str(),
        "name": node.name,
        "qualified_name": node.qualified_name,
        "line": line,
        "end_line": end_line,
        "visibility": node.visibility.as_str(),
        "signature": signature,
        "is_async": node.is_async,
    }))
}

fn symbol_overlaps_range(node: &Node, range: LineRange) -> bool {
    let (start, end) = node_user_line_span(node);
    start <= range.end && end >= range.start
}

fn node_user_line_span(node: &Node) -> (u32, u32) {
    (
        node.start_line.saturating_add(1),
        node.end_line.saturating_add(1),
    )
}

/// Kinds whose `signature` column carries useful information for the
/// `signatures` mode. Excludes plain identifiers, modules, and string-literal
/// nodes whose "signature" would be redundant with the name.
fn is_signature_kind(kind: &NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::Function
            | NodeKind::Method
            | NodeKind::Struct
            | NodeKind::Trait
            | NodeKind::Interface
            | NodeKind::Enum
            | NodeKind::Class
            | NodeKind::TypeAlias
            | NodeKind::Const
            | NodeKind::Static
    )
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::types::Visibility;

    fn node_fixture(kind: NodeKind, signature: Option<&str>) -> Node {
        Node {
            id: "node-1".to_string(),
            kind,
            name: "sample".to_string(),
            qualified_name: "crate::sample".to_string(),
            file_path: "src/sample.rs".to_string(),
            start_line: 12,
            attrs_start_line: 10,
            end_line: 18,
            start_column: 4,
            end_column: 1,
            signature: signature.map(str::to_string),
            docstring: Some("sample docs".to_string()),
            visibility: Visibility::Pub,
            is_async: true,
            branches: 1,
            loops: 2,
            returns: 3,
            max_nesting: 4,
            unsafe_blocks: 5,
            unchecked_calls: 6,
            assertions: 7,
            updated_at: 8,
            parent_id: Some("parent-1".to_string()),
        }
    }

    #[test]
    fn kind_filter_is_empty_or_case_insensitive() {
        assert!(kind_matches_filter(&NodeKind::Function, None));
        assert!(kind_matches_filter(&NodeKind::Function, Some(&[])));
        assert!(kind_matches_filter(
            &NodeKind::Function,
            Some(&["FUNCTION".to_string()])
        ));
        assert!(!kind_matches_filter(
            &NodeKind::Function,
            Some(&["struct".to_string()])
        ));
    }

    #[test]
    fn map_symbol_entry_preserves_outline_schema() {
        let node = node_fixture(NodeKind::Function, Some("pub async fn sample()"));

        assert_eq!(
            map_symbol_entry(&node),
            json!({
                "kind": "function",
                "name": "sample",
                "line": 12,
                "end_line": 18,
                "visibility": "public",
            })
        );
    }

    #[test]
    fn signature_symbol_entry_preserves_signature_schema() {
        let node = node_fixture(NodeKind::Function, Some("pub async fn sample()"));

        assert_eq!(
            signature_symbol_entry(&node),
            Some(json!({
                "kind": "function",
                "name": "sample",
                "qualified_name": "crate::sample",
                "line": 12,
                "end_line": 18,
                "visibility": "public",
                "signature": "pub async fn sample()",
                "is_async": true,
            }))
        );
    }

    #[test]
    fn signature_symbol_entry_skips_missing_signature() {
        let node = node_fixture(NodeKind::Function, None);

        assert_eq!(signature_symbol_entry(&node), None);
    }

    #[test]
    fn context_symbol_entry_uses_user_facing_line_numbers() {
        let node = node_fixture(NodeKind::Function, Some("pub async fn sample()"));

        assert_eq!(
            context_symbol_entry(&node),
            Some(json!({
                "kind": "function",
                "name": "sample",
                "qualified_name": "crate::sample",
                "line": 13,
                "end_line": 19,
                "visibility": "public",
                "signature": "pub async fn sample()",
                "is_async": true,
            }))
        );
    }

    #[test]
    fn symbol_overlap_detects_enclosing_ranges() {
        let node = node_fixture(NodeKind::Function, Some("pub async fn sample()"));

        assert!(symbol_overlaps_range(
            &node,
            LineRange { start: 14, end: 16 }
        ));
        assert!(symbol_overlaps_range(
            &node,
            LineRange { start: 1, end: 13 }
        ));
        assert!(symbol_overlaps_range(
            &node,
            LineRange { start: 19, end: 30 }
        ));
        assert!(!symbol_overlaps_range(
            &node,
            LineRange { start: 1, end: 12 }
        ));
        assert!(!symbol_overlaps_range(
            &node,
            LineRange { start: 20, end: 30 }
        ));
    }

    #[test]
    fn symbol_overlap_handles_single_line_boundaries() {
        let mut node = node_fixture(NodeKind::Function, Some("pub fn sample()"));
        node.start_line = 12;
        node.end_line = 12;

        assert!(symbol_overlaps_range(
            &node,
            LineRange { start: 13, end: 13 }
        ));
        assert!(!symbol_overlaps_range(
            &node,
            LineRange { start: 12, end: 12 }
        ));
        assert!(!symbol_overlaps_range(
            &node,
            LineRange { start: 14, end: 14 }
        ));
    }
}
