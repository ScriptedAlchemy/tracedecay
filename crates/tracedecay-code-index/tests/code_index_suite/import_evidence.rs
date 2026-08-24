use std::sync::Arc;

use serde_json::Value;
use tracedecay_code_extraction::{ImportModuleKindV1, ImportNamespaceV1};
use tracedecay_code_index::{
    chunks::{
        ChunkingFailureV1, CodeFileIndexArtifactsV1, CodeIndexImportEvidenceV1, content_digest,
    },
    extract::EXTRACTION_ROWS_SEPARATOR,
    production::{
        CodeIndexBuildRequestV1, CodeIndexCapturedFileV1, CodeIndexProductionErrorV1,
        CodeIndexProductionOwnerV1, CodeIndexPublishedGenerationV1,
        SEALED_GENERATION_FORMAT_REVISION_V1, sealed_generation_payload_digest,
    },
};
use tracedecay_domain::{
    CodeGenerationId, FileOccurrenceId, LanguageId, SanitizedCodeFileV1, SensitivityLevelV1,
    SnapshotFileDispositionV1, SourceSpan, canonical_sha256,
};

use crate::{
    production_orchestration::{
        ActiveControl, ApplyingProjectionSink, SharedPublicationStore, config, request_with_source,
    },
    support::id,
};

const FIRST_SOURCE: &str = concat!(
    "import type { Foo, Bar as Baz } from \"../models\";\n",
    "export function local() { return Foo; }\n",
);
const SECOND_SOURCE: &str = concat!(
    "import { run as execute } from \"runner\";\n",
    "execute();\n",
);

fn import_request() -> CodeIndexBuildRequestV1 {
    let mut request = request_with_source(
        "file.import.a",
        1_500_000,
        "commit.imports.1",
        "tree.imports.1",
        FIRST_SOURCE,
    );
    let first = &mut request.snapshot.files[0];
    first.logical_path = "src/a.ts".to_owned();
    first.language = Some(id::<LanguageId>("typescript"));

    let second_bytes = SECOND_SOURCE.as_bytes().to_vec();
    let second_occurrence = id::<FileOccurrenceId>("file.import.b");
    request.snapshot.files.push(SanitizedCodeFileV1 {
        file_occurrence_id: second_occurrence.clone(),
        logical_path: "src/b.ts".to_owned(),
        language: Some(id::<LanguageId>("typescript")),
        content_digest: content_digest(&second_bytes),
        disposition: SnapshotFileDispositionV1::Present,
    });
    request.captured_files.push(CodeIndexCapturedFileV1 {
        file_occurrence_id: second_occurrence,
        sanitized_bytes: second_bytes,
        sensitivity_level: SensitivityLevelV1::Public,
    });
    request.snapshot.content_identity = content_digest(
        [FIRST_SOURCE.as_bytes(), SECOND_SOURCE.as_bytes()]
            .concat()
            .as_slice(),
    );
    request.changed_files.clear();
    request.changed_files.insert("src/a.ts".to_owned());
    request.changed_files.insert("src/b.ts".to_owned());
    request
        .snapshot
        .validate()
        .expect("two-file import snapshot is canonical");
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

fn sealed_envelope(generation: &CodeIndexPublishedGenerationV1) -> Value {
    serde_json::from_slice(&generation.encode_sealed().expect("import generation seals"))
        .expect("sealed generation JSON")
}

fn file_artifact(envelope: &Value, index: usize) -> CodeFileIndexArtifactsV1 {
    serde_json::from_value(envelope["generation"]["files"][index]["artifacts"].clone())
        .expect("file artifact JSON")
}

fn import_rows_mut(envelope: &mut Value, file_index: usize) -> &mut Vec<Value> {
    envelope["generation"]["files"][file_index]["artifacts"]["imports"]
        .as_array_mut()
        .expect("sealed file imports")
}

fn assert_serialized_artifact_has_no_self_digest(envelope: &Value, file_index: usize) {
    let imports = serde_json::from_value::<Vec<CodeIndexImportEvidenceV1>>(
        envelope["generation"]["files"][file_index]["artifacts"]["imports"].clone(),
    )
    .expect("forged canonical import rows");
    let recomputed = canonical_sha256(&("attacker-controlled-import-rows", imports.as_slice()))
        .expect("forged canonical import-row digest");
    assert!(recomputed.as_str().starts_with("sha256:"));
    assert!(
        envelope["generation"]["files"][file_index]["artifacts"]
            .get("import_rows_digest")
            .is_none(),
        "sealed file artifacts must not contain a recomputable self-digest authority"
    );
}

fn reseal_import_envelope(mut envelope: Value) -> Vec<u8> {
    let format_revision = u32::try_from(
        envelope["generation"]["format_revision"]
            .as_u64()
            .expect("forged payload format revision"),
    )
    .expect("format revision fits u32");
    let state_digest = sealed_generation_payload_digest(format_revision, &envelope["generation"])
        .expect("forged payload state digest");
    envelope["state_digest"] = Value::String(state_digest.as_str().to_owned());
    serde_json::to_vec(&envelope).expect("forged sealed generation JSON")
}

fn resealed_import_payload_error(envelope: Value, mutation: &str) -> CodeIndexProductionErrorV1 {
    let bytes = reseal_import_envelope(envelope);

    match CodeIndexPublishedGenerationV1::decode_sealed(&bytes) {
        Ok(_) => panic!("{mutation} restored after the outer state digest was recomputed"),
        Err(error) => error,
    }
}

fn assert_resealed_import_payload_is_rejected(envelope: Value, mutation: &str) {
    let _ = resealed_import_payload_error(envelope, mutation);
}

fn assert_sealed_envelope_restores(envelope: &Value) {
    CodeIndexPublishedGenerationV1::decode_sealed(&reseal_import_envelope(envelope.clone()))
        .expect("baseline generation restores");
}

#[test]
fn file_import_artifacts_require_nondefault_canonical_rows() {
    let generation = published_import_generation();
    let envelope = sealed_envelope(&generation);
    let artifacts = file_artifact(&envelope, 0);

    assert_eq!(
        artifacts
            .imports
            .iter()
            .map(|row| (
                row.logical_path.as_str(),
                row.file_occurrence_id.as_str(),
                row.module_specifier.as_str(),
                row.imported_name.as_deref(),
                row.local_name.as_deref(),
                row.namespace,
                row.module_kind,
                row.span,
                row.start_line,
                row.start_column,
            ))
            .collect::<Vec<_>>(),
        vec![
            (
                "src/a.ts",
                "file.import.a",
                "../models",
                Some("Foo"),
                Some("Foo"),
                ImportNamespaceV1::Type,
                ImportModuleKindV1::ProjectRelative,
                SourceSpan {
                    start_byte: 14,
                    end_byte: 17,
                },
                0,
                14,
            ),
            (
                "src/a.ts",
                "file.import.a",
                "../models",
                Some("Bar"),
                Some("Baz"),
                ImportNamespaceV1::Type,
                ImportModuleKindV1::ProjectRelative,
                SourceSpan {
                    start_byte: 19,
                    end_byte: 29,
                },
                0,
                19,
            ),
        ]
    );
    artifacts.validate().expect("canonical import rows");

    let mut missing = envelope["generation"]["files"][0]["artifacts"].clone();
    assert!(
        missing
            .as_object_mut()
            .expect("file artifact object")
            .remove("imports")
            .is_some(),
        "the serialized artifact must carry its required imports field"
    );
    let error = serde_json::from_value::<CodeFileIndexArtifactsV1>(missing)
        .expect_err("imports must be a required field without a serde default");
    assert!(
        error.to_string().contains("missing field `imports`"),
        "unexpected missing-imports error: {error}"
    );

    let mut reordered = artifacts.clone();
    reordered.imports.swap(0, 1);
    let error = reordered
        .validate()
        .expect_err("source-order reversal must be rejected");
    assert!(matches!(error, ChunkingFailureV1::NonCanonicalIdentity(_)));

    let mut duplicated = artifacts;
    let duplicate = duplicated.imports[0].clone();
    duplicated.imports.insert(1, duplicate);
    let error = duplicated
        .validate()
        .expect_err("duplicate import rows must be rejected");
    assert!(matches!(error, ChunkingFailureV1::NonCanonicalIdentity(_)));
}

#[test]
fn file_import_artifacts_bind_file_consistent_path_and_nonempty_span_to_indexed_extent() {
    let generation = published_import_generation();
    let artifacts = file_artifact(&sealed_envelope(&generation), 0);
    let indexed_end = artifacts
        .chunks
        .chunks
        .iter()
        .map(|chunk| chunk.anchor.source_span.end_byte)
        .max()
        .expect("complete file has indexed chunks");

    assert!(artifacts.imports.iter().all(|row| {
        row.logical_path == "src/a.ts"
            && row.file_occurrence_id == artifacts.chunks.document.file_occurrence_id
            && !row.span.is_empty()
            && row.span.end_byte <= indexed_end
    }));
    assert_eq!(
        artifacts
            .imports
            .iter()
            .map(|row| {
                &FIRST_SOURCE[usize::try_from(row.span.start_byte).expect("span start")
                    ..usize::try_from(row.span.end_byte).expect("span end")]
            })
            .collect::<Vec<_>>(),
        vec!["Foo", "Bar as Baz"]
    );

    let mut wrong_file = artifacts.clone();
    for row in &mut wrong_file.imports {
        row.file_occurrence_id = id("file.foreign");
    }
    assert!(wrong_file.validate().is_err());

    let mut inconsistent_path = artifacts.clone();
    inconsistent_path.imports[1].logical_path = "src/foreign.ts".to_owned();
    assert!(inconsistent_path.validate().is_err());

    let mut empty_span = artifacts.clone();
    empty_span.imports[0].span.end_byte = empty_span.imports[0].span.start_byte;
    assert!(empty_span.validate().is_err());

    let mut out_of_bounds = artifacts;
    out_of_bounds.imports[0].span.end_byte = indexed_end + 1;
    assert!(out_of_bounds.validate().is_err());
}

#[test]
fn import_rows_rematerialize_file_occurrence_and_aggregate_exactly_per_generation() {
    let generation = published_import_generation();
    let envelope = sealed_envelope(&generation);
    let artifacts = file_artifact(&envelope, 0);
    let rematerialized = artifacts
        .rematerialize_for_generation(
            id::<CodeGenerationId>("generation.imports.rematerialized"),
            id::<FileOccurrenceId>("file.import.rematerialized"),
        )
        .expect("import artifact rematerializes");

    assert!(
        rematerialized
            .imports
            .iter()
            .all(|row| row.file_occurrence_id.as_str() == "file.import.rematerialized")
    );
    assert_eq!(
        rematerialized
            .imports
            .iter()
            .map(|row| (
                row.logical_path.as_str(),
                row.module_specifier.as_str(),
                row.imported_name.as_deref(),
                row.local_name.as_deref(),
                row.namespace,
                row.module_kind,
                row.span,
            ))
            .collect::<Vec<_>>(),
        artifacts
            .imports
            .iter()
            .map(|row| (
                row.logical_path.as_str(),
                row.module_specifier.as_str(),
                row.imported_name.as_deref(),
                row.local_name.as_deref(),
                row.namespace,
                row.module_kind,
                row.span,
            ))
            .collect::<Vec<_>>()
    );

    assert_eq!(
        generation
            .imports()
            .iter()
            .map(|row| (
                row.logical_path.as_str(),
                row.file_occurrence_id.as_str(),
                row.module_specifier.as_str(),
                row.imported_name.as_deref(),
                row.local_name.as_deref(),
                row.namespace,
                row.module_kind,
                row.span,
            ))
            .collect::<Vec<_>>(),
        vec![
            (
                "src/a.ts",
                "file.import.a",
                "../models",
                Some("Foo"),
                Some("Foo"),
                ImportNamespaceV1::Type,
                ImportModuleKindV1::ProjectRelative,
                SourceSpan {
                    start_byte: 14,
                    end_byte: 17,
                },
            ),
            (
                "src/a.ts",
                "file.import.a",
                "../models",
                Some("Bar"),
                Some("Baz"),
                ImportNamespaceV1::Type,
                ImportModuleKindV1::ProjectRelative,
                SourceSpan {
                    start_byte: 19,
                    end_byte: 29,
                },
            ),
            (
                "src/b.ts",
                "file.import.b",
                "runner",
                Some("run"),
                Some("execute"),
                ImportNamespaceV1::Value,
                ImportModuleKindV1::BareModule,
                SourceSpan {
                    start_byte: 9,
                    end_byte: 23,
                },
            ),
        ]
    );
}

#[test]
fn raw_use_and_imported_bindings_never_become_canonical_symbols() {
    let generation = published_import_generation();
    let symbols = &generation.symbols().symbols;

    assert!(
        symbols.iter().any(|symbol| symbol.simple_name == "local"),
        "the fixture must exercise canonical TypeScript symbol materialization"
    );
    assert!(symbols.iter().all(|symbol| {
        symbol.kind != "use"
            && !matches!(
                symbol.simple_name.as_str(),
                "../models" | "runner" | "Foo" | "Bar" | "Baz" | "run" | "execute"
            )
    }));
}

#[test]
fn import_generation_pins_extractor_rows_and_chunker_revision_axes() {
    assert_eq!(EXTRACTION_ROWS_SEPARATOR, "tracedecay.extraction-rows.v2");
    let generation = published_import_generation();
    let manifest = generation.manifest();

    assert_eq!(manifest.extractor_revisions.len(), 1);
    let (language, extractor_revision) = &manifest.extractor_revisions[0];
    assert_eq!(language.as_str(), "typescript");
    assert_eq!(extractor_revision.as_str(), "extractor.typescript.v2");
    assert_eq!(manifest.chunker_revision.as_str(), "chunker.v2");
}

#[test]
fn sealed_revision_six_import_generation_round_trips_to_identical_bytes() {
    assert_eq!(SEALED_GENERATION_FORMAT_REVISION_V1, 6);
    let first = published_import_generation();
    let first_sealed = first.encode_sealed().expect("first generation seals");
    let second_sealed = published_import_generation()
        .encode_sealed()
        .expect("identical generation seals");
    assert_eq!(first_sealed, second_sealed);

    let envelope: Value = serde_json::from_slice(&first_sealed).expect("sealed generation JSON");
    assert_eq!(envelope["generation"]["format_revision"], 6);
    let restored =
        CodeIndexPublishedGenerationV1::decode_sealed(&first_sealed).expect("rev6 restores");
    assert_eq!(restored.imports(), first.imports());
    assert_eq!(
        restored.encode_sealed().expect("restored generation seals"),
        first_sealed
    );
}

#[test]
fn sealed_import_generation_rejects_semantic_tampering_after_outer_digest_recompute() {
    let generation = published_import_generation();
    let mut envelope = sealed_envelope(&generation);
    assert_sealed_envelope_restores(&envelope);

    import_rows_mut(&mut envelope, 0)[0]["imported_name"] = Value::String("Forged".to_owned());
    file_artifact(&envelope, 0)
        .validate()
        .expect("binding-name tamper remains structurally canonical");
    let error = resealed_import_payload_error(envelope, "binding-name tamper");
    assert!(
        error
            .to_string()
            .contains("import evidence does not match parser-backed extraction rows"),
        "semantic tamper reached the wrong authority rejection: {error}"
    );
}

#[test]
fn sealed_import_generation_rejects_semantic_tampering_after_import_and_outer_digest_recompute() {
    let generation = published_import_generation();
    let mut envelope = sealed_envelope(&generation);
    assert_sealed_envelope_restores(&envelope);

    import_rows_mut(&mut envelope, 0)[0]["imported_name"] = Value::String("Forged".to_owned());
    assert_serialized_artifact_has_no_self_digest(&envelope, 0);
    file_artifact(&envelope, 0)
        .validate()
        .expect("self-consistent import digest remains structurally canonical");

    let error = resealed_import_payload_error(
        envelope,
        "binding-name tamper with recomputed import-row digest",
    );
    assert!(
        error
            .to_string()
            .contains("import evidence does not match parser-backed extraction rows"),
        "self-consistent import forgery reached the wrong authority rejection: {error}"
    );
}

#[test]
fn sealed_import_generation_rejects_reorder_and_duplicate_after_outer_digest_recompute() {
    let generation = published_import_generation();
    let envelope = sealed_envelope(&generation);
    assert_sealed_envelope_restores(&envelope);

    let mut reordered = envelope.clone();
    import_rows_mut(&mut reordered, 0).swap(0, 1);
    assert_resealed_import_payload_is_rejected(reordered, "row reorder");

    let mut duplicated = envelope;
    let duplicate = import_rows_mut(&mut duplicated, 0)[0].clone();
    import_rows_mut(&mut duplicated, 0).insert(1, duplicate);
    assert_resealed_import_payload_is_rejected(duplicated, "duplicate row");
}

#[test]
fn sealed_import_generation_rejects_wrong_file_path_and_span_after_outer_digest_recompute() {
    let generation = published_import_generation();
    let envelope = sealed_envelope(&generation);
    assert_sealed_envelope_restores(&envelope);

    let mut wrong_file = envelope.clone();
    for row in import_rows_mut(&mut wrong_file, 0) {
        row["file_occurrence_id"] = Value::String("file.foreign".to_owned());
    }
    assert_resealed_import_payload_is_rejected(wrong_file, "foreign file occurrence");

    let mut wrong_path = envelope.clone();
    for row in import_rows_mut(&mut wrong_path, 0) {
        row["logical_path"] = Value::String("src/foreign.ts".to_owned());
    }
    assert_resealed_import_payload_is_rejected(wrong_path, "foreign logical path");

    let mut empty_span = envelope.clone();
    let start = import_rows_mut(&mut empty_span, 0)[0]["span"]["start_byte"].clone();
    import_rows_mut(&mut empty_span, 0)[0]["span"]["end_byte"] = start;
    assert_resealed_import_payload_is_rejected(empty_span, "empty source span");

    let mut out_of_bounds = envelope;
    import_rows_mut(&mut out_of_bounds, 0)[0]["span"]["end_byte"] =
        Value::from(FIRST_SOURCE.len() as u64 + 1);
    assert_resealed_import_payload_is_rejected(out_of_bounds, "out-of-bounds source span");
}
