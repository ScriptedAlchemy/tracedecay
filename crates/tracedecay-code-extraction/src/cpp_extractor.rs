/// Tree-sitter based C++ source code extractor.
///
/// Parses C++ source files and emits nodes and edges for the code graph.
/// Handles `.cpp`, `.cc`, `.cxx`, `.hpp`, `.hxx`, `.hh` files.
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use tree_sitter::{Node as TsNode, Parser, Tree};

use crate::traversal::{find_descendant_by_kind, find_direct_child_by_kind};
use crate::types::{
    Edge, EdgeKind, ExtractionResult, Node, NodeKind, UnresolvedRef, Visibility, generate_node_id,
};

mod functions;
mod metadata;
mod types;

/// Extracts code graph nodes and edges from C++ source files using tree-sitter.
pub struct CppExtractor;

/// Internal state used during AST traversal.
struct ExtractionState {
    nodes: Vec<Node>,
    edges: Vec<Edge>,
    unresolved_refs: Vec<UnresolvedRef>,
    errors: Vec<String>,
    /// Stack of (name, `node_id`) for building qualified names and parent edges.
    node_stack: Vec<(String, String)>,
    file_path: String,
    source: Vec<u8>,
    timestamp: u64,
    /// Current access specifier visibility inside a class/struct body.
    access_specifier: Visibility,
    /// Tracks class nesting depth (for inner classes).
    class_depth: usize,
}

impl ExtractionState {
    fn new(file_path: &str, source: &str) -> Self {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        Self {
            nodes: Vec::new(),
            edges: Vec::new(),
            unresolved_refs: Vec::new(),
            errors: Vec::new(),
            node_stack: Vec::new(),
            file_path: file_path.to_string(),
            source: source.as_bytes().to_vec(),
            timestamp,
            access_specifier: Visibility::Private,
            class_depth: 0,
        }
    }

    /// Returns the current qualified name prefix from the node stack.
    fn qualified_prefix(&self) -> String {
        let mut parts = vec![self.file_path.clone()];
        for (name, _) in &self.node_stack {
            parts.push(name.clone());
        }
        parts.join("::")
    }

    /// Returns the current parent node ID, or None if at file root level.
    fn parent_node_id(&self) -> Option<&str> {
        self.node_stack.last().map(|(_, id)| id.as_str())
    }

    /// Gets the text of a tree-sitter node from the source.
    fn node_text(&self, node: TsNode<'_>) -> String {
        node.utf8_text(&self.source)
            .unwrap_or("<invalid utf8>")
            .to_string()
    }
}

impl CppExtractor {
    /// Extract code graph nodes and edges from a C++ source file.
    pub fn extract_source(file_path: &str, source: &str) -> ExtractionResult {
        let start = Instant::now();
        let mut state = ExtractionState::new(file_path, source);

        let tree = match Self::parse_source(source) {
            Ok(tree) => tree,
            Err(msg) => {
                state.errors.push(msg);
                return Self::build_result(state, start);
            }
        };

        // Create the File root node.
        let file_node = Node {
            id: generate_node_id(file_path, &NodeKind::File, file_path, 0),
            kind: NodeKind::File,
            name: file_path.to_string(),
            qualified_name: file_path.to_string(),
            file_path: file_path.to_string(),
            start_line: 0,
            attrs_start_line: 0,
            end_line: source.lines().count().saturating_sub(1) as u32,
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
            updated_at: state.timestamp,
            parent_id: None,
        };
        let file_node_id = file_node.id.clone();
        state.nodes.push(file_node);
        state.node_stack.push((file_path.to_string(), file_node_id));

        // Walk the AST.
        let root = tree.root_node();
        Self::visit_children(&mut state, root);

        state.node_stack.pop();

        Self::build_result(state, start)
    }

    /// Parse source code into a tree-sitter AST.
    fn parse_source(source: &str) -> Result<Tree, String> {
        let mut parser = Parser::new();
        let language = crate::ts_provider::try_language("cpp")?;
        parser
            .set_language(&language)
            .map_err(|e| format!("failed to load C++ grammar: {e}"))?;
        parser
            .parse(source, None)
            .ok_or_else(|| "tree-sitter parse returned None".to_string())
    }

    /// Visit all children of a node.
    fn visit_children(state: &mut ExtractionState, node: TsNode<'_>) {
        let mut cursor = node.walk();
        if cursor.goto_first_child() {
            loop {
                let child = cursor.node();
                Self::visit_node(state, child);
                if !cursor.goto_next_sibling() {
                    break;
                }
            }
        }
    }

    /// Visit a single AST node, dispatching on its type.
    fn visit_node(state: &mut ExtractionState, node: TsNode<'_>) {
        match node.kind() {
            "function_definition" => Self::visit_function_definition(state, node),
            "declaration" => Self::visit_declaration(state, node),
            "type_definition" => Self::visit_type_definition(state, node),
            "class_specifier" => Self::visit_class_specifier(state, node),
            "struct_specifier" => Self::visit_struct_specifier(state, node),
            "union_specifier" => Self::visit_standalone_union(state, node),
            "enum_specifier" => Self::visit_standalone_enum(state, node),
            "namespace_definition" => Self::visit_namespace(state, node),
            "template_declaration" => Self::visit_template(state, node),
            "using_declaration" => Self::visit_using_declaration(state, node),
            "preproc_def" => Self::visit_preproc_def(state, node),
            "preproc_include" => Self::visit_preproc_include(state, node),
            "access_specifier" => Self::visit_access_specifier(state, node),
            _ => {
                // For other node types, skip. Comments are picked up as docstrings.
            }
        }
    }

    // -------------------------------------------------------
    // class_specifier
    // -------------------------------------------------------

    /// Visit a class specifier (default visibility: Private).
    fn visit_class_specifier(state: &mut ExtractionState, node: TsNode<'_>) {
        if find_direct_child_by_kind(node, "field_declaration_list").is_none() {
            return;
        }
        let name = find_direct_child_by_kind(node, "type_identifier")
            .map_or_else(|| "<anonymous>".to_string(), |n| state.node_text(n));

        if name == "<anonymous>" {
            return;
        }

        let docstring = Self::extract_docstring(state, node);
        Self::create_class_node(state, &name, node, docstring, true);
    }

    /// Visit a struct specifier (default visibility: Pub).
    fn visit_struct_specifier(state: &mut ExtractionState, node: TsNode<'_>) {
        if find_direct_child_by_kind(node, "field_declaration_list").is_none() {
            return;
        }
        let name = find_direct_child_by_kind(node, "type_identifier")
            .map_or_else(|| "<anonymous>".to_string(), |n| state.node_text(n));

        if name == "<anonymous>" {
            return;
        }

        let docstring = Self::extract_docstring(state, node);
        Self::create_struct_node(state, &name, node, docstring);
    }

    /// Create a Class node and walk its body.
    fn create_class_node(
        state: &mut ExtractionState,
        name: &str,
        node: TsNode<'_>,
        docstring: Option<String>,
        default_private: bool,
    ) {
        let start_line = node.start_position().row as u32;
        let end_line = node.end_position().row as u32;
        let start_column = node.start_position().column as u32;
        let end_column = node.end_position().column as u32;
        let qualified_name = format!("{}::{}", state.qualified_prefix(), name);
        let id = generate_node_id(&state.file_path, &NodeKind::Class, name, start_line);
        let text = state.node_text(node);
        let signature = text.find('{').map(|pos| text[..pos].trim().to_string());

        let graph_node = Node {
            id: id.clone(),
            kind: NodeKind::Class,
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
        // Extract base classes (inheritance).
        Self::extract_base_classes(state, node, &id);

        // Save and set access specifier state
        let old_access = state.access_specifier.clone();
        let old_depth = state.class_depth;

        state.access_specifier = if default_private {
            Visibility::Private
        } else {
            Visibility::Pub
        };
        state.class_depth += 1;

        // Walk the class body
        state.node_stack.push((name.to_string(), id.clone()));
        if let Some(body) = find_direct_child_by_kind(node, "field_declaration_list") {
            Self::visit_class_body(state, body);
        }
        state.node_stack.pop();

        // Restore state
        state.access_specifier = old_access;
        state.class_depth = old_depth;
    }

    /// Create a Struct node (C++ struct with default public).
    fn create_struct_node(
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
        let id = generate_node_id(&state.file_path, &NodeKind::Struct, name, start_line);
        let text = state.node_text(node);
        let signature = text.find('{').map(|pos| text[..pos].trim().to_string());

        let graph_node = Node {
            id: id.clone(),
            kind: NodeKind::Struct,
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
        // Extract base classes (inheritance).
        Self::extract_base_classes(state, node, &id);

        // Save and set access specifier state
        let old_access = state.access_specifier.clone();
        let old_depth = state.class_depth;

        state.access_specifier = Visibility::Pub;
        state.class_depth += 1;

        // Walk the struct body
        state.node_stack.push((name.to_string(), id.clone()));
        if let Some(body) = find_direct_child_by_kind(node, "field_declaration_list") {
            Self::visit_class_body(state, body);
        }
        state.node_stack.pop();

        // Restore state
        state.access_specifier = old_access;
        state.class_depth = old_depth;
    }

    /// Walk the body of a class/struct, handling access specifiers and members.
    fn visit_class_body(state: &mut ExtractionState, body: TsNode<'_>) {
        let mut cursor = body.walk();
        if cursor.goto_first_child() {
            loop {
                let child = cursor.node();
                match child.kind() {
                    "access_specifier" => Self::visit_access_specifier(state, child),
                    "field_declaration" => Self::visit_field_declaration(state, child),
                    "function_definition" => Self::visit_function_definition(state, child),
                    "declaration" => Self::visit_declaration(state, child),
                    "class_specifier" => Self::visit_class_specifier(state, child),
                    "struct_specifier" => Self::visit_struct_specifier(state, child),
                    "template_declaration" => Self::visit_template(state, child),
                    _ => {}
                }
                if !cursor.goto_next_sibling() {
                    break;
                }
            }
        }
    }

    /// Visit a `field_declaration` inside a class/struct body.
    fn visit_field_declaration(state: &mut ExtractionState, node: TsNode<'_>) {
        // Check if this is actually a method declaration (has a function_declarator)
        if find_descendant_by_kind(node, "function_declarator").is_some() {
            Self::visit_class_method_declaration(state, node);
            return;
        }

        // It's a field
        let name = find_descendant_by_kind(node, "field_identifier")
            .map_or_else(|| "<anonymous>".to_string(), |n| state.node_text(n));

        if name == "<anonymous>" {
            return;
        }

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

    // visit_field_method_declaration was identical to visit_class_method_declaration
    // and has been removed. Both call sites now use visit_class_method_declaration.

    // -------------------------------------------------------
    // access_specifier
    // -------------------------------------------------------

    /// Update the current access specifier based on an `access_specifier` node.
    fn visit_access_specifier(state: &mut ExtractionState, node: TsNode<'_>) {
        let text = state
            .node_text(node)
            .trim()
            .trim_end_matches(':')
            .trim()
            .to_string();
        state.access_specifier = match text.as_str() {
            "public" => Visibility::Pub,
            "private" => Visibility::Private,
            "protected" => Visibility::PubSuper,
            _ => state.access_specifier.clone(),
        };
    }

    // -------------------------------------------------------
    // namespace
    // -------------------------------------------------------

    /// Visit a namespace definition.
    fn visit_namespace(state: &mut ExtractionState, node: TsNode<'_>) {
        let name = find_direct_child_by_kind(node, "identifier")
            .or_else(|| find_direct_child_by_kind(node, "namespace_identifier"))
            .map_or_else(|| "<anonymous>".to_string(), |n| state.node_text(n));

        let docstring = Self::extract_docstring(state, node);
        let start_line = node.start_position().row as u32;
        let end_line = node.end_position().row as u32;
        let start_column = node.start_position().column as u32;
        let end_column = node.end_position().column as u32;
        let qualified_name = format!("{}::{}", state.qualified_prefix(), name);
        let id = generate_node_id(&state.file_path, &NodeKind::Namespace, &name, start_line);
        let text = state.node_text(node);
        let signature = text.find('{').map(|pos| text[..pos].trim().to_string());

        let graph_node = Node {
            id: id.clone(),
            kind: NodeKind::Namespace,
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

        // Walk namespace body
        state.node_stack.push((name, id));
        if let Some(body) = find_direct_child_by_kind(node, "declaration_list") {
            Self::visit_children(state, body);
        }
        state.node_stack.pop();
    }

    // -------------------------------------------------------
    // template
    // -------------------------------------------------------

    /// Visit a template declaration.
    fn visit_template(state: &mut ExtractionState, node: TsNode<'_>) {
        let inner_name = Self::extract_template_inner_name(state, node);
        let name = inner_name.unwrap_or_else(|| "<anonymous>".to_string());

        let docstring = Self::extract_docstring(state, node);
        let start_line = node.start_position().row as u32;
        let end_line = node.end_position().row as u32;
        let start_column = node.start_position().column as u32;
        let end_column = node.end_position().column as u32;
        let qualified_name = format!("{}::{}", state.qualified_prefix(), name);
        let id = generate_node_id(&state.file_path, &NodeKind::Template, &name, start_line);
        let text = state.node_text(node);
        let signature = text
            .find('{')
            .map(|pos| text[..pos].trim().to_string())
            .or_else(|| Some(text.trim().trim_end_matches(';').trim().to_string()));

        let graph_node = Node {
            id: id.clone(),
            kind: NodeKind::Template,
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

        // If the template wraps a function, extract call sites
        if let Some(func_def) = find_direct_child_by_kind(node, "function_definition")
            && let Some(body) = find_direct_child_by_kind(func_def, "compound_statement")
        {
            Self::extract_call_sites(state, body, &id);
        }
    }

    /// Extract the name of the inner declaration in a template.
    fn extract_template_inner_name(state: &ExtractionState, node: TsNode<'_>) -> Option<String> {
        if let Some(func_def) = find_direct_child_by_kind(node, "function_definition") {
            return Self::extract_function_name(state, func_def);
        }
        if let Some(class_spec) = find_direct_child_by_kind(node, "class_specifier") {
            return find_direct_child_by_kind(class_spec, "type_identifier")
                .map(|n| state.node_text(n));
        }
        if let Some(struct_spec) = find_direct_child_by_kind(node, "struct_specifier") {
            return find_direct_child_by_kind(struct_spec, "type_identifier")
                .map(|n| state.node_text(n));
        }
        if let Some(decl) = find_direct_child_by_kind(node, "declaration") {
            return Self::extract_function_name(state, decl);
        }
        None
    }
}
impl crate::LanguageExtractor for CppExtractor {
    fn extensions(&self) -> &[&str] {
        &["cpp", "cc", "cxx", "hpp", "hxx", "hh"]
    }

    fn language_name(&self) -> &'static str {
        "C++"
    }

    fn extract(&self, file_path: &str, source: &str) -> ExtractionResult {
        CppExtractor::extract_source(file_path, source)
    }
}
