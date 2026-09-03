// Shared docstring assertions for the extractor test suite.
//
// Every language's comment-style coverage asks the same question: after
// extracting a small snippet, does the node the comment sits above carry that
// comment as its docstring? Keeping that assertion in one place lets each
// language drive its comment styles from a table instead of restating the
// extract / no-errors / locate-node / check-docstring sequence per style.

use tracedecay_domain::{ExtractionResult, NodeKind};

/// Asserts that extraction produced no errors and that one node carries every
/// expected docstring fragment.
///
/// `name` selects the node. `None` requires that exactly one node of `kind`
/// exists — the "this snippet declares a single function" shape, which also
/// keeps the extractor honest about not inventing extra nodes. `Some(name)`
/// looks the node up by name instead, for snippets that necessarily declare
/// more than one node of that kind (a Pascal program body, for instance).
///
/// `case` labels every failure so a table row is identifiable from the message
/// alone.
pub fn assert_node_docstring(
    case: &str,
    result: &ExtractionResult,
    kind: NodeKind,
    name: Option<&str>,
    expected_fragments: &[&str],
) {
    assert!(
        result.errors.is_empty(),
        "{case}: errors: {:?}",
        result.errors
    );

    let node = match name {
        None => {
            let matching: Vec<_> = result.nodes.iter().filter(|n| n.kind == kind).collect();
            assert_eq!(
                matching.len(),
                1,
                "{case}: expected exactly one {kind:?} node, nodes: {:?}",
                result.nodes
            );
            matching[0]
        }
        Some(name) => result
            .nodes
            .iter()
            .find(|n| n.kind == kind && n.name == name)
            .unwrap_or_else(|| panic!("{case}: should find {kind:?} node named {name}")),
    };

    let doc = node
        .docstring
        .as_ref()
        .unwrap_or_else(|| panic!("{case}: {kind:?} node should have a docstring"));
    for fragment in expected_fragments {
        assert!(
            doc.contains(*fragment),
            "{case}: docstring {doc:?} is missing {fragment:?}"
        );
    }
}
