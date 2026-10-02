//! Go satisfaction is implicit: a named type implements every interface whose
//! method set it carries, matched on qualified signatures, wherever in the
//! project the two are declared (`fixtures/go-satisfaction`).

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;

use tracedecay_code_index::production::CodeIndexPublishedGenerationV1;
use tracedecay_domain::{RelationEdgeKindV1, SymbolOccurrenceId};

use crate::cross_file_import_calls::publish_fixture_tree;

const FIXTURE_ROOT: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../tracedecay-code-extraction/fixtures/go-satisfaction"
);

fn published() -> Arc<CodeIndexPublishedGenerationV1> {
    publish_fixture_tree(Path::new(FIXTURE_ROOT), "go-satisfaction")
}

/// Every `Implements` edge as `(implementor, interface)` qualified names.
fn implements(generation: &CodeIndexPublishedGenerationV1) -> BTreeSet<(String, String)> {
    let name = |occurrence: &SymbolOccurrenceId| {
        generation
            .symbols()
            .symbols
            .iter()
            .find(|symbol| &symbol.occurrence == occurrence)
            .map(|symbol| symbol.qualified_name.clone())
            .expect("edge endpoint is a published symbol")
    };
    generation
        .edges()
        .iter()
        .filter(|edge| edge.kind == RelationEdgeKindV1::Implements)
        .map(|edge| (name(&edge.from_occurrence), name(&edge.to_occurrence)))
        .collect()
}

fn edge(from: &str, to: &str) -> (String, String) {
    (from.to_owned(), to.to_owned())
}

#[test]
fn go_type_in_another_file_implements_interface_by_qualified_signature() {
    let edges = implements(&published());
    assert!(
        edges.contains(&edge("calc/simple.go::Simple", "calc/adder.go::Adder")),
        "Simple implements Adder: {edges:?}"
    );
    assert!(
        !edges.contains(&edge("calc/wrong.go::Wrong", "calc/adder.go::Adder")),
        "Add(int64, int64) does not satisfy Add(int, int): {edges:?}"
    );
}

#[test]
fn go_signature_match_qualifies_types_through_import_aliases() {
    let edges = implements(&published());
    assert!(
        edges.contains(&edge("shapes/box.go::Box", "shapes/shape.go::Shape")),
        "g.Rect and geom.Rect are one type: {edges:?}"
    );
}

#[test]
fn go_unexported_interface_methods_bind_only_within_the_package() {
    let edges = implements(&published());
    assert!(
        edges.contains(&edge("priv/priv.go::Ok", "priv/priv.go::sealed")),
        "Ok implements sealed: {edges:?}"
    );
    assert!(
        !edges.contains(&edge("other/other.go::Intruder", "priv/priv.go::sealed")),
        "an unexported method outside the package satisfies nothing: {edges:?}"
    );
}
