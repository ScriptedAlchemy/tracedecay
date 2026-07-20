use std::fmt::Debug;

use tracedecay::code_index::chunks::{CodeFileChunksV1, content_digest};
use tracedecay::code_index::incremental::{
    ChunkIncrementErrorV1, GenerationChunkManifestV1, plan_chunk_increment,
};
use tracedecay_domain::{
    BoundedSanitizedText, ChunkerRevision, CodeGenerationId, CodeSearchChunkAnchorV1,
    CodeSearchChunkGrainV1, CodeSearchChunkId, CodeSearchChunkV1, CodeSearchDocumentV1,
    CodeSearchEligibilityV1, FileOccurrenceId, LanguageDescriptorRevision, PolicyRevisionId,
    SanitizerRevision, SensitivityDecision, SensitivityLevelV1, SourceSpan, SymbolOccurrenceId,
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
) -> CodeSearchChunkV1 {
    CodeSearchChunkV1 {
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
    }
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
fn noop_generation_reuses_every_chunk_and_schedules_zero_projection_work() {
    let prior_generation = generation(1);
    let current_generation = generation(2);
    let prior = manifest(
        &prior_generation,
        vec![baseline_file(&prior_generation, "file.a.1", "src/lib.rs")],
    );
    let current = manifest(
        &current_generation,
        vec![baseline_file(&current_generation, "file.a.2", "src/lib.rs")],
    );

    let changes = plan_chunk_increment(Some(&prior), &current).expect("no-op plan");

    assert!(changes.added_or_changed.is_empty());
    assert!(changes.deleted.is_empty());
    assert_eq!(changes.reused.len(), prior.chunks().len());
    assert!(changes.reused.iter().all(|change| {
        change.prior_digest.is_some() && change.prior_digest == change.current_digest
    }));
    changes.validate().expect("changes validate");
}

#[test]
fn one_symbol_edit_reprojects_changed_symbol_but_reuses_siblings_and_file_context() {
    let prior_generation = generation(1);
    let current_generation = generation(2);
    let prior = manifest(
        &prior_generation,
        vec![baseline_file(&prior_generation, "file.a.1", "src/lib.rs")],
    );
    let current = manifest(
        &current_generation,
        vec![file_chunks(
            &current_generation,
            "file.a.2",
            "src/lib.rs",
            "//! Original module docs.",
            "pub fn alpha(value: u32) -> u32 { value + 2 }",
            "pub fn beta(value: u32) -> u32 { value * 2 }",
        )],
    );

    let changes = plan_chunk_increment(Some(&prior), &current).expect("symbol edit plan");

    assert_eq!(changes.added_or_changed.len(), 1);
    assert!(changes.added_or_changed.iter().all(|change| {
        current.chunk(&change.chunk_id).is_some_and(|chunk| {
            chunk.anchor.grain == CodeSearchChunkGrainV1::SymbolBody
                && chunk.sanitized_text.as_str().contains("alpha")
        })
    }));
    assert!(changes.reused.iter().any(|change| {
        current.chunk(&change.chunk_id).is_some_and(|chunk| {
            chunk.anchor.grain == CodeSearchChunkGrainV1::SymbolBody
                && chunk.sanitized_text.as_str().contains("beta")
        })
    }));
    assert!(changes.reused.iter().any(|change| {
        current
            .chunk(&change.chunk_id)
            .is_some_and(|chunk| chunk.anchor.grain == CodeSearchChunkGrainV1::FileWindow)
    }));
    assert!(changes.deleted.is_empty());
}

#[test]
fn preamble_edit_invalidates_only_the_preamble_chunk() {
    let prior_generation = generation(1);
    let current_generation = generation(2);
    let prior = manifest(
        &prior_generation,
        vec![baseline_file(&prior_generation, "file.a.1", "src/lib.rs")],
    );
    let current = manifest(
        &current_generation,
        vec![file_chunks(
            &current_generation,
            "file.a.2",
            "src/lib.rs",
            "//! Revised module docs.",
            "pub fn alpha(value: u32) -> u32 { value + 1 }",
            "pub fn beta(value: u32) -> u32 { value * 2 }",
        )],
    );

    let changes = plan_chunk_increment(Some(&prior), &current).expect("preamble edit plan");

    assert_eq!(changes.added_or_changed.len(), 1);
    assert!(changes.added_or_changed.iter().all(|change| {
        current
            .chunk(&change.chunk_id)
            .is_some_and(|chunk| chunk.anchor.grain == CodeSearchChunkGrainV1::FilePreamble)
    }));
    assert_eq!(changes.reused.len(), current.chunks().len() - 1);
}

#[test]
fn deletion_emits_removed_file_chunks_and_preserves_unchanged_file_reuse() {
    let prior_generation = generation(1);
    let current_generation = generation(2);
    let prior_a = baseline_file(&prior_generation, "file.a.1", "src/a.rs");
    let prior_b = baseline_file(&prior_generation, "file.b.1", "src/b.rs");
    let removed_chunk_count = prior_b.chunks.len();
    let current_a = baseline_file(&current_generation, "file.a.2", "src/a.rs");
    let prior = manifest(&prior_generation, vec![prior_a, prior_b]);
    let current = manifest(&current_generation, vec![current_a]);

    let changes = plan_chunk_increment(Some(&prior), &current).expect("deletion plan");

    assert!(changes.added_or_changed.is_empty());
    assert_eq!(changes.deleted.len(), removed_chunk_count);
    assert_eq!(changes.reused.len(), current.chunks().len());
    assert!(
        changes
            .deleted
            .iter()
            .all(|change| change.prior_digest.is_some() && change.current_digest.is_none())
    );
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
