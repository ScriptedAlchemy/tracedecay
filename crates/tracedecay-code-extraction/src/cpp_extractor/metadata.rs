use std::time::Instant;

use tree_sitter::Node as TsNode;

use super::{CppExtractor, ExtractionState};
use crate::{
    common::{
        clean_c_doc_comment, docstring_from_preceding_comments, extract_call_expression_sites,
    },
    traversal::find_direct_child_by_kind,
    types::{
        Edge, EdgeKind, ExtractionResult, Node, NodeKind, UnresolvedRef, Visibility,
        generate_node_id,
    },
};

impl CppExtractor {
    pub(super) fn visit_preproc_def(state: &mut ExtractionState, node: TsNode<'_>) {
        let name = find_direct_child_by_kind(node, "identifier")
            .map_or_else(|| "<anonymous>".to_string(), |child| state.node_text(child));
        let text = state.node_text(node);
        let start_line = node.start_position().row as u32;
        let end_line = node.end_position().row as u32;
        let start_column = node.start_position().column as u32;
        let end_column = node.end_position().column as u32;
        let qualified_name = format!("{}::{}", state.qualified_prefix(), name);
        let id = generate_node_id(
            &state.file_path,
            &NodeKind::PreprocessorDef,
            &name,
            start_line,
        );
        state.nodes.push(Node {
            id: id.clone(),
            kind: NodeKind::PreprocessorDef,
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
            updated_at: state.timestamp,
            parent_id: None,
        });
        if let Some(parent_id) = state.parent_node_id() {
            state.edges.push(Edge {
                source: parent_id.to_string(),
                target: id,
                kind: EdgeKind::Contains,
                line: Some(start_line),
            });
        }
    }

    pub(super) fn visit_preproc_include(state: &mut ExtractionState, node: TsNode<'_>) {
        let path = find_direct_child_by_kind(node, "string_literal")
            .or_else(|| find_direct_child_by_kind(node, "system_lib_string"))
            .map_or_else(
                || "<unknown>".to_string(),
                |child| {
                    state
                        .node_text(child)
                        .trim_matches(|character| {
                            character == '"' || character == '<' || character == '>'
                        })
                        .to_string()
                },
            );
        let text = state.node_text(node);
        let start_line = node.start_position().row as u32;
        let end_line = node.end_position().row as u32;
        let start_column = node.start_position().column as u32;
        let end_column = node.end_position().column as u32;
        let qualified_name = format!("{}::{}", state.qualified_prefix(), path);
        let id = generate_node_id(&state.file_path, &NodeKind::Include, &path, start_line);
        state.nodes.push(Node {
            id: id.clone(),
            kind: NodeKind::Include,
            name: path,
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
            updated_at: state.timestamp,
            parent_id: None,
        });
        if let Some(parent_id) = state.parent_node_id() {
            state.edges.push(Edge {
                source: parent_id.to_string(),
                target: id,
                kind: EdgeKind::Contains,
                line: Some(start_line),
            });
        }
    }

    pub(super) fn extract_enum_variants(state: &mut ExtractionState, enum_spec: TsNode<'_>) {
        if let Some(enumerators) = find_direct_child_by_kind(enum_spec, "enumerator_list") {
            let mut cursor = enumerators.walk();
            if cursor.goto_first_child() {
                loop {
                    let child = cursor.node();
                    if child.kind() == "enumerator" {
                        Self::extract_single_enumerator(state, child);
                    }
                    if !cursor.goto_next_sibling() {
                        break;
                    }
                }
            }
        }
    }

    fn extract_single_enumerator(state: &mut ExtractionState, node: TsNode<'_>) {
        let name = find_direct_child_by_kind(node, "identifier")
            .map_or_else(|| "<anonymous>".to_string(), |child| state.node_text(child));
        let text = state.node_text(node);
        let start_line = node.start_position().row as u32;
        let end_line = node.end_position().row as u32;
        let start_column = node.start_position().column as u32;
        let end_column = node.end_position().column as u32;
        let qualified_name = format!("{}::{}", state.qualified_prefix(), name);
        let id = generate_node_id(&state.file_path, &NodeKind::EnumVariant, &name, start_line);
        state.nodes.push(Node {
            id: id.clone(),
            kind: NodeKind::EnumVariant,
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
            updated_at: state.timestamp,
            parent_id: None,
        });
        if let Some(parent_id) = state.parent_node_id() {
            state.edges.push(Edge {
                source: parent_id.to_string(),
                target: id,
                kind: EdgeKind::Contains,
                line: Some(start_line),
            });
        }
    }

    pub(super) fn extract_base_classes(
        state: &mut ExtractionState,
        node: TsNode<'_>,
        class_id: &str,
    ) {
        if let Some(base_clause) = find_direct_child_by_kind(node, "base_class_clause") {
            let mut cursor = base_clause.walk();
            if cursor.goto_first_child() {
                loop {
                    let child = cursor.node();
                    if child.kind() == "type_identifier" || child.kind() == "qualified_identifier" {
                        state.unresolved_refs.push(UnresolvedRef {
                            from_node_id: class_id.to_string(),
                            reference_name: state.node_text(child),
                            reference_kind: EdgeKind::Extends,
                            line: child.start_position().row as u32,
                            column: child.start_position().column as u32,
                            file_path: state.file_path.clone(),
                        });
                    }
                    if !cursor.goto_next_sibling() {
                        break;
                    }
                }
            }
        }
    }

    pub(super) fn extract_call_sites(
        state: &mut ExtractionState,
        node: TsNode<'_>,
        fn_node_id: &str,
    ) {
        extract_call_expression_sites(
            &state.source,
            &state.file_path,
            &mut state.unresolved_refs,
            node,
            fn_node_id,
        );
    }

    pub(super) fn extract_docstring(state: &ExtractionState, node: TsNode<'_>) -> Option<String> {
        docstring_from_preceding_comments(&state.source, node, clean_c_doc_comment)
    }

    pub(super) fn has_storage_class(
        state: &ExtractionState,
        node: TsNode<'_>,
        class: &str,
    ) -> bool {
        let mut cursor = node.walk();
        if cursor.goto_first_child() {
            loop {
                let child = cursor.node();
                if child.kind() == "storage_class_specifier" && state.node_text(child) == class {
                    return true;
                }
                if !cursor.goto_next_sibling() {
                    break;
                }
            }
        }
        false
    }

    pub(super) fn is_pure_virtual(state: &ExtractionState, node: TsNode<'_>) -> bool {
        state.node_text(node).contains("= 0")
    }

    pub(super) fn extract_annotations(
        state: &mut ExtractionState,
        node: TsNode<'_>,
        target_id: &str,
    ) {
        let mut cursor = node.walk();
        if cursor.goto_first_child() {
            loop {
                let child = cursor.node();
                if child.kind() == "attribute_declaration" {
                    Self::extract_attributes_from_decl(state, child, target_id);
                }
                if !cursor.goto_next_sibling() {
                    break;
                }
            }
        }
    }

    fn extract_attributes_from_decl(
        state: &mut ExtractionState,
        declaration: TsNode<'_>,
        target_id: &str,
    ) {
        let mut cursor = declaration.walk();
        if cursor.goto_first_child() {
            loop {
                let child = cursor.node();
                if child.kind() == "attribute" {
                    let name = Self::extract_cpp_attribute_name(state, child);
                    let start_line = child.start_position().row as u32;
                    let end_line = child.end_position().row as u32;
                    let start_column = child.start_position().column as u32;
                    let end_column = child.end_position().column as u32;
                    let qualified_name = format!("{}::@{}", state.qualified_prefix(), name);
                    let id = generate_node_id(
                        &state.file_path,
                        &NodeKind::AnnotationUsage,
                        &name,
                        start_line,
                    );
                    state.nodes.push(Node {
                        id: id.clone(),
                        kind: NodeKind::AnnotationUsage,
                        name: name.clone(),
                        qualified_name,
                        file_path: state.file_path.clone(),
                        start_line,
                        attrs_start_line: start_line,
                        end_line,
                        start_column,
                        end_column,
                        signature: Some(format!("[[{}]]", state.node_text(child).trim())),
                        docstring: None,
                        visibility: Visibility::Private,
                        is_async: false,
                        branches: 0,
                        loops: 0,
                        returns: 0,
                        max_nesting: 0,
                        unsafe_blocks: 0,
                        unchecked_calls: 0,
                        assertions: 0,
                        updated_at: state.timestamp,
                        parent_id: None,
                    });
                    state.unresolved_refs.push(UnresolvedRef {
                        from_node_id: id.clone(),
                        reference_name: name,
                        reference_kind: EdgeKind::Annotates,
                        line: start_line,
                        column: start_column,
                        file_path: state.file_path.clone(),
                    });
                    state.edges.push(Edge {
                        source: id,
                        target: target_id.to_string(),
                        kind: EdgeKind::Annotates,
                        line: Some(start_line),
                    });
                }
                if !cursor.goto_next_sibling() {
                    break;
                }
            }
        }
    }

    fn extract_cpp_attribute_name(state: &ExtractionState, node: TsNode<'_>) -> String {
        if let Some(identifier) = find_direct_child_by_kind(node, "identifier") {
            return state.node_text(identifier);
        }
        let text = state.node_text(node);
        text.split('(').next().unwrap_or(&text).trim().to_string()
    }

    pub(super) fn build_result(state: ExtractionState, start: Instant) -> ExtractionResult {
        ExtractionResult {
            nodes: state.nodes,
            edges: state.edges,
            unresolved_refs: state.unresolved_refs,
            errors: state.errors,
            duration_ms: start.elapsed().as_millis() as u64,
        }
    }
}
