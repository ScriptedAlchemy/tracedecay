use std::fmt::Debug;
use std::sync::Arc;

use tracedecay_code_index::chunks::{CodeFileChunksV1, content_digest};
use tracedecay_code_index::chunks::{CodeSearchDocumentV1, CodeSearchEligibilityV1};
use tracedecay_code_index::generations::{
    FileExtractionActionV1, FileExtractionPlanV1, GenerationIncrementPlanV1,
};
use tracedecay_code_index::incremental::{
    ChunkIncrementErrorV1, GenerationChunkManifestV1, materialize_generation_increment,
    plan_chunk_increment,
};
use tracedecay_code_index::lineage::LineageKindV1;
use tracedecay_code_index::lineage::{GenerationSymbolIndexV1, LineageSymbolRecordV1};
use tracedecay_domain::{
    BoundedSanitizedText, ChunkerRevision, CodeGenerationId, CodeSearchChunkAnchorV1,
    CodeSearchChunkGrainV1, CodeSearchChunkId, CodeSearchChunkV1, ComplexityAnalysisV1,
    FileIdentityDigest, FileOccurrenceId, LanguageDescriptorRevision, ManifestDigest,
    PolicyRevisionId, SanitizerRevision, SensitivityDecision, SensitivityLevelV1, SourceSpan,
    SymbolIdentityDigest, SymbolOccurrenceId,
};

fn id<T>(value: &str) -> T
where
    T: TryFrom<String>,
    <T as TryFrom<String>>::Error: Debug,
{
    T::try_from(value.to_owned()).expect("valid fixture identity")
}

fn generation(sequence: u64) -> CodeGenerationId {
    id(&format!("generation.v1.aaaaaaaa.{sequence:08}"))
}

fn chunk(
    generation_id: &CodeGenerationId,
    file_occurrence_id: &FileOccurrenceId,
    chunk_id: &str,
    symbol_occurrence_id: Option<&str>,
    grain: CodeSearchChunkGrainV1,
    text: &str,
    start_byte: u64,
) -> Arc<CodeSearchChunkV1> {
    Arc::new(CodeSearchChunkV1 {
        id: id::<CodeSearchChunkId>(chunk_id),
        anchor: CodeSearchChunkAnchorV1 {
            generation_id: generation_id.clone(),
            file_occurrence_id: file_occurrence_id.clone(),
            symbol_occurrence_id: symbol_occurrence_id.map(id::<SymbolOccurrenceId>),
            parent_chunk_id: None,
            source_span: SourceSpan {
                start_byte,
                end_byte: start_byte + text.len() as u64,
            },
            grain,
            ordinal: 0,
        },
        content_digest: content_digest(text.as_bytes()),
        language_descriptor_revision: id::<LanguageDescriptorRevision>("descriptor.rust.v1"),
        chunker_revision: id::<ChunkerRevision>("chunker.v1"),
        sanitizer_revision: id::<SanitizerRevision>("sanitizer.v1"),
        sensitivity: SensitivityDecision {
            level: SensitivityLevelV1::Public,
            policy_revision: id::<PolicyRevisionId>("policy.v1"),
        },
        exact_terms: vec![],
        subtokens: vec![],
        sanitized_text: BoundedSanitizedText::new(text).expect("bounded fixture text"),
    })
}

fn file_chunks(
    generation_id: &CodeGenerationId,
    occurrence: &str,
    path: &str,
    preamble: &str,
    alpha_body: &str,
    beta_body: &str,
) -> CodeFileChunksV1 {
    let file_occurrence_id = id::<FileOccurrenceId>(occurrence);
    let key = path.replace(['/', '.'], "-");
    let chunks = vec![
        chunk(
            generation_id,
            &file_occurrence_id,
            &format!("chunk.v1.{key}.preamble"),
            None,
            CodeSearchChunkGrainV1::FilePreamble,
            preamble,
            0,
        ),
        chunk(
            generation_id,
            &file_occurrence_id,
            &format!("chunk.v1.{key}.alpha"),
            Some(&format!("symbol.{occurrence}.alpha")),
            CodeSearchChunkGrainV1::SymbolBody,
            alpha_body,
            100,
        ),
        chunk(
            generation_id,
            &file_occurrence_id,
            &format!("chunk.v1.{key}.beta"),
            Some(&format!("symbol.{occurrence}.beta")),
            CodeSearchChunkGrainV1::SymbolBody,
            beta_body,
            200,
        ),
        chunk(
            generation_id,
            &file_occurrence_id,
            &format!("chunk.v1.{key}.window"),
            None,
            CodeSearchChunkGrainV1::FileWindow,
            "trailing file context",
            300,
        ),
    ];
    let document_text = format!("{preamble}\n{alpha_body}\n{beta_body}\ntrailing file context");
    CodeFileChunksV1 {
        document: CodeSearchDocumentV1 {
            generation_id: generation_id.clone(),
            file_occurrence_id,
            content_digest: content_digest(document_text.as_bytes()),
            eligibility: CodeSearchEligibilityV1::Eligible,
            chunk_ids: chunks.iter().map(|chunk| chunk.id.clone()).collect(),
        },
        chunks,
    }
}

fn baseline_file(
    generation_id: &CodeGenerationId,
    occurrence: &str,
    path: &str,
) -> CodeFileChunksV1 {
    file_chunks(
        generation_id,
        occurrence,
        path,
        "//! Original module docs.",
        "pub fn alpha(value: u32) -> u32 { value + 1 }",
        "pub fn beta(value: u32) -> u32 { value * 2 }",
    )
}

#[test]
fn carry_forward_execution_rematerializes_chunks_and_preserves_lineage_continuity() {
    let prior_generation = generation(1);
    let current_generation = generation(2);
    let prior_file = baseline_file(&prior_generation, "file.a.1", "src/lib.rs");
    let prior_symbols = prior_file
        .chunks
        .iter()
        .filter_map(|chunk| {
            chunk
                .anchor
                .symbol_occurrence_id
                .as_ref()
                .map(|occurrence| {
                    Arc::new(LineageSymbolRecordV1 {
                        occurrence: occurrence.clone(),
                        identity: id::<SymbolIdentityDigest>(&format!(
                            "sha256:{}",
                            if chunk.sanitized_text.as_str().contains("alpha") {
                                "a".repeat(64)
                            } else {
                                "b".repeat(64)
                            }
                        )),
                        qualified_name: if chunk.sanitized_text.as_str().contains("alpha") {
                            "crate::alpha"
                        } else {
                            "crate::beta"
                        }
                        .to_owned(),
                        kind: "function".to_owned(),
                        simple_name: if chunk.sanitized_text.as_str().contains("alpha") {
                            "alpha"
                        } else {
                            "beta"
                        }
                        .to_owned(),
                        visibility: "private".to_owned(),
                        branches: 0,
                        loops: 0,
                        max_nesting: 0,
                        complexity_analysis: ComplexityAnalysisV1::Complete,
                        line_span: 1,
                        start_line: 0,
                        signature: None,
                        docstring: None,
                        is_async: false,
                        derives: Vec::new(),
                        skip_test_coverage: false,
                        file_identity: id::<FileIdentityDigest>(&format!(
                            "sha256:{}",
                            "f".repeat(64)
                        )),
                        content_digest: chunk.content_digest.clone(),
                    })
                })
        })
        .collect();
    let prior_symbols =
        GenerationSymbolIndexV1::new(prior_generation.clone(), prior_symbols).expect("prior index");
    let plan = GenerationIncrementPlanV1 {
        prior_generation: prior_generation.clone(),
        rebuild_triggers: vec![],
        invalidation_digest: id::<ManifestDigest>(&format!("sha256:{}", "d".repeat(64))),
        files: vec![FileExtractionPlanV1 {
            logical_path: "src/lib.rs".to_owned(),
            action: FileExtractionActionV1::CarryForward {
                file_occurrence_id: id("file.a.2"),
                prior_file_occurrence_id: id("file.a.1"),
                content_digest: prior_file.document.content_digest.clone(),
            },
        }],
        capture_changed_files: vec![],
        carried_forward: 1,
        reextract: 0,
        deleted: 0,
    };

    let materialized = materialize_generation_increment(
        &plan,
        current_generation.clone(),
        &[prior_file],
        vec![],
        &prior_symbols,
        vec![],
    )
    .expect("carry-forward materializes");

    assert_eq!(materialized.chunks.generation_id(), &current_generation);
    assert_eq!(materialized.symbols.generation_id, current_generation);
    assert_eq!(materialized.lineage.len(), 2);
    assert!(
        materialized
            .lineage
            .iter()
            .all(|candidate| candidate.kind == LineageKindV1::Unchanged)
    );
    for candidate in &materialized.lineage {
        assert_ne!(candidate.prior_occurrence, candidate.current_occurrence);
    }
}

fn manifest(
    generation_id: &CodeGenerationId,
    files: Vec<CodeFileChunksV1>,
) -> GenerationChunkManifestV1 {
    GenerationChunkManifestV1::new(generation_id.clone(), files).expect("canonical manifest")
}

#[test]
fn mixed_increment_preserves_sorted_change_partitions() {
    let prior_generation = generation(1);
    let current_generation = generation(2);
    let prior = manifest(
        &prior_generation,
        ["a", "c", "e"]
            .into_iter()
            .map(|name| {
                baseline_file(
                    &prior_generation,
                    &format!("file.{name}.1"),
                    &format!("src/{name}.rs"),
                )
            })
            .collect(),
    );
    let current = manifest(
        &current_generation,
        ["b", "c", "d"]
            .into_iter()
            .map(|name| {
                baseline_file(
                    &current_generation,
                    &format!("file.{name}.2"),
                    &format!("src/{name}.rs"),
                )
            })
            .collect(),
    );
    let changes = plan_chunk_increment(Some(&prior), &current).unwrap();
    assert_eq!(changes.added_or_changed.len(), 8);
    assert_eq!(changes.deleted.len(), 8);
    assert_eq!(changes.reused.len(), 4);
    for change in changes
        .added_or_changed
        .iter()
        .chain(&changes.deleted)
        .chain(&changes.reused)
    {
        assert_eq!(
            change.prior_digest.as_ref(),
            prior
                .chunk(&change.chunk_id)
                .map(|chunk| &chunk.content_digest)
        );
        assert_eq!(
            change.current_digest.as_ref(),
            current
                .chunk(&change.chunk_id)
                .map(|chunk| &chunk.content_digest)
        );
    }
    changes.validate().unwrap();

    let empty = manifest(&current_generation, vec![]);
    let removed = plan_chunk_increment(Some(&prior), &empty).unwrap();
    assert_eq!(removed.deleted.len(), prior.chunks().len());
    assert!(removed.added_or_changed.is_empty() && removed.reused.is_empty());
    let initial = plan_chunk_increment(None, &current).unwrap();
    assert_eq!(initial.added_or_changed.len(), current.chunks().len());
    assert!(initial.deleted.is_empty() && initial.reused.is_empty());
}

#[test]
fn mixed_snapshot_and_duplicate_chunk_identities_are_rejected_before_diffing() {
    let expected_generation = generation(2);
    let foreign_generation = generation(3);
    let foreign = baseline_file(&foreign_generation, "file.foreign", "src/lib.rs");
    assert_eq!(
        GenerationChunkManifestV1::new(expected_generation.clone(), vec![foreign]),
        Err(ChunkIncrementErrorV1::MixedGeneration)
    );

    let mut duplicate = baseline_file(&expected_generation, "file.duplicate", "src/lib.rs");
    duplicate.chunks.push(duplicate.chunks[0].clone());
    duplicate
        .document
        .chunk_ids
        .push(duplicate.chunks[0].id.clone());
    assert!(matches!(
        GenerationChunkManifestV1::new(expected_generation, vec![duplicate]),
        Err(ChunkIncrementErrorV1::DuplicateChunk(_))
    ));
}

#[test]
fn duplicate_file_occurrences_are_rejected_before_manifest_flattening() {
    let expected_generation = generation(2);
    let first = baseline_file(&expected_generation, "file.duplicate", "src/first.rs");
    let second = baseline_file(&expected_generation, "file.duplicate", "src/second.rs");

    assert_eq!(
        GenerationChunkManifestV1::new(expected_generation, vec![first, second]),
        Err(ChunkIncrementErrorV1::DuplicateFileOccurrence(id(
            "file.duplicate"
        )))
    );
}
