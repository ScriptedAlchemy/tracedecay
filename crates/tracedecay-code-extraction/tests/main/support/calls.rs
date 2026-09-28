// Shared call-attribution assertion for the extractor test suite.
//
// Paths stay fully qualified so this file can be `include!`d next to the
// other support files without duplicate imports.

/// `(owner kind, owner name, owner start line, call names)` for every node
/// that owns a Calls reference, in source order.
pub fn calls_by_owner(
    result: &tracedecay_domain::ExtractionResult,
) -> Vec<(&str, &str, u32, Vec<&str>)> {
    let mut owners = result
        .nodes
        .iter()
        .map(|node| {
            let calls = result
                .unresolved_refs
                .iter()
                .filter(|reference| {
                    reference.reference_kind == tracedecay_domain::EdgeKind::Calls
                        && reference.from_node_id == node.id
                })
                .map(|reference| reference.reference_name.as_str())
                .collect::<Vec<_>>();
            (
                node.kind.as_str(),
                node.name.as_str(),
                node.start_line,
                calls,
            )
        })
        .filter(|(_, _, _, calls)| !calls.is_empty())
        .collect::<Vec<_>>();
    owners.sort_by_key(|(_, _, line, _)| *line);
    owners
}
