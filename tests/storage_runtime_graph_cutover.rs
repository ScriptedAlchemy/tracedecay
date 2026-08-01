#[path = "fixtures/storage_runtime/source_ast.rs"]
mod source_ast;

use std::collections::BTreeSet;

use serde::Deserialize;

use source_ast::{RustAst, has_path_suffix};

const GRAPH_ROUTES: &str = include_str!("fixtures/storage_runtime/graph_cutover_routes.json");

#[derive(Debug, Deserialize)]
struct GraphCutoverFixture {
    native_reader: String,
    native_mutation: String,
    physical_attachment: String,
    read_variants: Vec<String>,
    mutation_variants: Vec<String>,
    attachment_api: Vec<String>,
}

#[test]
fn graph_read_routes_cover_the_native_reader() {
    let fixture: GraphCutoverFixture =
        serde_json::from_str(GRAPH_ROUTES).expect("decode graph route fixture");
    let native = RustAst::parse(&fixture.native_reader);
    let native_paths = native.method_paths("GraphReaderExecutor", "execute_read");

    for variant in &fixture.read_variants {
        let path = format!("RuntimeReadOperationV1::{variant}");
        assert!(
            has_path_suffix(&native_paths, &path),
            "native graph reader omitted {path}"
        );
    }
}

#[test]
fn graph_mutations_are_a_closed_complete_vocabulary() {
    let fixture: GraphCutoverFixture =
        serde_json::from_str(GRAPH_ROUTES).expect("decode graph route fixture");
    let mutation = RustAst::parse(&fixture.native_mutation);
    let expected = fixture
        .mutation_variants
        .into_iter()
        .collect::<BTreeSet<_>>();
    // Subset, not equality: the fixture pins the mutations that must survive the
    // cutover. Production may add mutation payloads ahead of the fixture without
    // that being a regression; the dispatch-coverage loop below still proves
    // every declared mutation is routed by the executor.
    let published = mutation.enum_variants("GraphMutationPayloadV1");
    let missing = expected.difference(&published).cloned().collect::<Vec<_>>();
    assert!(
        missing.is_empty(),
        "graph cutover dropped declared mutation payload variants: {missing:?}"
    );

    let dispatch = mutation.method_paths("GraphMutationExecutor", "execute");
    for variant in expected {
        assert!(
            has_path_suffix(&dispatch, &format!("GraphMutationPayloadV1::{variant}")),
            "graph mutation executor omitted {variant}"
        );
    }
}

#[test]
fn graph_attachment_exposes_registry_consumable_parts_without_opening() {
    let fixture: GraphCutoverFixture =
        serde_json::from_str(GRAPH_ROUTES).expect("decode graph route fixture");
    let attachment = RustAst::parse(&fixture.physical_attachment);
    let structs = attachment.item_names("struct_item");
    assert!(structs.contains("GraphPhysicalAttachmentParts"));
    assert!(structs.contains("GraphPhysicalAttachmentFactory"));

    let methods = attachment.method_names("GraphPhysicalAttachmentParts");
    for method in fixture.attachment_api {
        assert!(
            methods.contains(&method),
            "graph physical attachment API omitted {method}"
        );
    }

    let prepare_calls = attachment.method_calls("GraphPhysicalAttachmentFactory", "prepare");
    for required in ["ExistingReaderLocator::new", "ExistingWriterLocator::new"] {
        assert!(
            prepare_calls
                .iter()
                .any(|call| call.callee.ends_with(required)),
            "graph attachment preparation omitted {required}"
        );
    }
    assert!(
        prepare_calls.iter().all(|call| {
            !call.callee.ends_with("Connection::open")
                && !call.callee.ends_with("Connection::open_with_flags")
        }),
        "graph attachment preparation must hand locators to the registry, not open SQLite"
    );
}
