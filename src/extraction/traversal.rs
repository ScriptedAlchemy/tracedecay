use tree_sitter::Node as TsNode;

/// Returns whether `node` has a direct child whose tree-sitter kind exactly
/// matches `kind`.
///
/// This helper is intentionally language-agnostic: callers provide the raw
/// `kind()` string they expect, and the traversal checks direct children in
/// source order without filtering to named nodes.
#[allow(dead_code)]
pub(crate) fn has_direct_child_kind(node: TsNode<'_>, kind: &str) -> bool {
    find_direct_child_by_kind(node, kind).is_some()
}

/// Returns the first direct child whose tree-sitter kind exactly matches
/// `kind`.
///
/// The match is an exact `Node::kind()` string comparison. Both named and
/// anonymous children participate so extractor migrations preserve existing
/// behavior.
#[allow(dead_code)]
pub(crate) fn find_direct_child_by_kind<'tree>(
    node: TsNode<'tree>,
    kind: &str,
) -> Option<TsNode<'tree>> {
    let mut cursor = node.walk();
    if cursor.goto_first_child() {
        loop {
            let child = cursor.node();
            if child.kind() == kind {
                return Some(child);
            }
            if !cursor.goto_next_sibling() {
                break;
            }
        }
    }

    None
}

/// Returns the first descendant whose tree-sitter kind exactly matches `kind`.
///
/// Descendants are visited with a pre-order depth-first traversal over all
/// children so callers can replace the duplicated recursive extractor helpers
/// without changing search order.
#[allow(dead_code)]
pub(crate) fn find_descendant_by_kind<'tree>(
    node: TsNode<'tree>,
    kind: &str,
) -> Option<TsNode<'tree>> {
    let mut cursor = node.walk();
    if cursor.goto_first_child() {
        loop {
            let child = cursor.node();
            if child.kind() == kind {
                return Some(child);
            }
            if let Some(found) = find_descendant_by_kind(child, kind) {
                return Some(found);
            }
            if !cursor.goto_next_sibling() {
                break;
            }
        }
    }

    None
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::{find_descendant_by_kind, find_direct_child_by_kind, has_direct_child_kind};
    use crate::extraction::ts_provider;
    use tree_sitter::{Node as TsNode, Parser};

    fn parse_c_function(source: &str) -> TsNode<'_> {
        let mut parser = Parser::new();
        parser
            .set_language(&ts_provider::language("c"))
            .expect("c grammar should load");
        let tree = parser.parse(source, None).expect("c source should parse");
        let leaked = Box::leak(Box::new(tree));
        leaked
            .root_node()
            .named_child(0)
            .expect("expected a top-level item")
    }

    #[test]
    fn finds_direct_child_by_exact_kind() {
        let function = parse_c_function("int answer(void) { return 42; }");

        let body = find_direct_child_by_kind(function, "compound_statement")
            .expect("function_definition should contain a compound_statement child");

        assert_eq!(body.kind(), "compound_statement");
    }

    #[test]
    fn direct_child_helper_does_not_match_nested_children() {
        let function = parse_c_function("int answer(void) { return 42; }");

        assert!(find_direct_child_by_kind(function, "identifier").is_none());
        assert!(find_descendant_by_kind(function, "identifier").is_some());
    }

    #[test]
    fn reports_presence_of_direct_children() {
        let function = parse_c_function("int answer(void) { return 42; }");

        assert!(has_direct_child_kind(function, "function_declarator"));
        assert!(!has_direct_child_kind(function, "identifier"));
    }
}
