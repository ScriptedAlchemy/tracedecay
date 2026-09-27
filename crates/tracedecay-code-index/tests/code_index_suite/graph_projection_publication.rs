use std::sync::Arc;

use tracedecay_code_extraction::{ImportModuleKindV1, ImportNamespaceV1};
use tracedecay_code_index::{
    chunks::CodeIndexImportEvidenceV1,
    graph_projection::{
        CODE_GRAPH_PROJECTOR_REVISION, CodeGraphProjectionError, CodeGraphProjectionStore,
        code_graph_generation_id, code_graph_projection_identity,
    },
    parallelism,
    production::{
        CodeIndexBuildRequestV1, CodeIndexProductionOwnerV1, CodeIndexPublishedGenerationV1,
    },
};
use tracedecay_domain::{FileOccurrenceId, LanguageId, SourceSpan};
use tracedecay_graph_db::{
    GraphCancellation, GraphEntity, GraphGenerationManifest, GraphNamespace,
    GraphProjectorRevision, GraphProperty, NeverCancelled, VerifiedGraphSnapshot,
};

use crate::{
    production_orchestration::{
        ActiveControl, ApplyingProjectionSink, SharedPublicationStore, config, request_with_source,
    },
    support::{PartitionedSealV1, id},
};

const IMPORT_SOURCE: &str = concat!(
    "import type { Foo as LocalFoo } from \"pkg\";\n",
    "export function local() { return 1; }\n",
);
const IMPORT_LABEL: &str = "CodeImport";
const FILE_LABEL: &str = "CodeFile";
const SYMBOL_LABEL: &str = "CodeSymbol";
const FILE_IMPORT_RELATION: &str = "CodeFileContainsImport";

struct Cancelled;

impl GraphCancellation for Cancelled {
    fn is_cancelled(&self) -> bool {
        true
    }
}

fn import_request() -> CodeIndexBuildRequestV1 {
    let mut request = request_with_source(
        "file.graph-import",
        1_600_000,
        "commit.graph-import",
        "tree.graph-import",
        IMPORT_SOURCE,
    );
    request.snapshot.files[0].logical_path = "src/imports.ts".to_owned();
    request.snapshot.files[0].language = Some(id::<LanguageId>("typescript"));
    request.changed_files.clear();
    request.changed_files.insert("src/imports.ts".to_owned());
    request
        .snapshot
        .validate()
        .expect("TypeScript import snapshot is canonical");
    request
}

fn published_import_generation() -> Arc<CodeIndexPublishedGenerationV1> {
    let mut owner = CodeIndexProductionOwnerV1::new(
        config(),
        SharedPublicationStore::default(),
        ApplyingProjectionSink,
    )
    .expect("production owner");
    owner
        .build_and_publish(import_request(), &ActiveControl)
        .expect("parser-backed import generation publishes")
}

fn expected_import() -> CodeIndexImportEvidenceV1 {
    CodeIndexImportEvidenceV1 {
        logical_path: "src/imports.ts".to_owned(),
        file_occurrence_id: id::<FileOccurrenceId>("file.graph-import"),
        module_specifier: "pkg".to_owned(),
        imported_name: Some("Foo".to_owned()),
        local_name: Some("LocalFoo".to_owned()),
        is_public: false,
        reexport_scope: None,
        is_glob: false,
        namespace: ImportNamespaceV1::Type,
        module_kind: ImportModuleKindV1::BareModule,
        span: SourceSpan {
            start_byte: 14,
            end_byte: 29,
        },
        start_line: 0,
        start_column: 14,
    }
}

fn projection_manifest(
    generation: &CodeIndexPublishedGenerationV1,
    revision: &GraphProjectorRevision,
) -> GraphGenerationManifest {
    PartitionedSealV1::of(generation).graph_manifest(
        code_graph_projection_identity(
            GraphNamespace::new("code-graph-import-publication").expect("graph namespace"),
        )
        .expect("projection identity"),
        revision,
    )
}

fn current_projector_revision() -> GraphProjectorRevision {
    GraphProjectorRevision::try_from(CODE_GRAPH_PROJECTOR_REVISION.to_owned())
        .expect("current projector revision")
}

struct ForcedGraphWidth;

impl ForcedGraphWidth {
    fn install(width: usize) -> Self {
        parallelism::force_indexing_workers_for_test(width);
        Self
    }
}

impl Drop for ForcedGraphWidth {
    fn drop(&mut self) {
        parallelism::clear_forced_indexing_workers_for_test();
    }
}

#[test]
fn graph_manifest_is_identical_at_serial_and_parallel_widths() {
    let serial = {
        let _width = ForcedGraphWidth::install(1);
        projection_manifest(
            &published_import_generation(),
            &current_projector_revision(),
        )
    };
    let parallel = {
        let _width = ForcedGraphWidth::install(4);
        projection_manifest(
            &published_import_generation(),
            &current_projector_revision(),
        )
    };

    assert_eq!(serial, parallel);
    assert_eq!(
        serial
            .expected_recovered_digest(&|| Ok(()))
            .expect("serial graph digest"),
        parallel
            .expected_recovered_digest(&|| Ok(()))
            .expect("parallel graph digest"),
    );
}

fn has_label(entity: &GraphEntity, label: &str) -> bool {
    entity
        .labels
        .iter()
        .any(|candidate| candidate.as_str() == label)
}

fn verified_store(
    manifest: GraphGenerationManifest,
    generation: &CodeIndexPublishedGenerationV1,
) -> CodeGraphProjectionStore {
    let snapshot = VerifiedGraphSnapshot::memory(manifest, Arc::new(NeverCancelled))
        .expect("verified graph snapshot");
    CodeGraphProjectionStore::from_verified_snapshot(
        snapshot,
        generation.manifest().generation_id.clone(),
    )
    .expect("current verified graph store")
}

#[test]
fn published_generation_imports_survive_verified_projection_and_reader_open() {
    let generation = published_import_generation();
    let expected = expected_import();
    assert_eq!(generation.imports(), std::slice::from_ref(&expected));
    assert!(
        generation
            .symbols()
            .symbols
            .iter()
            .any(|symbol| symbol.simple_name == "local"),
        "the fixture must exercise real TypeScript symbol projection"
    );
    assert!(generation.symbols().symbols.iter().all(|symbol| {
        symbol.kind != "use" && !matches!(symbol.simple_name.as_str(), "pkg" | "Foo" | "LocalFoo")
    }));

    let manifest = projection_manifest(&generation, &current_projector_revision());
    let import_entities = manifest
        .entities
        .iter()
        .filter(|entity| has_label(entity, IMPORT_LABEL))
        .collect::<Vec<_>>();
    assert_eq!(import_entities.len(), 1);
    let import_entity = import_entities[0];
    assert!(!has_label(import_entity, SYMBOL_LABEL));

    let file_entity = manifest
        .entities
        .iter()
        .find(|entity| has_label(entity, FILE_LABEL))
        .expect("snapshot file is projected");
    let file_import_relations = manifest
        .relations
        .iter()
        .filter(|relation| relation.kind.as_str() == FILE_IMPORT_RELATION)
        .collect::<Vec<_>>();
    assert_eq!(file_import_relations.len(), 1);
    assert_eq!(file_import_relations[0].from.identity, file_entity.identity);
    assert_eq!(file_import_relations[0].to.identity, import_entity.identity);
    assert_eq!(
        manifest
            .entities
            .iter()
            .filter(|entity| has_label(entity, SYMBOL_LABEL))
            .count(),
        generation.symbols().symbols.len(),
        "import bindings and NodeKind::Use never become CodeSymbol entities"
    );

    let store = verified_store(manifest, &generation);
    let reader = store
        .interactive_reader_with_cancellation(
            &generation.manifest().generation_id,
            Arc::new(NeverCancelled),
        )
        .expect("generation-pinned reader");
    assert_eq!(
        reader
            .external_type_import_candidates("Foo", Some("src"), 8, Arc::new(NeverCancelled),)
            .expect("verified import candidates"),
        vec![expected]
    );
    assert_eq!(
        reader
            .external_type_import_candidates("Foo", Some("src"), 8, Arc::new(Cancelled))
            .expect_err("request cancellation remains typed"),
        CodeGraphProjectionError::Cancelled
    );
}

#[test]
fn sealed_generation_replay_rebuilds_identical_import_manifest_and_digest() {
    let generation = published_import_generation();
    let revision = current_projector_revision();
    let original = projection_manifest(&generation, &revision);
    let restored = PartitionedSealV1::of(&generation).restored();
    assert_eq!(restored.imports(), generation.imports());

    let replayed = projection_manifest(&restored, &revision);
    assert_eq!(
        replayed
            .canonical_replay_source(&|| Ok(()))
            .expect("replayed canonical bytes"),
        original
            .canonical_replay_source(&|| Ok(()))
            .expect("original canonical bytes")
    );
    assert_eq!(
        replayed
            .expected_recovered_digest(&|| Ok(()))
            .expect("replayed recovered digest"),
        original
            .expected_recovered_digest(&|| Ok(()))
            .expect("original recovered digest")
    );
}

#[test]
fn current_projector_changes_generation_identity_without_a_v4_alias() {
    let generation = published_import_generation();
    let current = current_projector_revision();
    let v4 = GraphProjectorRevision::try_from("code-graph-projector.v4".to_owned())
        .expect("prior projector revision remains valid data");
    let current_identity = code_graph_generation_id(&generation.manifest().generation_id, &current)
        .expect("current graph generation identity");
    let v4_identity = code_graph_generation_id(&generation.manifest().generation_id, &v4)
        .expect("v4 graph generation identity");
    assert_ne!(current_identity, v4_identity);

    let current_manifest = projection_manifest(&generation, &current);
    assert_eq!(current_manifest.generation, current_identity);
    let _store = verified_store(current_manifest, &generation);

    let v4_manifest = projection_manifest(&generation, &v4);
    assert_eq!(v4_manifest.generation, v4_identity);
    let v4_snapshot = VerifiedGraphSnapshot::memory(v4_manifest, Arc::new(NeverCancelled))
        .expect("prior graph snapshot is structurally valid");
    let error = CodeGraphProjectionStore::from_verified_snapshot(
        v4_snapshot,
        generation.manifest().generation_id.clone(),
    )
    .expect_err("a v4 graph snapshot cannot serve the current generation authority");
    assert_eq!(error, CodeGraphProjectionError::GenerationMismatch);
}

const CALL_SOURCE: &str = concat!(
    "pub fn alpha() -> u32 { beta() + gamma() }\n",
    "pub fn beta() -> u32 { gamma() }\n",
    "pub fn gamma() -> u32 { 1 }\n",
);

fn published_call_generation() -> Arc<CodeIndexPublishedGenerationV1> {
    let mut owner = CodeIndexProductionOwnerV1::new(
        config(),
        SharedPublicationStore::default(),
        ApplyingProjectionSink,
    )
    .expect("production owner");
    owner
        .build_and_publish(
            request_with_source(
                "file.graph-calls",
                1_600_000,
                "commit.graph-calls",
                "tree.graph-calls",
                CALL_SOURCE,
            ),
            &ActiveControl,
        )
        .expect("parser-backed call generation publishes")
}

/// Each code edge is one relation row from its source symbol to its target
/// symbol carrying the edge record, and every record is stored as compact
/// text, so a generation's rows and record bytes stay within this budget.
/// The previous projector added an edge entity plus two relations per edge
/// and stored records as JSON: 8 entities, 9 relations, 4,259 record bytes.
#[test]
fn graph_manifest_stores_each_code_edge_as_one_row_within_the_byte_budget() {
    let generation = published_call_generation();
    let manifest = projection_manifest(&generation, &current_projector_revision());
    let record_bytes = manifest
        .entities
        .iter()
        .flat_map(|entity| entity.properties.values())
        .chain(
            manifest
                .relations
                .iter()
                .flat_map(|relation| relation.properties.values()),
        )
        .map(|property| match property {
            GraphProperty::String(text) => text.len(),
            _ => 0,
        })
        .sum::<usize>();
    let mut relation_kinds = manifest
        .relations
        .iter()
        .map(|relation| relation.kind.as_str().to_owned())
        .collect::<Vec<_>>();
    relation_kinds.sort();
    assert_eq!(manifest.entities.len(), 5);
    assert_eq!(
        relation_kinds,
        [
            "CodeEdge.calls",
            "CodeEdge.calls",
            "CodeEdge.calls",
            "CodeFileContainsSymbol",
            "CodeFileContainsSymbol",
            "CodeFileContainsSymbol",
        ]
    );
    assert!(record_bytes <= 2_200, "records took {record_bytes} bytes");
    let symbol_record_bytes = manifest
        .entities
        .iter()
        .filter(|entity| has_label(entity, SYMBOL_LABEL))
        .flat_map(|entity| entity.properties.values())
        .map(|property| match property {
            GraphProperty::String(text) => text.len(),
            _ => 0,
        })
        .collect::<Vec<_>>();
    // A symbol record names its occurrence once, through its metadata; the
    // previous projector also spelled it at the top level: [494, 490, 492].
    assert_eq!(symbol_record_bytes, [444, 440, 442], "symbol record bytes");

    let names = generation
        .symbols()
        .symbols
        .iter()
        .map(|symbol| (symbol.occurrence.clone(), symbol.simple_name.clone()))
        .collect::<std::collections::BTreeMap<_, _>>();
    let occurrences = names.keys().cloned().collect::<Vec<_>>();
    let reader = verified_store(manifest, &generation)
        .interactive_reader_with_cancellation(
            &generation.manifest().generation_id,
            Arc::new(NeverCancelled),
        )
        .expect("generation-pinned reader");
    let mut edges = reader
        .edges_among(&occurrences, &[], 64, Arc::new(NeverCancelled))
        .expect("edges among the fixture symbols")
        .into_iter()
        .map(|edge| {
            (
                names[&edge.from_occurrence].as_str().to_owned(),
                names[&edge.to_occurrence].as_str().to_owned(),
            )
        })
        .collect::<Vec<_>>();
    edges.sort();
    assert_eq!(
        edges,
        vec![
            ("alpha".to_owned(), "beta".to_owned()),
            ("alpha".to_owned(), "gamma".to_owned()),
            ("beta".to_owned(), "gamma".to_owned()),
        ]
    );
}
