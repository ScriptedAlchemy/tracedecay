//! Publication must name a divergent reused chunk before accepting its seal.
//!
//! The planner refuses this shape. This test builds the count envelope the
//! planner used to emit, so a publication boundary that only checks cardinality
//! cannot hide the unit.

use std::collections::HashSet;
use std::sync::Arc;

use tracedecay_domain::{
    BoundedSanitizedText, ChangedCodeChunkSetV1, ChunkerRevision, CodeGenerationId,
    CodeSearchChunkAnchorV1, CodeSearchChunkGrainV1, CodeSearchChunkId, CodeSearchChunkV1,
    ContentDigest, FileOccurrenceId, LanguageDescriptorRevision, ManifestDigest, PolicyRevisionId,
    SanitizerRevision, SensitivityDecision, SensitivityLevelV1, SourceSpan,
};

fn identity<T>(value: &str) -> T
where
    T: TryFrom<String>,
    <T as TryFrom<String>>::Error: std::fmt::Debug,
{
    T::try_from(value.to_owned()).expect("fixture identity")
}

fn row(file: &FileOccurrenceId, chunk_id: &str, text: &str) -> Arc<CodeSearchChunkV1> {
    Arc::new(CodeSearchChunkV1 {
        id: identity::<CodeSearchChunkId>(chunk_id),
        anchor: CodeSearchChunkAnchorV1 {
            generation_id: identity::<CodeGenerationId>("generation.row"),
            file_occurrence_id: file.clone(),
            symbol_occurrence_id: None,
            parent_chunk_id: None,
            source_span: SourceSpan {
                start_byte: 0,
                end_byte: text.len() as u64,
            },
            grain: CodeSearchChunkGrainV1::FileWindow,
            ordinal: 0,
        },
        content_digest: ContentDigest::of_bytes(text.as_bytes()),
        language_descriptor_revision: identity::<LanguageDescriptorRevision>("descriptor.v1"),
        chunker_revision: identity::<ChunkerRevision>("chunker.v1"),
        sanitizer_revision: identity::<SanitizerRevision>("sanitizer.v1"),
        sensitivity: SensitivityDecision {
            level: SensitivityLevelV1::Public,
            policy_revision: identity::<PolicyRevisionId>("policy.v1"),
        },
        exact_terms: vec![],
        subtokens: vec![],
        sanitized_text: BoundedSanitizedText::new(text).expect("bounded fixture text"),
    })
}

fn envelope(reused_count: u64) -> ChangedCodeChunkSetV1 {
    let prior = identity::<CodeGenerationId>("generation.prior");
    let current = identity::<CodeGenerationId>("generation.current");
    let parent = ManifestDigest::new(format!("sha256:{}", "a".repeat(64))).expect("parent digest");
    let (reused_count, reused_digest) = ChangedCodeChunkSetV1::seal_arc_shared_reused_partition(
        &parent,
        &prior,
        &current,
        reused_count,
        1,
    )
    .expect("count envelope");
    let mut changes = ChangedCodeChunkSetV1 {
        from_generation: Some(prior),
        to_generation: current,
        manifest_digest: ManifestDigest::new(format!("sha256:{}", "0".repeat(64)))
            .expect("placeholder"),
        added_or_changed: vec![],
        deleted: vec![],
        reused_count,
        reused_digest,
    };
    changes.manifest_digest = changes.compute_digest().expect("manifest digest");
    changes.validate().expect("sealed count envelope");
    changes
}

/// Pointer-equal rows are the known-good unit. A later same-id digest change
/// must be named before the count envelope is accepted, and a still-later
/// divergent row must not be the one reported.
#[test]
fn publication_names_the_divergent_reused_chunk_before_accepting_the_seal() {
    let file = identity::<FileOccurrenceId>("file.shared");
    let stable = row(&file, "chunk.a-stable", "stable");
    let early = row(&file, "chunk.m-diverged", "before");
    let later = row(&file, "chunk.z-later", "before-later");
    let prior = [Arc::clone(&stable), Arc::clone(&early), Arc::clone(&later)];
    let shared = HashSet::from([file.clone()]);
    let carried = prior.clone();

    super::validate_arc_shared_reused_complement(&envelope(3), &prior, &carried, &shared)
        .expect("pointer-equal reused rows are a known-good publication");

    let diverged = [
        stable,
        row(&file, "chunk.m-diverged", "after"),
        row(&file, "chunk.z-later", "after-later"),
    ];
    let error =
        super::validate_arc_shared_reused_complement(&envelope(3), &prior, &diverged, &shared)
            .expect_err("a divergent reused chunk must not pass on cardinality");
    let rendered = error.to_string();
    assert!(
        rendered.contains("chunk.m-diverged"),
        "publication must name the lowest-id divergent chunk, got {rendered}"
    );
    assert!(
        !rendered.contains("chunk.z-later"),
        "a later divergent chunk must not bury the first, got {rendered}"
    );
}
