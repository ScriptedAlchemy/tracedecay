/// Tree-sitter based Lua source code extractor.
///
/// Parses Lua source files and emits nodes and edges for the code graph.
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use tree_sitter::{Node as TsNode, Tree};

use crate::complexity::{LUA_COMPLEXITY, count_complexity};
use crate::traversal::find_direct_child_by_kind;
use crate::types::{
    Edge, EdgeKind, ExtractionResult, Node, NodeKind, UnresolvedRef, Visibility, generate_node_id,
};

/// Extracts code graph nodes and edges from Lua source files using tree-sitter.
pub struct LuaExtractor;

/// Internal state used during AST traversal.
struct ExtractionState<'s> {
    nodes: Vec<Node>,
    edges: Vec<Edge>,
    unresolved_refs: Vec<UnresolvedRef>,
    errors: Vec<String>,
    /// Stack of (name, `node_id`) for building qualified names and parent edges.
    node_stack: Vec<(String, String)>,
    file_path: String,
    source: &'s [u8],
    timestamp: u64,
}

impl<'s> ExtractionState<'s> {
    fn new(file_path: &str, source: &'s str) -> Self {
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
            source: source.as_bytes(),
            timestamp,
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

    /// Gets the text of a tree-sitter node from the source.
    fn node_text(&self, node: TsNode<'_>) -> &'s str {
        node.utf8_text(self.source).unwrap_or("<invalid utf8>")
    }
}

impl LuaExtractor {
    /// `file_path` is used for qualified names and node IDs (not for I/O).
    pub fn extract_lua(file_path: &str, source: &str) -> ExtractionResult {
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

    #[hotpath::measure(label = "code_extraction.lua.extract_tree")]
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
    #[hotpath::measure(label = "code_extraction.lua.parse_source")]
    fn parse_source(source: &str) -> Result<Tree, String> {
        crate::ts_provider::parse_extractor_source("lua", "Lua", source)
    }

    fn visit_node(state: &mut ExtractionState, node: TsNode<'_>) {
        match node.kind() {
            "function_declaration" => Self::visit_function_declaration(state, node),
            "variable_declaration" => Self::visit_variable_declaration(state, node),
            _ => {}
        }
    }

    /// Extract a function declaration.
    ///
    /// Lua function declarations come in three flavours:
    /// - `local function foo(...)` → name is `identifier`, local/private function
    /// - `function Foo.bar(...)` → name is `dot_index_expression`, public function with class context
    /// - `function Foo:bar(...)` → name is `method_index_expression`, method
    fn visit_function_declaration(state: &mut ExtractionState, node: TsNode<'_>) {
        let Some(name_node) = node.child_by_field_name("name") else {
            return;
        };

        let is_local = node.child(0).is_some_and(|c| c.kind() == "local");

        let (name, kind, visibility, class_context) = match name_node.kind() {
            "identifier" => {
                let name = state.node_text(name_node).to_string();
                (
                    name,
                    NodeKind::Function,
                    if is_local {
                        Visibility::Private
                    } else {
                        Visibility::Pub
                    },
                    None,
                )
            }
            "dot_index_expression" => {
                // e.g. Connection.new
                let table_name = name_node
                    .child_by_field_name("table")
                    .map(|n| state.node_text(n));
                let field_name = name_node.child_by_field_name("field").map_or_else(
                    || "<anonymous>".to_string(),
                    |n| state.node_text(n).to_string(),
                );
                (field_name, NodeKind::Function, Visibility::Pub, table_name)
            }
            "method_index_expression" => {
                // e.g. Connection:connect
                let table_name = name_node
                    .child_by_field_name("table")
                    .map(|n| state.node_text(n));
                let method_name = name_node.child_by_field_name("method").map_or_else(
                    || "<anonymous>".to_string(),
                    |n| state.node_text(n).to_string(),
                );
                (method_name, NodeKind::Method, Visibility::Pub, table_name)
            }
            _ => return,
        };

        let docstring = Self::extract_docstring(state, node);
        let signature = Self::extract_function_signature(state, node);
        let start_line = node.start_position().row as u32;
        let end_line = node.end_position().row as u32;
        let start_column = node.start_position().column as u32;
        let end_column = node.end_position().column as u32;
        let qualified_name = if let Some(ref ctx) = class_context {
            format!("{}::{}::{}", state.qualified_prefix(), ctx, name)
        } else {
            format!("{}::{}", state.qualified_prefix(), name)
        };
        let id = generate_node_id(&state.file_path, &kind, &name, start_line);
        let metrics = count_complexity(node, &LUA_COMPLEXITY, state.source);

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

        // Contains edge from parent.
        if let Some(parent_id) = state.parent_node_id() {
            state.edges.push(Edge {
                source: parent_id.to_string(),
                target: id.clone(),
                kind: EdgeKind::Contains,
                line: Some(start_line),
            });
        }

        if let Some(body) = node.child_by_field_name("body") {
            Self::extract_call_sites(state, body, &id);
        }
    }

    /// Extract a variable declaration (`local name = value`).
    ///
    /// Handles:
    /// - `local x = require("mod")` → Use node
    /// - `local CONST = <literal>` → Const node (uppercase names)
    fn visit_variable_declaration(state: &mut ExtractionState, node: TsNode<'_>) {
        // variable_declaration contains an assignment_statement child.
        let Some(assignment) = find_direct_child_by_kind(node, "assignment_statement") else {
            return;
        };

        let var_list = assignment
            .child_by_field_name("variable_list")
            .or_else(|| find_direct_child_by_kind(assignment, "variable_list"));
        let name_node = var_list.and_then(|vl| {
            // The first named child of variable_list should be the identifier.
            find_direct_child_by_kind(vl, "identifier")
        });
        let Some(n) = name_node else {
            return;
        };
        let name = state.node_text(n);

        let expr_list = assignment
            .child_by_field_name("expression_list")
            .or_else(|| find_direct_child_by_kind(assignment, "expression_list"));
        let value_node = expr_list.and_then(|el| el.named_child(0));

        let Some(value_node) = value_node else {
            return;
        };

        // Check if this is a require call → Use node.
        if value_node.kind() == "function_call" {
            let call_name = value_node
                .child_by_field_name("name")
                .map(|n| state.node_text(n));
            if call_name == Some("require") {
                // Extract the module name from the arguments.
                let mod_name = Self::extract_require_module(state, value_node)
                    .unwrap_or_else(|| name.to_string());
                Self::emit_use_node(state, node, &mod_name);
                return;
            }
        }

        // Check if the value is a table constructor → skip (table declaration, not a const).
        if value_node.kind() == "table_constructor" {
            return;
        }

        // Treat as Const node (Lua convention: uppercase names are constants,
        // but we emit all local variable declarations with literal values as Const).
        let is_literal = matches!(
            value_node.kind(),
            "number" | "string" | "true" | "false" | "nil"
        );
        if !is_literal {
            return;
        }

        let docstring = Self::extract_docstring(state, node);
        let start_line = node.start_position().row as u32;
        let end_line = node.end_position().row as u32;
        let start_column = node.start_position().column as u32;
        let end_column = node.end_position().column as u32;
        let text = state.node_text(node);
        let qualified_name = format!("{}::{}", state.qualified_prefix(), name);
        let id = generate_node_id(&state.file_path, &NodeKind::Const, name, start_line);

        let graph_node = Node {
            id: id.clone(),
            kind: NodeKind::Const,
            name: name.to_string(),
            qualified_name,
            file_path: state.file_path.clone(),
            start_line,
            attrs_start_line: start_line,
            end_line,
            start_column,
            end_column,
            signature: Some(text.trim().to_string()),
            docstring,
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
        };
        state.nodes.push(graph_node);

        // Contains edge from parent.
        if let Some(parent_id) = state.parent_node_id() {
            state.edges.push(Edge {
                source: parent_id.to_string(),
                target: id,
                kind: EdgeKind::Contains,
                line: Some(start_line),
            });
        }
    }

    /// Emit a Use node for a `require` call.
    fn emit_use_node(state: &mut ExtractionState, node: TsNode<'_>, mod_name: &str) {
        let start_line = node.start_position().row as u32;
        let end_line = node.end_position().row as u32;
        let start_column = node.start_position().column as u32;
        let end_column = node.end_position().column as u32;
        let text = state.node_text(node);
        let qualified_name = format!("{}::{}", state.qualified_prefix(), mod_name);
        let id = generate_node_id(&state.file_path, &NodeKind::Use, mod_name, start_line);

        let graph_node = Node {
            id: id.clone(),
            kind: NodeKind::Use,
            name: mod_name.to_string(),
            qualified_name,
            file_path: state.file_path.clone(),
            start_line,
            attrs_start_line: start_line,
            end_line,
            start_column,
            end_column,
            signature: Some(text.trim().to_string()),
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
        };
        state.nodes.push(graph_node);

        // Contains edge from parent.
        if let Some(parent_id) = state.parent_node_id() {
            state.edges.push(Edge {
                source: parent_id.to_string(),
                target: id,
                kind: EdgeKind::Contains,
                line: Some(start_line),
            });
        }
    }

    /// Extract the module name from a `require("module")` call.
    ///
    /// Looks for the first string argument inside the arguments node.
    fn extract_require_module(state: &ExtractionState, call_node: TsNode<'_>) -> Option<String> {
        let args = call_node
            .child_by_field_name("arguments")
            .or_else(|| find_direct_child_by_kind(call_node, "arguments"))?;
        // Look for a string node inside arguments.
        let mut cursor = args.walk();
        if cursor.goto_first_child() {
            loop {
                let child = cursor.node();
                if child.kind() == "string" {
                    // The string node contains a string_content child.
                    if let Some(content) = find_direct_child_by_kind(child, "string_content") {
                        return Some(state.node_text(content).to_string());
                    }
                    // Fall back to stripping quotes from the full text.
                    let text = state.node_text(child);
                    return Some(text.trim_matches(|c| c == '"' || c == '\'').to_string());
                }
                if !cursor.goto_next_sibling() {
                    break;
                }
            }
        }
        None
    }

    /// Extract the function signature (first line of the definition).
    fn extract_function_signature(state: &ExtractionState, node: TsNode<'_>) -> Option<String> {
        let text = state.node_text(node);
        let first_line = text.lines().next()?.trim().to_string();
        if first_line.is_empty() {
            None
        } else {
            Some(first_line)
        }
    }

    /// Extract docstrings from `--` or `---` comment lines preceding definitions.
    ///
    /// Lua uses comment lines (-- or --- for `LDoc`) as documentation. We look for
    /// `comment` sibling nodes that immediately precede the given definition node.
    fn extract_docstring(state: &ExtractionState, node: TsNode<'_>) -> Option<String> {
        let mut comments: Vec<String> = Vec::new();
        let mut prev = node.prev_named_sibling();
        while let Some(prev_node) = prev {
            if prev_node.kind() == "comment" {
                let text = state.node_text(prev_node);
                // Strip leading dashes and whitespace: "--- foo" → "foo", "-- bar" → "bar"
                let stripped = text.trim_start_matches('-').trim().to_string();
                comments.push(stripped);
                prev = prev_node.prev_named_sibling();
            } else {
                break;
            }
        }
        if comments.is_empty() {
            return None;
        }
        // Comments were collected in reverse order; reverse them back.
        comments.reverse();
        Some(comments.join("\n"))
    }

    /// Recursively find `function_call` nodes inside a given node and create unresolved Calls references.
    fn extract_call_sites(state: &mut ExtractionState, node: TsNode<'_>, fn_node_id: &str) {
        let mut cursor = node.walk();
        if cursor.goto_first_child() {
            loop {
                let child = cursor.node();
                match child.kind() {
                    "function_call" => {
                        if let Some(name_node) = child.child_by_field_name("name") {
                            // Dotted (`string.format`) and method (`conn:connect`) callees
                            // keep their full source text as the reference name.
                            let callee_name = state.node_text(name_node);
                            state.unresolved_refs.push(UnresolvedRef {
                                from_node_id: fn_node_id.to_string(),
                                reference_name: callee_name.to_string(),
                                reference_kind: EdgeKind::Calls,
                                line: child.start_position().row as u32,
                                column: child.start_position().column as u32,
                                file_path: state.file_path.clone(),
                            });
                        }
                        // Recurse into the call for nested calls.
                        Self::extract_call_sites(state, child, fn_node_id);
                    }
                    // Skip nested function declarations.
                    "function_declaration" => {}
                    _ => {
                        Self::extract_call_sites(state, child, fn_node_id);
                    }
                }
                if !cursor.goto_next_sibling() {
                    break;
                }
            }
        }
    }

    /// Build the final `ExtractionResult` from the accumulated state.
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

impl crate::LanguageExtractor for LuaExtractor {
    fn extensions(&self) -> &[&str] {
        &["lua"]
    }

    fn language_name(&self) -> &'static str {
        "Lua"
    }

    #[hotpath::measure(label = "code_extraction.lua.extract")]
    fn extract(&self, file_path: &str, source: &str) -> ExtractionResult {
        Self::extract_lua(file_path, source)
    }

    #[hotpath::measure(label = "code_extraction.lua.extract_parsed")]
    fn extract_parsed(
        &self,
        file_path: &str,
        source: &str,
        tree: &Tree,
        scope: crate::parsed_extraction::ParsedExtractionScope<'_>,
    ) -> crate::parsed_extraction::ParsedExtraction {
        Self::extract_tree(file_path, source, tree, scope)
    }
}
