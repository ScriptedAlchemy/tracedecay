//! Tree-sitter based JSON extractor.
//!
//! The grammar's root is `document`, whose value is normally one `object`.
//! Each top-level `pair` becomes a `Const` node named by its key, parented to
//! the file, mirroring how the TOML extractor exposes `Cargo.toml` pairs.
//!
//! A pair's signature is its full source text when that text is bounded, so
//! `package.json` (`name`, `exports`, `main`) and `tsconfig.json`
//! (`compilerOptions`, `extends`) stay readable to the TypeScript cross-file
//! resolver from the sealed file set alone. An oversized value (a lockfile's
//! `packages` table) keeps only its first line: the graph row is still a
//! symbol, but the seal never stores a second copy of the file in a signature.
use std::time::Instant;

use tree_sitter::{Node as TsNode, Tree};

use crate::common::local_node_id;
use crate::types::{
    ComplexityAnalysisV1, Edge, EdgeKind, ExtractionResult, Node, NodeKind, Visibility,
    generate_node_id,
};

/// Largest pair whose whole text is kept as its signature.
pub const MAX_JSON_PAIR_SIGNATURE_BYTES_V1: usize = 16 * 1024;

pub struct JsonExtractor;

struct ExtractionState<'s> {
    nodes: Vec<Node>,
    edges: Vec<Edge>,
    file_path: String,
    source: &'s [u8],
    file_node_id: String,
    timestamp: u64,
}

impl<'s> ExtractionState<'s> {
    fn new(file_path: &str, source: &'s str) -> Self {
        let timestamp = crate::common::unix_timestamp_secs();
        let file_node_id = generate_node_id(file_path, &NodeKind::File, file_path, 0);
        Self {
            nodes: Vec::new(),
            edges: Vec::new(),
            file_path: file_path.to_string(),
            source: source.as_bytes(),
            file_node_id,
            timestamp,
        }
    }

    fn node_text(&self, node: TsNode<'_>) -> &'s str {
        node.utf8_text(self.source).unwrap_or("<invalid utf8>")
    }
}

impl JsonExtractor {
    fn extract_tree(
        file_path: &str,
        source: &str,
        tree: &Tree,
        scope: crate::parsed_extraction::ParsedExtractionScope<'_>,
    ) -> crate::parsed_extraction::ParsedExtraction {
        let start = Instant::now();
        let mut state = Self::initialize_state(
            file_path,
            source,
            crate::common::file_end_line(source, tree),
        );

        let metrics = crate::parsed_extraction::visit_root_children(tree, scope, |child| {
            Self::visit_root_value(&mut state, child);
        });

        crate::parsed_extraction::ParsedExtraction::complete(
            ExtractionResult {
                nodes: state.nodes,
                edges: state.edges,
                unresolved_refs: Vec::new(),
                errors: Vec::new(),
                duration_ms: start.elapsed().as_millis() as u64,
            },
            scope,
            metrics,
        )
    }

    fn initialize_state<'s>(
        file_path: &str,
        source: &'s str,
        end_line: u32,
    ) -> ExtractionState<'s> {
        let mut state = ExtractionState::new(file_path, source);
        state.nodes.push(Node {
            id: state.file_node_id.clone(),
            kind: NodeKind::File,
            name: file_path.to_string(),
            qualified_name: file_path.to_string(),
            file_path: file_path.to_string(),
            start_line: 0,
            attrs_start_line: 0,
            end_line,
            start_column: 0,
            end_column: 0,
            signature: None,
            docstring: None,
            visibility: Visibility::Pub,
            is_async: false,
            branches: 0,
            loops: 0,
            returns: 0,
            max_nesting: 0,
            unsafe_blocks: 0,
            unchecked_calls: 0,
            assertions: 0,
            complexity_analysis: ComplexityAnalysisV1::Complete,
            updated_at: state.timestamp,
            parent_id: None,
        });
        state
    }

    /// Only an object document has named members; arrays and scalars stay a
    /// bare file node.
    fn visit_root_value(state: &mut ExtractionState<'_>, node: TsNode<'_>) {
        if node.kind() != "object" {
            return;
        }
        let mut cursor = node.walk();
        if !cursor.goto_first_child() {
            return;
        }
        loop {
            let child = cursor.node();
            if child.kind() == "pair" {
                Self::emit_pair(state, child);
            }
            if !cursor.goto_next_sibling() {
                break;
            }
        }
    }

    fn emit_pair(state: &mut ExtractionState<'_>, pair: TsNode<'_>) {
        let Some(key) = pair.child_by_field_name("key") else {
            return;
        };
        let name = unquote(state.node_text(key));
        if name.is_empty() {
            return;
        }
        let start_line = pair.start_position().row as u32;
        let end_line = pair.end_position().row as u32;
        let text = state.node_text(pair);
        let signature = if text.len() <= MAX_JSON_PAIR_SIGNATURE_BYTES_V1 {
            text.to_string()
        } else {
            text.lines().next().unwrap_or("").to_string()
        };
        let id = local_node_id(
            &state.file_path,
            state.source,
            &NodeKind::Const,
            &name,
            pair,
        );
        state.nodes.push(Node {
            id: id.clone(),
            kind: NodeKind::Const,
            name: name.clone(),
            qualified_name: format!("{}::{name}", state.file_path),
            file_path: state.file_path.clone(),
            start_line,
            attrs_start_line: start_line,
            end_line,
            start_column: pair.start_position().column as u32,
            end_column: pair.end_position().column as u32,
            signature: Some(signature),
            docstring: None,
            visibility: Visibility::Pub,
            is_async: false,
            branches: 0,
            loops: 0,
            returns: 0,
            max_nesting: 0,
            unsafe_blocks: 0,
            unchecked_calls: 0,
            assertions: 0,
            complexity_analysis: ComplexityAnalysisV1::Complete,
            updated_at: state.timestamp,
            parent_id: None,
        });
        state.edges.push(Edge {
            source: state.file_node_id.clone(),
            target: id,
            kind: EdgeKind::Contains,
            line: Some(start_line),
        });
    }
}

/// A JSON key without its quotes; escapes stay as written.
fn unquote(text: &str) -> String {
    text.strip_prefix('"')
        .and_then(|inner| inner.strip_suffix('"'))
        .unwrap_or(text)
        .to_string()
}

impl crate::LanguageExtractor for JsonExtractor {
    fn extensions(&self) -> &[&str] {
        &["json"]
    }

    fn language_name(&self) -> &'static str {
        "JSON"
    }

    fn extract_parsed_artifact_prepared(
        &self,
        file_path: &str,
        source: &str,
        _parsed_source: &str,
        tree: &Tree,
        scope: crate::parsed_extraction::ParsedExtractionScope<'_>,
    ) -> crate::parsed_extraction::ParsedExtractionArtifactV1 {
        crate::parsed_extraction::ParsedExtractionArtifactV1::from_parsed(Self::extract_tree(
            file_path, source, tree, scope,
        ))
    }
}
