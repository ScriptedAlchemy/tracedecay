// Shared edge assertions for the extractor test suite.
//
// Paths stay fully qualified so this file can be `include!`d next to
// `docstrings.rs` without duplicate imports.

/// `Contains` edges as `(parent name, child name)` pairs in emission order.
pub fn contains_pairs(result: &tracedecay_domain::ExtractionResult) -> Vec<(&str, &str)> {
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
        .filter(|e| e.kind == tracedecay_domain::EdgeKind::Contains)
        .map(|e| (name_of(&e.source), name_of(&e.target)))
        .collect()
}
