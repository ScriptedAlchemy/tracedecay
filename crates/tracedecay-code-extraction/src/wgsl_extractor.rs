/// Tree-sitter based WGSL (WebGPU Shading Language) source code extractor.
///
/// Parses WGSL source files and emits nodes and edges for the code graph.
/// Handles `.wgsl` files.
use std::time::Instant;

use tree_sitter::{Node as TsNode, Tree};

use crate::common::{ExtractionState, local_node_id};
use crate::complexity::{C_COMPLEXITY, count_complexity};
use crate::traversal::{find_descendant_by_kind, find_direct_child_by_kind};
use crate::types::{
    ComplexityAnalysisV1, Edge, EdgeKind, ExtractionResult, Node, NodeKind, UnresolvedRef,
    Visibility, generate_node_id,
};

/// Extracts code graph nodes and edges from WGSL source files using tree-sitter.
pub struct WgslExtractor;

impl WgslExtractor {
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

    fn visit_node(state: &mut ExtractionState, node: TsNode<'_>) {
        match node.kind() {
            "function_decl" => Self::visit_function_decl(state, node),
            "struct_decl" => Self::visit_struct_decl(state, node),
            "global_variable_decl" => Self::visit_global_variable(state, node),
            "global_constant_decl" => Self::visit_global_constant(state, node),
            "type_alias_decl" => Self::visit_type_alias(state, node),
            _ => {}
        }
    }

    fn visit_function_decl(state: &mut ExtractionState, node: TsNode<'_>) {
        let Some(header) = find_direct_child_by_kind(node, "function_header") else {
            return;
        };
        let name = find_direct_child_by_kind(header, "ident").map_or_else(
            || "<anonymous>".to_string(),
            |n| state.node_text(n).to_string(),
        );

        // Collect stage attributes (@vertex, @fragment, @compute).
        let attrs = Self::collect_attributes(state, node);
        let docstring = if attrs.is_empty() {
            None
        } else {
            Some(attrs.join(" "))
        };

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

        let body = find_direct_child_by_kind(node, "compound_statement");
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
            docstring,
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

    fn extract_function_signature(state: &ExtractionState, node: TsNode<'_>) -> String {
        let text = state.node_text(node);
        if let Some(brace_pos) = text.find('{') {
            text[..brace_pos].trim().to_string()
        } else {
            text.trim().to_string()
        }
    }

    fn collect_attributes(state: &ExtractionState, node: TsNode<'_>) -> Vec<String> {
        let mut attrs = Vec::new();
        let mut cursor = node.walk();
        if cursor.goto_first_child() {
            loop {
                let child = cursor.node();
                if child.kind() == "attribute" {
                    // attribute children: "@", ident, optional "(…)"
                    if let Some(ident) = find_direct_child_by_kind(child, "ident") {
                        attrs.push(format!("@{}", state.node_text(ident)));
                    }
                }
                if !cursor.goto_next_sibling() {
                    break;
                }
            }
        }
        attrs
    }

    fn visit_struct_decl(state: &mut ExtractionState, node: TsNode<'_>) {
        let name = find_direct_child_by_kind(node, "ident").map_or_else(
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

        if let Some(body) = find_direct_child_by_kind(node, "struct_body_decl") {
            state.node_stack.push((name, id));
            Self::visit_struct_members(state, body);
            state.node_stack.pop();
        }
    }

    fn visit_struct_members(state: &mut ExtractionState, body: TsNode<'_>) {
        let mut cursor = body.walk();
        if cursor.goto_first_child() {
            loop {
                let child = cursor.node();
                if child.kind() == "struct_member" {
                    Self::visit_struct_member(state, child);
                }
                if !cursor.goto_next_sibling() {
                    break;
                }
            }
        }
    }

    fn visit_struct_member(state: &mut ExtractionState, node: TsNode<'_>) {
        // struct_member: attribute* ident ":" type_decl
        let Some(ident) = find_direct_child_by_kind(node, "ident") else {
            return;
        };
        let name = state.node_text(ident);
        let start_line = node.start_position().row as u32;
        let end_line = node.end_position().row as u32;
        let start_column = node.start_position().column as u32;
        let end_column = node.end_position().column as u32;
        let sig = state.node_text(node);
        let qualified_name = format!("{}::{}", state.qualified_prefix(), name);
        let id = local_node_id(&state.file_path, state.source, &NodeKind::Field, name, node);

        let graph_node = Node {
            id: id.clone(),
            kind: NodeKind::Field,
            name: name.to_string(),
            qualified_name,
            file_path: state.file_path.clone(),
            start_line,
            attrs_start_line: start_line,
            end_line,
            start_column,
            end_column,
            signature: Some(sig.trim().trim_end_matches(',').trim().to_string()),
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

    fn visit_global_variable(state: &mut ExtractionState, node: TsNode<'_>) {
        // global_variable_decl: attribute* variable_decl ("=" expression)?
        // variable_decl: "var" variable_qualifier? variable_ident_decl
        // variable_ident_decl: ident (":" type_decl)?
        let name = find_descendant_by_kind(node, "variable_ident_decl")
            .and_then(|vid| find_direct_child_by_kind(vid, "ident"))
            .map_or_else(
                || "<anonymous>".to_string(),
                |n| state.node_text(n).to_string(),
            );

        Self::emit_variable_node(state, node, name, NodeKind::Static);
    }

    fn visit_global_constant(state: &mut ExtractionState, node: TsNode<'_>) {
        // global_constant_decl: attribute* ("const"|"override") (ident | variable_ident_decl) "=" expression
        let name = find_descendant_by_kind(node, "variable_ident_decl")
            .and_then(|vid| find_direct_child_by_kind(vid, "ident"))
            .or_else(|| find_direct_child_by_kind(node, "ident"))
            .map_or_else(
                || "<anonymous>".to_string(),
                |n| state.node_text(n).to_string(),
            );

        Self::emit_variable_node(state, node, name, NodeKind::Const);
    }

    fn emit_variable_node(
        state: &mut ExtractionState,
        node: TsNode<'_>,
        name: String,
        kind: NodeKind,
    ) {
        let start_line = node.start_position().row as u32;
        let end_line = node.end_position().row as u32;
        let start_column = node.start_position().column as u32;
        let end_column = node.end_position().column as u32;
        let text = state.node_text(node);
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

    fn visit_type_alias(state: &mut ExtractionState, node: TsNode<'_>) {
        // type_alias_decl: "type" ident "=" type_decl
        let name = find_direct_child_by_kind(node, "ident").map_or_else(
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
            &NodeKind::TypeAlias,
            &name,
            node,
        );

        let graph_node = Node {
            id: id.clone(),
            kind: NodeKind::TypeAlias,
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

    fn extract_call_sites(state: &mut ExtractionState, node: TsNode<'_>, fn_node_id: &str) {
        let mut cursor = node.walk();
        if cursor.goto_first_child() {
            loop {
                let child = cursor.node();
                // WGSL: func_call_statement is a statement-level call.
                // The callee name is the first named child (the callable ident).
                if child.kind() == "func_call_statement"
                    && let Some(callee) = child.named_child(0)
                {
                    let callee_name = state.node_text(callee);
                    state.unresolved_refs.push(UnresolvedRef {
                        from_node_id: fn_node_id.to_string(),
                        reference_name: callee_name.to_string(),
                        reference_kind: EdgeKind::Calls,
                        line: child.start_position().row as u32,
                        column: child.start_position().column as u32,
                        file_path: state.file_path.clone(),
                    });
                }
                Self::extract_call_sites(state, child, fn_node_id);
                if !cursor.goto_next_sibling() {
                    break;
                }
            }
        }
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

impl crate::LanguageExtractor for WgslExtractor {
    fn extensions(&self) -> &[&str] {
        &["wgsl"]
    }

    fn language_name(&self) -> &'static str {
        "WGSL"
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
            WgslExtractor::extract_tree(file_path, source, tree, scope),
        )
    }
}
