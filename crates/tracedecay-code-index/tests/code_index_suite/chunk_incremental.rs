use std::sync::Arc;

use tracedecay_code_index::chunks::{CodeFileChunksV1, content_digest};
use tracedecay_code_index::chunks::{CodeSearchDocumentV1, CodeSearchEligibilityV1};
use tracedecay_code_index::incremental::{
    ChunkIncrementErrorV1, GenerationChunkManifestV1, plan_chunk_increment,
};
use tracedecay_domain::{
    BoundedSanitizedText, ChunkerRevision, CodeGenerationId, CodeSearchChunkAnchorV1,
    CodeSearchChunkGrainV1, CodeSearchChunkId, CodeSearchChunkV1, FileOccurrenceId,
    LanguageDescriptorRevision, PolicyRevisionId, SanitizerRevision, SensitivityDecision,
    SensitivityLevelV1, SourceSpan, SymbolOccurrenceId,
};

use tracedecay_domain::test_fixtures::id;

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
    assert_eq!(changes.reused_count, 4);
    for change in changes.added_or_changed.iter().chain(&changes.deleted) {
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
    assert!(removed.added_or_changed.is_empty() && removed.reused_count == 0);
    let initial = plan_chunk_increment(None, &current).unwrap();
    assert_eq!(initial.added_or_changed.len(), current.chunks().len());
    assert!(initial.deleted.is_empty() && initial.reused_count == 0);
}

#[test]
fn extraction_provenance_may_predate_publish_generation_and_duplicates_are_rejected() {
    let publish_generation = generation(2);
    let extraction_generation = generation(3);
    let carried = baseline_file(&extraction_generation, "file.carried", "src/lib.rs");
    let manifest = GenerationChunkManifestV1::new(publish_generation.clone(), vec![carried])
        .expect("carried extraction provenance may predate the publish generation");
    assert_eq!(manifest.generation_id(), &publish_generation);
    assert!(
        manifest
            .chunks()
            .iter()
            .all(|chunk| chunk.anchor.generation_id == extraction_generation)
    );

    let mut duplicate = baseline_file(&publish_generation, "file.duplicate", "src/lib.rs");
    duplicate.chunks.push(duplicate.chunks[0].clone());
    duplicate
        .document
        .chunk_ids
        .push(duplicate.chunks[0].id.clone());
    assert!(matches!(
        GenerationChunkManifestV1::new(publish_generation, vec![duplicate]),
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
