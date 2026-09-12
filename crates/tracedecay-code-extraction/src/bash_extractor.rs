/// Tree-sitter based Bash source code extractor.
///
/// Parses Bash/shell source files and emits nodes and edges for the code graph.
use std::path::Path;
use std::time::Instant;

use tree_sitter::{Node as TsNode, Tree};

use crate::common::{ExtractionState, docstring_from_hash_comments, local_node_id};
use crate::complexity::{BASH_COMPLEXITY, count_complexity};
use crate::traversal::find_direct_child_by_kind;
use crate::types::{
    ComplexityAnalysisV1, Edge, EdgeKind, ExtractionResult, Node, NodeKind, UnresolvedRef,
    Visibility, generate_node_id,
};

/// Extracts code graph nodes and edges from Bash source files using tree-sitter.
pub struct BashExtractor;

impl BashExtractor {
    fn extract_tree(
        file_path: &str,
        source: &str,
        tree: &Tree,
        scope: crate::parsed_extraction::ParsedExtractionScope<'_>,
    ) -> crate::parsed_extraction::ParsedExtraction {
        if matches!(
            scope,
            crate::parsed_extraction::ParsedExtractionScope::ChangedRegions(_)
        ) {
            // Bash reextracts the whole file on incremental edits because its script module
            // spans and parents the whole document.
            let full = Self::extract_tree(
                file_path,
                source,
                tree,
                crate::parsed_extraction::ParsedExtractionScope::FullDocument,
            );
            return crate::parsed_extraction::ParsedExtraction::reset(
                full.result,
                crate::parsed_extraction::ParsedExtractionResetReason::ChangedRootIdentity,
                source.len(),
            );
        }
        let start = Instant::now();
        let mut state = ExtractionState::new(file_path, source);
        let root = tree.root_node();

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
        let script_name = Path::new(file_path)
            .file_stem()
            .and_then(|stem| stem.to_str())
            .filter(|stem| !stem.is_empty())
            .unwrap_or(file_path);
        let script_node = Node {
            id: local_node_id(
                file_path,
                source.as_bytes(),
                &NodeKind::Module,
                script_name,
                root,
            ),
            kind: NodeKind::Module,
            name: script_name.to_owned(),
            qualified_name: format!("{file_path}::{script_name}"),
            start_line: root.start_position().row as u32,
            attrs_start_line: root.start_position().row as u32,
            end_line: root.end_position().row as u32,
            start_column: root.start_position().column as u32,
            end_column: root.end_position().column as u32,
            parent_id: Some(file_node_id.clone()),
            ..file_node.clone()
        };
        let script_node_id = script_node.id.clone();
        state.nodes.push(file_node);
        state.nodes.push(script_node);
        state.edges.push(Edge {
            source: file_node_id.clone(),
            target: script_node_id.clone(),
            kind: EdgeKind::Contains,
            line: Some(0),
        });
        state
            .node_stack
            .push((script_name.to_owned(), script_node_id.clone()));

        let metrics = crate::parsed_extraction::visit_root_children(tree, scope, |child| {
            Self::visit_node(&mut state, child);
            if child.kind() != "function_definition" {
                Self::extract_call_sites(&mut state, child, &script_node_id);
            }
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
            "function_definition" => Self::visit_function(state, node),
            "declaration_command" => Self::visit_declaration(state, node),
            "command" => Self::visit_command(state, node),
            _ => {}
        }
    }

    /// Extract a function definition.
    ///
    /// Bash functions are always top-level (no classes), so they get `NodeKind::Function`.
    fn visit_function(state: &mut ExtractionState, node: TsNode<'_>) {
        let name = node.child_by_field_name("name").map_or_else(
            || "<anonymous>".to_string(),
            |n| state.node_text(n).to_string(),
        );

        let kind = NodeKind::Function;
        let visibility = Visibility::Pub;
        let signature = Self::extract_function_signature(state, node);
        let docstring = Self::extract_docstring(state, node);
        let start_line = node.start_position().row as u32;
        let end_line = node.end_position().row as u32;
        let start_column = node.start_position().column as u32;
        let end_column = node.end_position().column as u32;
        let qualified_name = format!("{}::{name}", state.file_path);
        let id = local_node_id(&state.file_path, state.source, &kind, &name, node);
        let metrics = count_complexity(node, &BASH_COMPLEXITY, state.source);

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
            complexity_analysis: metrics.analysis,
            updated_at: state.timestamp,
            parent_id: state.parent_node_id().map(str::to_owned),
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

        Self::extract_call_sites(state, node, &id);
    }

    /// Extract a `readonly VAR=value` or `local VAR=value` declaration at the top level.
    ///
    /// Only top-level `readonly` declarations are treated as constants.
    fn visit_declaration(state: &mut ExtractionState, node: TsNode<'_>) {
        // Only treat top-level readonly as constants.
        // A declaration_command starts with a word like "readonly", "local", "declare", "export".
        let text = state.node_text(node);
        if !text.starts_with("readonly") {
            return;
        }

        // Find the variable_assignment child to get the name.
        if let Some(assignment) = find_direct_child_by_kind(node, "variable_assignment")
            && let Some(name_node) = assignment.child_by_field_name("name")
        {
            let name = state.node_text(name_node);
            let start_line = node.start_position().row as u32;
            let end_line = node.end_position().row as u32;
            let start_column = node.start_position().column as u32;
            let end_column = node.end_position().column as u32;
            let qualified_name = format!("{}::{name}", state.file_path);
            let id = local_node_id(&state.file_path, state.source, &NodeKind::Const, name, node);

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
    }

    /// Extract a top-level command node.
    ///
    /// Detects `source` and `.` commands as Use (import) nodes.
    fn visit_command(state: &mut ExtractionState, node: TsNode<'_>) {
        let cmd_name = node
            .child_by_field_name("name")
            .map(|n| state.node_text(n))
            .unwrap_or_default();

        if cmd_name == "source" || cmd_name == "." {
            // Extract the source/dot import path from the first argument.
            if let Some(arg) = Self::find_first_argument(state, node) {
                let start_line = node.start_position().row as u32;
                let end_line = node.end_position().row as u32;
                let start_column = node.start_position().column as u32;
                let end_column = node.end_position().column as u32;
                let qualified_name = format!("{}::{arg}", state.file_path);
                let id = local_node_id(&state.file_path, state.source, &NodeKind::Use, &arg, node);
                let text = state.node_text(node);

                let graph_node = Node {
                    id: id.clone(),
                    kind: NodeKind::Use,
                    name: arg,
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
        }
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

    /// Extract docstrings from `# comment` lines preceding definitions.
    ///
    /// Bash uses comment lines (# ...) as documentation. We look for `comment`
    /// sibling nodes that immediately precede the given definition node.
    fn extract_docstring(state: &ExtractionState, node: TsNode<'_>) -> Option<String> {
        docstring_from_hash_comments(state.source, node)
    }

    /// Recursively find command nodes inside a given node and create unresolved Calls references.
    fn extract_call_sites(state: &mut ExtractionState, node: TsNode<'_>, fn_node_id: &str) {
        if node.kind() == "command"
            && let Some(name_node) = node.child_by_field_name("name")
        {
            let callee_name = state.node_text(name_node);
            state.unresolved_refs.push(UnresolvedRef {
                from_node_id: fn_node_id.to_string(),
                reference_name: callee_name.to_string(),
                reference_kind: EdgeKind::Calls,
                line: node.start_position().row as u32,
                column: node.start_position().column as u32,
                file_path: state.file_path.clone(),
            });
        }
        let mut cursor = node.walk();
        if cursor.goto_first_child() {
            loop {
                let child = cursor.node();
                match child.kind() {
                    // Skip nested function definitions.
                    "function_definition" => {}
                    _ => Self::extract_call_sites(state, child, fn_node_id),
                }
                if !cursor.goto_next_sibling() {
                    break;
                }
            }
        }
    }

    /// Find the first argument of a command node.
    ///
    /// In tree-sitter-bash, command arguments have the field name "argument".
    fn find_first_argument(state: &ExtractionState, node: TsNode<'_>) -> Option<String> {
        let mut cursor = node.walk();
        if cursor.goto_first_child() {
            loop {
                let child = cursor.node();
                if cursor.field_name() == Some("argument") {
                    return Some(state.node_text(child).to_string());
                }
                if !cursor.goto_next_sibling() {
                    break;
                }
            }
        }
        None
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

impl crate::LanguageExtractor for BashExtractor {
    fn extensions(&self) -> &[&str] {
        &["sh", "bash"]
    }

    fn language_name(&self) -> &'static str {
        "Bash"
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
