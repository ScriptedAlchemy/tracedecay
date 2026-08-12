use std::collections::BTreeSet;
use std::sync::atomic::{AtomicUsize, Ordering};

use tracedecay_code_extraction::{ImportModuleKindV1, ImportNamespaceV1};
use tracedecay_domain::UtcMicros;
use tracedecay_graph_db::{GraphProperty, GraphPropertyName};

use super::*;
use crate::chunks::CodeIndexImportEvidenceV1;
use crate::graph_projection::CODE_GRAPH_PROJECTOR_REVISION;

const IMPORT_LABEL: &str = "CodeImport";
const FILE_IMPORT_RELATION: &str = "CodeFileContainsImport";

struct CancelAfterObservations {
    observations: AtomicUsize,
    allowed: usize,
}

impl GraphCancellation for CancelAfterObservations {
    fn is_cancelled(&self) -> bool {
        self.observations.fetch_add(1, Ordering::SeqCst) >= self.allowed
    }
}

fn import(
    file_occurrence: &str,
    logical_path: &str,
    module_specifier: &str,
    imported_name: Option<&str>,
    local_name: Option<&str>,
    namespace: ImportNamespaceV1,
    module_kind: ImportModuleKindV1,
    start_byte: u64,
) -> CodeIndexImportEvidenceV1 {
    CodeIndexImportEvidenceV1 {
        logical_path: logical_path.to_owned(),
        file_occurrence_id: id(file_occurrence),
        module_specifier: module_specifier.to_owned(),
        imported_name: imported_name.map(str::to_owned),
        local_name: local_name.map(str::to_owned),
        namespace,
        module_kind,
        span: SourceSpan {
            start_byte,
            end_byte: start_byte + 1,
        },
        start_line: u32::try_from(start_byte).expect("fixture line fits"),
        start_column: 0,
    }
}

pub(super) fn external_type_import(
    file_occurrence: &str,
    logical_path: &str,
    module_specifier: &str,
    imported_name: &str,
    local_name: &str,
    start_byte: u64,
) -> CodeIndexImportEvidenceV1 {
    import(
        file_occurrence,
        logical_path,
        module_specifier,
        Some(imported_name),
        Some(local_name),
        ImportNamespaceV1::Type,
        ImportModuleKindV1::BareModule,
        start_byte,
    )
}

pub(super) fn import_manifest(
    files: &[SanitizedCodeFileV1],
    imports: &[CodeIndexImportEvidenceV1],
    revision: &str,
) -> GraphGenerationManifest {
    let projection =
        code_graph_projection_identity(GraphNamespace::new("code-graph").expect("namespace"))
            .expect("projection identity");
    let symbols = GenerationSymbolIndexV1::new(generation(), Vec::new())
        .expect("empty symbol index is valid");
    build_code_graph_manifest_inputs_checked(
        projection,
        &generation(),
        &[],
        &[],
        Some(ProductionCodeGraphInputs {
            files,
            symbols: &symbols,
            imports,
        }),
        &GraphProjectorRevision::try_from(revision.to_owned()).expect("projector revision"),
        &|| Ok(()),
    )
    .expect("valid import projection")
}

fn import_reader(
    files: &[SanitizedCodeFileV1],
    imports: &[CodeIndexImportEvidenceV1],
) -> super::super::CodeGraphInteractiveReader {
    reader(&store_for(import_manifest(
        files,
        imports,
        CODE_GRAPH_PROJECTOR_REVISION,
    )))
}

pub(super) fn two_import_fixture() -> (Vec<SanitizedCodeFileV1>, Vec<CodeIndexImportEvidenceV1>) {
    (
        vec![
            file("file.import.a", "src/a.ts"),
            file("file.import.b", "src/b.ts"),
        ],
        vec![
            external_type_import("file.import.a", "src/a.ts", "pkg", "Foo", "FooA", 0),
            external_type_import("file.import.b", "src/b.ts", "pkg", "Foo", "FooB", 0),
        ],
    )
}

#[test]
fn all_import_rows_project_as_full_file_bound_entities_without_symbols() {
    let files = vec![file("file.import.main", "src/main.ts")];
    let admitted = external_type_import(
        "file.import.main",
        "src/main.ts",
        "@scope/pkg",
        "FooWidget",
        "LocalWidget",
        0,
    );
    let imports = vec![
        admitted.clone(),
        import(
            "file.import.main",
            "src/main.ts",
            "@scope/pkg",
            Some("FooWidget"),
            Some("ValueWidget"),
            ImportNamespaceV1::Value,
            ImportModuleKindV1::BareModule,
            2,
        ),
        import(
            "file.import.main",
            "src/main.ts",
            "./local",
            Some("FooWidget"),
            Some("RelativeWidget"),
            ImportNamespaceV1::Type,
            ImportModuleKindV1::ProjectRelative,
            4,
        ),
        import(
            "file.import.main",
            "src/main.ts",
            "@scope/pkg",
            None,
            None,
            ImportNamespaceV1::SideEffect,
            ImportModuleKindV1::BareModule,
            6,
        ),
    ];
    let manifest = import_manifest(&files, &imports, CODE_GRAPH_PROJECTOR_REVISION);

    assert_eq!(
        manifest
            .entities
            .iter()
            .filter(|entity| has_label(entity, IMPORT_LABEL))
            .count(),
        4,
        "the verified graph retains every canonical structured import row"
    );
    assert_eq!(
        manifest
            .entities
            .iter()
            .filter(|entity| has_label(entity, "CodeSymbol"))
            .count(),
        0,
        "an import needs no synthetic symbol entity"
    );
    assert_eq!(
        manifest
            .relations
            .iter()
            .filter(|relation| relation.kind.as_str() == FILE_IMPORT_RELATION)
            .count(),
        4
    );

    let reader = reader(&store_for(manifest));
    for query in ["SCOPE/P", "FooWidget"] {
        let candidates = reader
            .external_type_import_candidates(query, None, 8, request())
            .expect("read projected import");
        assert_eq!(
            candidates,
            vec![admitted.clone()],
            "candidate reads admit only Type + BareModule while preserving the full parser row"
        );
    }
}

#[test]
fn candidate_query_is_case_insensitive_but_never_matches_the_local_alias() {
    let files = vec![file("file.import.main", "src/main.ts")];
    let imports = vec![external_type_import(
        "file.import.main",
        "src/main.ts",
        "@scope/pkg",
        "FooWidget",
        "LocalWidget",
        0,
    )];
    let reader = import_reader(&files, &imports);

    assert_eq!(
        reader
            .external_type_import_candidates("SCOPE/P", None, 8, request())
            .expect("module containment query"),
        imports
    );
    assert_eq!(
        reader
            .external_type_import_candidates("oOw", None, 8, request())
            .expect("imported-name containment query"),
        imports
    );
    for query in ["LocalWidget", "   ", "does-not-match"] {
        assert!(
            reader
                .external_type_import_candidates(query, None, 8, request())
                .expect("truthful empty candidate query")
                .is_empty(),
            "query `{query}` must not alias a candidate"
        );
    }
    assert!(matches!(
        reader.external_type_import_candidates("pkg", None, 0, request()),
        Err(CodeGraphProjectionError::Contract(_))
    ));
}

#[test]
fn canonical_scope_is_applied_before_limit() {
    let mut files = Vec::new();
    let mut imports = Vec::new();
    for index in 0..8 {
        let occurrence = format!("file.import.outside.{index}");
        let path = format!("outside/{index:02}.ts");
        files.push(file(&occurrence, &path));
        imports.push(external_type_import(
            &occurrence,
            &path,
            "pkg",
            "Foo",
            "Foo",
            0,
        ));
    }
    files.push(file("file.import.inside", "src/inside.ts"));
    imports.push(external_type_import(
        "file.import.inside",
        "src/inside.ts",
        "pkg",
        "Foo",
        "Foo",
        0,
    ));

    let candidates = import_reader(&files, &imports)
        .external_type_import_candidates("PKG", Some("src"), 1, request())
        .expect("scope before limit");
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].logical_path, "src/inside.ts");
}

#[test]
fn candidates_are_deterministic_and_unique() {
    let files = vec![
        file("file.import.a", "src/a.ts"),
        file("file.import.b", "src/b.ts"),
        file("file.import.c", "src/c.ts"),
    ];
    let imports = vec![
        external_type_import("file.import.a", "src/a.ts", "pkg", "Foo", "FooA", 0),
        external_type_import("file.import.b", "src/b.ts", "pkg", "Foo", "FooB", 0),
        external_type_import("file.import.c", "src/c.ts", "pkg", "Foo", "FooC", 0),
    ];
    let reader = import_reader(&files, &imports);
    let first = reader
        .external_type_import_candidates("foo", None, 8, request())
        .expect("first query");
    let second = reader
        .external_type_import_candidates("FOO", None, 8, request())
        .expect("repeat query");

    assert_eq!(first, imports);
    assert_eq!(second, first);
    let unique: BTreeSet<_> = first
        .iter()
        .map(|row| {
            (
                row.logical_path.clone(),
                row.span.start_byte,
                row.module_specifier.clone(),
                row.imported_name.clone(),
            )
        })
        .collect();
    assert_eq!(unique.len(), first.len());
}

#[test]
fn lifecycle_and_request_cancellation_refuse_import_scans() {
    let (files, imports) = two_import_fixture();
    let store = store_for(import_manifest(
        &files,
        &imports,
        CODE_GRAPH_PROJECTOR_REVISION,
    ));
    let lifecycle = CancellationSignal::active("cancellation.import.lifecycle").expect("token");
    let reader = store
        .interactive_reader(&generation(), &lifecycle)
        .expect("reader before lifecycle cancellation");
    assert!(lifecycle.cancel(UtcMicros(1)));
    assert_eq!(
        reader
            .external_type_import_candidates("pkg", None, 8, request())
            .expect_err("cancelled lifecycle"),
        CodeGraphProjectionError::Cancelled
    );

    let reader = import_reader(&files, &imports);
    assert_eq!(
        reader
            .external_type_import_candidates("pkg", None, 8, Arc::new(CancelledNow))
            .expect_err("cancelled request before scan"),
        CodeGraphProjectionError::Cancelled
    );

    let (files, imports) = {
        let mut files = Vec::new();
        let mut imports = Vec::new();
        for index in 0..32 {
            let occurrence = format!("file.import.scan.{index:02}");
            let path = format!("src/scan-{index:02}.ts");
            files.push(file(&occurrence, &path));
            imports.push(external_type_import(
                &occurrence,
                &path,
                "pkg",
                "Foo",
                "Foo",
                0,
            ));
        }
        (files, imports)
    };
    let reader = import_reader(&files, &imports);
    let during_scan: Arc<dyn GraphCancellation> = Arc::new(CancelAfterObservations {
        observations: AtomicUsize::new(0),
        allowed: 16,
    });
    assert_eq!(
        reader
            .external_type_import_candidates("pkg", None, 64, during_scan)
            .expect_err("request cancelled during scan"),
        CodeGraphProjectionError::Cancelled
    );
}

#[test]
fn corrupt_import_payload_identity_and_file_link_are_refused() {
    let (files, imports) = two_import_fixture();

    let mut malformed = import_manifest(&files, &imports, CODE_GRAPH_PROJECTOR_REVISION);
    let entity = malformed
        .entities
        .iter_mut()
        .find(|entity| has_label(entity, IMPORT_LABEL))
        .expect("import entity");
    *entity
        .properties
        .values_mut()
        .next()
        .expect("import payload") = GraphProperty::Bytes(vec![b'{']);
    assert!(matches!(
        reader(&store_for(malformed)).external_type_import_candidates("pkg", None, 8, request()),
        Err(CodeGraphProjectionError::Corrupt(_))
    ));

    let mut wrong_identity = import_manifest(&files, &imports, CODE_GRAPH_PROJECTOR_REVISION);
    let indices: Vec<_> = wrong_identity
        .entities
        .iter()
        .enumerate()
        .filter_map(|(index, entity)| has_label(entity, IMPORT_LABEL).then_some(index))
        .collect();
    assert_eq!(indices.len(), 2);
    let first_payload = wrong_identity.entities[indices[0]].properties.clone();
    wrong_identity.entities[indices[0]].properties =
        wrong_identity.entities[indices[1]].properties.clone();
    wrong_identity.entities[indices[1]].properties = first_payload;
    assert!(matches!(
        reader(&store_for(wrong_identity)).external_type_import_candidates(
            "pkg",
            None,
            8,
            request()
        ),
        Err(CodeGraphProjectionError::Corrupt(_))
    ));

    let mut wrong_file = import_manifest(&files, &imports, CODE_GRAPH_PROJECTOR_REVISION);
    let relations: Vec<_> = wrong_file
        .relations
        .iter()
        .enumerate()
        .filter_map(|(index, relation)| {
            (relation.kind.as_str() == FILE_IMPORT_RELATION).then_some(index)
        })
        .collect();
    assert_eq!(relations.len(), 2);
    wrong_file.relations[relations[0]].from = wrong_file.relations[relations[1]].from.clone();
    assert!(matches!(
        reader(&store_for(wrong_file)).external_type_import_candidates("pkg", None, 8, request()),
        Err(CodeGraphProjectionError::Corrupt(_))
    ));
}

#[test]
fn corrupt_import_relation_identity_and_properties_are_refused() {
    let (files, imports) = two_import_fixture();

    let mut wrong_identity = import_manifest(&files, &imports, CODE_GRAPH_PROJECTOR_REVISION);
    wrong_identity
        .relations
        .iter_mut()
        .find(|relation| relation.kind.as_str() == FILE_IMPORT_RELATION)
        .expect("file-import relation")
        .identity = tracedecay_graph_db::GraphRelationId::new("relation.forged.import")
        .expect("valid forged relation identity");
    wrong_identity
        .relations
        .sort_by(|left, right| left.identity.cmp(&right.identity));
    assert!(matches!(
        reader(&store_for(wrong_identity)).external_type_import_candidates(
            "pkg",
            None,
            8,
            request()
        ),
        Err(CodeGraphProjectionError::Corrupt(_))
    ));

    let mut unexpected_property = import_manifest(&files, &imports, CODE_GRAPH_PROJECTOR_REVISION);
    unexpected_property
        .relations
        .iter_mut()
        .find(|relation| relation.kind.as_str() == FILE_IMPORT_RELATION)
        .expect("file-import relation")
        .properties
        .insert(
            GraphPropertyName::new("unexpected").expect("property name"),
            GraphProperty::String("forged".to_owned()),
        );
    assert!(matches!(
        reader(&store_for(unexpected_property)).external_type_import_candidates(
            "pkg",
            None,
            8,
            request()
        ),
        Err(CodeGraphProjectionError::Corrupt(_))
    ));
}

#[test]
fn projector_v4_is_accepted_and_v3_is_a_generation_mismatch() {
    assert_eq!(CODE_GRAPH_PROJECTOR_REVISION, "code-graph-projector.v4");
    let (files, imports) = two_import_fixture();
    let reader = import_reader(&files, &imports);
    assert_eq!(reader.generation(), &generation());

    let v3 = import_manifest(&files, &imports, "code-graph-projector.v3");
    let snapshot = VerifiedGraphSnapshot::memory(v3, Arc::new(NeverCancelled))
        .expect("open legacy-revision snapshot");
    assert_eq!(
        CodeGraphProjectionStore::from_verified_snapshot(snapshot, generation())
            .expect_err("v3 snapshot must not satisfy the v4 reader"),
        CodeGraphProjectionError::GenerationMismatch
    );
}
