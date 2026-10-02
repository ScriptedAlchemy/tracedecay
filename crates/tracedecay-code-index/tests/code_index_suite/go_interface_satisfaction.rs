//! Go satisfaction is implicit: a named type implements every interface whose
//! method set it carries, matched on qualified signatures, wherever in the
//! project the two are declared (`fixtures/go-satisfaction`).

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;

use tracedecay_code_index::production::{
    CodeIndexProductionOwnerV1, CodeIndexPublishedGenerationV1,
};
use tracedecay_domain::{RelationEdgeKindV1, SymbolOccurrenceId};
use tracedecay_graph_db::NeverCancelled;

use crate::{
    cross_file_import_calls::{fixture_files, fixture_tree_request, publish_fixture_tree},
    production_orchestration::{
        ActiveControl, ApplyingProjectionSink, SharedPublicationStore, config,
    },
    support::PartitionedSealV1,
    typescript_module_resolution::reader,
};

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

fn occurrence(
    generation: &CodeIndexPublishedGenerationV1,
    qualified_name: &str,
) -> SymbolOccurrenceId {
    generation
        .symbols()
        .symbols
        .iter()
        .find(|symbol| symbol.qualified_name == qualified_name && symbol.kind != "go_package")
        .unwrap_or_else(|| panic!("missing symbol {qualified_name}"))
        .occurrence
        .clone()
}

fn undecided(generation: &CodeIndexPublishedGenerationV1, interface: &str) -> bool {
    reader(generation)
        .has_undecided_implementors(
            &[occurrence(generation, interface)],
            Arc::new(NeverCancelled),
        )
        .expect("implementor gaps read")
}

/// Publishes the fixture, then `edits` as one increment over it (a path
/// absent from the fixture adds a file). Returns the increment and a cold
/// publish of the same tree.
fn increment(
    tag: &str,
    edits: &[(&str, &str)],
) -> (
    Arc<CodeIndexPublishedGenerationV1>,
    Arc<CodeIndexPublishedGenerationV1>,
) {
    let owner = || {
        CodeIndexProductionOwnerV1::new(
            config(),
            SharedPublicationStore::default(),
            ApplyingProjectionSink,
        )
        .expect("production owner")
    };
    let paths = |files: &[(String, String)]| files.iter().map(|(path, _)| path.clone()).collect();
    let mut files = fixture_files(Path::new(FIXTURE_ROOT));
    let mut incremental = owner();
    incremental
        .build_and_publish(
            fixture_tree_request(tag, 1, &files, &paths(&files)),
            &ActiveControl,
        )
        .expect("parent publishes");
    let mut changed = BTreeSet::new();
    for (path, source) in edits {
        match files.iter_mut().find(|(existing, _)| existing == path) {
            Some(file) => (*source).clone_into(&mut file.1),
            None => files.push(((*path).to_owned(), (*source).to_owned())),
        }
        changed.insert((*path).to_owned());
    }
    let child = incremental
        .build_and_publish(
            fixture_tree_request(tag, 2, &files, &changed),
            &ActiveControl,
        )
        .expect("increment publishes");
    let cold = owner()
        .build_and_publish(
            fixture_tree_request(&format!("{tag}-cold"), 1, &files, &paths(&files)),
            &ActiveControl,
        )
        .expect("cold tree publishes");
    (child, cold)
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

#[test]
fn go_interface_embedding_an_external_type_emits_a_gap_not_edges() {
    let generation = published();
    let edges = implements(&generation);
    assert!(
        !edges.iter().any(|(_, to)| to == "io/reader.go::Rows"),
        "io.Reader is not in the project, so Rows has no decided implementor: {edges:?}"
    );
    assert!(undecided(&generation, "io/reader.go::Rows"));
    assert!(!undecided(&generation, "calc/adder.go::Adder"));
}

#[test]
fn go_generic_interface_is_a_disclosed_gap() {
    let generation = published();
    let edges = implements(&generation);
    assert!(
        !edges.iter().any(|(_, to)| to == "gen/gen.go::Box"),
        "a generic interface has no decided implementor: {edges:?}"
    );
    assert!(undecided(&generation, "gen/gen.go::Box"));
}

#[test]
fn go_satisfaction_edges_leave_callers_coverage_alone() {
    let generation = published();
    let rows = occurrence(&generation, "io/reader.go::Rows");
    assert!(
        !reader(&generation)
            .has_unresolved_callers(&[rows], None, Arc::new(NeverCancelled))
            .expect("caller gaps read"),
        "an implementor gap is not an unresolved call"
    );
}

#[test]
fn go_increment_editing_an_implementor_recomputes_satisfaction() {
    let (child, cold) = increment(
        "go-sat-edit",
        &[(
            "calc/wrong.go",
            "package calc\n\ntype Wrong struct{}\n\nfunc (Wrong) Add(a, b int) int {\n\treturn a + b\n}\n",
        )],
    );
    let edges = implements(&child);
    assert!(edges.contains(&edge("calc/wrong.go::Wrong", "calc/adder.go::Adder")));
    assert!(edges.contains(&edge("calc/simple.go::Simple", "calc/adder.go::Adder")));
    assert_eq!(edges, implements(&cold));
    assert!(undecided(&child, "io/reader.go::Rows"));
}

#[test]
fn go_increment_removing_a_method_drops_the_edge() {
    let (child, cold) = increment(
        "go-sat-remove",
        &[("calc/simple.go", "package calc\n\ntype Simple struct{}\n")],
    );
    let edges = implements(&child);
    assert!(!edges.contains(&edge("calc/simple.go::Simple", "calc/adder.go::Adder")));
    assert!(edges.contains(&edge("shapes/box.go::Box", "shapes/shape.go::Shape")));
    assert_eq!(edges, implements(&cold));
}

#[test]
fn go_increment_without_go_edits_carries_satisfaction_forward() {
    let (child, cold) = increment(
        "go-sat-carry",
        &[("notes/note.ts", "export const note = 1;\n")],
    );
    let edges = implements(&child);
    assert_eq!(edges, implements(&published()));
    assert_eq!(edges, implements(&cold));
    assert!(undecided(&child, "io/reader.go::Rows"));
    assert!(undecided(&child, "gen/gen.go::Box"));
}

#[test]
fn go_satisfaction_survives_sealed_restore() {
    let generation = published();
    let sealed = PartitionedSealV1::of(&generation);
    let restored = sealed.restored();
    assert_eq!(implements(&restored), implements(&generation));
    assert!(undecided(&restored, "io/reader.go::Rows"));
    assert_eq!(PartitionedSealV1::of(&restored).manifest, sealed.manifest);
}
