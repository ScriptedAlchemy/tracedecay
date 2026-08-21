use std::sync::Arc;

use tracedecay_code_extraction::{ImportModuleKindV1, ImportNamespaceV1};
use tracedecay_code_index::{
    chunks::CodeIndexImportEvidenceV1,
    graph_projection::{
        CODE_GRAPH_PROJECTOR_REVISION, CodeGraphProjectionError, CodeGraphProjectionStore,
        build_published_code_graph_manifest_checked, code_graph_generation_id,
        code_graph_projection_identity,
    },
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
    support::id,
};

const IMPORT_SOURCE: &str = concat!(
    "import type { Foo as LocalFoo } from \"pkg\";\n",
    "export function local() { return 1; }\n",
);
const IMPORT_RECORD_PROPERTY: &str = "import-record";
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
    Arc::unwrap_or_clone(
        build_published_code_graph_manifest_checked(
            code_graph_projection_identity(
                GraphNamespace::new("code-graph-import-publication").expect("graph namespace"),
            )
            .expect("projection identity"),
            generation,
            revision,
            &|| Ok(()),
        )
        .expect("published generation projects"),
    )
}

fn current_projector_revision() -> GraphProjectorRevision {
    GraphProjectorRevision::try_from(CODE_GRAPH_PROJECTOR_REVISION.to_owned())
        .expect("current projector revision")
}

fn has_label(entity: &GraphEntity, label: &str) -> bool {
    entity
        .labels
        .iter()
        .any(|candidate| candidate.as_str() == label)
}

fn projected_import(entity: &GraphEntity) -> CodeIndexImportEvidenceV1 {
    let property = entity
        .properties
        .iter()
        .find(|(name, _)| name.as_str() == IMPORT_RECORD_PROPERTY)
        .map(|(_, value)| value)
        .expect("CodeImport carries its exact parser-backed record");
    let GraphProperty::Bytes(bytes) = property else {
        panic!("CodeImport record must use the canonical byte property");
    };
    serde_json::from_slice(bytes).expect("CodeImport record decodes")
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
    assert_eq!(projected_import(import_entity), expected);

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
    let sealed = generation.encode_sealed().expect("generation seals");
    let restored = CodeIndexPublishedGenerationV1::decode_sealed(&sealed)
        .expect("sealed import generation restores");
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
fn projector_v4_changes_generation_identity_without_a_v3_alias() {
    assert_eq!(CODE_GRAPH_PROJECTOR_REVISION, "code-graph-projector.v4");
    let generation = published_import_generation();
    let v4 = current_projector_revision();
    let v3 = GraphProjectorRevision::try_from("code-graph-projector.v3".to_owned())
        .expect("prior projector revision remains valid data");
    let v4_identity = code_graph_generation_id(&generation.manifest().generation_id, &v4)
        .expect("v4 graph generation identity");
    let v3_identity = code_graph_generation_id(&generation.manifest().generation_id, &v3)
        .expect("v3 graph generation identity");
    assert_ne!(v4_identity, v3_identity);

    let v4_manifest = projection_manifest(&generation, &v4);
    assert_eq!(v4_manifest.generation, v4_identity);
    let _store = verified_store(v4_manifest, &generation);

    let v3_manifest = projection_manifest(&generation, &v3);
    assert_eq!(v3_manifest.generation, v3_identity);
    let v3_snapshot = VerifiedGraphSnapshot::memory(v3_manifest, Arc::new(NeverCancelled))
        .expect("prior graph snapshot is structurally valid");
    let error = CodeGraphProjectionStore::from_verified_snapshot(
        v3_snapshot,
        generation.manifest().generation_id.clone(),
    )
    .expect_err("a v3 graph snapshot cannot serve the v4 generation authority");
    assert_eq!(error, CodeGraphProjectionError::GenerationMismatch);
}
