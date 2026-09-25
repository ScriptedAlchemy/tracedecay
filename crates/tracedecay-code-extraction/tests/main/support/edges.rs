// Shared edge assertions for the extractor test suite.
//
// Paths stay fully qualified so this file can be `include!`d next to
// `docstrings.rs` without duplicate imports.

/// Edges of `kind` as `(source name, target name)` pairs in emission order.
pub fn edge_pairs(
    result: &tracedecay_domain::ExtractionResult,
    kind: tracedecay_domain::EdgeKind,
) -> Vec<(&str, &str)> {
    let name_of = |id: &str| {
        result
            .nodes
            .iter()
            .find(|n| n.id == id)
            .map(|n| n.name.as_str())
            .unwrap_or_else(|| panic!("edge endpoint {id} is not an extracted node"))
    };
    result
        .edges
        .iter()
        .filter(|e| e.kind == kind)
        .map(|e| (name_of(&e.source), name_of(&e.target)))
        .collect()
}
