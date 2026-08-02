use tree_sitter::Node as TsNode;

use super::{CppExtractor, ExtractionState};
use crate::{
    complexity::{CPP_COMPLEXITY, count_complexity},
    traversal::{find_descendant_by_kind, find_direct_child_by_kind, has_direct_child_kind},
    types::{Edge, EdgeKind, Node, NodeKind, Visibility, generate_node_id},
};

impl CppExtractor {
    /// Extract a function definition (has a body).
    pub(super) fn visit_function_definition(state: &mut ExtractionState, node: TsNode<'_>) {
        let in_class = state.class_depth > 0;
        if in_class {
            if Self::is_constructor(state, node) {
                Self::visit_constructor(state, node);
                return;
            }
            if Self::is_destructor(state, node) {
                Self::visit_destructor(state, node);
                return;
            }
        }
        let is_static = Self::has_storage_class(state, node, "static");
        let is_pure_virtual = Self::is_pure_virtual(state, node);
        let visibility = if in_class {
            state.access_specifier.clone()
        } else if is_static {
            Visibility::Private
        } else {
            Visibility::Pub
        };
        let name =
            Self::extract_function_name(state, node).unwrap_or_else(|| "<anonymous>".to_string());
        let signature = Some(Self::extract_function_signature(state, node));
        let docstring = Self::extract_docstring(state, node);
        let start_line = node.start_position().row as u32;
        let end_line = node.end_position().row as u32;
        let start_column = node.start_position().column as u32;
        let end_column = node.end_position().column as u32;
        let qualified_name = format!("{}::{}", state.qualified_prefix(), name);
        let kind = if in_class {
            if is_pure_virtual {
                NodeKind::AbstractMethod
            } else {
                NodeKind::Method
            }
        } else {
            NodeKind::Function
        };
        let id = generate_node_id(&state.file_path, &kind, &name, start_line);
        let metrics = count_complexity(node, &CPP_COMPLEXITY, &state.source);
        let graph_node = Node {
            id: id.clone(),
            kind,
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
            visibility,
            is_async: false,
            branches: metrics.branches,
            loops: metrics.loops,
            returns: metrics.returns,
            max_nesting: metrics.max_nesting,
            unsafe_blocks: metrics.unsafe_blocks,
            unchecked_calls: metrics.unchecked_calls,
            assertions: metrics.assertions,
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
        Self::extract_annotations(state, node, &id);
        if let Some(body) = find_direct_child_by_kind(node, "compound_statement") {
            Self::extract_call_sites(state, body, &id);
        }
    }

    pub(super) fn is_constructor(state: &ExtractionState, node: TsNode<'_>) -> bool {
        let name = Self::extract_function_name(state, node);
        if let Some(name) = &name
            && let Some((class_name, _)) = state.node_stack.last()
            && name == class_name
        {
            return true;
        }
        false
    }

    pub(super) fn is_destructor(state: &ExtractionState, node: TsNode<'_>) -> bool {
        let name = Self::extract_function_name(state, node);
        if let Some(name) = &name
            && name.starts_with('~')
        {
            return true;
        }
        find_descendant_by_kind(node, "destructor_name").is_some()
    }

    pub(super) fn visit_constructor(state: &mut ExtractionState, node: TsNode<'_>) {
        let name =
            Self::extract_function_name(state, node).unwrap_or_else(|| "<anonymous>".to_string());
        let signature = Some(Self::extract_function_signature(state, node));
        let docstring = Self::extract_docstring(state, node);
        let start_line = node.start_position().row as u32;
        let end_line = node.end_position().row as u32;
        let start_column = node.start_position().column as u32;
        let end_column = node.end_position().column as u32;
        let qualified_name = format!("{}::{}", state.qualified_prefix(), name);
        let id = generate_node_id(&state.file_path, &NodeKind::Constructor, &name, start_line);
        let metrics = count_complexity(node, &CPP_COMPLEXITY, &state.source);
        let graph_node = Node {
            id: id.clone(),
            kind: NodeKind::Constructor,
            name,
            qualified_name,
            file_path: state.file_path.clone(),
            start_line,
            attrs_start_line: start_line,
            end_line,
            start_column,
            end_column,
            signature,
            docstring,
            visibility: state.access_specifier.clone(),
            is_async: false,
            branches: metrics.branches,
            loops: metrics.loops,
            returns: metrics.returns,
            max_nesting: metrics.max_nesting,
            unsafe_blocks: metrics.unsafe_blocks,
            unchecked_calls: metrics.unchecked_calls,
            assertions: metrics.assertions,
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
        if let Some(body) = find_direct_child_by_kind(node, "compound_statement") {
            Self::extract_call_sites(state, body, &id);
        }
    }

    pub(super) fn visit_destructor(state: &mut ExtractionState, node: TsNode<'_>) {
        let name = Self::extract_function_name(state, node)
            .unwrap_or_else(|| Self::extract_destructor_name(state, node));
        let signature = Some(Self::extract_function_signature(state, node));
        let docstring = Self::extract_docstring(state, node);
        let start_line = node.start_position().row as u32;
        let end_line = node.end_position().row as u32;
        let start_column = node.start_position().column as u32;
        let end_column = node.end_position().column as u32;
        let qualified_name = format!("{}::{}", state.qualified_prefix(), name);
        let id = generate_node_id(&state.file_path, &NodeKind::Method, &name, start_line);
        let metrics = count_complexity(node, &CPP_COMPLEXITY, &state.source);
        let graph_node = Node {
            id: id.clone(),
            kind: NodeKind::Method,
            name,
            qualified_name,
            file_path: state.file_path.clone(),
            start_line,
            attrs_start_line: start_line,
            end_line,
            start_column,
            end_column,
            signature,
            docstring,
            visibility: state.access_specifier.clone(),
            is_async: false,
            branches: metrics.branches,
            loops: metrics.loops,
            returns: metrics.returns,
            max_nesting: metrics.max_nesting,
            unsafe_blocks: metrics.unsafe_blocks,
            unchecked_calls: metrics.unchecked_calls,
            assertions: metrics.assertions,
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
        if let Some(body) = find_direct_child_by_kind(node, "compound_statement") {
            Self::extract_call_sites(state, body, &id);
        }
    }

    pub(super) fn extract_destructor_name(state: &ExtractionState, node: TsNode<'_>) -> String {
        if let Some(dtor) = find_descendant_by_kind(node, "destructor_name") {
            return state.node_text(dtor);
        }
        if let Some((class_name, _)) = state.node_stack.last() {
            return format!("~{class_name}");
        }
        "~<unknown>".to_string()
    }

    pub(super) fn extract_function_name(
        state: &ExtractionState,
        node: TsNode<'_>,
    ) -> Option<String> {
        if let Some(declarator) = find_descendant_by_kind(node, "function_declarator") {
            if let Some(dtor) = find_direct_child_by_kind(declarator, "destructor_name") {
                return Some(state.node_text(dtor));
            }
            if let Some(ident) = find_direct_child_by_kind(declarator, "identifier") {
                return Some(state.node_text(ident));
            }
            if let Some(ident) = find_direct_child_by_kind(declarator, "field_identifier") {
                return Some(state.node_text(ident));
            }
            if let Some(qi) = find_direct_child_by_kind(declarator, "qualified_identifier")
                && let Some(ident) = find_direct_child_by_kind(qi, "identifier")
            {
                return Some(state.node_text(ident));
            }
            if let Some(ident) = find_direct_child_by_kind(declarator, "parenthesized_declarator")
                && let Some(inner_ident) = find_descendant_by_kind(ident, "identifier")
            {
                return Some(state.node_text(inner_ident));
            }
            if let Some(ident) = find_direct_child_by_kind(declarator, "type_identifier") {
                return Some(state.node_text(ident));
            }
        }
        None
    }

    pub(super) fn extract_function_signature(state: &ExtractionState, node: TsNode<'_>) -> String {
        let text = state.node_text(node);
        if let Some(brace_pos) = text.find('{') {
            text[..brace_pos].trim().to_string()
        } else {
            text.trim().trim_end_matches(';').trim().to_string()
        }
    }

    pub(super) fn visit_declaration(state: &mut ExtractionState, node: TsNode<'_>) {
        let in_class = state.class_depth > 0;
        if has_direct_child_kind(node, "class_specifier")
            || has_direct_child_kind(node, "struct_specifier")
            || has_direct_child_kind(node, "union_specifier")
            || has_direct_child_kind(node, "enum_specifier")
        {
            Self::visit_children(state, node);
            return;
        }
        if find_descendant_by_kind(node, "function_declarator").is_some() {
            if in_class {
                Self::visit_class_method_declaration(state, node);
            } else {
                Self::visit_function_prototype(state, node);
            }
            return;
        }
        if in_class {
            Self::visit_field_declaration_from_declaration(state, node);
            return;
        }
        Self::visit_global_variable(state, node);
    }

    pub(super) fn visit_class_method_declaration(state: &mut ExtractionState, node: TsNode<'_>) {
        let is_pure_virtual = Self::is_pure_virtual(state, node);
        let name =
            Self::extract_function_name(state, node).unwrap_or_else(|| "<anonymous>".to_string());
        if let Some((class_name, _)) = state.node_stack.last()
            && name == *class_name
        {
            let text = state.node_text(node);
            let signature = Some(text.trim().trim_end_matches(';').trim().to_string());
            let docstring = Self::extract_docstring(state, node);
            let start_line = node.start_position().row as u32;
            let end_line = node.end_position().row as u32;
            let start_column = node.start_position().column as u32;
            let end_column = node.end_position().column as u32;
            let qualified_name = format!("{}::{}", state.qualified_prefix(), name);
            let id = generate_node_id(&state.file_path, &NodeKind::Constructor, &name, start_line);
            let metrics = count_complexity(node, &CPP_COMPLEXITY, &state.source);
            let graph_node = Node {
                id: id.clone(),
                kind: NodeKind::Constructor,
                name,
                qualified_name,
                file_path: state.file_path.clone(),
                start_line,
                attrs_start_line: start_line,
                end_line,
                start_column,
                end_column,
                signature,
                docstring,
                visibility: state.access_specifier.clone(),
                is_async: false,
                branches: metrics.branches,
                loops: metrics.loops,
                returns: metrics.returns,
                max_nesting: metrics.max_nesting,
                unsafe_blocks: metrics.unsafe_blocks,
                unchecked_calls: metrics.unchecked_calls,
                assertions: metrics.assertions,
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
            return;
        }
        let text = state.node_text(node);
        let signature = Some(text.trim().trim_end_matches(';').trim().to_string());
        let docstring = Self::extract_docstring(state, node);
        let start_line = node.start_position().row as u32;
        let end_line = node.end_position().row as u32;
        let start_column = node.start_position().column as u32;
        let end_column = node.end_position().column as u32;
        let qualified_name = format!("{}::{}", state.qualified_prefix(), name);
        let kind = if is_pure_virtual {
            NodeKind::AbstractMethod
        } else {
            NodeKind::Method
        };
        let id = generate_node_id(&state.file_path, &kind, &name, start_line);
        let metrics = count_complexity(node, &CPP_COMPLEXITY, &state.source);
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
            signature,
            docstring,
            visibility: state.access_specifier.clone(),
            is_async: false,
            branches: metrics.branches,
            loops: metrics.loops,
            returns: metrics.returns,
            max_nesting: metrics.max_nesting,
            unsafe_blocks: metrics.unsafe_blocks,
            unchecked_calls: metrics.unchecked_calls,
            assertions: metrics.assertions,
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
        Self::extract_annotations(state, node, &id);
    }

    pub(super) fn visit_field_declaration_from_declaration(
        state: &mut ExtractionState,
        node: TsNode<'_>,
    ) {
        let Some(name) = Self::extract_variable_name(state, node) else {
            return;
        };
        let text = state.node_text(node);
        let start_line = node.start_position().row as u32;
        let end_line = node.end_position().row as u32;
        let start_column = node.start_position().column as u32;
        let end_column = node.end_position().column as u32;
        let qualified_name = format!("{}::{}", state.qualified_prefix(), name);
        let id = generate_node_id(&state.file_path, &NodeKind::Field, &name, start_line);
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
            signature: Some(text.trim().trim_end_matches(';').trim().to_string()),
            docstring: None,
            visibility: state.access_specifier.clone(),
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

    pub(super) fn visit_function_prototype(state: &mut ExtractionState, node: TsNode<'_>) {
        let is_static = Self::has_storage_class(state, node, "static");
        let visibility = if is_static {
            Visibility::Private
        } else {
            Visibility::Pub
        };
        let name =
            Self::extract_function_name(state, node).unwrap_or_else(|| "<anonymous>".to_string());
        let text = state.node_text(node);
        let signature = Some(text.trim().trim_end_matches(';').trim().to_string());
        let docstring = Self::extract_docstring(state, node);
        let start_line = node.start_position().row as u32;
        let end_line = node.end_position().row as u32;
        let start_column = node.start_position().column as u32;
        let end_column = node.end_position().column as u32;
        let qualified_name = format!("{}::{}", state.qualified_prefix(), name);
        let id = generate_node_id(&state.file_path, &NodeKind::Function, &name, start_line);
        let graph_node = Node {
            id: id.clone(),
            kind: NodeKind::Function,
            name,
            qualified_name,
            file_path: state.file_path.clone(),
            start_line,
            attrs_start_line: start_line,
            end_line,
            start_column,
            end_column,
            signature,
            docstring,
            visibility,
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
        Self::extract_annotations(state, node, &id);
    }

    pub(super) fn visit_global_variable(state: &mut ExtractionState, node: TsNode<'_>) {
        let is_static = Self::has_storage_class(state, node, "static");
        let visibility = if is_static {
            Visibility::Private
        } else {
            Visibility::Pub
        };
        let Some(name) = Self::extract_variable_name(state, node) else {
            return;
        };
        let text = state.node_text(node);
        let signature = Some(text.trim().trim_end_matches(';').trim().to_string());
        let start_line = node.start_position().row as u32;
        let end_line = node.end_position().row as u32;
        let start_column = node.start_position().column as u32;
        let end_column = node.end_position().column as u32;
        let qualified_name = format!("{}::{}", state.qualified_prefix(), name);
        let id = generate_node_id(&state.file_path, &NodeKind::Static, &name, start_line);
        let graph_node = Node {
            id: id.clone(),
            kind: NodeKind::Static,
            name,
            qualified_name,
            file_path: state.file_path.clone(),
            start_line,
            attrs_start_line: start_line,
            end_line,
            start_column,
            end_column,
            signature,
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

    pub(super) fn extract_variable_name(
        state: &ExtractionState,
        node: TsNode<'_>,
    ) -> Option<String> {
        if let Some(init_decl) = find_direct_child_by_kind(node, "init_declarator") {
            if let Some(ident) = find_direct_child_by_kind(init_decl, "identifier") {
                return Some(state.node_text(ident));
            }
            if let Some(ptr_decl) = find_direct_child_by_kind(init_decl, "pointer_declarator")
                && let Some(ident) = find_direct_child_by_kind(ptr_decl, "identifier")
            {
                return Some(state.node_text(ident));
            }
        }
        if let Some(ident) = find_direct_child_by_kind(node, "identifier") {
            return Some(state.node_text(ident));
        }
        if let Some(ptr_decl) = find_direct_child_by_kind(node, "pointer_declarator")
            && let Some(ident) = find_direct_child_by_kind(ptr_decl, "identifier")
        {
            return Some(state.node_text(ident));
        }
        None
    }
}
