/// Tree-sitter based HLSL (High-Level Shading Language) source code extractor.
///
/// Parses HLSL source files and emits nodes and edges for the code graph.
/// Handles `.hlsl` and `.fx` files.
use std::time::Instant;

use tree_sitter::{Node as TsNode, Tree};

use crate::common::{ExtractionState, extract_call_expression_sites, local_node_id};
use crate::complexity::{C_COMPLEXITY, count_complexity};
use crate::traversal::{find_descendant_by_kind, find_direct_child_by_kind, has_direct_child_kind};
use crate::types::{
    ComplexityAnalysisV1, Edge, EdgeKind, ExtractionResult, Node, NodeKind, UnresolvedRef,
    Visibility, generate_node_id,
};

/// Extracts code graph nodes and edges from HLSL source files using tree-sitter.
pub struct HlslExtractor;

impl HlslExtractor {
    fn extract_tree(
        file_path: &str,
        source: &str,
        tree: &Tree,
        scope: crate::parsed_extraction::ParsedExtractionScope<'_>,
    ) -> crate::parsed_extraction::ParsedExtraction {
        let start = Instant::now();
        let mut state = ExtractionState::new(file_path, source);

        let file_node = Node {
            id: generate_node_id(file_path, &NodeKind::File, file_path, 0),
            kind: NodeKind::File,
            name: file_path.to_string(),
            qualified_name: file_path.to_string(),
            file_path: file_path.to_string(),
            start_line: 0,
            attrs_start_line: 0,
            end_line: crate::common::file_end_line(source, tree),
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
        };
        let file_node_id = file_node.id.clone();
        state.nodes.push(file_node);
        state.node_stack.push((file_path.to_string(), file_node_id));

        let metrics = crate::parsed_extraction::visit_root_children(tree, scope, |child| {
            Self::visit_node(&mut state, child);
        });

        state.node_stack.pop();
        crate::parsed_extraction::ParsedExtraction::complete(
            Self::build_result(state, start),
            scope,
            metrics,
        )
    }

    fn visit_children(state: &mut ExtractionState, node: TsNode<'_>) {
        let mut cursor = node.walk();
        if cursor.goto_first_child() {
            loop {
                Self::visit_node(state, cursor.node());
                if !cursor.goto_next_sibling() {
                    break;
                }
            }
        }
    }

    fn visit_node(state: &mut ExtractionState, node: TsNode<'_>) {
        match node.kind() {
            "function_definition" => Self::visit_function_definition(state, node),
            "struct_specifier" => Self::visit_struct_specifier(state, node),
            "cbuffer_specifier" => Self::visit_cbuffer_specifier(state, node),
            "declaration" => Self::visit_declaration(state, node),
            "preproc_def" => Self::visit_preproc_def(state, node),
            "preproc_include" => Self::visit_preproc_include(state, node),
            _ => {}
        }
    }

    fn visit_function_definition(state: &mut ExtractionState, node: TsNode<'_>) {
        let name =
            Self::extract_function_name(state, node).unwrap_or_else(|| "<anonymous>".to_string());
        let signature = Some(Self::extract_function_signature(state, node));
        let start_line = node.start_position().row as u32;
        let end_line = node.end_position().row as u32;
        let start_column = node.start_position().column as u32;
        let end_column = node.end_position().column as u32;
        let qualified_name = format!("{}::{}", state.qualified_prefix(), name);
        let id = local_node_id(
            &state.file_path,
            state.source,
            &NodeKind::Function,
            &name,
            node,
        );

        let body = node.child_by_field_name("body");
        let metrics = body
            .map(|b| count_complexity(b, &C_COMPLEXITY, state.source))
            .unwrap_or_default();

        let graph_node = Node {
            id: id.clone(),
            kind: NodeKind::Function,
            name: name.clone(),
            qualified_name,
            file_path: state.file_path.clone(),
            start_line,
            attrs_start_line: start_line,
            end_line,
            start_column,
            end_column,
            signature,
            docstring: None,
            visibility: Visibility::Pub,
            is_async: false,
            branches: metrics.branches,
            loops: metrics.loops,
            returns: metrics.returns,
            max_nesting: metrics.max_nesting,
            unsafe_blocks: metrics.unsafe_blocks,
            unchecked_calls: metrics.unchecked_calls,
            assertions: metrics.assertions,
            complexity_analysis: metrics.analysis,
            updated_at: state.timestamp,
            parent_id: None,
        };
        state.nodes.push(graph_node);

        if let Some(parent_id) = state.parent_node_id() {
            state.edges.push(Edge {
                source: parent_id.to_string(),
                target: id.clone(),
                kind: EdgeKind::Contains,
                line: Some(start_line),
            });
        }

        if let Some(body) = body {
            Self::extract_call_sites(state, body, &id);
        }
    }

    fn extract_function_name(state: &ExtractionState, node: TsNode<'_>) -> Option<String> {
        // function_definition.declarator → function_declarator.declarator → identifier
        if let Some(func_decl) = find_descendant_by_kind(node, "function_declarator") {
            if let Some(ident) = find_direct_child_by_kind(func_decl, "identifier") {
                return Some(state.node_text(ident).to_string());
            }
            // Qualified identifier (e.g. ClassName::method)
            if let Some(qi) = find_direct_child_by_kind(func_decl, "qualified_identifier") {
                return Some(state.node_text(qi).to_string());
            }
        }
        None
    }

    fn extract_function_signature(state: &ExtractionState, node: TsNode<'_>) -> String {
        let text = state.node_text(node);
        if let Some(brace_pos) = text.find('{') {
            text[..brace_pos].trim().to_string()
        } else {
            text.trim().trim_end_matches(';').trim().to_string()
        }
    }

    fn visit_struct_specifier(state: &mut ExtractionState, node: TsNode<'_>) {
        let name = node.child_by_field_name("name").map_or_else(
            || "<anonymous>".to_string(),
            |n| state.node_text(n).to_string(),
        );

        let start_line = node.start_position().row as u32;
        let end_line = node.end_position().row as u32;
        let start_column = node.start_position().column as u32;
        let end_column = node.end_position().column as u32;
        let qualified_name = format!("{}::{}", state.qualified_prefix(), name);
        let id = local_node_id(
            &state.file_path,
            state.source,
            &NodeKind::Struct,
            &name,
            node,
        );

        let graph_node = Node {
            id: id.clone(),
            kind: NodeKind::Struct,
            name: name.clone(),
            qualified_name,
            file_path: state.file_path.clone(),
            start_line,
            attrs_start_line: start_line,
            end_line,
            start_column,
            end_column,
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
        };
        state.nodes.push(graph_node);

        if let Some(parent_id) = state.parent_node_id() {
            state.edges.push(Edge {
                source: parent_id.to_string(),
                target: id.clone(),
                kind: EdgeKind::Contains,
                line: Some(start_line),
            });
        }

        if let Some(body) = node.child_by_field_name("body") {
            state.node_stack.push((name, id));
            Self::visit_struct_fields(state, body);
            state.node_stack.pop();
        }
    }

    fn visit_struct_fields(state: &mut ExtractionState, body: TsNode<'_>) {
        let mut cursor = body.walk();
        if cursor.goto_first_child() {
            loop {
                let child = cursor.node();
                if child.kind() == "field_declaration" {
                    Self::visit_field_declaration(state, child);
                }
                if !cursor.goto_next_sibling() {
                    break;
                }
            }
        }
    }

    fn visit_field_declaration(state: &mut ExtractionState, node: TsNode<'_>) {
        let name = find_descendant_by_kind(node, "field_identifier")
            .or_else(|| find_descendant_by_kind(node, "identifier"))
            .map_or_else(
                || "<anonymous>".to_string(),
                |n| state.node_text(n).to_string(),
            );

        let start_line = node.start_position().row as u32;
        let end_line = node.end_position().row as u32;
        let start_column = node.start_position().column as u32;
        let end_column = node.end_position().column as u32;
        let sig = state.node_text(node);
        let qualified_name = format!("{}::{}", state.qualified_prefix(), name);
        let id = local_node_id(
            &state.file_path,
            state.source,
            &NodeKind::Field,
            &name,
            node,
        );

        let graph_node = Node {
            id: id.clone(),
            kind: NodeKind::Field,
            name,
            qualified_name,
            file_path: state.file_path.clone(),
            start_line,
            attrs_start_line: start_line,
            end_line,
            start_column,
            end_column,
            signature: Some(sig.trim().trim_end_matches(';').trim().to_string()),
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
        };
        state.nodes.push(graph_node);

        if let Some(parent_id) = state.parent_node_id() {
            state.edges.push(Edge {
                source: parent_id.to_string(),
                target: id,
                kind: EdgeKind::Contains,
                line: Some(start_line),
            });
        }
    }

    fn visit_cbuffer_specifier(state: &mut ExtractionState, node: TsNode<'_>) {
        let name = node.child_by_field_name("name").map_or_else(
            || "<anonymous>".to_string(),
            |n| state.node_text(n).to_string(),
        );

        let start_line = node.start_position().row as u32;
        let end_line = node.end_position().row as u32;
        let start_column = node.start_position().column as u32;
        let end_column = node.end_position().column as u32;
        let qualified_name = format!("{}::{}", state.qualified_prefix(), name);
        let id = local_node_id(
            &state.file_path,
            state.source,
            &NodeKind::Struct,
            &name,
            node,
        );

        let graph_node = Node {
            id: id.clone(),
            kind: NodeKind::Struct,
            name: name.clone(),
            qualified_name,
            file_path: state.file_path.clone(),
            start_line,
            attrs_start_line: start_line,
            end_line,
            start_column,
            end_column,
            signature: Some("cbuffer".to_string()),
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
        };
        state.nodes.push(graph_node);

        if let Some(parent_id) = state.parent_node_id() {
            state.edges.push(Edge {
                source: parent_id.to_string(),
                target: id.clone(),
                kind: EdgeKind::Contains,
                line: Some(start_line),
            });
        }

        if let Some(body) = node.child_by_field_name("body") {
            state.node_stack.push((name, id));
            Self::visit_cbuffer_members(state, body);
            state.node_stack.pop();
        }
    }

    fn visit_cbuffer_members(state: &mut ExtractionState, body: TsNode<'_>) {
        let mut cursor = body.walk();
        if cursor.goto_first_child() {
            loop {
                let child = cursor.node();
                if child.kind() == "declaration" {
                    Self::visit_field_declaration(state, child);
                }
                if !cursor.goto_next_sibling() {
                    break;
                }
            }
        }
    }

    fn visit_declaration(state: &mut ExtractionState, node: TsNode<'_>) {
        // Skip function prototypes — handled by function_definition.
        if find_descendant_by_kind(node, "function_declarator").is_some() {
            return;
        }
        // Skip struct/cbuffer declarations (visited separately).
        if has_direct_child_kind(node, "struct_specifier")
            || has_direct_child_kind(node, "cbuffer_specifier")
        {
            Self::visit_children(state, node);
            return;
        }

        let name = Self::extract_declaration_name(state, node)
            .unwrap_or_else(|| "<anonymous>".to_string());

        let text = state.node_text(node);
        let is_const = text.contains("const ") || text.starts_with("static const");
        let (kind, visibility) = if is_const {
            (NodeKind::Const, Visibility::Pub)
        } else {
            (NodeKind::Static, Visibility::Private)
        };

        let start_line = node.start_position().row as u32;
        let end_line = node.end_position().row as u32;
        let start_column = node.start_position().column as u32;
        let end_column = node.end_position().column as u32;
        let qualified_name = format!("{}::{}", state.qualified_prefix(), name);
        let id = local_node_id(&state.file_path, state.source, &kind, &name, node);

        let graph_node = Node {
            id: id.clone(),
            kind,
            name,
            qualified_name,
            file_path: state.file_path.clone(),
            start_line,
            attrs_start_line: start_line,
            end_line,
            start_column,
            end_column,
            signature: Some(text.trim().trim_end_matches(';').trim().to_string()),
            docstring: None,
            visibility,
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
        };
        state.nodes.push(graph_node);

        if let Some(parent_id) = state.parent_node_id() {
            state.edges.push(Edge {
                source: parent_id.to_string(),
                target: id,
                kind: EdgeKind::Contains,
                line: Some(start_line),
            });
        }
    }

    fn extract_declaration_name(state: &ExtractionState, node: TsNode<'_>) -> Option<String> {
        if let Some(decl) = node.child_by_field_name("declarator") {
            // identifier directly
            if decl.kind() == "identifier" {
                return Some(state.node_text(decl).to_string());
            }
            // init_declarator: identifier "=" value
            if let Some(ident) = find_direct_child_by_kind(decl, "identifier") {
                return Some(state.node_text(ident).to_string());
            }
            // array_declarator: identifier "[" ... "]"
            if let Some(arr) = find_direct_child_by_kind(decl, "array_declarator")
                && let Some(ident) = find_direct_child_by_kind(arr, "identifier")
            {
                return Some(state.node_text(ident).to_string());
            }
        }
        // Fallback: any identifier child
        find_direct_child_by_kind(node, "identifier").map(|n| state.node_text(n).to_string())
    }

    fn visit_preproc_def(state: &mut ExtractionState, node: TsNode<'_>) {
        let name = find_direct_child_by_kind(node, "identifier").map_or_else(
            || "<anonymous>".to_string(),
            |n| state.node_text(n).to_string(),
        );

        let start_line = node.start_position().row as u32;
        let end_line = node.end_position().row as u32;
        let start_column = node.start_position().column as u32;
        let end_column = node.end_position().column as u32;
        let text = state.node_text(node);
        let qualified_name = format!("{}::{}", state.qualified_prefix(), name);
        let id = local_node_id(
            &state.file_path,
            state.source,
            &NodeKind::Const,
            &name,
            node,
        );

        let graph_node = Node {
            id: id.clone(),
            kind: NodeKind::Const,
            name,
            qualified_name,
            file_path: state.file_path.clone(),
            start_line,
            attrs_start_line: start_line,
            end_line,
            start_column,
            end_column,
            signature: Some(text.trim().to_string()),
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
        };
        state.nodes.push(graph_node);

        if let Some(parent_id) = state.parent_node_id() {
            state.edges.push(Edge {
                source: parent_id.to_string(),
                target: id,
                kind: EdgeKind::Contains,
                line: Some(start_line),
            });
        }
    }

    fn visit_preproc_include(state: &mut ExtractionState, node: TsNode<'_>) {
        let include_path = find_direct_child_by_kind(node, "string_literal")
            .or_else(|| find_direct_child_by_kind(node, "system_lib_string"))
            .map_or_else(
                || "<unknown>".to_string(),
                |n| state.node_text(n).to_string(),
            );

        let line = node.start_position().row as u32;
        let column = node.start_position().column as u32;

        if let Some(parent_id) = state.parent_node_id() {
            state.unresolved_refs.push(UnresolvedRef {
                from_node_id: parent_id.to_string(),
                reference_name: include_path,
                reference_kind: EdgeKind::Uses,
                line,
                column,
                file_path: state.file_path.clone(),
            });
        }
    }

    fn extract_call_sites(state: &mut ExtractionState, node: TsNode<'_>, fn_node_id: &str) {
        extract_call_expression_sites(
            state.source,
            &state.file_path,
            &mut state.unresolved_refs,
            node,
            fn_node_id,
        );
    }

    fn build_result(state: ExtractionState, start: Instant) -> ExtractionResult {
        ExtractionResult {
            nodes: state.nodes,
            edges: state.edges,
            unresolved_refs: state.unresolved_refs,
            errors: state.errors,
            duration_ms: start.elapsed().as_millis() as u64,
        }
    }
}

impl crate::LanguageExtractor for HlslExtractor {
    fn extensions(&self) -> &[&str] {
        &["hlsl", "fx"]
    }

    fn language_name(&self) -> &'static str {
        "HLSL"
    }

    fn extract_parsed_artifact_prepared(
        &self,
        file_path: &str,
        source: &str,
        _parsed_source: &str,
        tree: &Tree,
        scope: crate::parsed_extraction::ParsedExtractionScope<'_>,
    ) -> crate::parsed_extraction::ParsedExtractionArtifactV1 {
        crate::parsed_extraction::ParsedExtractionArtifactV1::from_parsed(
            HlslExtractor::extract_tree(file_path, source, tree, scope),
        )
    }
}
