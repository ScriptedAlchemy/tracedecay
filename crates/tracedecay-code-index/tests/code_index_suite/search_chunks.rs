use std::collections::BTreeSet;

use tracedecay_code_index::capabilities::{
    BaseCapabilityEmitter, BaseCapabilityValidator, CodeIndexCapabilityEmitter,
    CodeIndexCapabilityValidator, capability_manifest_digest, expected_seal_digest,
};
use tracedecay_code_index::chunks::{CodeChunker, DeterministicCodeChunker};
use tracedecay_code_index::extract::{
    LanguageExtractor, MAX_EXTRACTION_SOURCE_BYTES, NeverCancelled, TreeSitterExtractor,
};
use tracedecay_code_index::languages::LanguageRegistry;
use tracedecay_domain::{
    ChunkerRevision, CodeGenerationManifestV1, CodeSearchChunkGrainV1, ComponentVersion,
    CoverageSummaryV1, ExactTechnicalTermKindV1, GenerationSealV1, MAX_CHUNK_TEXT_BYTES,
    PrivacyDomainId, ProjectionKeyV1, ProjectionKindV1, RepositoryId, SanitizationReceiptId,
    SanitizerRevision, UtcMicros,
};

use crate::support::{RUST_SOURCE, digest, id, registry, rust_descriptor, validated_rust_file};

#[test]
fn extraction_to_chunks_is_deterministic_and_covers_all_grains() {
    let file = validated_rust_file(RUST_SOURCE.as_bytes());
    let descriptor = rust_descriptor();
    let batch = TreeSitterExtractor::new()
        .extract(&file, &descriptor, &NeverCancelled)
        .expect("extract source");
    let chunker = DeterministicCodeChunker::new(
        file.generation_id.clone(),
        id::<RepositoryId>("repo.fixture"),
        id::<SanitizerRevision>("sanitizer.v1"),
        id("policy.v1"),
        id::<ChunkerRevision>("chunker.v1"),
        tracedecay_code_index::extraction::LanguageRegistry::new(),
    );

    let first = chunker
        .chunk_file(&file, batch.batch(), &descriptor, &NeverCancelled)
        .expect("chunk source");
    let second = chunker
        .chunk_file(&file, batch.batch(), &descriptor, &NeverCancelled)
        .expect("chunk source again");

    assert_eq!(first, second);
    first.validate().expect("chunk result validates");
    let grains: BTreeSet<_> = first
        .chunks
        .iter()
        .map(|chunk| chunk.anchor.grain)
        .collect();
    assert_eq!(
        grains,
        BTreeSet::from([
            CodeSearchChunkGrainV1::SymbolSignature,
            CodeSearchChunkGrainV1::SymbolBody,
            CodeSearchChunkGrainV1::SymbolMember,
            CodeSearchChunkGrainV1::FilePreamble,
            CodeSearchChunkGrainV1::FileWindow,
        ])
    );
    assert!(first.chunks.iter().any(|chunk| {
        chunk
            .exact_terms
            .iter()
            .any(|term| term.kind() == ExactTechnicalTermKindV1::WholeSymbol)
    }));
}

#[test]
fn partial_extraction_never_chunks_unsupported_tail_bytes() {
    let source = RUST_SOURCE.repeat((MAX_EXTRACTION_SOURCE_BYTES / RUST_SOURCE.len()) + 2);
    let file = validated_rust_file(source.as_bytes());
    let descriptor = rust_descriptor();
    let batch = TreeSitterExtractor::new()
        .extract(&file, &descriptor, &NeverCancelled)
        .expect("bounded extraction");
    let parsed_end = batch.batch().parsed_ranges[0].end_byte;
    let result = DeterministicCodeChunker::new(
        file.generation_id.clone(),
        id::<RepositoryId>("repo.fixture"),
        id::<SanitizerRevision>("sanitizer.v1"),
        id("policy.v1"),
        id::<ChunkerRevision>("chunker.v1"),
        tracedecay_code_index::extraction::LanguageRegistry::new(),
    )
    .chunk_file(&file, batch.batch(), &descriptor, &NeverCancelled)
    .expect("chunk bounded evidence");

    assert!(matches!(
        result.document.eligibility,
        tracedecay_domain::CodeSearchEligibilityV1::Partial { .. }
    ));
    assert!(
        result
            .chunks
            .iter()
            .all(|chunk| chunk.anchor.source_span.end_byte <= parsed_end),
        "unsupported tail bytes must not become searchable chunks"
    );
}

#[test]
fn exact_term_kinds_cover_the_frozen_plan25_contract() {
    let source = "// compiler error: mismatched types\n\
                  // runtime error: module not found\n\
                  // cargo E0308 ERR_MODULE_NOT_FOUND commit:deadbeef\n\
                  // fn comment_fake() {}\n\
                  const TEXT: &str = \"fn string_fake() {}\";\n\
                  // fn\n\
                  // newline_fake\n\
                  // fn;;;punctuation_fake\n\
                  pub fn alpha() {}\n";
    let file = validated_rust_file(source.as_bytes());
    let descriptor = rust_descriptor();
    let batch = TreeSitterExtractor::new()
        .extract(&file, &descriptor, &NeverCancelled)
        .expect("extract exact-term fixture");
    let result = DeterministicCodeChunker::new(
        file.generation_id.clone(),
        id::<RepositoryId>("repo.fixture"),
        id::<SanitizerRevision>("sanitizer.v1"),
        id("policy.v1"),
        id::<ChunkerRevision>("chunker.v1"),
        tracedecay_code_index::extraction::LanguageRegistry::new(),
    )
    .chunk_file(&file, batch.batch(), &descriptor, &NeverCancelled)
    .expect("chunk exact-term fixture");
    let kinds: BTreeSet<_> = result
        .chunks
        .iter()
        .flat_map(|chunk| chunk.exact_terms.iter().map(|term| term.kind()))
        .collect();

    for kind in [
        ExactTechnicalTermKindV1::WholeSymbol,
        ExactTechnicalTermKindV1::CompilerErrorCode,
        ExactTechnicalTermKindV1::CompilerErrorText,
        ExactTechnicalTermKindV1::RuntimeErrorCode,
        ExactTechnicalTermKindV1::RuntimeErrorText,
        ExactTechnicalTermKindV1::ToolName,
        ExactTechnicalTermKindV1::CommitIdentifier,
    ] {
        assert!(kinds.contains(&kind), "missing exact-term kind {kind:?}");
    }

    let symbols: BTreeSet<Vec<u8>> = result
        .chunks
        .iter()
        .flat_map(|chunk| {
            chunk
                .exact_terms
                .iter()
                .filter(|term| term.kind() == ExactTechnicalTermKindV1::WholeSymbol)
                .map(|term| term.original_bytes().to_vec())
        })
        .collect();
    assert!(symbols.contains(b"alpha".as_slice()));
    for rejected in [
        b"comment_fake".as_slice(),
        b"string_fake".as_slice(),
        b"newline_fake".as_slice(),
        b"punctuation_fake".as_slice(),
    ] {
        assert!(!symbols.contains(rejected));
    }
}

#[test]
fn oversized_symbol_bodies_use_bounded_deterministic_fallback_windows() {
    let mut source = String::from("pub fn huge() {\n");
    while source.len() <= MAX_CHUNK_TEXT_BYTES + 4096 {
        source.push_str("    let deterministic_value = 42usize;\n");
    }
    source.push_str("}\n");
    let file = validated_rust_file(source.as_bytes());
    let descriptor = rust_descriptor();
    let batch = TreeSitterExtractor::new()
        .extract(&file, &descriptor, &NeverCancelled)
        .expect("extract oversized body");
    let chunker = DeterministicCodeChunker::new(
        file.generation_id.clone(),
        id::<RepositoryId>("repo.fixture"),
        id::<SanitizerRevision>("sanitizer.v1"),
        id("policy.v1"),
        id::<ChunkerRevision>("chunker.v1"),
        tracedecay_code_index::extraction::LanguageRegistry::new(),
    );

    let first = chunker
        .chunk_file(&file, batch.batch(), &descriptor, &NeverCancelled)
        .expect("chunk oversized body");
    let second = chunker
        .chunk_file(&file, batch.batch(), &descriptor, &NeverCancelled)
        .expect("chunk oversized body again");
    let bodies: Vec<_> = first
        .chunks
        .iter()
        .filter(|chunk| chunk.anchor.grain == CodeSearchChunkGrainV1::SymbolBody)
        .collect();

    assert_eq!(first, second);
    assert!(bodies.len() > 1);
    assert!(
        bodies
            .iter()
            .all(|chunk| chunk.sanitized_text.as_str().len() <= MAX_CHUNK_TEXT_BYTES)
    );
}

#[test]
fn multiple_file_windows_have_unique_stable_ids_and_ordinals() {
    let mut gap = String::new();
    while gap.len() <= MAX_CHUNK_TEXT_BYTES + 4096 {
        gap.push_str("use crate::deterministic_unowned_gap;\n");
    }
    let source =
        format!("pub fn first() {{}}\n{gap}pub fn second() {{}}\n{gap}pub fn third() {{}}\n");
    let file = validated_rust_file(source.as_bytes());
    let descriptor = rust_descriptor();
    let batch = TreeSitterExtractor::new()
        .extract(&file, &descriptor, &NeverCancelled)
        .expect("extract multiple file windows");
    let chunker = DeterministicCodeChunker::new(
        file.generation_id.clone(),
        id::<RepositoryId>("repo.fixture"),
        id::<SanitizerRevision>("sanitizer.v1"),
        id("policy.v1"),
        id::<ChunkerRevision>("chunker.v1"),
        tracedecay_code_index::extraction::LanguageRegistry::new(),
    );

    let first = chunker
        .chunk_file(&file, batch.batch(), &descriptor, &NeverCancelled)
        .expect("chunk multiple file windows");
    let second = chunker
        .chunk_file(&file, batch.batch(), &descriptor, &NeverCancelled)
        .expect("replay multiple file windows");
    let first_windows: Vec<_> = first
        .chunks
        .iter()
        .filter(|chunk| chunk.anchor.grain == CodeSearchChunkGrainV1::FileWindow)
        .map(|chunk| {
            (
                chunk.id.clone(),
                chunk.anchor.ordinal,
                chunk.anchor.source_span,
            )
        })
        .collect();
    let second_windows: Vec<_> = second
        .chunks
        .iter()
        .filter(|chunk| chunk.anchor.grain == CodeSearchChunkGrainV1::FileWindow)
        .map(|chunk| {
            (
                chunk.id.clone(),
                chunk.anchor.ordinal,
                chunk.anchor.source_span,
            )
        })
        .collect();

    assert!(
        first_windows.len() >= 4,
        "fixture must emit several windows"
    );
    assert_eq!(
        first_windows, second_windows,
        "unchanged replay is byte-stable"
    );
    assert_eq!(
        first_windows
            .iter()
            .map(|(id, _, _)| id)
            .collect::<BTreeSet<_>>()
            .len(),
        first_windows.len(),
        "every file window has a unique logical identity"
    );
    assert_eq!(
        first_windows
            .iter()
            .map(|(_, ordinal, _)| ordinal)
            .collect::<BTreeSet<_>>()
            .len(),
        first_windows.len(),
        "every file window has a unique canonical ordinal"
    );
}

#[test]
fn base_capability_manifest_is_deterministic_and_candidate_authorized() {
    let registry = registry();
    let grammar_revisions = registry
        .descriptors()
        .iter()
        .map(|descriptor| {
            (
                descriptor.language.clone(),
                descriptor.grammar_revision.clone(),
            )
        })
        .collect();
    let extractor_revisions = registry
        .descriptors()
        .iter()
        .map(|descriptor| {
            (
                descriptor.language.clone(),
                descriptor.extractor_revision.clone(),
            )
        })
        .collect();
    let privacy_domain = id::<PrivacyDomainId>("privacy.fixture");
    let mut generation = CodeGenerationManifestV1 {
        generation_id: id("generation.v1.aaaaaaaa.00000001"),
        snapshot_digest: digest('a'),
        invalidation_digest: digest('d'),
        registry_revision: registry.registry_revision(),
        grammar_revisions,
        extractor_revisions,
        sanitizer_revision: id("sanitizer.v1"),
        chunker_revision: id("chunker.v1"),
        privacy_domain: privacy_domain.clone(),
        privacy_key_epoch: 7,
        parent_generation: None,
        seal: GenerationSealV1 {
            expected_digest: digest('b'),
            sealed_at: UtcMicros(1_000_000),
            planner: id::<ComponentVersion>("planner.v1"),
        },
    };
    generation.invalidation_digest = generation
        .expected_legacy_invalidation_digest()
        .expect("legacy invalidation digest computes");
    generation.seal.expected_digest =
        expected_seal_digest(&generation).expect("seal digest computes");

    let coverage = CoverageSummaryV1 {
        files_eligible: 1,
        files_excluded: 0,
        files_partial: 0,
        files_unsupported: 0,
        ranges_excluded: 0,
        ranges_unsupported: 0,
    };
    let receipt = id::<SanitizationReceiptId>("receipt.fixture");
    let emitter = BaseCapabilityEmitter::new(registry, coverage, vec![receipt]);
    let first = emitter.emit(&generation).expect("emit capability");
    let second = emitter.emit(&generation).expect("emit capability again");

    assert_eq!(first, second);
    assert_eq!(
        first.manifest_digest,
        capability_manifest_digest(&first).expect("digest recomputes")
    );
    BaseCapabilityValidator::new()
        .authorize_privacy_domain(&privacy_domain, 7)
        .validate_for_candidates(
            &generation.generation_id,
            &ProjectionKeyV1 {
                kind: ProjectionKindV1::Lexical,
                schema_revision: "lexical.v1".to_owned(),
                profile_digest: digest('c'),
            },
            &first,
        )
        .expect("authorized lexical candidate production");

    let mut mixed_registry = generation.clone();
    mixed_registry.registry_revision = id("registry.other.v1");
    mixed_registry.invalidation_digest = mixed_registry
        .expected_legacy_invalidation_digest()
        .expect("mixed invalidation digest computes");
    mixed_registry.seal.expected_digest =
        expected_seal_digest(&mixed_registry).expect("mixed manifest still seals");
    assert_eq!(
        emitter.emit(&mixed_registry),
        Err(tracedecay_code_index::capabilities::CapabilityEmissionErrorV1::MixedGeneration)
    );
}
