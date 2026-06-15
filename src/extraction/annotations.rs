use tree_sitter::Node as TsNode;

use crate::types::{generate_node_id, Edge, EdgeKind, Node, NodeKind, UnresolvedRef, Visibility};

pub(crate) trait AnnotationEmitterState {
    fn extract_annotation_name(&self, annotation_node: TsNode<'_>) -> String;
    fn file_path(&self) -> &str;
    fn qualified_prefix(&self) -> String;
    fn node_text(&self, node: TsNode<'_>) -> String;
    fn timestamp(&self) -> u64;
    fn push_node(&mut self, node: Node);
    fn push_edge(&mut self, edge: Edge);
    fn push_unresolved_ref(&mut self, unresolved_ref: UnresolvedRef);
}

pub(crate) fn emit_annotation_usage<S: AnnotationEmitterState>(
    state: &mut S,
    annotation_node: TsNode<'_>,
    target_id: &str,
    attrs_start_line: u32,
) {
    let annot_name = state.extract_annotation_name(annotation_node);
    let start_line = annotation_node.start_position().row as u32;
    let end_line = annotation_node.end_position().row as u32;
    let start_column = annotation_node.start_position().column as u32;
    let end_column = annotation_node.end_position().column as u32;
    let qualified_name = format!("{}::@{}", state.qualified_prefix(), annot_name);
    let id = generate_node_id(
        state.file_path(),
        &NodeKind::AnnotationUsage,
        &annot_name,
        start_line,
    );

    state.push_node(Node {
        id: id.clone(),
        kind: NodeKind::AnnotationUsage,
        name: annot_name.clone(),
        qualified_name,
        file_path: state.file_path().to_string(),
        start_line,
        attrs_start_line,
        end_line,
        start_column,
        end_column,
        signature: Some(state.node_text(annotation_node).trim().to_string()),
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
        updated_at: state.timestamp(),
        parent_id: None,
    });

    state.push_unresolved_ref(UnresolvedRef {
        from_node_id: id.clone(),
        reference_name: annot_name,
        reference_kind: EdgeKind::Annotates,
        line: start_line,
        column: start_column,
        file_path: state.file_path().to_string(),
    });

    state.push_edge(Edge {
        source: id,
        target: target_id.to_string(),
        kind: EdgeKind::Annotates,
        line: Some(start_line),
    });
}

pub(crate) fn scan_children_for_annotation_kinds<'tree>(
    node: TsNode<'tree>,
    accepted_kinds: &[&str],
    mut visit: impl FnMut(TsNode<'tree>),
) {
    let mut cursor = node.walk();
    if cursor.goto_first_child() {
        loop {
            let child = cursor.node();
            if child.kind() == "modifiers" {
                let mut modifier_cursor = child.walk();
                if modifier_cursor.goto_first_child() {
                    loop {
                        let modifier_child = modifier_cursor.node();
                        if accepted_kinds.contains(&modifier_child.kind()) {
                            visit(modifier_child);
                        }
                        if !modifier_cursor.goto_next_sibling() {
                            break;
                        }
                    }
                }
            }
            if !cursor.goto_next_sibling() {
                break;
            }
        }
    }
}
