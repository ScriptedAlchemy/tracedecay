//! Constructs that share a source line must receive distinct extraction-local
//! identities, so the edges and unresolved references minted while visiting
//! each construct bind to their own symbol. Constructs that begin their line
//! keep the line-keyed identity, so indentation never moves an id.

use std::collections::{BTreeMap, BTreeSet};

use tracedecay_code_extraction::{LanguageExtractor, RustExtractor, TypeScriptExtractor};
use tracedecay_domain::{EdgeKind, ExtractionResult, NodeKind, generate_node_id};

const SAME_LINE_RUST: &str = "struct A; struct B; fn alpha() {} fn beta() {} impl A { fn run() { alpha(); } } impl B { fn run() { beta(); } }\n";

const SAME_LINE_TYPESCRIPT: &str = "class A { run() { alpha(); } x = 1; } class B { run() { beta(); } x = 2; } function alpha() {} function beta() {}\n";

/// Qualified name of every node, keyed by id; fails when two nodes share one.
fn qualified_names_by_id(result: &ExtractionResult) -> BTreeMap<&str, &str> {
    let mut by_id = BTreeMap::new();
    for node in &result.nodes {
        if let Some(previous) = by_id.insert(node.id.as_str(), node.qualified_name.as_str()) {
            panic!(
                "node id {} is shared by {previous} and {}",
                node.id, node.qualified_name
            );
        }
    }
    by_id
}

/// `(owner qualified name, callee)` for every unresolved Calls reference.
fn calls(result: &ExtractionResult) -> BTreeSet<(String, String)> {
    let by_id = qualified_names_by_id(result);
    result
        .unresolved_refs
        .iter()
        .filter(|reference| reference.reference_kind == EdgeKind::Calls)
        .map(|reference| {
            (
                by_id[reference.from_node_id.as_str()].to_owned(),
                reference.reference_name.clone(),
            )
        })
        .collect()
}

/// `(container qualified name, member qualified name)` for every Contains edge.
fn containment(result: &ExtractionResult) -> BTreeSet<(String, String)> {
    let by_id = qualified_names_by_id(result);
    result
        .edges
        .iter()
        .filter(|edge| edge.kind == EdgeKind::Contains)
        .map(|edge| {
            (
                by_id[edge.source.as_str()].to_owned(),
                by_id[edge.target.as_str()].to_owned(),
            )
        })
        .collect()
}

fn pairs(items: &[(&str, &str)]) -> BTreeSet<(String, String)> {
    items
        .iter()
        .map(|(left, right)| ((*left).to_owned(), (*right).to_owned()))
        .collect()
}

#[test]
fn rust_same_line_methods_bind_their_own_calls_and_containers() {
    let result = RustExtractor.extract("test.rs", SAME_LINE_RUST);
    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);

    let runs = result
        .nodes
        .iter()
        .filter(|node| node.kind == NodeKind::Method && node.name == "run")
        .count();
    assert_eq!(runs, 2, "both same-line `run` methods must be extracted");

    assert!(calls(&result).is_superset(&pairs(&[
        ("test.rs::A::run", "alpha"),
        ("test.rs::B::run", "beta"),
    ])));
    assert!(containment(&result).is_superset(&pairs(&[
        ("test.rs::A", "test.rs::A::run"),
        ("test.rs::B", "test.rs::B::run"),
    ])));
}

#[test]
fn typescript_same_line_methods_and_fields_stay_distinct() {
    let result = TypeScriptExtractor.extract("test.ts", SAME_LINE_TYPESCRIPT);
    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);

    let fields = result
        .nodes
        .iter()
        .filter(|node| node.kind == NodeKind::Field && node.name == "x")
        .count();
    assert_eq!(fields, 2, "both same-line `x` fields must be extracted");

    assert!(calls(&result).is_superset(&pairs(&[
        ("test.ts::A::run", "alpha"),
        ("test.ts::B::run", "beta"),
    ])));
    assert!(containment(&result).is_superset(&pairs(&[
        ("test.ts::A", "test.ts::A::run"),
        ("test.ts::A", "test.ts::A::x"),
        ("test.ts::B", "test.ts::B::run"),
        ("test.ts::B", "test.ts::B::x"),
    ])));
}

#[test]
fn line_leading_constructs_keep_line_keyed_ids_regardless_of_indentation() {
    let compact = "impl A {\nfn run() {}\n}\n";
    let indented = "impl A {\n        fn run() {}\n}\n";
    let compact_ids: BTreeSet<String> = RustExtractor
        .extract("test.rs", compact)
        .nodes
        .into_iter()
        .map(|node| node.id)
        .collect();
    let indented_ids: BTreeSet<String> = RustExtractor
        .extract("test.rs", indented)
        .nodes
        .into_iter()
        .map(|node| node.id)
        .collect();
    assert_eq!(compact_ids, indented_ids, "indentation must not move ids");
    assert!(
        indented_ids.contains(&generate_node_id("test.rs", &NodeKind::Method, "run", 1)),
        "a construct that begins its line keeps the line-keyed id"
    );
}

#[test]
fn only_constructs_sharing_a_line_with_earlier_source_carry_a_column() {
    let result = RustExtractor.extract("test.rs", SAME_LINE_RUST);
    let struct_a = result
        .nodes
        .iter()
        .find(|node| node.kind == NodeKind::Struct && node.name == "A")
        .expect("struct A");
    assert_eq!(
        struct_a.id,
        generate_node_id("test.rs", &NodeKind::Struct, "A", 0),
        "the first construct on a line keeps the line-keyed id"
    );
    let struct_b = result
        .nodes
        .iter()
        .find(|node| node.kind == NodeKind::Struct && node.name == "B")
        .expect("struct B");
    assert_ne!(
        struct_b.id,
        generate_node_id("test.rs", &NodeKind::Struct, "B", 0),
        "a construct preceded by other source on its line carries its column"
    );
}
