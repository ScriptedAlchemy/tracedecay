/// Tree-sitter based TypeScript/JavaScript source code extractor.
///
/// Parses TypeScript (.ts, .tsx) and JavaScript (.js, .jsx) source files and
/// emits nodes and edges for the code graph.
use std::time::Instant;

use tree_sitter::{Node as TsNode, Tree};

use crate::common::{declaration_start, local_node_id};
use crate::complexity::{TYPESCRIPT_COMPLEXITY, count_complexity};
use crate::extraction_artifact::{ExtractedImportEvidenceV1, ExtractionArtifactV1};
use crate::traversal::find_direct_child_by_kind;
use crate::types::{
    ComplexityAnalysisV1, Edge, EdgeKind, ExtractionResult, Node, NodeKind, UnresolvedRef,
    Visibility, generate_node_id,
};

mod imports;
mod test_calls;

pub use test_calls::is_test_framework_call_signature;

/// Extracts code graph nodes and edges from TypeScript/JavaScript source files
/// using tree-sitter.
pub struct TypeScriptExtractor;

#[derive(Default)]
struct ShadowedCallNames {
    names: Vec<String>,
}

/// Internal state used during AST traversal.
///
/// Borrows the caller's source for the lifetime of the walk: copying the
/// whole file here made every `extract_parsed` pass, including incremental
/// walks of one tiny item, pay a full-file memcpy before visiting a node.
struct ExtractionState<'s> {
    nodes: Vec<Node>,
    edges: Vec<Edge>,
    unresolved_refs: Vec<UnresolvedRef>,
    errors: Vec<String>,
    imports: Vec<ExtractedImportEvidenceV1>,
    /// Stack of (name, `node_id`) for building qualified names and parent edges.
    node_stack: Vec<(String, String)>,
    file_path: String,
    source: &'s [u8],
    timestamp: u64,
    /// Whether the current declaration is inside an `export_statement`.
    in_export: bool,
}

impl<'s> ExtractionState<'s> {
    fn new(file_path: &str, source: &'s str) -> Self {
        let timestamp = crate::common::unix_timestamp_secs();
        Self {
            nodes: Vec::new(),
            edges: Vec::new(),
            unresolved_refs: Vec::new(),
            errors: Vec::new(),
            imports: Vec::new(),
            node_stack: Vec::new(),
            file_path: file_path.to_string(),
            source: source.as_bytes(),
            timestamp,
            in_export: false,
        }
    }

    /// Returns the current qualified name prefix from the node stack.
    ///
    /// The file root is pushed onto `node_stack` as the first frame when
    /// extraction begins, so iterating the stack already yields the file
    /// path as the leading segment.
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

    /// Gets the text of a tree-sitter node, borrowed from the source.
    ///
    /// The `'s` lifetime is tied to the source, not `&self`, so callers can
    /// keep the text across mutations of the state. Signature helpers slice
    /// a small prefix from it and own only that prefix.
    fn node_text(&self, node: TsNode<'_>) -> &'s str {
        node.utf8_text(self.source).unwrap_or("<invalid utf8>")
    }
}

impl TypeScriptExtractor {
    pub(crate) fn extract_typescript_artifact(
        file_path: &str,
        source: &str,
    ) -> ExtractionArtifactV1 {
        let ext = file_path.rsplit('.').next().unwrap_or("ts");
        let tree = match Self::parse_source(source, ext) {
            Ok(tree) => tree,
            Err(msg) => {
                let start = Instant::now();
                let mut state = ExtractionState::new(file_path, source);
                state.errors.push(msg);
                return Self::build_artifact(state, start);
            }
        };
        Self::extract_tree_artifact(
            file_path,
            source,
            &tree,
            crate::parsed_extraction::ParsedExtractionScope::FullDocument,
        )
        .with_conservative_clone_bodies(
            &tree,
            source,
            match ext {
                "tsx" => "tsx",
                "js" | "jsx" => "javascript",
                _ => "typescript",
            },
            file_path,
        )
        .artifact
    }

    fn extract_tree_artifact(
        file_path: &str,
        source: &str,
        tree: &Tree,
        scope: crate::parsed_extraction::ParsedExtractionScope<'_>,
    ) -> crate::parsed_extraction::ParsedExtractionArtifactV1 {
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
        state
            .node_stack
            .push((file_path.to_string(), file_node_id.clone()));

        let metrics = crate::parsed_extraction::visit_root_children(tree, scope, |child| {
            Self::visit_node(&mut state, child);
            Self::visit_module_scope_calls(&mut state, &file_node_id, child);
        });

        state.node_stack.pop();

        crate::parsed_extraction::ParsedExtractionArtifactV1::complete(
            Self::build_artifact(state, start),
            scope,
            metrics,
        )
    }

    fn node_name(state: &ExtractionState<'_>, node: TsNode<'_>) -> String {
        Self::clean_name(state.node_text(node))
    }

    fn child_name(state: &ExtractionState<'_>, node: Option<TsNode<'_>>) -> String {
        node.map_or_else(Self::anonymous_name, |n| Self::node_name(state, n))
    }

    fn clean_name(name: &str) -> String {
        let name = name.trim();
        if name.is_empty() {
            Self::anonymous_name()
        } else {
            name.to_string()
        }
    }

    fn anonymous_name() -> String {
        "<anonymous>".to_string()
    }

    /// Parse source code into a tree-sitter AST, selecting grammar by file extension.
    fn parse_source(source: &str, extension: &str) -> Result<Tree, String> {
        let (key, label) = match extension {
            "ts" | "astro" | "svelte" => ("typescript", "TypeScript"),
            "tsx" => ("tsx", "TSX"),
            "js" | "jsx" => ("javascript", "JavaScript"),
            other => (other, other),
        };
        crate::ts_provider::parse_extractor_source_with_labeled_lookup(key, label, source)
    }

    /// Visit all children of a node.
    fn visit_children(state: &mut ExtractionState<'_>, node: TsNode<'_>) {
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
    fn visit_node(state: &mut ExtractionState<'_>, node: TsNode<'_>) {
        match node.kind() {
            "export_statement" => Self::visit_export_statement(state, node),
            "function_declaration" | "generator_function_declaration" => {
                Self::visit_function(state, node);
            }
            "lexical_declaration" | "variable_declaration" => {
                Self::visit_lexical_declaration(state, node, None);
            }
            "class_declaration" | "abstract_class_declaration" => Self::visit_class(state, node),
            "interface_declaration" => Self::visit_interface(state, node),
            "enum_declaration" => Self::visit_enum(state, node),
            "type_alias_declaration" => Self::visit_type_alias(state, node),
            "internal_module" | "module" => Self::visit_namespace(state, node),
            "import_statement" => imports::visit_import(state, node),
            "expression_statement" => {
                // Namespace declarations appear as expression_statement > internal_module.
                if let Some(internal) = find_direct_child_by_kind(node, "internal_module") {
                    Self::visit_namespace(state, internal);
                } else if let Some(call) = find_direct_child_by_kind(node, "call_expression") {
                    // Test-framework calls (describe/it/test/…) carry their body in
                    // a callback argument, which is otherwise invisible to the
                    // graph. Attribute those callbacks so tests map to sources.
                    if test_calls::is_test_framework_call(state, call) {
                        test_calls::visit_test_call(state, call);
                    }
                }
            }
            _ => {
                // For other node types, skip. Children are visited explicitly
                // by the specific visit_* methods when needed.
            }
        }
    }

    /// Whether no symbol visit owns a module- or namespace-scope statement's
    /// calls: `createRoot(el).render(<App />)`,
    /// `export default defineConfig({ plugins: [react()] })`. Declarations own
    /// their calls, and namespaces and test-framework calls are visited as
    /// their own symbols.
    fn has_unowned_calls(state: &ExtractionState<'_>, statement: TsNode<'_>) -> bool {
        match statement.kind() {
            "expression_statement" => {
                find_direct_child_by_kind(statement, "internal_module").is_none()
                    && !find_direct_child_by_kind(statement, "call_expression")
                        .is_some_and(|call| test_calls::is_test_framework_call(state, call))
            }
            "export_statement" => statement.child_by_field_name("declaration").is_none(),
            "import_statement" | "comment" | "internal_module" | "module" => false,
            kind => !kind.ends_with("declaration"),
        }
    }

    /// Give a module-scope statement's unowned calls a `<module>` owner.
    fn visit_module_scope_calls(
        state: &mut ExtractionState<'_>,
        file_node_id: &str,
        statement: TsNode<'_>,
    ) {
        if !Self::has_unowned_calls(state, statement) {
            return;
        }
        let (owner, contains) = crate::common::module_scope_init_block(
            &state.file_path,
            state.source,
            file_node_id,
            statement,
            state.timestamp,
        );
        let before = state.unresolved_refs.len();
        Self::extract_owned_call_sites(state, statement, &owner.id);
        if state.unresolved_refs.len() > before {
            state.nodes.push(owner);
            state.edges.push(contains);
        }
    }

    /// Visit an `export_statement`. Sets `in_export` flag and recurses into the
    /// inner declaration.
    fn visit_export_statement(state: &mut ExtractionState<'_>, node: TsNode<'_>) {
        let prev_in_export = state.in_export;
        state.in_export = true;

        let start_line = node.start_position().row as u32;
        imports::visit_reexport(state, node);
        imports::visit_default_export(state, node);

        let mut cursor = node.walk();
        if cursor.goto_first_child() {
            loop {
                let child = cursor.node();
                match child.kind() {
                    "function_declaration" | "generator_function_declaration" => {
                        Self::visit_function(state, child);
                    }
                    "class_declaration" | "abstract_class_declaration" => {
                        Self::visit_class(state, child);
                    }
                    "interface_declaration" => Self::visit_interface(state, child),
                    "enum_declaration" => Self::visit_enum(state, child),
                    "type_alias_declaration" => Self::visit_type_alias(state, child),
                    "internal_module" | "module" => Self::visit_namespace(state, child),
                    "lexical_declaration" | "variable_declaration" => {
                        Self::visit_lexical_declaration(state, child, None);
                    }
                    // Re-export or bare export like `export { foo }`
                    "export_clause" => {
                        let text = state.node_text(node);
                        let name = "export";
                        let qualified_name = format!("{}::{}", state.qualified_prefix(), name);
                        let id = local_node_id(
                            &state.file_path,
                            state.source,
                            &NodeKind::Export,
                            text,
                            node,
                        );
                        let graph_node = Node {
                            id: id.clone(),
                            kind: NodeKind::Export,
                            name: name.to_string(),
                            qualified_name,
                            file_path: state.file_path.clone(),
                            start_line,
                            attrs_start_line: start_line,
                            end_line: node.end_position().row as u32,
                            start_column: node.start_position().column as u32,
                            end_column: node.end_position().column as u32,
                            signature: Some(text.to_string()),
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
                    _ => {}
                }
                if !cursor.goto_next_sibling() {
                    break;
                }
            }
        }

        state.in_export = prev_in_export;
    }

    /// Extract a function declaration node.
    fn visit_function(state: &mut ExtractionState<'_>, node: TsNode<'_>) {
        let name = Self::child_name(state, find_direct_child_by_kind(node, "identifier"));
        let visibility = if state.in_export {
            Visibility::Pub
        } else {
            Visibility::Private
        };
        let is_async = Self::has_child_kind(node, "async");
        let signature = Some(Self::extract_signature(state, node));
        let docstring = Self::extract_jsdoc(state, node);
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
        let metrics = count_complexity(node, &TYPESCRIPT_COMPLEXITY, state.source);

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
            visibility,
            is_async,
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

        Self::extract_type_refs(state, node, &id);

        // The body and default parameter values (`x = defaults()`) run on
        // each call.
        Self::extract_owned_call_sites(state, node, &id);
    }

    /// Extract a `const`/`let`/`var` declaration, looking for arrow functions
    /// and variable declarations. An arrow binding owns its body's calls;
    /// other initializer calls belong to `initializer_owner` when given (a
    /// test callback that maps its setup to sources), else to the binding.
    fn visit_lexical_declaration(
        state: &mut ExtractionState<'_>,
        node: TsNode<'_>,
        initializer_owner: Option<&str>,
    ) {
        let variable_kind = if Self::has_child_kind(node, "const") {
            NodeKind::Const
        } else {
            NodeKind::VarField
        };

        let mut cursor = node.walk();
        for declarator in node
            .named_children(&mut cursor)
            .filter(|child| child.kind() == "variable_declarator")
        {
            if let Some(arrow) = find_direct_child_by_kind(declarator, "arrow_function") {
                Self::visit_arrow_function(state, declarator, arrow);
                continue;
            }
            let binding = Self::visit_variable(state, declarator, variable_kind.clone());
            match initializer_owner {
                Some(owner) => Self::extract_call_sites(state, declarator, owner),
                None => Self::extract_owned_call_sites(state, declarator, &binding),
            }
        }
    }

    /// Extract an arrow function from a `variable_declarator` node.
    fn visit_arrow_function(
        state: &mut ExtractionState<'_>,
        declarator: TsNode<'_>,
        arrow_node: TsNode<'_>,
    ) {
        let name = Self::child_name(state, find_direct_child_by_kind(declarator, "identifier"));
        let visibility = if state.in_export {
            Visibility::Pub
        } else {
            Visibility::Private
        };
        let is_async = Self::has_child_kind(arrow_node, "async");

        // Use the declarator's parent (lexical_declaration) for docstring lookup.
        let docstring = if let Some(parent) = declarator.parent() {
            Self::extract_jsdoc(state, parent)
        } else {
            None
        };

        let signature = Some(Self::extract_arrow_signature(state, declarator));
        let start_line = declarator.start_position().row as u32;
        let end_line = arrow_node.end_position().row as u32;
        let start_column = declarator.start_position().column as u32;
        let end_column = arrow_node.end_position().column as u32;
        let qualified_name = format!("{}::{}", state.qualified_prefix(), name);
        let id = local_node_id(
            &state.file_path,
            state.source,
            &NodeKind::ArrowFunction,
            &name,
            declarator,
        );
        let metrics = count_complexity(arrow_node, &TYPESCRIPT_COMPLEXITY, state.source);

        let graph_node = Node {
            id: id.clone(),
            kind: NodeKind::ArrowFunction,
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
            is_async,
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

        Self::extract_type_refs(state, arrow_node, &id);

        // Extract call sites from the arrow function body. Block-bodied arrows
        // (`() => { ... }`) have a `statement_block`; expression-bodied arrows
        // (`() => foo()`) have their call expression directly under the arrow
        // node, so fall back to scanning the arrow node itself.
        if let Some(body) = find_direct_child_by_kind(arrow_node, "statement_block") {
            Self::extract_call_sites(state, body, &id);
        } else {
            Self::extract_call_sites(state, arrow_node, &id);
        }
        Self::suppress_shadowed_calls(state, arrow_node, &id);
    }

    /// Extract a typed or untyped variable declaration (not an arrow function).
    fn visit_variable(
        state: &mut ExtractionState<'_>,
        declarator: TsNode<'_>,
        kind: NodeKind,
    ) -> String {
        let name = Self::child_name(state, find_direct_child_by_kind(declarator, "identifier"));
        let visibility = if state.in_export {
            Visibility::Pub
        } else {
            Visibility::Private
        };
        let text = state.node_text(declarator);
        let start_line = declarator.start_position().row as u32;
        let end_line = declarator.end_position().row as u32;
        let start_column = declarator.start_position().column as u32;
        let end_column = declarator.end_position().column as u32;
        let qualified_name = format!("{}::{}", state.qualified_prefix(), name);
        let id = local_node_id(&state.file_path, state.source, &kind, &name, declarator);

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
            signature: Some(text.trim().to_string()),
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
                target: id.clone(),
                kind: EdgeKind::Contains,
                line: Some(start_line),
            });
        }

        if let Some(annotation) = find_direct_child_by_kind(declarator, "type_annotation") {
            Self::collect_type_identifiers(state, annotation, &id, EdgeKind::TypeOf);
        }
        id
    }

    /// Extract a class declaration node.
    fn visit_class(state: &mut ExtractionState<'_>, node: TsNode<'_>) {
        // TS uses type_identifier, JS uses identifier for class names.
        let name = Self::child_name(
            state,
            find_direct_child_by_kind(node, "type_identifier")
                .or_else(|| find_direct_child_by_kind(node, "identifier")),
        );
        let visibility = if state.in_export {
            Visibility::Pub
        } else {
            Visibility::Private
        };
        let docstring = Self::extract_jsdoc(state, node);
        let signature = Some(Self::extract_signature(state, node));
        let (start_line, start_column) = declaration_start(node, &["decorator"]);
        let end_line = node.end_position().row as u32;
        let end_column = node.end_position().column as u32;
        let qualified_name = format!("{}::{}", state.qualified_prefix(), name);
        let id = local_node_id(
            &state.file_path,
            state.source,
            &NodeKind::Class,
            &name,
            node,
        );

        let graph_node = Node {
            id: id.clone(),
            kind: NodeKind::Class,
            name: name.clone(),
            qualified_name,
            file_path: state.file_path.clone(),
            start_line,
            attrs_start_line: node.start_position().row as u32,
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

        Self::extract_decorators(state, node, &id);
        // `@Component({ … })` is called to wrap the declaration.
        for decorator in node
            .children(&mut node.walk())
            .filter(|child| child.kind() == "decorator")
        {
            Self::extract_call_sites(state, decorator, &id);
        }

        Self::extract_class_heritage(state, node, &id);

        if let Some(body) = find_direct_child_by_kind(node, "class_body") {
            state.node_stack.push((name, id.clone()));
            Self::visit_class_body(state, body);
            state.node_stack.pop();
        }
    }

    /// Visit the body of a class, extracting methods and fields.
    fn visit_class_body(state: &mut ExtractionState<'_>, body: TsNode<'_>) {
        let mut cursor = body.walk();
        if cursor.goto_first_child() {
            loop {
                let child = cursor.node();
                match child.kind() {
                    "method_definition" => Self::visit_method(state, child),
                    "public_field_definition" | "field_definition" => {
                        Self::visit_field(state, child);
                    }
                    // `static { … }` and member decorators (`@memo()`) run
                    // when the class is defined.
                    "class_static_block" | "decorator" => {
                        if let Some(class_id) = state.parent_node_id().map(str::to_owned) {
                            Self::extract_owned_call_sites(state, child, &class_id);
                        }
                    }
                    _ => {}
                }
                if !cursor.goto_next_sibling() {
                    break;
                }
            }
        }
    }

    /// Extract a `method_definition` from a class body.
    fn visit_method(state: &mut ExtractionState<'_>, node: TsNode<'_>) {
        let name = Self::child_name(
            state,
            find_direct_child_by_kind(node, "property_identifier"),
        );

        let kind = if name == "constructor" {
            NodeKind::Constructor
        } else {
            NodeKind::Method
        };

        let visibility = Self::extract_ts_accessibility(state, node);
        let is_async = Self::has_child_kind(node, "async");
        let signature = Some(Self::extract_signature(state, node));
        let docstring = Self::extract_jsdoc(state, node);
        let start_line = node.start_position().row as u32;
        let end_line = node.end_position().row as u32;
        let start_column = node.start_position().column as u32;
        let end_column = node.end_position().column as u32;
        let qualified_name = format!("{}::{}", state.qualified_prefix(), name);
        let id = local_node_id(&state.file_path, state.source, &kind, &name, node);
        let metrics = count_complexity(node, &TYPESCRIPT_COMPLEXITY, state.source);

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
            visibility,
            is_async,
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

        Self::extract_type_refs(state, node, &id);

        // Default parameter values and the body run on each call.
        Self::extract_owned_call_sites(state, node, &id);
    }

    /// Extract a field from a class body (`public_field_definition`).
    fn visit_field(state: &mut ExtractionState<'_>, node: TsNode<'_>) {
        let name = Self::child_name(
            state,
            find_direct_child_by_kind(node, "property_identifier"),
        );
        let visibility = Self::extract_ts_accessibility(state, node);
        let text = state.node_text(node);
        // Unlike a method's, a field's decorators are its own leading children.
        let (start_line, start_column) = declaration_start(node, &["decorator"]);
        let end_line = node.end_position().row as u32;
        let end_column = node.end_position().column as u32;
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
            attrs_start_line: node.start_position().row as u32,
            end_line,
            start_column,
            end_column,
            signature: Some(text.trim().to_string()),
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
                target: id.clone(),
                kind: EdgeKind::Contains,
                line: Some(start_line),
            });
        }

        Self::extract_decorators(state, node, &id);

        // A field initializer (`onClick = () => this.save()`) runs as part of
        // construction, and `@Input()` wraps the field; the field is the
        // named symbol that owns both calls.
        Self::extract_owned_call_sites(state, node, &id);
    }

    /// Extract an interface declaration node.
    fn visit_interface(state: &mut ExtractionState<'_>, node: TsNode<'_>) {
        let name = Self::child_name(state, find_direct_child_by_kind(node, "type_identifier"));
        let visibility = if state.in_export {
            Visibility::Pub
        } else {
            Visibility::Private
        };
        let docstring = Self::extract_jsdoc(state, node);
        let signature = Some(Self::extract_signature(state, node));
        let start_line = node.start_position().row as u32;
        let end_line = node.end_position().row as u32;
        let start_column = node.start_position().column as u32;
        let end_column = node.end_position().column as u32;
        let qualified_name = format!("{}::{}", state.qualified_prefix(), name);
        let id = local_node_id(
            &state.file_path,
            state.source,
            &NodeKind::Interface,
            &name,
            node,
        );

        let graph_node = Node {
            id: id.clone(),
            kind: NodeKind::Interface,
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

        if let Some(heritage) = find_direct_child_by_kind(node, "extends_type_clause") {
            let mut cursor = heritage.walk();
            if cursor.goto_first_child() {
                loop {
                    let parent = cursor.node();
                    if let Some(reference_name) = Self::declared_type_name(state, parent) {
                        state.unresolved_refs.push(UnresolvedRef {
                            from_node_id: id.clone(),
                            reference_name,
                            reference_kind: EdgeKind::Extends,
                            line: parent.start_position().row as u32,
                            column: parent.start_position().column as u32,
                            file_path: state.file_path.clone(),
                            unmodeled_import: None,
                            argument_count: None,
                        });
                    }
                    if !cursor.goto_next_sibling() {
                        break;
                    }
                }
            }
        }

        if let Some(body) = find_direct_child_by_kind(node, "interface_body") {
            state.node_stack.push((name, id.clone()));
            Self::visit_interface_body(state, body);
            state.node_stack.pop();
        }
    }

    /// Visit the body of an interface, extracting method signatures.
    fn visit_interface_body(state: &mut ExtractionState<'_>, body: TsNode<'_>) {
        let mut cursor = body.walk();
        if cursor.goto_first_child() {
            loop {
                let child = cursor.node();
                if child.kind() == "method_signature" {
                    Self::visit_interface_method(state, child);
                }
                if !cursor.goto_next_sibling() {
                    break;
                }
            }
        }
    }

    /// Extract a `method_signature` from an interface body.
    fn visit_interface_method(state: &mut ExtractionState<'_>, node: TsNode<'_>) {
        let name = Self::child_name(
            state,
            find_direct_child_by_kind(node, "property_identifier"),
        );
        let text = state.node_text(node);
        let start_line = node.start_position().row as u32;
        let end_line = node.end_position().row as u32;
        let start_column = node.start_position().column as u32;
        let end_column = node.end_position().column as u32;
        let qualified_name = format!("{}::{}", state.qualified_prefix(), name);
        let id = local_node_id(
            &state.file_path,
            state.source,
            &NodeKind::Method,
            &name,
            node,
        );

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

    /// Extract an enum declaration node.
    fn visit_enum(state: &mut ExtractionState<'_>, node: TsNode<'_>) {
        let name = Self::child_name(state, find_direct_child_by_kind(node, "identifier"));
        let visibility = if state.in_export {
            Visibility::Pub
        } else {
            Visibility::Private
        };
        let docstring = Self::extract_jsdoc(state, node);
        let text = state.node_text(node);
        let signature = text.find('{').map(|pos| text[..pos].trim().to_string());
        let start_line = node.start_position().row as u32;
        let end_line = node.end_position().row as u32;
        let start_column = node.start_position().column as u32;
        let end_column = node.end_position().column as u32;
        let qualified_name = format!("{}::{}", state.qualified_prefix(), name);
        let id = local_node_id(&state.file_path, state.source, &NodeKind::Enum, &name, node);

        let graph_node = Node {
            id: id.clone(),
            kind: NodeKind::Enum,
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

        if let Some(body) = find_direct_child_by_kind(node, "enum_body") {
            state.node_stack.push((name, id.clone()));
            Self::visit_enum_body(state, body);
            state.node_stack.pop();
        }
    }

    /// Visit the body of an enum, extracting variants.
    fn visit_enum_body(state: &mut ExtractionState<'_>, body: TsNode<'_>) {
        let mut cursor = body.walk();
        if cursor.goto_first_child() {
            loop {
                let child = cursor.node();
                match child.kind() {
                    "property_identifier" => Self::visit_enum_member(state, child),
                    // `A = compute()` runs when the enum is defined.
                    "enum_assignment" => {
                        if let Some(enum_id) = state.parent_node_id().map(str::to_owned) {
                            Self::extract_call_sites(state, child, &enum_id);
                        }
                    }
                    _ => {}
                }
                if !cursor.goto_next_sibling() {
                    break;
                }
            }
        }
    }

    /// Extract an enum member (variant).
    fn visit_enum_member(state: &mut ExtractionState<'_>, node: TsNode<'_>) {
        let name = Self::node_name(state, node);
        let start_line = node.start_position().row as u32;
        let end_line = node.end_position().row as u32;
        let start_column = node.start_position().column as u32;
        let end_column = node.end_position().column as u32;
        let qualified_name = format!("{}::{}", state.qualified_prefix(), name);
        let id = local_node_id(
            &state.file_path,
            state.source,
            &NodeKind::EnumVariant,
            &name,
            node,
        );

        let graph_node = Node {
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
                target: id,
                kind: EdgeKind::Contains,
                line: Some(start_line),
            });
        }
    }

    /// Extract a type alias declaration.
    fn visit_type_alias(state: &mut ExtractionState<'_>, node: TsNode<'_>) {
        let name = Self::child_name(state, find_direct_child_by_kind(node, "type_identifier"));
        let visibility = if state.in_export {
            Visibility::Pub
        } else {
            Visibility::Private
        };
        let text = state.node_text(node);
        let start_line = node.start_position().row as u32;
        let end_line = node.end_position().row as u32;
        let start_column = node.start_position().column as u32;
        let end_column = node.end_position().column as u32;
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
            signature: Some(text.trim().to_string()),
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

    /// Extract a `namespace` (`internal_module`) or `module` declaration.
    /// `namespace A.B {}` nests `B` in `A`, so it qualifies as `A::B`.
    fn visit_namespace(state: &mut ExtractionState<'_>, node: TsNode<'_>) {
        let name_node = node.child_by_field_name("name");
        let mut name = Self::child_name(state, name_node);
        if name_node.is_some_and(|n| n.kind() == "nested_identifier") {
            name = name
                .split('.')
                .map(str::trim)
                .collect::<Vec<_>>()
                .join("::");
        }
        let visibility = if state.in_export {
            Visibility::Pub
        } else {
            Visibility::Private
        };
        let docstring = Self::extract_jsdoc(state, node);
        let text = state.node_text(node);
        let signature = text.find('{').map(|pos| text[..pos].trim().to_string());
        let start_line = node.start_position().row as u32;
        let end_line = node.end_position().row as u32;
        let start_column = node.start_position().column as u32;
        let end_column = node.end_position().column as u32;
        let qualified_name = format!("{}::{}", state.qualified_prefix(), name);
        let id = local_node_id(
            &state.file_path,
            state.source,
            &NodeKind::Namespace,
            &name,
            node,
        );

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
                target: id.clone(),
                kind: EdgeKind::Contains,
                line: Some(start_line),
            });
        }

        if let Some(body) = find_direct_child_by_kind(node, "statement_block") {
            state.node_stack.push((name, id.clone()));
            let in_export = std::mem::replace(&mut state.in_export, false);
            Self::visit_children(state, body);
            state.in_export = in_export;
            state.node_stack.pop();
            // The namespace owns the calls its body runs when it is entered.
            let mut cursor = body.walk();
            for statement in body.named_children(&mut cursor) {
                if Self::has_unowned_calls(state, statement) {
                    Self::extract_owned_call_sites(state, statement, &id);
                }
            }
        }
    }

    // ----------------------------
    // Helper extraction methods
    // ----------------------------

    /// Extract the leading decorators of a class or field declaration. Their
    /// call sites are left to the caller.
    fn extract_decorators(state: &mut ExtractionState<'_>, node: TsNode<'_>, parent_id: &str) {
        let mut cursor = node.walk();
        if cursor.goto_first_child() {
            loop {
                let child = cursor.node();
                if child.kind() == "decorator" {
                    let text = state.node_text(child);
                    let name = Self::clean_name(
                        text.trim_start_matches('@')
                            .split('(')
                            .next()
                            .unwrap_or(text),
                    );
                    let start_line = child.start_position().row as u32;
                    let end_line = child.end_position().row as u32;
                    let start_column = child.start_position().column as u32;
                    let end_column = child.end_position().column as u32;
                    let qualified_name = format!("{}::@{}", state.qualified_prefix(), name);
                    let id = local_node_id(
                        &state.file_path,
                        state.source,
                        &NodeKind::Decorator,
                        &name,
                        child,
                    );

                    let graph_node = Node {
                        id: id.clone(),
                        kind: NodeKind::Decorator,
                        name: name.clone(),
                        qualified_name,
                        file_path: state.file_path.clone(),
                        start_line,
                        attrs_start_line: start_line,
                        end_line,
                        start_column,
                        end_column,
                        signature: Some(text.to_string()),
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
                        complexity_analysis: ComplexityAnalysisV1::Complete,
                        updated_at: state.timestamp,
                        parent_id: None,
                    };
                    state.nodes.push(graph_node);

                    state.edges.push(Edge {
                        source: id,
                        target: parent_id.to_string(),
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

    /// Extract extends/implements from a class heritage clause.
    fn extract_class_heritage(state: &mut ExtractionState<'_>, node: TsNode<'_>, class_id: &str) {
        if let Some(heritage) = find_direct_child_by_kind(node, "class_heritage") {
            let mut cursor = heritage.walk();
            if cursor.goto_first_child() {
                loop {
                    let child = cursor.node();
                    match child.kind() {
                        "extends_clause" => {
                            // Find the extended class name (identifier or type_identifier).
                            let ext_name = find_direct_child_by_kind(child, "identifier")
                                .or_else(|| find_direct_child_by_kind(child, "type_identifier"))
                                .map(|n| state.node_text(n).to_string());
                            if let Some(name) = ext_name {
                                state.unresolved_refs.push(UnresolvedRef {
                                    from_node_id: class_id.to_string(),
                                    reference_name: name,
                                    reference_kind: EdgeKind::Extends,
                                    line: child.start_position().row as u32,
                                    column: child.start_position().column as u32,
                                    file_path: state.file_path.clone(),
                                    unmodeled_import: None,
                                    argument_count: None,
                                });
                            }
                        }
                        "implements_clause" => {
                            // May implement multiple interfaces.
                            let mut inner = child.walk();
                            if inner.goto_first_child() {
                                loop {
                                    let iface = inner.node();
                                    if let Some(name) = Self::declared_type_name(state, iface) {
                                        state.unresolved_refs.push(UnresolvedRef {
                                            from_node_id: class_id.to_string(),
                                            reference_name: name,
                                            reference_kind: EdgeKind::Implements,
                                            line: iface.start_position().row as u32,
                                            column: iface.start_position().column as u32,
                                            file_path: state.file_path.clone(),
                                            unmodeled_import: None,
                                            argument_count: None,
                                        });
                                    }
                                    if !inner.goto_next_sibling() {
                                        break;
                                    }
                                }
                            }
                        }
                        _ => {}
                    }
                    if !cursor.goto_next_sibling() {
                        break;
                    }
                }
            }
        }
    }

    fn declared_type_name(state: &ExtractionState<'_>, node: TsNode<'_>) -> Option<String> {
        match node.kind() {
            "type_identifier" => Some(state.node_text(node).to_string()),
            "generic_type" => node
                .child_by_field_name("name")
                .and_then(|name| Self::declared_type_name(state, name)),
            "nested_type_identifier" => Some(state.node_text(node).replace('.', "::")),
            _ => None,
        }
    }

    /// Recursively find call sites inside a node and create unresolved Calls
    /// references. Nested arrow functions, function expressions, and local
    /// function declarations are not graph symbols, so the calls in their
    /// bodies (`.map((row) => format(row))`, JSX event handlers) belong to the
    /// enclosing symbol. A JSX element naming a component (`<Card />`,
    /// `<Layout.Header>`) renders, and so calls, that component.
    fn extract_call_sites(state: &mut ExtractionState<'_>, node: TsNode<'_>, fn_node_id: &str) {
        let mut cursor = node.walk();
        if cursor.goto_first_child() {
            loop {
                let child = cursor.node();
                let callee = match child.kind() {
                    "call_expression" => child.named_child(0).map(|callee| (callee, child)),
                    "jsx_opening_element" | "jsx_self_closing_element" => child
                        .child_by_field_name("name")
                        .filter(|name| Self::is_jsx_component_name(state, *name))
                        .map(|name| (name, name)),
                    _ => None,
                };
                if let Some((callee, site)) = callee {
                    state.unresolved_refs.push(UnresolvedRef {
                        from_node_id: fn_node_id.to_string(),
                        reference_name: state.node_text(callee).to_string(),
                        reference_kind: EdgeKind::Calls,
                        line: site.start_position().row as u32,
                        column: site.start_position().column as u32,
                        file_path: state.file_path.clone(),
                        unmodeled_import: None,
                        argument_count: None,
                    });
                }
                Self::extract_call_sites(state, child, fn_node_id);
                if !cursor.goto_next_sibling() {
                    break;
                }
            }
        }
    }

    /// Lower-case JSX names (`<div>`) and namespaced names (`<svg:rect>`) are
    /// intrinsic elements, not components.
    fn is_jsx_component_name(state: &ExtractionState<'_>, name: TsNode<'_>) -> bool {
        match name.kind() {
            "member_expression" => true,
            "identifier" => state
                .node_text(name)
                .starts_with(|first: char| first.is_ascii_uppercase()),
            _ => false,
        }
    }

    /// Call sites owned by the symbol `owner_id`, less calls to names a
    /// binding inside `scope` shadows.
    fn extract_owned_call_sites(
        state: &mut ExtractionState<'_>,
        scope: TsNode<'_>,
        owner_id: &str,
    ) {
        Self::extract_call_sites(state, scope, owner_id);
        Self::suppress_shadowed_calls(state, scope, owner_id);
    }

    /// Import rows are file-scoped, so a local binding makes the same bare
    /// call name ambiguous for its whole owning function. Withhold that call
    /// rather than claiming statement-level resolution the artifact lacks.
    fn suppress_shadowed_calls(
        state: &mut ExtractionState<'_>,
        function: TsNode<'_>,
        fn_node_id: &str,
    ) {
        let mut shadows = ShadowedCallNames::default();
        Self::collect_shadowed_names(state, function, function, &mut shadows);
        state.unresolved_refs.retain(|reference| {
            reference.from_node_id != fn_node_id
                || reference.reference_kind != EdgeKind::Calls
                || !shadows.names.contains(&reference.reference_name)
        });
    }

    fn collect_shadowed_names(
        state: &ExtractionState<'_>,
        node: TsNode<'_>,
        function: TsNode<'_>,
        shadows: &mut ShadowedCallNames,
    ) {
        if let Some(binding) = Self::declared_binding(node, function) {
            Self::record_binding_pattern(state, binding, shadows);
        }
        if node.kind() == "formal_parameters" {
            Self::record_bare_parameters(state, node, shadows);
        }
        let mut cursor = node.walk();
        if cursor.goto_first_child() {
            loop {
                Self::collect_shadowed_names(state, cursor.node(), function, shadows);
                if !cursor.goto_next_sibling() {
                    break;
                }
            }
        }
    }

    /// JavaScript parameters are bare patterns, not `required_parameter`.
    fn record_bare_parameters(
        state: &ExtractionState<'_>,
        parameters: TsNode<'_>,
        shadows: &mut ShadowedCallNames,
    ) {
        let mut cursor = parameters.walk();
        for parameter in parameters.named_children(&mut cursor) {
            let binding = match parameter.kind() {
                "identifier" | "object_pattern" | "array_pattern" | "rest_pattern" => {
                    Some(parameter)
                }
                "assignment_pattern" => parameter.child_by_field_name("left"),
                _ => None,
            };
            if let Some(binding) = binding {
                Self::record_binding_pattern(state, binding, shadows);
            }
        }
    }

    /// The binding pattern `node` declares in `function`'s scope. A nested
    /// function declaration is a local binding too: its body's calls are
    /// attributed to `function`, so its name shadows there.
    fn declared_binding<'t>(node: TsNode<'t>, function: TsNode<'t>) -> Option<TsNode<'t>> {
        match node.kind() {
            "required_parameter" | "optional_parameter" | "rest_parameter" => {
                node.child_by_field_name("pattern")
            }
            "variable_declarator" => node.child_by_field_name("name"),
            "catch_clause" | "for_in_statement" => node
                .child_by_field_name("parameter")
                .or_else(|| node.child_by_field_name("left")),
            "arrow_function" => node.child_by_field_name("parameter"),
            "function_declaration" | "generator_function_declaration" if node != function => {
                node.child_by_field_name("name")
            }
            _ => None,
        }
    }

    fn record_binding_pattern(
        state: &ExtractionState<'_>,
        pattern: TsNode<'_>,
        shadows: &mut ShadowedCallNames,
    ) {
        if pattern.kind() == "identifier" {
            shadows.names.push(state.node_text(pattern).to_owned());
            return;
        }
        // Only walk the parser's binding field. Destructuring property and
        // default-value subtrees may add names, deliberately withholding an
        // ambiguous edge rather than inventing one.
        let mut cursor = pattern.walk();
        if cursor.goto_first_child() {
            loop {
                Self::record_binding_pattern(state, cursor.node(), shadows);
                if !cursor.goto_next_sibling() {
                    break;
                }
            }
        }
    }

    /// Extract type references from parameter annotations and return type.
    ///
    /// In tree-sitter-typescript, type annotations appear as `type_annotation`
    /// children on parameter nodes and on the function itself (return type).
    /// Each `type_identifier` inside creates a "uses" unresolved ref.
    fn extract_type_refs(state: &mut ExtractionState<'_>, node: TsNode<'_>, fn_node_id: &str) {
        let mut cursor = node.walk();
        if !cursor.goto_first_child() {
            return;
        }
        loop {
            let child = cursor.node();
            match child.kind() {
                // Parameter nodes contain type_annotation children; also the return type annotation
                "required_parameter" | "optional_parameter" | "rest_parameter"
                | "type_annotation" => {
                    Self::collect_type_identifiers(state, child, fn_node_id, EdgeKind::Uses);
                }
                // Formal parameters container
                "formal_parameters" => {
                    Self::extract_type_refs(state, child, fn_node_id);
                }
                _ => {}
            }
            if !cursor.goto_next_sibling() {
                break;
            }
        }
    }

    /// Recursively collect `type_identifier` nodes and emit unresolved refs.
    fn collect_type_identifiers(
        state: &mut ExtractionState<'_>,
        node: TsNode<'_>,
        from_node_id: &str,
        reference_kind: EdgeKind,
    ) {
        let mut cursor = node.walk();
        if !cursor.goto_first_child() {
            return;
        }
        loop {
            let child = cursor.node();
            if child.kind() == "type_identifier" {
                let type_name = state.node_text(child);
                // Skip built-in types
                if !matches!(
                    type_name,
                    "string"
                        | "number"
                        | "boolean"
                        | "void"
                        | "null"
                        | "undefined"
                        | "any"
                        | "never"
                        | "unknown"
                        | "object"
                        | "symbol"
                        | "bigint"
                ) {
                    state.unresolved_refs.push(UnresolvedRef {
                        from_node_id: from_node_id.to_string(),
                        reference_name: type_name.to_string(),
                        reference_kind,
                        line: child.start_position().row as u32,
                        column: child.start_position().column as u32,
                        file_path: state.file_path.clone(),
                        unmodeled_import: None,
                        argument_count: None,
                    });
                }
            } else {
                Self::collect_type_identifiers(state, child, from_node_id, reference_kind);
            }
            if !cursor.goto_next_sibling() {
                break;
            }
        }
    }

    /// Extract the function/method signature (everything up to the body `{`).
    fn extract_signature(state: &ExtractionState<'_>, node: TsNode<'_>) -> String {
        let text = state.node_text(node);
        if let Some(body) = node.child_by_field_name("body") {
            // Prefer the body child's start so a `{` in a parameter type
            // (`x: { a: number }`) cannot truncate the header, and the
            // (possibly huge) body is never scanned.
            let body_offset = body.start_byte() - node.start_byte();
            text[..body_offset].trim().to_string()
        } else if let Some(brace_pos) = text.find('{') {
            text[..brace_pos].trim().to_string()
        } else {
            // Signature-only declaration: the whole (small) text is the
            // signature.
            text.trim().to_string()
        }
    }

    /// Extract the signature for an arrow function from its `variable_declarator`.
    fn extract_arrow_signature(state: &ExtractionState<'_>, declarator: TsNode<'_>) -> String {
        let text = state.node_text(declarator);
        // For arrow functions, the signature is "name = (params) => ..."
        // We want everything up to the arrow body.
        if let Some(arrow_pos) = text.find("=>") {
            text[..arrow_pos + 2].trim().to_string()
        } else if let Some(body) = find_direct_child_by_kind(declarator, "arrow_function")
            .and_then(|arrow| arrow.child_by_field_name("body"))
        {
            // Arrow shapes where the `=>` token is not found in the text:
            // own only the header before the body child instead of copying
            // the whole item.
            let body_offset = body.start_byte() - declarator.start_byte();
            text[..body_offset].trim().to_string()
        } else {
            text.trim().to_string()
        }
    }

    /// Extract `JSDoc` docstrings from preceding comment nodes.
    /// Only picks up `/** ... */` style comments (`JSDoc`).
    fn extract_jsdoc(state: &ExtractionState<'_>, node: TsNode<'_>) -> Option<String> {
        // In TS, we also need to check the parent if this is inside an export_statement.
        let mut target = node;
        if let Some(parent) = node.parent()
            && parent.kind() == "export_statement"
        {
            target = parent;
        }

        let current = target.prev_named_sibling();
        if let Some(sibling) = current
            && sibling.kind() == "comment"
        {
            let text = state.node_text(sibling);
            if text.starts_with("/**") {
                return Some(Self::clean_jsdoc(text));
            }
        }
        None
    }

    /// Clean `JSDoc` comment markers.
    fn clean_jsdoc(comment: &str) -> String {
        let trimmed = comment.trim();
        if trimmed.starts_with("/**") && trimmed.ends_with("*/") {
            if trimmed.len() <= 5 {
                return String::new();
            }
            let inner = &trimmed[3..trimmed.len() - 2];
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

    /// Extract TypeScript accessibility modifier (public/private/protected).
    fn extract_ts_accessibility(state: &ExtractionState<'_>, node: TsNode<'_>) -> Visibility {
        if let Some(modifier) = find_direct_child_by_kind(node, "accessibility_modifier") {
            let text = state.node_text(modifier);
            match text {
                "private" => Visibility::Private,
                "protected" => Visibility::PubSuper,
                _ => Visibility::Pub,
            }
        } else {
            // In TypeScript, class members without explicit modifier are public by default.
            Visibility::Pub
        }
    }

    /// Check if a node has a direct child of a given kind.
    fn has_child_kind(node: TsNode<'_>, kind: &str) -> bool {
        let mut cursor = node.walk();
        if cursor.goto_first_child() {
            loop {
                if cursor.node().kind() == kind {
                    return true;
                }
                if !cursor.goto_next_sibling() {
                    break;
                }
            }
        }
        false
    }

    /// Build the graph and parser-backed evidence accumulated in one traversal.
    fn build_artifact(state: ExtractionState<'_>, start: Instant) -> ExtractionArtifactV1 {
        ExtractionArtifactV1 {
            result: ExtractionResult {
                nodes: state.nodes,
                edges: state.edges,
                unresolved_refs: state.unresolved_refs,
                errors: state.errors,
                duration_ms: start.elapsed().as_millis() as u64,
            },
            imports: state.imports,
            clone_bodies: Vec::new(),
            schema_evidence: None,
            callable_arities: Vec::new(),
            go_method_sets: Vec::new(),
        }
    }
}

impl crate::LanguageExtractor for TypeScriptExtractor {
    fn extensions(&self) -> &[&str] {
        &["ts", "tsx", "js", "jsx"]
    }

    fn language_name(&self) -> &'static str {
        "TypeScript"
    }

    fn extract_artifact(&self, file_path: &str, source: &str) -> ExtractionArtifactV1 {
        crate::hotpath_observe::measure_extract_file(
            self.language_name(),
            source.len(),
            || TypeScriptExtractor::extract_typescript_artifact(file_path, source),
            crate::hotpath_observe::ExtractOutputCounts::from_artifact,
        )
    }

    fn extract_parsed_artifact_prepared(
        &self,
        file_path: &str,
        source: &str,
        _parsed_source: &str,
        tree: &Tree,
        scope: crate::parsed_extraction::ParsedExtractionScope<'_>,
    ) -> crate::parsed_extraction::ParsedExtractionArtifactV1 {
        TypeScriptExtractor::extract_tree_artifact(file_path, source, tree, scope)
    }
}
