/// Tree-sitter based GW-BASIC source code extractor.
///
/// Parses GW-BASIC source files and emits nodes and edges for the code graph.
/// GW-BASIC extends MS BASIC 2.0 with DEF FN, WHILE/WEND, and more string
/// functions. This extractor synthesizes Function nodes from REM-labelled
/// sections that end with RETURN, extracts LET/assignments as Const, DEF FN
/// as Function, GOSUB/GOTO as call references, and REM lines as docstrings.
use std::time::Instant;

use tree_sitter::{Node as TsNode, Tree};

use crate::basic_common::{
    BasicLine, collect_selected_lines, derive_function_name, extract_top_level_calls,
    extract_top_level_lets, walk_for_calls,
};
use crate::common::{ExtractionState, basic_identifier_text, local_node_id};
use crate::traversal::find_direct_child_by_kind;
use crate::types::{
    ComplexityAnalysisV1, Edge, EdgeKind, Node, NodeKind, Visibility, generate_node_id,
};

/// Extracts code graph nodes and edges from GW-BASIC source files using tree-sitter.
pub struct GwBasicExtractor;

impl GwBasicExtractor {
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

        let mut selected_lines = Vec::new();
        let metrics = crate::parsed_extraction::visit_root_children(tree, scope, |child| {
            if child.kind() == "line" {
                selected_lines.push((child.start_byte(), child.end_byte()));
            }
        });
        let lines = collect_selected_lines(&state, tree, &selected_lines);

        // First pass: extract DEF FN definitions as Function nodes.
        Self::extract_def_fn(&mut state, &lines);

        // Second pass: extract top-level LET constants (before the first subroutine).
        extract_top_level_lets(&mut state, &lines);

        // Third pass: synthesize subroutine Function nodes from REM ... RETURN blocks.
        Self::extract_subroutines(&mut state, &lines);

        // Fourth pass: extract GOSUB/GOTO references at the file level (those not in subroutines).
        extract_top_level_calls(&mut state, &lines);

        state.node_stack.pop();

        crate::parsed_extraction::ParsedExtraction::complete(
            state.into_result(start),
            scope,
            metrics,
        )
    }

    /// Extract DEF FN definitions as Function nodes.
    ///
    /// GW-BASIC supports `DEF FNname(params) = expression` for user-defined functions.
    fn extract_def_fn(state: &mut ExtractionState, lines: &[BasicLine<'_>]) {
        for basic_line in lines {
            if basic_line.statement_kind != "def_fn_statement" {
                continue;
            }
            let Some(statement_list) = find_direct_child_by_kind(basic_line.node, "statement_list")
            else {
                continue;
            };
            let Some(statement) = find_direct_child_by_kind(statement_list, "statement") else {
                continue;
            };
            let Some(def_fn) = find_direct_child_by_kind(statement, "def_fn_statement") else {
                continue;
            };

            // Extract function name from user_function child.
            let Some(fn_name_node) = find_direct_child_by_kind(def_fn, "user_function") else {
                continue;
            };
            let fn_name = basic_identifier_text(state.source, fn_name_node);

            let start_line = basic_line.node.start_position().row as u32;
            let end_line = basic_line.node.end_position().row as u32;
            let start_column = basic_line.node.start_position().column as u32;
            let end_column = basic_line.node.end_position().column as u32;
            let qualified_name = format!("{}::{}", state.qualified_prefix(), fn_name);
            let id = local_node_id(
                &state.file_path,
                state.source,
                &NodeKind::Function,
                fn_name,
                basic_line.node,
            );
            let text = state.node_text(basic_line.node);

            let graph_node = Node {
                id: id.clone(),
                kind: NodeKind::Function,
                name: fn_name.to_string(),
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
    }

    /// Synthesize subroutine Function nodes from REM-labelled sections that end with RETURN.
    ///
    /// GW-BASIC has no formal subroutine syntax. We detect patterns like:
    /// ```basic
    /// 1000 REM VALIDATE CONFIGURATION
    /// 1010 IF H$ = "" THEN PRINT "ERROR": RETURN
    /// 1040 RETURN
    /// ```
    /// A REM line (or consecutive REM lines) followed by code ending with RETURN
    /// is treated as a subroutine. The REM text becomes the docstring, and a
    /// function name is derived from the first REM text.
    fn extract_subroutines(state: &mut ExtractionState, lines: &[BasicLine<'_>]) {
        let mut i = 0;
        while i < lines.len() {
            // Look for a REM line that starts a potential subroutine.
            if lines[i].statement_kind == "comment" {
                // Gather consecutive REM lines.
                let rem_start = i;
                let mut rem_comments: Vec<String> = Vec::new();
                while i < lines.len() && lines[i].statement_kind == "comment" {
                    if let Some(ref text) = lines[i].comment_text {
                        rem_comments.push(text.clone());
                    }
                    i += 1;
                }

                // Check if the lines following the REM block end with RETURN.
                let body_start = i;
                let mut body_end = i;
                let mut has_return = false;
                while body_end < lines.len() {
                    if lines[body_end].statement_kind == "return_statement" {
                        has_return = true;
                        body_end += 1;
                        break;
                    }
                    // An inline RETURN in an IF statement (e.g. `1010 IF ...
                    // THEN PRINT "ERROR": RETURN`) does not end the subroutine;
                    // only a standalone return_statement line does.
                    // Stop at the next REM block (which would be the start of another subroutine).
                    if lines[body_end].statement_kind == "comment" {
                        break;
                    }
                    body_end += 1;
                }

                if has_return && body_start < body_end {
                    // Derive a function name from the first REM comment.
                    let fn_name = derive_function_name(&rem_comments);
                    let docstring = if rem_comments.is_empty() {
                        None
                    } else {
                        Some(rem_comments.join("\n"))
                    };

                    // The subroutine spans from the first REM line to the RETURN line.
                    let first_node = lines[rem_start].node;
                    let last_node = lines[body_end - 1].node;
                    let start_line = first_node.start_position().row as u32;
                    let end_line = last_node.end_position().row as u32;
                    let start_column = first_node.start_position().column as u32;
                    let end_column = last_node.end_position().column as u32;
                    let qualified_name = format!("{}::{}", state.qualified_prefix(), fn_name);
                    let fn_id = local_node_id(
                        &state.file_path,
                        state.source,
                        &NodeKind::Function,
                        &fn_name,
                        first_node,
                    );

                    // Count complexity by walking body lines' AST nodes.
                    let mut branches: u32 = 0;
                    let mut loops: u32 = 0;
                    let mut returns: u32 = 0;
                    for line in &lines[body_start..body_end] {
                        Self::count_line_complexity(
                            line.node,
                            &mut branches,
                            &mut loops,
                            &mut returns,
                        );
                    }

                    // Use the line number of the first code line (after REMs) as the
                    // signature, so callers can reference it by GOSUB <line_number>.
                    let sig_line_num = lines[body_start].line_number;
                    let signature = format!("GOSUB {sig_line_num}");

                    let graph_node = Node {
                        id: fn_id.clone(),
                        kind: NodeKind::Function,
                        name: fn_name.clone(),
                        qualified_name,
                        file_path: state.file_path.clone(),
                        start_line,
                        attrs_start_line: start_line,
                        end_line,
                        start_column,
                        end_column,
                        signature: Some(signature),
                        docstring,
                        visibility: Visibility::Pub,
                        is_async: false,
                        branches,
                        loops,
                        returns,
                        max_nesting: 0,
                        unsafe_blocks: 0,
                        unchecked_calls: 0,
                        assertions: 0,
                        complexity_analysis: ComplexityAnalysisV1::Complete,
                        updated_at: state.timestamp,
                        parent_id: None,
                    };
                    state.nodes.push(graph_node);

                    // Contains edge from parent.
                    if let Some(parent_id) = state.parent_node_id() {
                        state.edges.push(Edge {
                            source: parent_id.to_string(),
                            target: fn_id.clone(),
                            kind: EdgeKind::Contains,
                            line: Some(start_line),
                        });
                    }

                    for line in &lines[body_start..body_end] {
                        walk_for_calls(state, line.node, &fn_id);
                    }

                    i = body_end;
                } else {
                    // REM block not followed by RETURN. Skip.
                    i = body_end;
                }
            } else {
                i += 1;
            }
        }
    }

    /// Count complexity metrics for a single line node by recursively walking its AST.
    fn count_line_complexity(
        node: TsNode<'_>,
        branches: &mut u32,
        loops: &mut u32,
        returns: &mut u32,
    ) {
        let kind = node.kind();
        match kind {
            "if_statement" => *branches += 1,
            "for_statement" | "while_statement" => *loops += 1,
            "return_statement" => *returns += 1,
            _ => {}
        }
        // Recurse into children.
        let mut cursor = node.walk();
        if cursor.goto_first_child() {
            loop {
                let child = cursor.node();
                Self::count_line_complexity(child, branches, loops, returns);
                if !cursor.goto_next_sibling() {
                    break;
                }
            }
        }
    }
}

impl crate::LanguageExtractor for GwBasicExtractor {
    fn extensions(&self) -> &[&str] {
        &["gw"]
    }

    fn language_name(&self) -> &'static str {
        "GW-BASIC"
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
