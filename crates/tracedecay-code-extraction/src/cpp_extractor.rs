/// Tree-sitter based C++ source code extractor.
///
/// Parses C++ source files and emits nodes and edges for the code graph.
/// Handles `.cpp`, `.cc`, `.cxx`, `.hpp`, `.hxx`, `.hh` files.
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use tree_sitter::{Node as TsNode, Tree};

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
    ///
    /// The file root is pushed onto `node_stack` as the first frame when
    /// extraction begins, so iterating the stack already yields the file
    /// path as the leading segment — prepending `self.file_path` here was
    /// a leftover that duplicated the prefix (`<file>::<file>::Type::method`).
    fn qualified_prefix(&self) -> String {
        self.node_stack
            .iter()
            .map(|(name, _)| name.as_str())
            .collect::<Vec<_>>()
            .join("::")
    }

    /// Returns the current parent node ID, or None if at file root level.
    fn parent_node_id(&self) -> Option<&str> {
        self.node_stack.last().map(|(_, id)| id.as_str())
    }

    /// Borrowed text of a tree-sitter node, sliced straight from the source.
    /// Use for signature extraction on nodes with bodies so the (possibly
    /// huge) body is never materialized into an owned `String`.
    fn node_str(&self, node: TsNode<'_>) -> &str {
        node.utf8_text(&self.source).unwrap_or("<invalid utf8>")
    }

    /// Source slice from `node.start_byte()` up to `end_byte`.
    ///
    /// Callers pass a body child's start so the (possibly huge) body is
    /// never even borrowed as a `&str`, let alone copied.
    fn text_before(&self, node: TsNode<'_>, end_byte: usize) -> &str {
        let start = node.start_byte();
        let end = end_byte.min(self.source.len()).max(start);
        std::str::from_utf8(&self.source[start..end]).unwrap_or("<invalid utf8>")
    }

    /// Header text up to a known body child, trimmed.
    fn signature_before_child(&self, node: TsNode<'_>, body: TsNode<'_>) -> String {
        self.text_before(node, body.start_byte()).trim().to_string()
    }

    /// Function/type header: slice to the body child when present, otherwise
    /// own the (small) declaration without a trailing `;`.
    fn signature_up_to_body(&self, node: TsNode<'_>) -> String {
        let end = node
            .child_by_field_name("body")
            .or_else(|| find_direct_child_by_kind(node, "compound_statement"))
            .or_else(|| find_direct_child_by_kind(node, "try_statement"))
            .or_else(|| find_direct_child_by_kind(node, "field_declaration_list"))
            .or_else(|| find_direct_child_by_kind(node, "declaration_list"))
            .or_else(|| find_direct_child_by_kind(node, "enumerator_list"))
            .map(|body| body.start_byte())
            .unwrap_or_else(|| node.end_byte());
        self.text_before(node, end)
            .trim()
            .trim_end_matches(';')
            .trim()
            .to_string()
    }
}

impl CppExtractor {
    pub fn extract_source(file_path: &str, source: &str) -> ExtractionResult {
        let tree = match Self::parse_source(source) {
            Ok(tree) => tree,
            Err(msg) => {
                let start = Instant::now();
                let mut state = ExtractionState::new(file_path, source);
                state.errors.push(msg);
                return Self::build_result(state, start);
            }
        };
        Self::extract_tree(
            file_path,
            source,
            &tree,
            crate::parsed_extraction::ParsedExtractionScope::FullDocument,
        )
        .result
    }

    #[hotpath::measure(label = "code_extraction.cpp.extract_tree")]
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

    /// Parse source code into a tree-sitter AST.
    #[hotpath::measure(label = "code_extraction.cpp.parse_source")]
    fn parse_source(source: &str) -> Result<Tree, String> {
        crate::ts_provider::parse_extractor_source("cpp", "C++", source)
    }

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
            // Comments are picked up as docstrings by the definitions they precede.
            _ => {}
        }
    }

    fn visit_class_specifier(state: &mut ExtractionState, node: TsNode<'_>) {
        if find_direct_child_by_kind(node, "field_declaration_list").is_none() {
            return;
        }
        let name = find_direct_child_by_kind(node, "type_identifier").map_or_else(
            || "<anonymous>".to_string(),
            |n| state.node_str(n).to_string(),
        );

        if name == "<anonymous>" {
            return;
        }

        let docstring = Self::extract_docstring(state, node);
        Self::create_record_node(
            state,
            &name,
            node,
            docstring,
            NodeKind::Class,
            Visibility::Private,
        );
    }

    fn visit_struct_specifier(state: &mut ExtractionState, node: TsNode<'_>) {
        if find_direct_child_by_kind(node, "field_declaration_list").is_none() {
            return;
        }
        let name = find_direct_child_by_kind(node, "type_identifier").map_or_else(
            || "<anonymous>".to_string(),
            |n| state.node_str(n).to_string(),
        );

        if name == "<anonymous>" {
            return;
        }

        let docstring = Self::extract_docstring(state, node);
        Self::create_record_node(
            state,
            &name,
            node,
            docstring,
            NodeKind::Struct,
            Visibility::Pub,
        );
    }

    /// C++ classes default members to private access, structs to public;
    /// the record bodies are otherwise handled identically.
    fn create_record_node(
        state: &mut ExtractionState,
        name: &str,
        node: TsNode<'_>,
        docstring: Option<String>,
        kind: NodeKind,
        default_access: Visibility,
    ) {
        let start_line = node.start_position().row as u32;
        let end_line = node.end_position().row as u32;
        let start_column = node.start_position().column as u32;
        let end_column = node.end_position().column as u32;
        let qualified_name = format!("{}::{}", state.qualified_prefix(), name);
        let id = generate_node_id(&state.file_path, &kind, name, start_line);
        let signature = find_direct_child_by_kind(node, "field_declaration_list")
            .map(|body| state.signature_before_child(node, body));

        let graph_node = Node {
            id: id.clone(),
            kind,
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
        Self::extract_base_classes(state, node, &id);

        let old_access = state.access_specifier.clone();
        let old_depth = state.class_depth;

        state.access_specifier = default_access;
        state.class_depth += 1;

        state.node_stack.push((name.to_string(), id.clone()));
        if let Some(body) = find_direct_child_by_kind(node, "field_declaration_list") {
            Self::visit_class_body(state, body);
        }
        state.node_stack.pop();

        state.access_specifier = old_access;
        state.class_depth = old_depth;
    }

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

    fn visit_field_declaration(state: &mut ExtractionState, node: TsNode<'_>) {
        // Check if this is actually a method declaration (has a function_declarator)
        if find_descendant_by_kind(node, "function_declarator").is_some() {
            Self::visit_class_method_declaration(state, node);
            return;
        }

        let name = find_descendant_by_kind(node, "field_identifier").map_or_else(
            || "<anonymous>".to_string(),
            |n| state.node_str(n).to_string(),
        );

        if name == "<anonymous>" {
            return;
        }

        let text = state.node_str(node);
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

    fn visit_access_specifier(state: &mut ExtractionState, node: TsNode<'_>) {
        let text = state.node_str(node).trim().trim_end_matches(':').trim();
        state.access_specifier = match text {
            "public" => Visibility::Pub,
            "private" => Visibility::Private,
            "protected" => Visibility::PubSuper,
            _ => state.access_specifier.clone(),
        };
    }

    fn visit_namespace(state: &mut ExtractionState, node: TsNode<'_>) {
        let name = find_direct_child_by_kind(node, "identifier")
            .or_else(|| find_direct_child_by_kind(node, "namespace_identifier"))
            .map_or_else(
                || "<anonymous>".to_string(),
                |n| state.node_str(n).to_string(),
            );

        let docstring = Self::extract_docstring(state, node);
        let start_line = node.start_position().row as u32;
        let end_line = node.end_position().row as u32;
        let start_column = node.start_position().column as u32;
        let end_column = node.end_position().column as u32;
        let qualified_name = format!("{}::{}", state.qualified_prefix(), name);
        let id = generate_node_id(&state.file_path, &NodeKind::Namespace, &name, start_line);
        let signature = find_direct_child_by_kind(node, "declaration_list")
            .map(|body| state.signature_before_child(node, body));

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

        state.node_stack.push((name, id));
        if let Some(body) = find_direct_child_by_kind(node, "declaration_list") {
            Self::visit_children(state, body);
        }
        state.node_stack.pop();
    }

    fn visit_template(state: &mut ExtractionState, node: TsNode<'_>) {
        let inner_name = Self::extract_template_inner_name(state, node);
        let name = inner_name.unwrap_or("<anonymous>").to_string();

        let docstring = Self::extract_docstring(state, node);
        let start_line = node.start_position().row as u32;
        let end_line = node.end_position().row as u32;
        let start_column = node.start_position().column as u32;
        let end_column = node.end_position().column as u32;
        let qualified_name = format!("{}::{}", state.qualified_prefix(), name);
        let id = generate_node_id(&state.file_path, &NodeKind::Template, &name, start_line);
        let inner_body = find_direct_child_by_kind(node, "function_definition")
            .and_then(|func| {
                func.child_by_field_name("body")
                    .or_else(|| find_direct_child_by_kind(func, "compound_statement"))
            })
            .or_else(|| {
                find_direct_child_by_kind(node, "class_specifier")
                    .or_else(|| find_direct_child_by_kind(node, "struct_specifier"))
                    .and_then(|spec| find_direct_child_by_kind(spec, "field_declaration_list"))
            });
        let signature = Some(inner_body.map_or_else(
            || state.signature_up_to_body(node),
            |body| state.signature_before_child(node, body),
        ));

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

        if let Some(func_def) = find_direct_child_by_kind(node, "function_definition")
            && let Some(body) = find_direct_child_by_kind(func_def, "compound_statement")
        {
            Self::extract_call_sites(state, body, &id);
        }
    }

    fn extract_template_inner_name<'a>(
        state: &'a ExtractionState,
        node: TsNode<'_>,
    ) -> Option<&'a str> {
        if let Some(func_def) = find_direct_child_by_kind(node, "function_definition") {
            return Self::extract_function_name(state, func_def);
        }
        if let Some(class_spec) = find_direct_child_by_kind(node, "class_specifier") {
            return find_direct_child_by_kind(class_spec, "type_identifier")
                .map(|n| state.node_str(n));
        }
        if let Some(struct_spec) = find_direct_child_by_kind(node, "struct_specifier") {
            return find_direct_child_by_kind(struct_spec, "type_identifier")
                .map(|n| state.node_str(n));
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

    #[hotpath::measure(label = "code_extraction.cpp.extract")]
    fn extract(&self, file_path: &str, source: &str) -> ExtractionResult {
        CppExtractor::extract_source(file_path, source)
    }

    #[hotpath::measure(label = "code_extraction.cpp.extract_parsed")]
    fn extract_parsed(
        &self,
        file_path: &str,
        source: &str,
        tree: &Tree,
        scope: crate::parsed_extraction::ParsedExtractionScope<'_>,
    ) -> crate::parsed_extraction::ParsedExtraction {
        CppExtractor::extract_tree(file_path, source, tree, scope)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        incremental::ParseChangedRange,
        parsed_extraction::{ParsedExtractionDisposition, ParsedExtractionScope},
    };

    #[test]
    fn parsed_extraction_limits_cpp_to_changed_top_level_declaration() {
        let source = "int untouched() { return 1; }\n\nint edited() { return 2; }\n";
        let tree = CppExtractor::parse_source(source).expect("parse C++ source");
        let root = tree.root_node();
        let mut cursor = root.walk();
        let edited = root
            .children(&mut cursor)
            .find(|node| {
                node.kind() == "function_definition"
                    && source[node.start_byte()..node.end_byte()].contains("edited")
            })
            .expect("edited top-level C++ declaration");
        let range = ParseChangedRange {
            start_byte: edited.start_byte().saturating_add(1),
            end_byte: edited.end_byte().saturating_sub(1),
            start_position: edited.start_position().into(),
            end_position: edited.end_position().into(),
        };

        let extracted = crate::LanguageExtractor::extract_parsed(
            &CppExtractor,
            "sample.cpp",
            source,
            &tree,
            ParsedExtractionScope::ChangedRegions(&[range]),
        );

        assert_eq!(
            extracted.disposition,
            ParsedExtractionDisposition::ChangedRegions
        );
        assert_eq!(extracted.metrics.visited_top_level_nodes, 1);
        assert_eq!(
            extracted.metrics.visited_bytes,
            edited.end_byte() - edited.start_byte()
        );
        let functions = extracted
            .result
            .nodes
            .iter()
            .filter(|node| node.kind == NodeKind::Function)
            .map(|node| node.name.as_str())
            .collect::<Vec<_>>();
        assert_eq!(functions, vec!["edited"]);
    }
}
