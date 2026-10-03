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
    support::{PartitionedSealV1, cold_generation},
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
    let owner = |store: &SharedPublicationStore| {
        CodeIndexProductionOwnerV1::new(config(), store.clone(), ApplyingProjectionSink)
            .expect("production owner")
    };
    let paths = |files: &[(String, String)]| files.iter().map(|(path, _)| path.clone()).collect();
    let mut files = fixture_files(Path::new(FIXTURE_ROOT));
    let store = SharedPublicationStore::default();
    let mut incremental = owner(&store);
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
    let child = store.generation(
        &incremental
            .build_and_publish(
                fixture_tree_request(tag, 2, &files, &changed),
                &ActiveControl,
            )
            .expect("increment publishes"),
    );
    let cold = cold_generation(
        &owner(&SharedPublicationStore::default())
            .build_and_publish(
                fixture_tree_request(&format!("{tag}-cold"), 1, &files, &paths(&files)),
                &ActiveControl,
            )
            .expect("cold tree publishes"),
    );
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

#[test]
fn go_embedded_fields_and_aliases_promote_methods() {
    let (child, cold) = increment(
        "go-sat-promote",
        &[
            (
                "calc/wrapped.go",
                "package calc\n\ntype Wrapped struct {\n\tSimple\n}\n\ntype PtrWrapped struct {\n\t*Simple\n}\n\ntype Deep struct {\n\tWrapped\n}\n\ntype Same = Simple\n\ntype Failer interface {\n\tError() string\n}\n\ntype Failure struct {\n\terror\n}\n",
            ),
            (
                "shapes/framed.go",
                "package shapes\n\nimport \"example.com/sat/calc\"\n\ntype Framed struct {\n\tcalc.Simple\n}\n",
            ),
        ],
    );
    let edges = implements(&cold);
    for implementor in [
        "calc/wrapped.go::Wrapped",
        "calc/wrapped.go::PtrWrapped",
        "calc/wrapped.go::Deep",
        "calc/wrapped.go::Same",
        "shapes/framed.go::Framed",
    ] {
        assert!(
            edges.contains(&edge(implementor, "calc/adder.go::Adder")),
            "{implementor} carries Simple's Add: {edges:?}"
        );
    }
    assert!(
        edges.contains(&edge("calc/wrapped.go::Failure", "calc/wrapped.go::Failer")),
        "an embedded error promotes Error() string: {edges:?}"
    );
    assert!(!undecided(&cold, "calc/adder.go::Adder"));
    assert!(!undecided(&cold, "calc/wrapped.go::Failer"));
    assert_eq!(implements(&child), edges);
}

#[test]
fn go_struct_embedding_an_external_type_discloses_a_gap() {
    let (child, cold) = increment(
        "go-sat-external-embed",
        &[(
            "buf/buf.go",
            "package buf\n\nimport \"bytes\"\n\ntype Writer interface {\n\tWrite(p []byte) (int, error)\n}\n\ntype Tagged interface {\n\tWrite(p []byte) (int, error)\n\ttag()\n}\n\ntype Buf struct {\n\tbytes.Buffer\n}\n\ntype Outer struct {\n\tBuf\n}\n\nfunc (Outer) tag() {}\n",
        )],
    );
    for generation in [&child, &cold] {
        let edges = implements(generation);
        assert!(
            !edges
                .iter()
                .any(|(_, to)| to == "buf/buf.go::Writer" || to == "buf/buf.go::Tagged"),
            "bytes.Buffer's methods are outside the project: {edges:?}"
        );
        assert!(
            undecided(generation, "buf/buf.go::Writer"),
            "Buf may write through bytes.Buffer"
        );
        assert!(
            undecided(generation, "buf/buf.go::Tagged"),
            "Outer declares tag and may write through Buf's bytes.Buffer"
        );
        assert!(
            !undecided(generation, "shapes/shape.go::Shape"),
            "bytes.Buffer cannot supply Bounds() geom.Rect"
        );
        assert!(
            !undecided(generation, "priv/priv.go::sealed"),
            "bytes.Buffer cannot supply an unexported method"
        );
        assert!(edges.contains(&edge("shapes/box.go::Box", "shapes/shape.go::Shape")));
    }
}

#[test]
fn go_promotion_follows_selector_depth_and_ambiguity() {
    let (child, cold) = increment(
        "go-sat-promote-depth",
        &[(
            "calc/depth.go",
            "package calc\n\ntype Left struct {\n\t*Right\n\tSimple\n}\n\ntype Right struct {\n\t*Left\n}\n\ntype Shadowed struct {\n\tSimple\n}\n\nfunc (Shadowed) Add(a, b int64) int { return int(a + b) }\n\ntype Twin struct{}\n\nfunc (Twin) Add(x, y int) int { return x + y }\n\ntype Both struct {\n\tSimple\n\tTwin\n}\n\ntype Picked struct {\n\tBoth\n\tSimple\n}\n\ntype ViaA struct {\n\tSimple\n}\n\ntype ViaB struct {\n\tSimple\n}\n\ntype Diamond struct {\n\tViaA\n\tViaB\n}\n",
        )],
    );
    let edges = implements(&cold);
    for implementor in ["Left", "Right", "Twin", "Picked", "ViaA", "ViaB"] {
        assert!(
            edges.contains(&edge(
                &format!("calc/depth.go::{implementor}"),
                "calc/adder.go::Adder"
            )),
            "{implementor} selects one Add(int, int) int: {edges:?}"
        );
    }
    for implementor in ["Shadowed", "Both", "Diamond"] {
        assert!(
            !edges.contains(&edge(
                &format!("calc/depth.go::{implementor}"),
                "calc/adder.go::Adder"
            )),
            "{implementor}'s Add is hidden or ambiguous: {edges:?}"
        );
    }
    assert!(!undecided(&cold, "calc/adder.go::Adder"));
    assert_eq!(implements(&child), edges);
}

#[test]
fn go_empty_interface_discloses_a_gap() {
    let (child, cold) = increment(
        "go-sat-empty",
        &[("calc/any.go", "package calc\n\ntype Any interface{}\n")],
    );
    for generation in [&child, &cold] {
        assert!(
            undecided(generation, "calc/any.go::Any"),
            "every type implements an empty interface, so its implementor list is partial"
        );
    }
}

#[test]
fn go_external_test_package_shares_a_dir_but_not_unexported_methods() {
    let (child, cold) = increment(
        "go-sat-external-test",
        &[(
            "priv/priv_ext_test.go",
            "package priv_test\n\ntype Fake struct{}\n\nfunc (Fake) mark() {}\n",
        )],
    );
    for generation in [&child, &cold] {
        let edges = implements(generation);
        assert!(
            !edges.contains(&edge("priv/priv_ext_test.go::Fake", "priv/priv.go::sealed")),
            "package priv_test cannot satisfy priv's unexported mark(): {edges:?}"
        );
        assert!(edges.contains(&edge("priv/priv.go::Ok", "priv/priv.go::sealed")));
    }
}

#[test]
fn go_fields_hide_and_tie_promoted_methods() {
    let (child, cold) = increment(
        "go-sat-fields",
        &[(
            "calc/fielded.go",
            "package calc\n\ntype Fielded struct {\n\tSimple\n\tAdd int\n}\n\ntype Inner struct {\n\tAdd int\n}\n\ntype FieldTie struct {\n\tInner\n\tSimple\n}\n\ntype Add struct{}\n\ntype ByEmbed struct {\n\tSimple\n\tAdd\n}\n\ntype Kept struct {\n\tSimple\n\tname string\n}\n",
        )],
    );
    let edges = implements(&cold);
    for implementor in ["Fielded", "FieldTie", "ByEmbed"] {
        assert!(
            !edges.contains(&edge(
                &format!("calc/fielded.go::{implementor}"),
                "calc/adder.go::Adder"
            )),
            "{implementor}'s field named Add hides or ties Simple's Add: {edges:?}"
        );
    }
    assert!(
        edges.contains(&edge("calc/fielded.go::Kept", "calc/adder.go::Adder")),
        "a field with another name hides nothing: {edges:?}"
    );
    assert_eq!(implements(&child), edges);
}

#[test]
fn go_external_embedding_makes_same_depth_promotions_uncertain() {
    let (child, cold) = increment(
        "go-sat-external-tie",
        &[(
            "shapes/mixed.go",
            "package shapes\n\nimport (\n\t\"bytes\"\n\n\t\"example.com/sat/geom\"\n)\n\ntype Mixed struct {\n\tbytes.Buffer\n\tBox\n}\n\ntype Owned struct {\n\tbytes.Buffer\n}\n\nfunc (Owned) Bounds() geom.Rect { return geom.Rect{} }\n\nfunc (Owned) Name() string { return \"owned\" }\n",
        )],
    );
    for generation in [&child, &cold] {
        let edges = implements(generation);
        assert!(
            !edges.contains(&edge("shapes/mixed.go::Mixed", "shapes/shape.go::Shape")),
            "bytes.Buffer may carry Bounds or Name at Box's depth and tie it: {edges:?}"
        );
        assert!(
            edges.contains(&edge("shapes/mixed.go::Owned", "shapes/shape.go::Shape")),
            "declared methods sit above every promotion: {edges:?}"
        );
        assert!(undecided(generation, "shapes/shape.go::Shape"));
    }
}

#[test]
fn go_instantiated_generic_embedding_is_undecided_not_matched() {
    let (child, cold) = increment(
        "go-sat-generic-embed",
        &[(
            "cells/cells.go",
            "package cells\n\ntype T int\n\ntype Getter interface {\n\tGet() T\n}\n\ntype Cell[T any] struct{}\n\nfunc (Cell[T]) Get() T {\n\tvar v T\n\treturn v\n}\n\ntype Strings struct {\n\tCell[string]\n}\n",
        )],
    );
    for generation in [&child, &cold] {
        let edges = implements(generation);
        assert!(
            !edges.contains(&edge("cells/cells.go::Strings", "cells/cells.go::Getter")),
            "Cell[string].Get returns string, not cells.T: {edges:?}"
        );
        assert!(
            undecided(generation, "cells/cells.go::Getter"),
            "the seal does not substitute type arguments, so Getter stays undecided"
        );
    }
}
