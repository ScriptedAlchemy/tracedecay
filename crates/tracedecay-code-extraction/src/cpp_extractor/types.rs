use tree_sitter::Node as TsNode;

use super::{CppExtractor, ExtractionState};
use crate::{
    traversal::{find_descendant_by_kind, find_direct_child_by_kind},
    types::{Edge, EdgeKind, Node, NodeKind, Visibility, generate_node_id},
};

impl CppExtractor {
    pub(super) fn visit_type_definition(state: &mut ExtractionState, node: TsNode<'_>) {
        if let Some(spec) = find_direct_child_by_kind(node, "struct_specifier") {
            Self::visit_typedef_struct(state, node, spec);
            return;
        }
        if let Some(spec) = find_direct_child_by_kind(node, "union_specifier") {
            Self::visit_typedef_union(state, node, spec);
            return;
        }
        if let Some(spec) = find_direct_child_by_kind(node, "enum_specifier") {
            Self::visit_typedef_enum(state, node, spec);
            return;
        }
        if find_descendant_by_kind(node, "function_declarator").is_some() {
            Self::visit_typedef_function_pointer(state, node);
            return;
        }
        Self::visit_simple_typedef(state, node);
    }

    fn emit_typedef(
        state: &mut ExtractionState,
        node: TsNode<'_>,
        name: String,
        docstring: Option<String>,
    ) {
        let start_line = node.start_position().row as u32;
        let end_line = node.end_position().row as u32;
        let start_column = node.start_position().column as u32;
        let end_column = node.end_position().column as u32;
        let text = state.node_text(node);
        let qualified_name = format!("{}::{}", state.qualified_prefix(), name);
        let id = generate_node_id(&state.file_path, &NodeKind::Typedef, &name, start_line);
        state.nodes.push(Node {
            id: id.clone(),
            kind: NodeKind::Typedef,
            name,
            qualified_name,
            file_path: state.file_path.clone(),
            start_line,
            attrs_start_line: start_line,
            end_line,
            start_column,
            end_column,
            signature: Some(text.trim().trim_end_matches(';').trim().to_string()),
            docstring,
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

    fn visit_typedef_struct(
        state: &mut ExtractionState,
        typedef_node: TsNode<'_>,
        spec: TsNode<'_>,
    ) {
        let name = Self::find_typedef_name(state, typedef_node)
            .unwrap_or_else(|| "<anonymous>".to_string());
        let docstring = Self::extract_docstring(state, typedef_node);
        Self::emit_typedef(state, typedef_node, name.clone(), docstring.clone());
        if find_direct_child_by_kind(spec, "field_declaration_list").is_some() {
            let struct_name = find_direct_child_by_kind(spec, "type_identifier")
                .map_or_else(|| name, |node| state.node_text(node));
            Self::create_struct_node(state, &struct_name, spec, docstring);
        }
    }

    fn visit_typedef_union(
        state: &mut ExtractionState,
        typedef_node: TsNode<'_>,
        spec: TsNode<'_>,
    ) {
        let name = Self::find_typedef_name(state, typedef_node)
            .unwrap_or_else(|| "<anonymous>".to_string());
        let docstring = Self::extract_docstring(state, typedef_node);
        Self::emit_typedef(state, typedef_node, name.clone(), docstring.clone());
        if find_direct_child_by_kind(spec, "field_declaration_list").is_some() {
            let union_name = find_direct_child_by_kind(spec, "type_identifier")
                .map_or_else(|| name, |node| state.node_text(node));
            Self::create_union_node(state, &union_name, spec, docstring);
        }
    }

    fn visit_typedef_enum(state: &mut ExtractionState, typedef_node: TsNode<'_>, spec: TsNode<'_>) {
        let name = Self::find_typedef_name(state, typedef_node)
            .unwrap_or_else(|| "<anonymous>".to_string());
        let docstring = Self::extract_docstring(state, typedef_node);
        Self::emit_typedef(state, typedef_node, name.clone(), docstring.clone());
        if find_direct_child_by_kind(spec, "enumerator_list").is_some() {
            let enum_name = find_direct_child_by_kind(spec, "type_identifier")
                .map_or_else(|| name, |node| state.node_text(node));
            Self::create_enum_node(state, &enum_name, spec, docstring);
        }
    }

    fn visit_typedef_function_pointer(state: &mut ExtractionState, node: TsNode<'_>) {
        let name = Self::extract_function_pointer_typedef_name(state, node).unwrap_or_else(|| {
            Self::find_typedef_name(state, node).unwrap_or_else(|| "<anonymous>".to_string())
        });
        let docstring = Self::extract_docstring(state, node);
        Self::emit_typedef(state, node, name, docstring);
    }

    fn extract_function_pointer_typedef_name(
        state: &ExtractionState,
        node: TsNode<'_>,
    ) -> Option<String> {
        if let Some(function) = find_descendant_by_kind(node, "function_declarator")
            && let Some(parenthesized) =
                find_direct_child_by_kind(function, "parenthesized_declarator")
        {
            if let Some(identifier) = find_descendant_by_kind(parenthesized, "identifier") {
                return Some(state.node_text(identifier));
            }
            if let Some(identifier) = find_descendant_by_kind(parenthesized, "type_identifier") {
                return Some(state.node_text(identifier));
            }
        }
        None
    }

    fn visit_simple_typedef(state: &mut ExtractionState, node: TsNode<'_>) {
        let name =
            Self::find_typedef_name(state, node).unwrap_or_else(|| "<anonymous>".to_string());
        let docstring = Self::extract_docstring(state, node);
        Self::emit_typedef(state, node, name, docstring);
    }

    fn find_typedef_name(state: &ExtractionState, node: TsNode<'_>) -> Option<String> {
        let mut last_type_id = None;
        let mut cursor = node.walk();
        if cursor.goto_first_child() {
            loop {
                let child = cursor.node();
                if child.kind() == "type_identifier" {
                    last_type_id = Some(state.node_text(child));
                }
                if !cursor.goto_next_sibling() {
                    break;
                }
            }
        }
        last_type_id
    }

    pub(super) fn visit_standalone_union(state: &mut ExtractionState, node: TsNode<'_>) {
        if find_direct_child_by_kind(node, "field_declaration_list").is_none() {
            return;
        }
        let name = find_direct_child_by_kind(node, "type_identifier")
            .map_or_else(|| "<anonymous>".to_string(), |child| state.node_text(child));
        if name == "<anonymous>" {
            return;
        }
        let docstring = Self::extract_docstring(state, node);
        Self::create_union_node(state, &name, node, docstring);
    }

    pub(super) fn visit_standalone_enum(state: &mut ExtractionState, node: TsNode<'_>) {
        if find_direct_child_by_kind(node, "enumerator_list").is_none() {
            return;
        }
        let name = find_direct_child_by_kind(node, "type_identifier")
            .map_or_else(|| "<anonymous>".to_string(), |child| state.node_text(child));
        if name == "<anonymous>" {
            return;
        }
        let docstring = Self::extract_docstring(state, node);
        Self::create_enum_node(state, &name, node, docstring);
    }

    pub(super) fn visit_using_declaration(state: &mut ExtractionState, node: TsNode<'_>) {
        let text = state.node_text(node);
        let name = text
            .trim()
            .trim_start_matches("using")
            .trim()
            .trim_start_matches("namespace")
            .trim()
            .trim_end_matches(';')
            .trim()
            .to_string();
        let start_line = node.start_position().row as u32;
        let end_line = node.end_position().row as u32;
        let start_column = node.start_position().column as u32;
        let end_column = node.end_position().column as u32;
        let qualified_name = format!("{}::{}", state.qualified_prefix(), name);
        let id = generate_node_id(&state.file_path, &NodeKind::Use, &name, start_line);
        state.nodes.push(Node {
            id: id.clone(),
            kind: NodeKind::Use,
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

    pub(super) fn create_union_node(
        state: &mut ExtractionState,
        name: &str,
        node: TsNode<'_>,
        docstring: Option<String>,
    ) {
        let start_line = node.start_position().row as u32;
        let end_line = node.end_position().row as u32;
        let start_column = node.start_position().column as u32;
        let end_column = node.end_position().column as u32;
        let qualified_name = format!("{}::{}", state.qualified_prefix(), name);
        let id = generate_node_id(&state.file_path, &NodeKind::Union, name, start_line);
        let text = state.node_text(node);
        let signature = text
            .find('{')
            .map(|position| text[..position].trim().to_string());
        state.nodes.push(Node {
            id: id.clone(),
            kind: NodeKind::Union,
            name: name.to_string(),
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

    pub(super) fn create_enum_node(
        state: &mut ExtractionState,
        name: &str,
        node: TsNode<'_>,
        docstring: Option<String>,
    ) {
        let start_line = node.start_position().row as u32;
        let end_line = node.end_position().row as u32;
        let start_column = node.start_position().column as u32;
        let end_column = node.end_position().column as u32;
        let qualified_name = format!("{}::{}", state.qualified_prefix(), name);
        let id = generate_node_id(&state.file_path, &NodeKind::Enum, name, start_line);
        let text = state.node_text(node);
        let signature = text
            .find('{')
            .map(|position| text[..position].trim().to_string());
        state.nodes.push(Node {
            id: id.clone(),
            kind: NodeKind::Enum,
            name: name.to_string(),
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
                target: id.clone(),
                kind: EdgeKind::Contains,
                line: Some(start_line),
            });
        }
        state.node_stack.push((name.to_string(), id));
        Self::extract_enum_variants(state, node);
        state.node_stack.pop();
    }
}
