use std::fmt::Debug;

use tracedecay_code_index::chunks::{CodeFileChunksV1, content_digest};
use tracedecay_code_index::generations::{
    FileExtractionActionV1, FileExtractionPlanV1, GenerationIncrementPlanV1,
};
use tracedecay_code_index::incremental::{
    ChunkIncrementErrorV1, GenerationChunkManifestV1, materialize_generation_increment,
    plan_chunk_increment,
};
use tracedecay_code_index::lineage::{GenerationSymbolIndexV1, LineageSymbolRecordV1};
use tracedecay_domain::{
    BoundedSanitizedText, ChunkerRevision, CodeGenerationId, CodeSearchChunkAnchorV1,
    CodeSearchChunkGrainV1, CodeSearchChunkId, CodeSearchChunkV1, CodeSearchDocumentV1,
    CodeSearchEligibilityV1, FileIdentityDigest, FileOccurrenceId, LanguageDescriptorRevision,
    LineageKindV1, ManifestDigest, PolicyRevisionId, SanitizerRevision, SensitivityDecision,
    SensitivityLevelV1, SourceSpan, SymbolIdentityDigest, SymbolOccurrenceId,
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
fn carried_chunks_rematerialize_generation_local_occurrence_ids() {
    let prior_generation = generation(1);
    let current_generation = generation(2);
    let prior_file = baseline_file(&prior_generation, "file.a.1", "src/lib.rs");
    let current_file = prior_file
        .rematerialize_for_generation(
            current_generation.clone(),
            id::<FileOccurrenceId>("file.a.2"),
        )
        .expect("rematerialized carried file");

    assert_eq!(current_file.document.generation_id, current_generation);
    assert_eq!(
        current_file.document.file_occurrence_id.as_str(),
        "file.a.2"
    );
    assert_eq!(current_file.chunks.len(), prior_file.chunks.len());
    for (prior_chunk, current_chunk) in prior_file.chunks.iter().zip(&current_file.chunks) {
        assert_eq!(current_chunk.id, prior_chunk.id);
        assert_eq!(current_chunk.content_digest, prior_chunk.content_digest);
        assert_eq!(
            current_chunk.anchor.generation_id,
            current_file.document.generation_id
        );
        assert_eq!(
            current_chunk.anchor.file_occurrence_id,
            current_file.document.file_occurrence_id
        );
        match (
            &prior_chunk.anchor.symbol_occurrence_id,
            &current_chunk.anchor.symbol_occurrence_id,
        ) {
            (Some(prior), Some(current)) => assert_ne!(prior, current),
            (None, None) => {}
            other => panic!("symbol occurrence shape changed during carry-forward: {other:?}"),
        }
    }
    current_file
        .validate()
        .expect("rematerialized file validates");

    let prior = manifest(&prior_generation, vec![prior_file]);
    let current = manifest(&current_generation, vec![current_file]);
    let changes = plan_chunk_increment(Some(&prior), &current).expect("reuse plan");

    assert!(changes.added_or_changed.is_empty());
    assert!(changes.deleted.is_empty());
    assert_eq!(changes.reused.len(), current.chunks().len());
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
                .map(|occurrence| LineageSymbolRecordV1 {
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
                    file_identity: id::<FileIdentityDigest>(&format!("sha256:{}", "f".repeat(64))),
                    content_digest: chunk.content_digest.clone(),
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
